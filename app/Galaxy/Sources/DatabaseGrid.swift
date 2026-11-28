import AppKit

extension SqliteValue {
    var isNull: Bool { if case .null = self { true } else { false } }
    var isBlob: Bool { if case .blob = self { true } else { false } }

    /// Text shown in a grid cell, and copied by Copy Value.
    var displayText: String {
        switch self {
        case .null: "NULL"
        case .integer(let value): String(value)
        case .real(let value): String(value)
        case .text(let value): value
        case .blob(let len, let bytes):
            "BLOB \(ByteCountFormatter.string(fromByteCount: Int64(len), countStyle: .binary))"
                + (bytes.isEmpty ? "" : " " + bytes.prefix(16).map { String(format: "%02x", $0) }.joined()
                    + (len > 16 ? "…" : ""))
        }
    }
}

/// A table of SQLite values with one column per result column. Cells the
/// owner allows become editable text fields; finished edits are reported
/// through `onEdit` and the grid shows the new value only if it is applied.
@MainActor
final class DatabaseGrid: NSObject, NSTableViewDataSource, NSTableViewDelegate, NSTextFieldDelegate {
    let tableView = NSTableView()
    let scrollView = NSScrollView()

    private(set) var columns: [String] = []
    private(set) var rows: [[SqliteValue]] = []
    /// Whether the cell at row and column may be edited.
    var canEdit: (Int, Int) -> Bool = { _, _ in false }
    /// A finished edit: row, column, the typed text. Returns the value that
    /// was written, which the grid then shows, or `nil` when it failed.
    var onEdit: ((Int, Int, String) -> SqliteValue?)?
    /// A header click; `nil` key clears the sort.
    var onSort: ((String?, Bool) -> Void)?

    private static let cellID = NSUserInterfaceItemIdentifier("database-cell")

    override init() {
        super.init()
        tableView.dataSource = self
        tableView.delegate = self
        tableView.usesAlternatingRowBackgroundColors = true
        tableView.allowsMultipleSelection = true
        tableView.allowsColumnReordering = false
        tableView.columnAutoresizingStyle = .noColumnAutoresizing
        tableView.style = .plain
        tableView.gridStyleMask = [.solidVerticalGridLineMask]
        tableView.intercellSpacing = NSSize(width: 6, height: 2)
        tableView.rowHeight = 20
        scrollView.documentView = tableView
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.drawsBackground = false
    }

    /// Replaces the shown result. `sortable` adds header sorting.
    func show(columns: [String], rows: [[SqliteValue]], sortable: Bool = false,
              sort: (column: String, descending: Bool)? = nil) {
        if columns != self.columns {
            for column in tableView.tableColumns { tableView.removeTableColumn(column) }
            for (index, name) in columns.enumerated() {
                let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("c\(index)"))
                column.title = name
                column.minWidth = 40
                column.width = Self.width(for: name, rows: rows, index: index)
                tableView.addTableColumn(column)
            }
        }
        for (index, column) in tableView.tableColumns.enumerated() {
            column.sortDescriptorPrototype = sortable
                ? NSSortDescriptor(key: columns[index], ascending: true) : nil
        }
        self.columns = columns
        self.rows = rows
        // Setting the descriptors calls back into `sortDescriptorsDidChange`.
        let descriptors = sort.map { [NSSortDescriptor(key: $0.column, ascending: !$0.descending)] } ?? []
        if tableView.sortDescriptors != descriptors {
            let handler = onSort
            onSort = nil
            tableView.sortDescriptors = descriptors
            onSort = handler
        }
        tableView.reloadData()
    }

    private static func width(for name: String, rows: [[SqliteValue]], index: Int) -> CGFloat {
        let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        let samples = rows.prefix(50).map { $0[index].displayText.prefix(60) }
        let widest = samples.map { ($0 as NSString).size(withAttributes: [.font: font]).width }.max() ?? 0
        let title = (name as NSString).size(withAttributes: [.font: NSFont.systemFont(ofSize: 12, weight: .semibold)]).width
        return min(360, max(60, widest, title + 24) + 12)
    }

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let tableColumn, let column = tableView.tableColumns.firstIndex(of: tableColumn),
              row < rows.count, column < rows[row].count else { return nil }
        let field = (tableView.makeView(withIdentifier: Self.cellID, owner: self) as? DatabaseCellField) ?? {
            let field = DatabaseCellField()
            field.identifier = Self.cellID
            field.isBordered = false
            field.drawsBackground = false
            field.lineBreakMode = .byTruncatingTail
            field.cell?.usesSingleLineMode = true
            field.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
            field.delegate = self
            return field
        }()
        let value = rows[row][column]
        // Newlines would grow the row; the full text shows while editing.
        field.show(display: value.displayText.replacingOccurrences(of: "\n", with: " "),
                   editing: value.isNull ? "" : value.displayText,
                   dimmed: value.isNull || value.isBlob)
        field.isEditable = canEdit(row, column)
        field.setAccessibilityIdentifier("database-cell-\(row)-\(column)")
        return field
    }

    func tableView(_ tableView: NSTableView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
        let first = tableView.sortDescriptors.first
        onSort?(first?.key, first.map { !$0.ascending } ?? false)
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        guard let field = notification.object as? NSTextField else { return }
        let row = tableView.row(for: field), column = tableView.column(for: field)
        guard row >= 0, column >= 0, row < rows.count, column < rows[row].count else { return }
        let previous = rows[row][column]
        let text = field.stringValue
        let unchanged = previous.isNull ? text.isEmpty : text == previous.displayText
        if !unchanged, let applied = onEdit?(row, column, text) {
            rows[row][column] = applied
        }
        tableView.reloadData(forRowIndexes: [row], columnIndexes: [column])
    }

    /// Sets a cell without an edit, after Set to NULL.
    func replace(row: Int, column: Int, with value: SqliteValue) {
        guard row < rows.count, column < rows[row].count else { return }
        rows[row][column] = value
        tableView.reloadData(forRowIndexes: [row], columnIndexes: [column])
    }
}

/// A grid cell that shows a one-line summary and edits the full value: NULL
/// reads "NULL" but edits as empty text, and newlines come back.
@MainActor
final class DatabaseCellField: NSTextField {
    private var editingText = ""
    private var dimmed = false

    func show(display: String, editing: String, dimmed: Bool) {
        stringValue = display
        editingText = editing
        self.dimmed = dimmed
        textColor = dimmed ? .tertiaryLabelColor : .labelColor
    }

    override func becomeFirstResponder() -> Bool {
        guard isEditable else { return super.becomeFirstResponder() }
        stringValue = editingText
        textColor = .labelColor
        return super.becomeFirstResponder()
    }
}
