import AppKit

/// Searchable history of folders and files, using the same floating layout as
/// Go to File. Unlike the menu, this shows the entire saved history.
@MainActor
final class RecentItemsPicker: NSView, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    struct Item: Equatable {
        let path: String
        let isFolder: Bool
    }

    static let width: CGFloat = 620
    private static let rowHeight: CGFloat = 34
    private static let maxVisibleRows = 12

    let field = NSTextField()
    let table = NSTableView()
    private let scroll = NSScrollView()
    private let message = NSTextField(labelWithString: "No recently opened items")
    private var heightConstraint: NSLayoutConstraint!
    private(set) var results: [Item] = []
    private var items: [Item] = []
    var onOpen: ((Item) -> Void)?
    var onRemove: ((Item) -> Void)?
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
        setAccessibilityLabel("Recently Opened")
        setAccessibilityIdentifier("recent-items-picker")

        field.placeholderString = "Select to open a recent folder or file"
        field.font = .systemFont(ofSize: 14)
        field.focusRingType = .none
        field.bezelStyle = .roundedBezel
        field.delegate = self
        field.setAccessibilityIdentifier("recent-items-field")
        field.translatesAutoresizingMaskIntoConstraints = false

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("recent-item"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.style = .plain
        table.rowHeight = Self.rowHeight
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.refusesFirstResponder = true
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.doubleAction = #selector(rowClicked(_:))
        table.setAccessibilityIdentifier("recent-items-results")

        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false

        message.font = .systemFont(ofSize: 12)
        message.textColor = .secondaryLabelColor
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

    func show(_ items: [Item], resetQuery: Bool = true) {
        self.items = items
        if resetQuery { field.stringValue = "" }
        filter()
    }

    func focus() { window?.makeFirstResponder(field) }

    func controlTextDidChange(_ obj: Notification) { filter() }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.moveUp(_:)): move(by: -1)
        case #selector(NSResponder.moveDown(_:)): move(by: 1)
        case #selector(NSResponder.insertNewline(_:)): openSelected()
        case #selector(NSResponder.cancelOperation(_:)): onClose?()
        default: return false
        }
        return true
    }

    private func filter() {
        let query = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        results = query.isEmpty ? items : items.filter {
            ($0.path as NSString).abbreviatingWithTildeInPath.localizedStandardContains(query)
        }
        table.reloadData()
        if !results.isEmpty { table.selectRowIndexes([0], byExtendingSelection: false) }
        scroll.isHidden = results.isEmpty
        message.isHidden = !results.isEmpty
        message.stringValue = items.isEmpty ? "No recently opened items" : "No matching items"
        heightConstraint.constant = results.isEmpty ? 76 : 8 + 28 + 6 + CGFloat(min(results.count, Self.maxVisibleRows)) * Self.rowHeight + 4
    }

    private func move(by delta: Int) {
        guard !results.isEmpty else { return }
        let next = (max(table.selectedRow, 0) + delta + results.count) % results.count
        table.selectRowIndexes([next], byExtendingSelection: false)
        table.scrollRowToVisible(next)
    }

    private func openSelected() {
        guard results.indices.contains(table.selectedRow) else { return }
        onOpen?(results[table.selectedRow])
    }

    @objc private func rowClicked(_ sender: Any?) {
        guard results.indices.contains(table.clickedRow) else { return }
        table.selectRowIndexes([table.clickedRow], byExtendingSelection: false)
        openSelected()
    }

    func numberOfRows(in tableView: NSTableView) -> Int { results.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let item = results[row]
        let cell = tableView.makeView(withIdentifier: NSUserInterfaceItemIdentifier("recent-cell"), owner: self) as? RecentItemCell
            ?? RecentItemCell()
        cell.configure(item)
        cell.onRemove = { [weak self] in self?.onRemove?(item) }
        return cell
    }
}

@MainActor
private final class RecentItemCell: NSTableCellView {
    private let icon = NSImageView()
    private let title = NSTextField(labelWithString: "")
    private let location = NSTextField(labelWithString: "")
    private let remove = NSButton()
    var onRemove: (() -> Void)?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        identifier = NSUserInterfaceItemIdentifier("recent-cell")
        icon.imageScaling = .scaleProportionallyDown
        icon.contentTintColor = .secondaryLabelColor
        title.font = .systemFont(ofSize: 13)
        title.lineBreakMode = .byTruncatingMiddle
        location.font = .systemFont(ofSize: 11)
        location.textColor = .secondaryLabelColor
        location.lineBreakMode = .byTruncatingHead
        remove.image = NSImage(systemSymbolName: "xmark", accessibilityDescription: "Remove from Recently Opened")
        remove.isBordered = false
        remove.contentTintColor = .secondaryLabelColor
        remove.target = self
        remove.action = #selector(removeClicked(_:))
        remove.toolTip = "Remove from Recently Opened"
        for view in [icon, title, location, remove] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 16),
            icon.heightAnchor.constraint(equalToConstant: 16),
            title.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 8),
            title.centerYAnchor.constraint(equalTo: centerYAnchor),
            location.leadingAnchor.constraint(equalTo: title.trailingAnchor, constant: 8),
            location.centerYAnchor.constraint(equalTo: centerYAnchor),
            location.trailingAnchor.constraint(lessThanOrEqualTo: remove.leadingAnchor, constant: -8),
            remove.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            remove.centerYAnchor.constraint(equalTo: centerYAnchor),
            remove.widthAnchor.constraint(equalToConstant: 24),
        ])
        title.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        location.setContentCompressionResistancePriority(.defaultHigh, for: .horizontal)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func configure(_ item: RecentItemsPicker.Item) {
        let url = URL(fileURLWithPath: item.path)
        icon.image = NSImage(systemSymbolName: item.isFolder ? "folder" : "doc", accessibilityDescription: nil)
        title.stringValue = url.lastPathComponent
        location.stringValue = (url.deletingLastPathComponent().path as NSString).abbreviatingWithTildeInPath
        toolTip = item.path
    }

    @objc private func removeClicked(_ sender: Any?) { onRemove?() }
}
