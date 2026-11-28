import AppKit

/// A SQLite file in an editor tab: tables and views on the left; on the
/// right their rows (editable in place), their structure, or a SQL console.
/// Every edit is written to the file at once.
@MainActor
final class DatabaseView: NSView, NSTableViewDataSource, NSTableViewDelegate, NSMenuDelegate {
    enum Mode: Int { case data, structure, query }

    static let pageSize: UInt64 = 200
    static let maxQueryRows: UInt32 = 1000

    private(set) var database: SqliteDatabase?
    private(set) var objects: [SqliteObject] = []
    private(set) var selectedObject: SqliteObject?
    private(set) var page: SqlitePage?
    private(set) var mode: Mode = .data
    private var tableColumns: [SqliteColumn] = []
    private var sort: (column: String, descending: Bool)?
    private var queryTask: Task<Void, Never>?
    /// Shows the confirmation and Add Row alerts; tests answer them directly.
    var runModal: (NSAlert) -> NSApplication.ModalResponse = { $0.runModal() }

    private let split = NSSplitView()
    private let sidebar = NSTableView()
    private let sidebarScroll = NSScrollView()
    private let content = NSView()
    let modeControl = NSSegmentedControl(labels: ["Data", "Structure", "SQL"],
                                         trackingMode: .selectOne, target: nil, action: nil)
    let addButton = NSButton(title: "Add Row", target: nil, action: nil)
    let deleteButton = NSButton(title: "Delete", target: nil, action: nil)
    let refreshButton = NSButton(title: "Refresh", target: nil, action: nil)
    let previousButton = NSButton(title: "‹", target: nil, action: nil)
    let nextButton = NSButton(title: "›", target: nil, action: nil)
    let pageLabel = NSTextField(labelWithString: "")
    /// Errors and the outcome of the last write or query.
    let messageLabel = NSTextField(labelWithString: "")

    let dataGrid = DatabaseGrid()
    let structureGrid = DatabaseGrid()
    let structureText = NSTextView()
    private let structureScroll = NSScrollView()
    private let structureSplit = NSSplitView()

    let queryText = NSTextView()
    private let queryScroll = NSScrollView()
    let runButton = NSButton(title: "Run", target: nil, action: nil)
    let resultGrid = DatabaseGrid()
    private let querySplit = NSSplitView()

    /// One line for the status bar.
    private(set) var summary = ""

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityIdentifier("database-view")
        buildSidebar()
        buildContent()
        split.isVertical = true
        split.dividerStyle = .thin
        split.addArrangedSubview(sidebarScroll)
        split.addArrangedSubview(content)
        split.setHoldingPriority(.defaultHigh, forSubviewAt: 0)
        addSubview(split)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    // MARK: Loading

    /// Opens `url` and shows its first table. Returns an error message
    /// when the file cannot be opened as a database.
    @discardableResult
    func load(url: URL) -> String? {
        queryTask?.cancel()
        database = nil
        objects = []
        selectedObject = nil
        page = nil
        sort = nil
        messageLabel.stringValue = ""
        do {
            let database = try SqliteDatabase.open(path: url.path)
            self.database = database
            objects = try database.objects()
        } catch {
            sidebar.reloadData()
            summary = "Cannot open \(url.lastPathComponent): \(Self.describe(error))"
            return summary
        }
        sidebar.reloadData()
        let size = ((try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber)
            .map { ByteCountFormatter.string(fromByteCount: $0.int64Value, countStyle: .file) }
        let tables = objects.filter { !$0.isView }.count
        summary = "SQLite database, \(tables) \(tables == 1 ? "table" : "tables")"
            + (size.map { ", \($0)" } ?? "")
            + (database?.isReadOnly() == true ? " (read-only)" : "")
        if objects.isEmpty {
            select(object: nil)
            setMode(.query)
        } else {
            // Start on a table: views are read-only and usually secondary.
            let first = objects.firstIndex { !$0.isView } ?? 0
            sidebar.selectRowIndexes([first], byExtendingSelection: false)
            select(object: objects[first])
        }
        return nil
    }

    static func describe(_ error: Error) -> String {
        if case CoreError.Io(let message) = error { return message }
        return error.localizedDescription
    }

    private func report(_ error: Error) {
        show(message: Self.describe(error), isError: true)
    }

    private func show(message: String, isError: Bool = false) {
        messageLabel.stringValue = message
        messageLabel.textColor = isError ? .systemRed : .secondaryLabelColor
    }

    /// Shows `object` in the current mode; `nil` empties the right side.
    func select(object: SqliteObject?) {
        selectedObject = object
        sort = nil
        page = nil
        tableColumns = []
        if let object, let database {
            tableColumns = (try? database.columns(table: object.name)) ?? []
        }
        if mode == .query { refreshQueryControls() } else { reloadSelected(offset: 0) }
    }

    /// Reloads the current table or structure, keeping the page when it exists.
    func reloadSelected(offset: UInt64? = nil) {
        refreshControls()
        guard let database, let object = selectedObject else {
            dataGrid.show(columns: [], rows: [])
            structureGrid.show(columns: [], rows: [])
            structureText.string = ""
            pageLabel.stringValue = ""
            return
        }
        switch mode {
        case .data:
            let start = offset ?? page?.offset ?? 0
            do {
                let page = try database.page(table: object.name, offset: start, limit: Self.pageSize,
                                             sortColumn: sort?.column, descending: sort?.descending ?? false)
                // Deleting the last rows of the last page leaves it empty.
                if page.rows.isEmpty, start > 0, page.total > 0 {
                    let last = (page.total - 1) / Self.pageSize * Self.pageSize
                    return reloadSelected(offset: last)
                }
                self.page = page
                dataGrid.show(columns: page.columns, rows: page.rows.map(\.values),
                              sortable: true, sort: sort)
            } catch {
                page = nil
                dataGrid.show(columns: [], rows: [])
                report(error)
            }
        case .structure:
            showStructure(of: object, in: database)
        case .query:
            break
        }
        refreshControls()
    }

    private func showStructure(of object: SqliteObject, in database: SqliteDatabase) {
        structureGrid.show(
            columns: ["Name", "Type", "Not Null", "Default", "Primary Key"],
            rows: tableColumns.map { column in
                [.text(value: column.name),
                 .text(value: column.declaredType + (column.generated ? " (generated)" : "")),
                 .text(value: column.notNull ? "yes" : ""),
                 column.defaultValue.map { .text(value: $0) } ?? .null,
                 .text(value: column.primaryKey > 0 ? String(column.primaryKey) : "")]
            })
        var sql = object.sql + ";"
        let indexes = (try? database.indexes(table: object.name)) ?? []
        for index in indexes {
            sql += "\n\n" + (index.sql.map { $0 + ";" }
                ?? "-- \(index.name): \(index.unique ? "unique " : "")index on (\(index.columns.joined(separator: ", ")))")
        }
        structureText.string = sql
    }

    // MARK: Controls

    private var isEditable: Bool {
        guard let page, let database else { return false }
        return !page.keyColumns.isEmpty && !database.isReadOnly()
    }

    private func refreshControls() {
        let data = mode == .data
        for view in [addButton, deleteButton, previousButton, nextButton, pageLabel] as [NSView] {
            view.isHidden = !data
        }
        refreshButton.isHidden = mode == .query
        runButton.isHidden = mode != .query
        addButton.isEnabled = isEditable
        deleteButton.isEnabled = isEditable && !dataGrid.tableView.selectedRowIndexes.isEmpty
        if let page, data {
            let first = page.rows.isEmpty ? 0 : page.offset + 1
            let last = page.offset + UInt64(page.rows.count)
            pageLabel.stringValue = "\(first)–\(last) of \(page.total)"
                + (page.keyColumns.isEmpty ? " (read-only)" : "")
            previousButton.isEnabled = page.offset > 0
            nextButton.isEnabled = last < page.total
        } else {
            pageLabel.stringValue = ""
            previousButton.isEnabled = false
            nextButton.isEnabled = false
        }
        dataGrid.scrollView.isHidden = mode != .data
        structureSplit.isHidden = mode != .structure
        querySplit.isHidden = mode != .query
        modeControl.selectedSegment = mode.rawValue
        modeControl.setEnabled(selectedObject != nil, forSegment: Mode.data.rawValue)
        modeControl.setEnabled(selectedObject != nil, forSegment: Mode.structure.rawValue)
    }

    private func refreshQueryControls() {
        refreshControls()
        if queryText.string.isEmpty, let object = selectedObject {
            queryText.string = "SELECT * FROM \(Self.quote(object.name)) LIMIT 100;"
        }
    }

    static func quote(_ name: String) -> String {
        "\"" + name.replacingOccurrences(of: "\"", with: "\"\"") + "\""
    }

    func setMode(_ mode: Mode) {
        self.mode = mode
        messageLabel.stringValue = ""
        if mode == .query {
            refreshQueryControls()
            window?.makeFirstResponder(queryText)
        } else {
            reloadSelected(offset: 0)
        }
    }

    @objc private func modeChanged(_ sender: NSSegmentedControl) {
        setMode(Mode(rawValue: sender.selectedSegment) ?? .data)
    }

    @objc func refresh(_ sender: Any?) {
        guard let database else { return }
        let name = selectedObject?.name
        objects = (try? database.objects()) ?? objects
        sidebar.reloadData()
        if let name, let index = objects.firstIndex(where: { $0.name == name }) {
            sidebar.selectRowIndexes([index], byExtendingSelection: false)
            selectedObject = objects[index]
            tableColumns = (try? database.columns(table: name)) ?? []
            reloadSelected()
        } else {
            select(object: objects.first)
            if !objects.isEmpty { sidebar.selectRowIndexes([0], byExtendingSelection: false) }
        }
    }

    @objc private func previousPage(_ sender: Any?) {
        guard let page else { return }
        reloadSelected(offset: page.offset >= Self.pageSize ? page.offset - Self.pageSize : 0)
    }

    @objc private func nextPage(_ sender: Any?) {
        guard let page else { return }
        reloadSelected(offset: page.offset + Self.pageSize)
    }

    func focus() {
        window?.makeFirstResponder(mode == .query ? queryText : dataGrid.tableView)
    }

    override func layout() {
        super.layout()
        split.frame = bounds
    }

    override func draw(_ dirtyRect: NSRect) {
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        dirtyRect.intersection(bounds).fill()
    }
}

// MARK: Layout

extension DatabaseView {
    private func buildSidebar() {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("object"))
        column.title = "Tables"
        sidebar.addTableColumn(column)
        sidebar.headerView = nil
        sidebar.style = .sourceList
        sidebar.rowHeight = 22
        sidebar.dataSource = self
        sidebar.delegate = self
        sidebar.setAccessibilityIdentifier("database-objects")
        sidebarScroll.documentView = sidebar
        sidebarScroll.hasVerticalScroller = true
        sidebarScroll.autohidesScrollers = true
        sidebarScroll.drawsBackground = false
        sidebarScroll.translatesAutoresizingMaskIntoConstraints = false
        sidebarScroll.widthAnchor.constraint(greaterThanOrEqualToConstant: 140).isActive = true
        let preferred = sidebarScroll.widthAnchor.constraint(equalToConstant: 200)
        preferred.priority = .defaultLow
        preferred.isActive = true
    }

    private func buildContent() {
        modeControl.target = self
        modeControl.action = #selector(modeChanged(_:))
        modeControl.selectedSegment = 0
        modeControl.setAccessibilityIdentifier("database-mode")
        for (button, action, id) in [
            (addButton, #selector(addRow(_:)), "database-add-row"),
            (deleteButton, #selector(deleteSelectedRows(_:)), "database-delete-rows"),
            (refreshButton, #selector(refresh(_:)), "database-refresh"),
            (previousButton, #selector(previousPage(_:)), "database-previous-page"),
            (nextButton, #selector(nextPage(_:)), "database-next-page"),
            (runButton, #selector(runQuery(_:)), "database-run"),
        ] as [(NSButton, Selector, String)] {
            button.target = self
            button.action = action
            button.bezelStyle = .rounded
            button.controlSize = .small
            button.setAccessibilityIdentifier(id)
        }
        runButton.keyEquivalent = "\r"
        runButton.keyEquivalentModifierMask = .command
        runButton.toolTip = "Run (⌘↩)"
        pageLabel.font = .systemFont(ofSize: 11)
        pageLabel.textColor = .secondaryLabelColor
        messageLabel.font = .systemFont(ofSize: 11)
        messageLabel.lineBreakMode = .byTruncatingTail
        messageLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        messageLabel.setAccessibilityIdentifier("database-message")

        let spacer = NSView()
        spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
        let bar = NSStackView(views: [modeControl, addButton, deleteButton, refreshButton, runButton,
                                      messageLabel, spacer, previousButton, pageLabel, nextButton])
        bar.orientation = .horizontal
        bar.spacing = 8
        bar.edgeInsets = NSEdgeInsets(top: 6, left: 8, bottom: 6, right: 8)
        bar.translatesAutoresizingMaskIntoConstraints = false

        // Data
        dataGrid.tableView.setAccessibilityIdentifier("database-rows")
        dataGrid.canEdit = { [weak self] row, column in self?.canEdit(row: row, column: column) ?? false }
        dataGrid.onEdit = { [weak self] row, column, text in self?.write(row: row, column: column, text: text) }
        dataGrid.onSort = { [weak self] column, descending in
            self?.sort = column.map { ($0, descending) }
            self?.reloadSelected(offset: 0)
        }
        let menu = NSMenu()
        menu.delegate = self
        dataGrid.tableView.menu = menu
        NotificationCenter.default.addObserver(
            self, selector: #selector(rowSelectionChanged(_:)),
            name: NSTableView.selectionDidChangeNotification, object: dataGrid.tableView)

        // Structure
        configure(textView: structureText, in: structureScroll, editable: false)
        structureGrid.tableView.allowsMultipleSelection = false
        structureSplit.isVertical = false
        structureSplit.dividerStyle = .thin
        structureSplit.addArrangedSubview(structureGrid.scrollView)
        structureSplit.addArrangedSubview(structureScroll)

        // SQL
        configure(textView: queryText, in: queryScroll, editable: true)
        queryText.setAccessibilityIdentifier("database-query")
        resultGrid.tableView.setAccessibilityIdentifier("database-results")
        querySplit.isVertical = false
        querySplit.dividerStyle = .thin
        querySplit.addArrangedSubview(queryScroll)
        querySplit.addArrangedSubview(resultGrid.scrollView)

        content.addSubview(bar)
        for view in [dataGrid.scrollView, structureSplit, querySplit] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(view)
            NSLayoutConstraint.activate([
                view.topAnchor.constraint(equalTo: bar.bottomAnchor),
                view.leadingAnchor.constraint(equalTo: content.leadingAnchor),
                view.trailingAnchor.constraint(equalTo: content.trailingAnchor),
                view.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            ])
        }
        NSLayoutConstraint.activate([
            bar.topAnchor.constraint(equalTo: content.topAnchor),
            bar.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            bar.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            queryScroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 80),
            structureScroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 80),
        ])
        refreshControls()
    }

    private func configure(textView: NSTextView, in scroll: NSScrollView, editable: Bool) {
        textView.isEditable = editable
        textView.isRichText = false
        textView.allowsUndo = editable
        textView.font = Theme.editorFont
        textView.textColor = Theme.editorForeground
        textView.backgroundColor = Theme.editorBackground
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticTextReplacementEnabled = false
        textView.isAutomaticSpellingCorrectionEnabled = false
        textView.isContinuousSpellCheckingEnabled = false
        textView.textContainerInset = NSSize(width: 6, height: 6)
        textView.isVerticallyResizable = true
        textView.autoresizingMask = [.width]
        textView.textContainer?.widthTracksTextView = true
        scroll.documentView = textView
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
    }
}

// MARK: Sidebar

extension DatabaseView {
    func numberOfRows(in tableView: NSTableView) -> Int { objects.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard row < objects.count else { return nil }
        let object = objects[row]
        let id = NSUserInterfaceItemIdentifier("database-object")
        let cell = (tableView.makeView(withIdentifier: id, owner: self) as? NSTableCellView) ?? {
            let cell = NSTableCellView()
            cell.identifier = id
            let image = NSImageView()
            let text = NSTextField(labelWithString: "")
            text.lineBreakMode = .byTruncatingTail
            for view in [image, text] as [NSView] {
                view.translatesAutoresizingMaskIntoConstraints = false
                cell.addSubview(view)
            }
            NSLayoutConstraint.activate([
                image.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2),
                image.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                image.widthAnchor.constraint(equalToConstant: 16),
                text.leadingAnchor.constraint(equalTo: image.trailingAnchor, constant: 6),
                text.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -2),
                text.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
            cell.imageView = image
            cell.textField = text
            return cell
        }()
        cell.textField?.stringValue = object.name
        cell.imageView?.image = NSImage(systemSymbolName: object.isView ? "eye" : "tablecells",
                                        accessibilityDescription: object.isView ? "View" : "Table")
        cell.imageView?.contentTintColor = .secondaryLabelColor
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard notification.object as? NSTableView === sidebar else { return }
        let row = sidebar.selectedRow
        let object = row >= 0 && row < objects.count ? objects[row] : nil
        if object?.name != selectedObject?.name {
            if mode == .query, object != nil { mode = .data }
            select(object: object)
        }
    }
}

// MARK: Editing

extension DatabaseView {
    private func columnInfo(_ name: String) -> SqliteColumn? {
        tableColumns.first { $0.name == name }
    }

    private func canEdit(row: Int, column: Int) -> Bool {
        guard isEditable, let page, row < page.rows.count, column < page.columns.count else { return false }
        if page.rows[row].values[column].isBlob { return false }
        return columnInfo(page.columns[column])?.generated != true
    }

    /// Writes typed text over a cell. Returns the stored value, or `nil`
    /// (with the reason shown) when SQLite refused it.
    private func write(row: Int, column: Int, text: String) -> SqliteValue? {
        guard let database, let object = selectedObject, let page,
              row < page.rows.count, column < page.columns.count else { return nil }
        let value = sqliteValueFromInput(text: text, previous: page.rows[row].values[column])
        return write(value, row: row, column: column, table: object.name, page: page, database: database)
    }

    private func write(_ value: SqliteValue, row: Int, column: Int, table: String,
                       page: SqlitePage, database: SqliteDatabase) -> SqliteValue? {
        let name = page.columns[column]
        do {
            try database.updateCell(table: table, key: page.rows[row].key, column: name, value: value)
        } catch {
            report(error)
            return nil
        }
        // Read the row back: a trigger or type affinity can store something
        // other than what was typed.
        let fresh = try? database.page(table: table, offset: page.offset, limit: Self.pageSize,
                                       sortColumn: sort?.column, descending: sort?.descending ?? false)
        show(message: "Saved \(name).")
        if let fresh, fresh.rows.count == page.rows.count, fresh.rows[row].key == page.rows[row].key {
            self.page = fresh
            return fresh.rows[row].values[column]
        }
        // The edit moved the row (a key or the sorted column changed). Let
        // the grid finish this edit, then show the page as it is now.
        var updated = page
        updated.rows[row].values[column] = value
        self.page = updated
        DispatchQueue.main.async { [weak self] in self?.reloadSelected() }
        return value
    }

    @objc private func rowSelectionChanged(_ notification: Notification) {
        refreshControls()
    }

    /// Asks for the new row's values; empty fields take the column default.
    @objc func addRow(_ sender: Any?) {
        guard isEditable, let database, let object = selectedObject else { return }
        let columns = tableColumns.filter { !$0.generated }
        let alert = NSAlert()
        alert.messageText = "Add Row to \(object.name)"
        alert.informativeText = "Leave a field empty to use the column's default value."
        alert.addButton(withTitle: "Add")
        alert.addButton(withTitle: "Cancel")
        let grid = NSGridView()
        grid.rowSpacing = 6
        grid.columnSpacing = 8
        var fields: [(SqliteColumn, NSTextField)] = []
        for column in columns {
            let label = NSTextField(labelWithString: column.name)
            label.alignment = .right
            let field = NSTextField()
            field.placeholderString = Self.placeholder(for: column)
            field.widthAnchor.constraint(equalToConstant: 260).isActive = true
            field.setAccessibilityIdentifier("database-new-\(column.name)")
            grid.addRow(with: [label, field])
            fields.append((column, field))
        }
        alert.accessoryView = grid
        grid.layoutSubtreeIfNeeded()
        grid.frame.size = grid.fittingSize
        if let first = fields.first?.1 { alert.window.initialFirstResponder = first }
        guard runModal(alert) == .alertFirstButtonReturn else { return }
        let values = fields.compactMap { column, field -> SqliteCellValue? in
            let text = field.stringValue
            guard !text.isEmpty else { return nil }
            return SqliteCellValue(column: column.name,
                                   value: sqliteValueFromInput(text: text, previous: Self.typedSample(for: column)))
        }
        do {
            try database.insertRow(table: object.name, values: values)
            sort = nil
            // New rows have the highest rowid and land on the last page.
            let total = (try? database.page(table: object.name, offset: 0, limit: 0,
                                            sortColumn: nil, descending: false).total) ?? 0
            reloadSelected(offset: total == 0 ? 0 : (total - 1) / Self.pageSize * Self.pageSize)
            show(message: "Added a row to \(object.name).")
        } catch {
            report(error)
        }
    }

    private static func placeholder(for column: SqliteColumn) -> String {
        var parts = [column.declaredType.isEmpty ? "any" : column.declaredType]
        if column.primaryKey > 0, column.declaredType.uppercased() == "INTEGER" { parts.append("auto") }
        if let value = column.defaultValue { parts.append("default \(value)") }
        if column.notNull, column.defaultValue == nil, column.primaryKey == 0 { parts.append("required") }
        return parts.joined(separator: ", ")
    }

    /// A value of the column's numeric type, so typed numbers stay numbers.
    private static func typedSample(for column: SqliteColumn) -> SqliteValue {
        let type = column.declaredType.uppercased()
        if type.contains("INT") { return .integer(value: 0) }
        if type.contains("REAL") || type.contains("FLOA") || type.contains("DOUB") { return .real(value: 0) }
        return .null
    }

    /// Deletes the selected rows after asking.
    @objc func deleteSelectedRows(_ sender: Any?) {
        guard isEditable, let database, let object = selectedObject, let page else { return }
        let rows = dataGrid.tableView.selectedRowIndexes.filter { $0 < page.rows.count }
        guard !rows.isEmpty else { return }
        let alert = NSAlert()
        alert.messageText = "Delete \(rows.count) \(rows.count == 1 ? "row" : "rows") from \(object.name)?"
        alert.informativeText = "This is written to the database file and cannot be undone."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Delete")
        alert.addButton(withTitle: "Cancel")
        alert.buttons.first?.hasDestructiveAction = true
        guard runModal(alert) == .alertFirstButtonReturn else { return }
        do {
            let deleted = try database.deleteRows(table: object.name, keys: rows.map { page.rows[$0].key })
            dataGrid.tableView.deselectAll(nil)
            reloadSelected()
            show(message: "Deleted \(deleted) \(deleted == 1 ? "row" : "rows").")
        } catch {
            report(error)
        }
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let table = dataGrid.tableView
        let row = table.clickedRow, column = table.clickedColumn
        guard row >= 0, let page, row < page.rows.count else { return }
        if !table.selectedRowIndexes.contains(row) {
            table.selectRowIndexes([row], byExtendingSelection: false)
        }
        if column >= 0, column < page.columns.count {
            let copy = NSMenuItem(title: "Copy Value", action: #selector(copyValue(_:)), keyEquivalent: "")
            copy.target = self
            copy.representedObject = [row, column]
            menu.addItem(copy)
            let null = NSMenuItem(title: "Set to NULL", action: #selector(setNull(_:)), keyEquivalent: "")
            null.target = self
            null.representedObject = [row, column]
            null.isEnabled = canEdit(row: row, column: column) && !page.rows[row].values[column].isNull
            menu.addItem(null)
            menu.addItem(.separator())
        }
        let delete = NSMenuItem(title: "Delete Row\(table.selectedRowIndexes.count == 1 ? "" : "s")…",
                                action: #selector(deleteSelectedRows(_:)), keyEquivalent: "")
        delete.target = self
        delete.isEnabled = isEditable
        menu.addItem(delete)
        menu.autoenablesItems = false
    }

    @objc private func copyValue(_ sender: NSMenuItem) {
        guard let cell = sender.representedObject as? [Int], let page,
              cell[0] < page.rows.count, cell[1] < page.columns.count else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(page.rows[cell[0]].values[cell[1]].displayText, forType: .string)
    }

    @objc private func setNull(_ sender: NSMenuItem) {
        guard let cell = sender.representedObject as? [Int], let database, let object = selectedObject,
              let page, cell[0] < page.rows.count, cell[1] < page.columns.count else { return }
        if let value = write(.null, row: cell[0], column: cell[1], table: object.name, page: page, database: database) {
            dataGrid.replace(row: cell[0], column: cell[1], with: value)
        }
    }
}

// MARK: SQL

extension DatabaseView {
    /// Runs the selected SQL, or all of it, off the main thread.
    @objc func runQuery(_ sender: Any?) {
        guard let database, queryTask == nil else { return }
        let range = queryText.selectedRange()
        let all = queryText.string as NSString
        let sql = range.length > 0 ? all.substring(with: range) : all as String
        guard !sql.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        show(message: "Running…")
        runButton.isEnabled = false
        let maxRows = Self.maxQueryRows
        queryTask = Task { [weak self] in
            let outcome = await Task.detached(priority: .userInitiated) {
                Result { try database.execute(sql: sql, maxRows: maxRows) }
            }.value
            guard let self, self.database === database else { return }
            self.queryTask = nil
            self.runButton.isEnabled = true
            self.finishQuery(outcome)
        }
    }

    /// Waits for a running query; for tests.
    func waitForQuery() async {
        await queryTask?.value
    }

    private func finishQuery(_ outcome: Result<SqliteQueryResult, Error>) {
        switch outcome {
        case .failure(let error):
            report(error)
        case .success(let result):
            resultGrid.show(columns: result.columns, rows: result.rows)
            var parts: [String] = []
            if !result.columns.isEmpty {
                parts.append("\(result.rows.count)\(result.truncated ? "+" : "") \(result.rows.count == 1 ? "row" : "rows")")
            }
            if result.changes > 0 || result.columns.isEmpty {
                parts.append("\(result.changes) \(result.changes == 1 ? "row" : "rows") changed")
            }
            if result.truncated { parts.append("showing the first \(Self.maxQueryRows)") }
            show(message: parts.joined(separator: ", ") + ".")
            if result.schemaChanged || result.changes > 0 {
                let name = selectedObject?.name
                objects = (try? database?.objects()) ?? objects
                sidebar.reloadData()
                if let name, let index = objects.firstIndex(where: { $0.name == name }) {
                    sidebar.selectRowIndexes([index], byExtendingSelection: false)
                    selectedObject = objects[index]
                    tableColumns = (try? database?.columns(table: name)) ?? []
                } else {
                    selectedObject = nil
                    page = nil
                }
                // The data tab reloads when shown again.
                page = nil
                refreshControls()
            }
        }
    }
}
