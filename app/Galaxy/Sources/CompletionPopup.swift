import AppKit

@MainActor
final class CompletionPopup: NSView, NSTableViewDataSource, NSTableViewDelegate {
    var onSelect: ((CompletionInfo) -> Void)?
    var onClose: (() -> Void)?
    private var all: [CompletionInfo] = []
    private var results: [CompletionInfo] = []
    private let table = NSTableView()
    private let scroll = NSScrollView()

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 5
        layer?.borderWidth = 1
        layer?.borderColor = NSColor.separatorColor.cgColor
        layer?.shadowOpacity = 0.22
        layer?.shadowRadius = 8
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("completion"))
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 24
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.dataSource = self
        table.delegate = self
        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.translatesAutoresizingMaskIntoConstraints = false
        addSubview(scroll)
        NSLayoutConstraint.activate([
            widthAnchor.constraint(equalToConstant: 420), heightAnchor.constraint(equalToConstant: 192),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor), scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.topAnchor.constraint(equalTo: topAnchor), scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
        setAccessibilityElement(true)
        setAccessibilityRole(.list)
        setAccessibilityIdentifier("completion-popup")
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func update(_ items: [CompletionInfo], query: String) {
        all = items
        let q = query.lowercased()
        results = items.sorted { a, b in score(a.label, query: q) > score(b.label, query: q) }
            .filter { q.isEmpty || score($0.label, query: q) >= 0 }
        table.reloadData()
        if !results.isEmpty { table.selectRowIndexes([0], byExtendingSelection: false); table.scrollRowToVisible(0) }
        isHidden = results.isEmpty
    }

    private func score(_ label: String, query: String) -> Int {
        guard !query.isEmpty else { return 0 }
        var index = label.lowercased().startIndex
        var score = 0
        for character in query {
            guard let found = label.lowercased()[index...].firstIndex(of: character) else { return -1 }
            score += found == index ? 3 : 1
            index = label.lowercased().index(after: found)
        }
        return score
    }

    func move(_ delta: Int) {
        guard !results.isEmpty else { return }
        let current = table.selectedRow < 0 ? 0 : table.selectedRow
        let next = (current + delta + results.count) % results.count
        table.selectRowIndexes([next], byExtendingSelection: false)
        table.scrollRowToVisible(next)
    }

    func accept() { guard table.selectedRow >= 0, results.indices.contains(table.selectedRow) else { return }; onSelect?(results[table.selectedRow]) }
    func cancel() { onClose?() }
    func numberOfRows(in tableView: NSTableView) -> Int { results.count }
    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = (tableView.makeView(withIdentifier: NSUserInterfaceItemIdentifier("completion-cell"), owner: self) as? NSTextField) ?? NSTextField(labelWithString: "")
        cell.identifier = NSUserInterfaceItemIdentifier("completion-cell")
        guard results.indices.contains(row) else { return cell }
        let item = results[row]
        cell.stringValue = item.detail.map { "\(item.label)  \($0)" } ?? item.label
        cell.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        cell.lineBreakMode = .byTruncatingTail
        return cell
    }
    @objc private func clicked(_ sender: Any?) { accept() }
}
