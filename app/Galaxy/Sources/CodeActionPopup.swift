import AppKit

@MainActor
final class CodeActionPopup: NSView, NSTableViewDataSource, NSTableViewDelegate {
    var onSelect: ((CodeActionInfo) -> Void)?
    var onClose: (() -> Void)?
    private var actions: [CodeActionInfo] = []
    private let table = NSTableView()
    private let scroll = NSScrollView()

    override init(frame: NSRect) {
        super.init(frame: frame); wantsLayer = true; layer?.cornerRadius = 5; layer?.borderWidth = 1; layer?.borderColor = NSColor.separatorColor.cgColor; layer?.shadowOpacity = 0.22; layer?.shadowRadius = 8
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("action")); table.addTableColumn(column); table.headerView = nil; table.rowHeight = 26; table.intercellSpacing = .zero; table.backgroundColor = .clear; table.dataSource = self; table.delegate = self; table.target = self; table.action = #selector(clicked(_:))
        scroll.documentView = table; scroll.drawsBackground = false; scroll.hasVerticalScroller = true; scroll.scrollerStyle = .overlay; scroll.translatesAutoresizingMaskIntoConstraints = false; addSubview(scroll)
        NSLayoutConstraint.activate([widthAnchor.constraint(equalToConstant: 420), heightAnchor.constraint(equalToConstant: 156), scroll.leadingAnchor.constraint(equalTo: leadingAnchor), scroll.trailingAnchor.constraint(equalTo: trailingAnchor), scroll.topAnchor.constraint(equalTo: topAnchor), scroll.bottomAnchor.constraint(equalTo: bottomAnchor)])
        setAccessibilityElement(true); setAccessibilityRole(.list); setAccessibilityIdentifier("code-action-popup")
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }
    func update(_ actions: [CodeActionInfo]) { self.actions = actions; table.reloadData(); isHidden = actions.isEmpty; if !actions.isEmpty { table.selectRowIndexes([0], byExtendingSelection: false) } }
    func move(_ delta: Int) { guard !actions.isEmpty else { return }; let row = (max(0, table.selectedRow) + delta + actions.count) % actions.count; table.selectRowIndexes([row], byExtendingSelection: false); table.scrollRowToVisible(row) }
    func accept() { guard actions.indices.contains(table.selectedRow) else { return }; onSelect?(actions[table.selectedRow]) }
    func cancel() { onClose?() }
    func numberOfRows(in tableView: NSTableView) -> Int { actions.count }
    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? { let cell = NSTextField(labelWithString: actions.indices.contains(row) ? actions[row].title : ""); cell.font = .systemFont(ofSize: 12); cell.lineBreakMode = .byTruncatingTail; return cell }
    @objc private func clicked(_ sender: Any?) { accept() }
}
