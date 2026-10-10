import AppKit

/// Go to Symbol (Shift-Command-O): filters the active document's outline and
/// reports the chosen source location to `MainWindowController`.
@MainActor
final class OutlinePicker: NSView, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    static let width: CGFloat = 620
    static let rowHeight: CGFloat = 26
    static let maxVisibleRows = 12

    let field = NSTextField()
    let table = NSTableView()
    let message = NSTextField(labelWithString: "No matching symbols")
    private let scroll = NSScrollView()
    private var heightConstraint: NSLayoutConstraint!

    private(set) var symbols: [OutlineSymbol] = []
    private(set) var results: [OutlineSymbol] = []
    var onGo: ((OutlineSymbol) -> Void)?
    var onQueryChange: ((String) -> Void)?
    var onClose: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        shadow = NSShadow()
        layer?.cornerRadius = 8
        layer?.borderWidth = 1
        layer?.shadowOpacity = 0.28
        layer?.shadowRadius = 14
        layer?.shadowOffset = NSSize(width: 0, height: -4)
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Go to Symbol")
        setAccessibilityIdentifier("outline-picker")

        field.placeholderString = "Search symbols by name"
        field.font = .systemFont(ofSize: 14)
        field.focusRingType = .none
        field.bezelStyle = .roundedBezel
        field.delegate = self
        field.setAccessibilityIdentifier("outline-picker-field")
        field.translatesAutoresizingMaskIntoConstraints = false

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("symbol"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.style = .plain
        table.rowSizeStyle = .custom
        table.rowHeight = Self.rowHeight
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.refusesFirstResponder = true
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.action = #selector(rowClicked(_:))
        table.setAccessibilityIdentifier("outline-picker-results")

        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false

        message.font = .systemFont(ofSize: 12)
        message.textColor = .secondaryLabelColor
        message.isHidden = true
        message.setAccessibilityIdentifier("outline-picker-message")
        message.translatesAutoresizingMaskIntoConstraints = false

        for view in [field, scroll, message] { addSubview(view) }
        heightConstraint = heightAnchor.constraint(equalToConstant: 76)
        NSLayoutConstraint.activate([
            heightConstraint,
            field.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            field.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            field.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            scroll.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 6),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -4),
            message.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 8),
            message.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        layer?.backgroundColor = Theme.sidebar.cgColor
        layer?.borderColor = NSColor.separatorColor.cgColor
    }

    var selectedIndex: Int? { table.selectedRow >= 0 ? table.selectedRow : nil }

    func reset(symbols: [OutlineSymbol]) {
        self.symbols = symbols
        field.stringValue = ""
        runSearch()
    }

    func focus() { window?.makeFirstResponder(field) }

    func setQuery(_ text: String) {
        field.stringValue = text
        runSearch()
    }

    private func runSearch() {
        let query = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        results = query.isEmpty ? symbols : symbols.filter {
            $0.name.range(of: query, options: .caseInsensitive) != nil
        }
        table.reloadData()
        if !results.isEmpty {
            table.selectRowIndexes([0], byExtendingSelection: false)
            table.scrollRowToVisible(0)
        }
        message.isHidden = !results.isEmpty
        scroll.isHidden = results.isEmpty
        let rows = min(results.count, Self.maxVisibleRows)
        heightConstraint.constant = results.isEmpty ? 76 : 8 + 28 + 6 + CGFloat(rows) * Self.rowHeight + 4
        setAccessibilityValue(results.isEmpty ? message.stringValue : "\(results.count) symbols")
    }

    func controlTextDidChange(_ obj: Notification) { runSearch(); onQueryChange?(field.stringValue) }

    func controlTextDidEndEditing(_ obj: Notification) {
        if window?.firstResponder !== field.currentEditor() { onClose?() }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.moveUp(_:)): move(by: -1)
        case #selector(NSResponder.moveDown(_:)): move(by: 1)
        case #selector(NSResponder.pageUp(_:)), #selector(NSResponder.scrollPageUp(_:)):
            move(by: -Self.maxVisibleRows)
        case #selector(NSResponder.pageDown(_:)), #selector(NSResponder.scrollPageDown(_:)):
            move(by: Self.maxVisibleRows)
        case #selector(NSResponder.insertNewline(_:)): goSelected()
        case #selector(NSResponder.cancelOperation(_:)): onClose?()
        default: return false
        }
        return true
    }

    func move(by delta: Int) {
        guard !results.isEmpty else { return }
        var next = (selectedIndex ?? 0) + delta
        if abs(delta) == 1 {
            next = (next + results.count) % results.count
        } else {
            next = min(max(next, 0), results.count - 1)
        }
        table.selectRowIndexes([next], byExtendingSelection: false)
        table.scrollRowToVisible(next)
    }

    func goSelected() {
        guard let i = selectedIndex, results.indices.contains(i) else { return NSSound.beep() }
        onGo?(results[i])
    }

    @objc private func rowClicked(_ sender: Any?) {
        guard table.clickedRow >= 0 else { return }
        table.selectRowIndexes([table.clickedRow], byExtendingSelection: false)
        goSelected()
    }

    func numberOfRows(in tableView: NSTableView) -> Int { results.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = tableView.makeView(withIdentifier: OutlinePickerCell.reuseID, owner: self) as? OutlinePickerCell
            ?? OutlinePickerCell()
        cell.show(results[row])
        return cell
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        tableView.makeView(withIdentifier: QuickOpenRowView.reuseID, owner: self) as? QuickOpenRowView
            ?? QuickOpenRowView()
    }
}

@MainActor
final class OutlinePickerCell: NSTableCellView {
    static let reuseID = NSUserInterfaceItemIdentifier("outline-picker-cell")

    let name = NSTextField(labelWithString: "")
    let detail = NSTextField(labelWithString: "")

    init() {
        super.init(frame: .zero)
        identifier = Self.reuseID
        name.lineBreakMode = .byTruncatingTail
        detail.font = .systemFont(ofSize: 11.5)
        detail.textColor = .secondaryLabelColor
        detail.setContentCompressionResistancePriority(.required, for: .horizontal)
        textField = name

        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let row = NSStackView(views: [name, spacer, detail])
        row.spacing = 10
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func show(_ symbol: OutlineSymbol) {
        name.stringValue = symbol.name
        detail.stringValue = "\(symbol.kind), line \(symbol.line + 1)"
        setAccessibilityLabel("\(symbol.name), \(detail.stringValue)")
    }
}
