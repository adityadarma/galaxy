import AppKit

/// Quick pick for the PHP analyzer target version, opened from the `PHP`
/// status bar item. Mirrors the layout of `OutlinePicker`.
@MainActor
final class PHPVersionPicker: NSView, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    struct Entry {
        let version: String?
        let title: String
        let detail: String
    }

    static let width: CGFloat = 620
    static let rowHeight: CGFloat = 28

    static let entries = [
        Entry(version: nil, title: "Composer/default", detail: "Use Composer platform PHP or PHP 8.5"),
        Entry(version: "8.5", title: "PHP 8.5", detail: "Latest supported target"),
        Entry(version: "8.4", title: "PHP 8.4", detail: "Property hooks, parentheseless new"),
        Entry(version: "8.3", title: "PHP 8.3", detail: "Typed class constants, dynamic class constants"),
        Entry(version: "8.2", title: "PHP 8.2", detail: "Readonly classes, DNF types"),
        Entry(version: "8.1", title: "PHP 8.1", detail: "Enums, intersection types, readonly properties"),
        Entry(version: "8.0", title: "PHP 8.0", detail: "Attributes, union types, match, nullsafe operator"),
    ]

    let field = NSTextField()
    let table = NSTableView()
    let message = NSTextField(labelWithString: "No matching PHP versions")
    private let scroll = NSScrollView()
    private var heightConstraint: NSLayoutConstraint!

    private(set) var selected: String?
    private(set) var results: [Entry] = []
    var onSelect: ((String?) -> Void)?
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
        setAccessibilityLabel("Select PHP Target Version")
        setAccessibilityIdentifier("php-version-picker")

        field.placeholderString = "Select PHP target version"
        field.font = .systemFont(ofSize: 14)
        field.focusRingType = .none
        field.bezelStyle = .roundedBezel
        field.delegate = self
        field.setAccessibilityIdentifier("php-version-picker-field")
        field.translatesAutoresizingMaskIntoConstraints = false

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("version"))
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
        table.setAccessibilityIdentifier("php-version-picker-results")

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
        message.setAccessibilityIdentifier("php-version-picker-message")
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

    func reset(selected: String?) {
        self.selected = selected
        field.stringValue = ""
        runSearch()
    }

    func focus() { window?.makeFirstResponder(field) }

    private func runSearch() {
        let query = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        results = query.isEmpty ? Self.entries : Self.entries.filter {
            "\($0.title) \($0.detail)".range(of: query, options: .caseInsensitive) != nil
        }
        table.reloadData()
        if !results.isEmpty {
            // Start on the current target so Enter keeps it, like VS Code.
            let current = query.isEmpty ? (results.firstIndex { $0.version == selected } ?? 0) : 0
            table.selectRowIndexes([current], byExtendingSelection: false)
            table.scrollRowToVisible(current)
        }
        message.isHidden = !results.isEmpty
        scroll.isHidden = results.isEmpty
        heightConstraint.constant = results.isEmpty ? 76 : 8 + 28 + 6 + CGFloat(results.count) * Self.rowHeight + 4
        setAccessibilityValue(results.isEmpty ? message.stringValue : "\(results.count) versions")
    }

    func controlTextDidChange(_ obj: Notification) { runSearch() }

    func controlTextDidEndEditing(_ obj: Notification) {
        if window?.firstResponder !== field.currentEditor() { onClose?() }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.moveUp(_:)): move(by: -1)
        case #selector(NSResponder.moveDown(_:)): move(by: 1)
        case #selector(NSResponder.insertNewline(_:)): chooseSelected()
        case #selector(NSResponder.cancelOperation(_:)): onClose?()
        default: return false
        }
        return true
    }

    func move(by delta: Int) {
        guard !results.isEmpty else { return }
        let next = ((selectedIndex ?? 0) + delta + results.count) % results.count
        table.selectRowIndexes([next], byExtendingSelection: false)
        table.scrollRowToVisible(next)
    }

    func chooseSelected() {
        guard let i = selectedIndex, results.indices.contains(i) else { return NSSound.beep() }
        onSelect?(results[i].version)
    }

    @objc private func rowClicked(_ sender: Any?) {
        guard table.clickedRow >= 0 else { return }
        table.selectRowIndexes([table.clickedRow], byExtendingSelection: false)
        chooseSelected()
    }

    func numberOfRows(in tableView: NSTableView) -> Int { results.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = tableView.makeView(withIdentifier: PHPVersionPickerCell.reuseID, owner: self) as? PHPVersionPickerCell
            ?? PHPVersionPickerCell()
        let entry = results[row]
        cell.show(entry, current: entry.version == selected)
        return cell
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        tableView.makeView(withIdentifier: QuickOpenRowView.reuseID, owner: self) as? QuickOpenRowView
            ?? QuickOpenRowView()
    }
}

@MainActor
final class PHPVersionPickerCell: NSTableCellView {
    static let reuseID = NSUserInterfaceItemIdentifier("php-version-picker-cell")
    static let titleWidth: CGFloat = 140

    let check = NSImageView()
    let title = NSTextField(labelWithString: "")
    let detail = NSTextField(labelWithString: "")
    let badge = NSTextField(labelWithString: "Current")

    init() {
        super.init(frame: .zero)
        identifier = Self.reuseID
        check.image = NSImage(systemSymbolName: "checkmark", accessibilityDescription: nil)
        check.symbolConfiguration = .init(pointSize: 11, weight: .semibold)
        check.contentTintColor = Theme.accent
        title.font = .systemFont(ofSize: 13)
        title.lineBreakMode = .byTruncatingTail
        detail.font = .systemFont(ofSize: 12)
        detail.textColor = .secondaryLabelColor
        detail.lineBreakMode = .byTruncatingTail
        detail.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        badge.font = .systemFont(ofSize: 11)
        badge.textColor = .secondaryLabelColor
        badge.setContentCompressionResistancePriority(.required, for: .horizontal)
        badge.setContentHuggingPriority(.required, for: .horizontal)
        textField = title

        for view in [check, title, detail, badge] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            check.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            check.centerYAnchor.constraint(equalTo: centerYAnchor),
            check.widthAnchor.constraint(equalToConstant: 14),
            title.leadingAnchor.constraint(equalTo: check.trailingAnchor, constant: 8),
            title.centerYAnchor.constraint(equalTo: centerYAnchor),
            title.widthAnchor.constraint(equalToConstant: Self.titleWidth),
            detail.leadingAnchor.constraint(equalTo: title.trailingAnchor, constant: 8),
            detail.centerYAnchor.constraint(equalTo: centerYAnchor),
            detail.trailingAnchor.constraint(lessThanOrEqualTo: badge.leadingAnchor, constant: -10),
            badge.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            badge.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func show(_ entry: PHPVersionPicker.Entry, current: Bool) {
        title.stringValue = entry.title
        title.font = .systemFont(ofSize: 13, weight: current ? .semibold : .regular)
        detail.stringValue = entry.detail
        check.isHidden = !current
        badge.isHidden = !current
        setAccessibilityLabel(current ? "\(entry.title), \(entry.detail), current" : "\(entry.title), \(entry.detail)")
    }
}

enum IndentationAction {
    case useTabs(Bool)
    case tabSize(UInt32)
    case convertToTabs(Bool)
}

/// VS Code-style indentation menu opened from the status bar.
@MainActor
final class IndentationPicker: NSView, NSTableViewDataSource, NSTableViewDelegate, NSTextFieldDelegate {
    struct Entry {
        let title: String
        let detail: String
        let action: IndentationAction?
    }

    static let width: CGFloat = 520
    private let field = NSTextField()
    private let table = NSTableView()
    private let scroll = NSScrollView()
    private var entries: [Entry] = []
    private var results: [Entry] = []
    private var useTabs = false
    private var tabSize: UInt32 = 4
    private var choosingSize = false
    var onSelect: ((IndentationAction) -> Void)?
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
        setAccessibilityLabel("Select Indentation")
        setAccessibilityIdentifier("indentation-picker")

        field.placeholderString = "Select indentation"
        field.font = .systemFont(ofSize: 14)
        field.focusRingType = .none
        field.bezelStyle = .roundedBezel
        field.delegate = self
        field.translatesAutoresizingMaskIntoConstraints = false

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("indentation"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.style = .plain
        table.rowHeight = 30
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.refusesFirstResponder = true
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.action = #selector(rowClicked(_:))

        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.borderType = .noBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false

        addSubview(field)
        addSubview(scroll)
        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 244),
            field.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            field.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            field.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            scroll.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 6),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -4),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        layer?.backgroundColor = Theme.sidebar.cgColor
        layer?.borderColor = NSColor.separatorColor.cgColor
    }

    func reset(useTabs: Bool, tabSize: UInt32) {
        self.useTabs = useTabs
        self.tabSize = tabSize
        choosingSize = false
        field.stringValue = ""
        rebuild()
    }

    func focus() { window?.makeFirstResponder(field) }

    private func rebuild() {
        if choosingSize {
            field.placeholderString = "Select tab size"
            entries = (1 ... 8).map {
                Entry(title: "Tab Size: \($0)", detail: UInt32($0) == tabSize ? "Current" : "", action: .tabSize(UInt32($0)))
            }
        } else {
            field.placeholderString = "Select indentation"
            entries = [
                Entry(title: "Indent Using Spaces", detail: useTabs ? "" : "Current", action: .useTabs(false)),
                Entry(title: "Indent Using Tabs", detail: useTabs ? "Current" : "", action: .useTabs(true)),
                Entry(title: "Change Tab Size", detail: "Current: \(tabSize)", action: nil),
                Entry(title: "Convert Indentation to Spaces", detail: "Rewrite leading tabs", action: .convertToTabs(false)),
                Entry(title: "Convert Indentation to Tabs", detail: "Rewrite leading spaces", action: .convertToTabs(true)),
            ]
        }
        filter()
    }

    private func filter() {
        let query = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        results = query.isEmpty ? entries : entries.filter {
            "\($0.title) \($0.detail)".localizedCaseInsensitiveContains(query)
        }
        table.reloadData()
        if !results.isEmpty {
            table.selectRowIndexes([0], byExtendingSelection: false)
            table.scrollRowToVisible(0)
        }
    }

    func controlTextDidChange(_ obj: Notification) { filter() }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.moveUp(_:)): move(by: -1)
        case #selector(NSResponder.moveDown(_:)): move(by: 1)
        case #selector(NSResponder.insertNewline(_:)): chooseSelected()
        case #selector(NSResponder.cancelOperation(_:)):
            if choosingSize { choosingSize = false; field.stringValue = ""; rebuild() } else { onClose?() }
        default: return false
        }
        return true
    }

    private func move(by delta: Int) {
        guard !results.isEmpty else { return }
        let current = max(table.selectedRow, 0)
        let next = (current + delta + results.count) % results.count
        table.selectRowIndexes([next], byExtendingSelection: false)
        table.scrollRowToVisible(next)
    }

    private func chooseSelected() {
        let row = table.selectedRow
        guard results.indices.contains(row) else { return NSSound.beep() }
        let entry = results[row]
        if entry.title == "Change Tab Size" {
            choosingSize = true
            field.stringValue = ""
            rebuild()
        } else if let action = entry.action {
            onSelect?(action)
        }
    }

    @objc private func rowClicked(_ sender: Any?) {
        guard table.clickedRow >= 0 else { return }
        table.selectRowIndexes([table.clickedRow], byExtendingSelection: false)
        chooseSelected()
    }

    func numberOfRows(in tableView: NSTableView) -> Int { results.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = tableView.makeView(withIdentifier: OutlinePickerCell.reuseID, owner: self) as? OutlinePickerCell
            ?? OutlinePickerCell()
        let entry = results[row]
        cell.name.stringValue = entry.title
        cell.detail.stringValue = entry.detail
        cell.setAccessibilityLabel(entry.detail.isEmpty ? entry.title : "\(entry.title), \(entry.detail)")
        return cell
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        tableView.makeView(withIdentifier: QuickOpenRowView.reuseID, owner: self) as? QuickOpenRowView
            ?? QuickOpenRowView()
    }
}
