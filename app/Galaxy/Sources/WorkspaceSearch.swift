import AppKit

@MainActor
final class WorkspaceSearch: NSView, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    static let width: CGFloat = 680
    static let rowHeight: CGFloat = 40

    let field = NSTextField()
    let includeField = NSTextField()
    let excludeField = NSTextField()
    let regexButton = NSButton(checkboxWithTitle: "Regex", target: nil, action: nil)
    let caseButton = NSButton(checkboxWithTitle: "Match case", target: nil, action: nil)
    let wordButton = NSButton(checkboxWithTitle: "Whole word", target: nil, action: nil)
    let table = NSTableView()
    let message = NSTextField(labelWithString: "")
    private let scroll = NSScrollView()
    private var heightConstraint: NSLayoutConstraint!
    private(set) var results: [WorkspaceSearchMatch] = []
    var onSearch: ((String, WorkspaceSearchOptions) -> Void)?
    var onOpen: ((WorkspaceSearchMatch) -> Void)?
    var onClose: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 8
        layer?.borderWidth = 1
        layer?.borderColor = NSColor.separatorColor.cgColor
        layer?.backgroundColor = Theme.sidebar.cgColor
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Workspace Search")
        setAccessibilityIdentifier("workspace-search")

        field.placeholderString = "Search in workspace"
        field.font = .systemFont(ofSize: 14)
        field.focusRingType = .none
        field.bezelStyle = .roundedBezel
        field.delegate = self
        field.setAccessibilityIdentifier("workspace-search-field")
        field.translatesAutoresizingMaskIntoConstraints = false
        includeField.placeholderString = "Include (glob, comma separated)"
        excludeField.placeholderString = "Exclude (glob, comma separated)"
        for control in [includeField, excludeField] {
            control.font = .systemFont(ofSize: 12)
            control.focusRingType = .none
            control.bezelStyle = .roundedBezel
            control.delegate = self
            control.translatesAutoresizingMaskIntoConstraints = false
        }
        for button in [regexButton, caseButton, wordButton] {
            button.setButtonType(.switch)
            button.target = self
            button.action = #selector(optionsChanged(_:))
        }

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("match"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.style = .plain
        table.rowHeight = Self.rowHeight
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.action = #selector(rowClicked(_:))
        table.setAccessibilityIdentifier("workspace-search-results")

        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false

        message.font = .systemFont(ofSize: 12)
        message.textColor = .secondaryLabelColor
        message.setAccessibilityIdentifier("workspace-search-message")
        message.translatesAutoresizingMaskIntoConstraints = false

        let options = NSStackView(views: [regexButton, caseButton, wordButton, includeField, excludeField])
        options.spacing = 8
        options.translatesAutoresizingMaskIntoConstraints = false
        addSubview(field); addSubview(options); addSubview(scroll); addSubview(message)
        heightConstraint = heightAnchor.constraint(equalToConstant: 88)
        NSLayoutConstraint.activate([
            heightConstraint,
            field.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            field.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            field.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            options.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 6),
            options.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            options.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            includeField.widthAnchor.constraint(equalToConstant: 150),
            excludeField.widthAnchor.constraint(equalToConstant: 150),
            scroll.topAnchor.constraint(equalTo: options.bottomAnchor, constant: 6),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -4),
            message.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 10),
            message.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func reset() {
        field.stringValue = ""
        includeField.stringValue = ""
        excludeField.stringValue = ""
        regexButton.state = .off; caseButton.state = .off; wordButton.state = .off
        show([], message: "Type to search the workspace")
    }

    func show(_ results: [WorkspaceSearchMatch], message: String? = nil) {
        self.results = results
        table.reloadData()
        table.deselectAll(nil)
        let visible = min(results.count, 10)
        heightConstraint.constant = results.isEmpty ? 88 : 88 + CGFloat(visible) * Self.rowHeight
        self.message.stringValue = message ?? (results.isEmpty ? "No results" : "")
        self.message.isHidden = !results.isEmpty
    }

    func focus() {
        window?.makeFirstResponder(field)
        field.selectText(nil)
    }

    func controlTextDidChange(_ obj: Notification) { searchChanged() }

    @objc private func optionsChanged(_ sender: Any?) { searchChanged() }

    private func searchChanged() {
        onSearch?(field.stringValue, WorkspaceSearchOptions(
            regex: regexButton.state == .on,
            caseSensitive: caseButton.state == .on,
            wholeWord: wordButton.state == .on,
            include: includeField.stringValue,
            exclude: excludeField.stringValue))
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        switch commandSelector {
        case #selector(NSResponder.cancelOperation(_:)): onClose?(); return true
        case #selector(NSResponder.insertNewline(_:)):
            if let row = table.selectedRow >= 0 ? table.selectedRow : (results.isEmpty ? nil : 0) { onOpen?(results[row]) }
            return true
        case #selector(NSResponder.moveDown(_:)): select(offset: 1); return true
        case #selector(NSResponder.moveUp(_:)): select(offset: -1); return true
        default: return false
        }
    }

    private func select(offset: Int) {
        guard !results.isEmpty else { return }
        let current = max(table.selectedRow, 0)
        table.selectRowIndexes(IndexSet(integer: min(max(current + offset, 0), results.count - 1)), byExtendingSelection: false)
        table.scrollRowToVisible(table.selectedRow)
    }

    func numberOfRows(in tableView: NSTableView) -> Int { results.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = WorkspaceSearchCell()
        cell.path.stringValue = results[row].relPath
        cell.detail.stringValue = "\(results[row].line + 1):\(results[row].column + 1)  \(results[row].text.trimmingCharacters(in: .whitespacesAndNewlines))"
        return cell
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool { true }

    @objc private func rowClicked(_ sender: Any?) {
        guard table.clickedRow >= 0, table.clickedRow < results.count else { return }
        onOpen?(results[table.clickedRow])
    }
}

@MainActor
private final class WorkspaceSearchCell: NSTableCellView {
    let path = NSTextField(labelWithString: "")
    let detail = NSTextField(labelWithString: "")

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        path.font = .systemFont(ofSize: 13)
        detail.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        detail.textColor = .secondaryLabelColor
        let stack = NSStackView(views: [path, detail])
        stack.orientation = .vertical; stack.alignment = .leading; stack.spacing = 2
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }
}
