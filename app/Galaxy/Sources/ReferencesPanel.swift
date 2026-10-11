import AppKit

@MainActor
final class ReferencesPanel: NSView, NSTableViewDataSource, NSTableViewDelegate {
    var onSelect: ((DefinitionLocation) -> Void)?
    private var locations: [DefinitionLocation] = []
    private let title = NSTextField(labelWithString: "References")
    private let count = NSTextField(labelWithString: "0")
    private let table = NSTableView()
    private let scroll = NSScrollView()
    private let empty = NSTextField(labelWithString: "No references")

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true; layer?.backgroundColor = Theme.sidebar.cgColor
        title.font = .boldSystemFont(ofSize: 12); count.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular); count.textColor = .secondaryLabelColor
        let header = NSStackView(views: [title, count]); header.spacing = 6; header.translatesAutoresizingMaskIntoConstraints = false
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("reference")); column.resizingMask = .autoresizingMask; table.addTableColumn(column)
        table.headerView = nil; table.rowHeight = 25; table.intercellSpacing = .zero; table.backgroundColor = .clear; table.dataSource = self; table.delegate = self; table.target = self; table.action = #selector(clicked(_:)); table.setAccessibilityIdentifier("references-results")
        scroll.documentView = table; scroll.drawsBackground = false; scroll.hasVerticalScroller = true; scroll.scrollerStyle = .overlay; scroll.translatesAutoresizingMaskIntoConstraints = false
        empty.font = .systemFont(ofSize: 11); empty.textColor = .secondaryLabelColor; empty.translatesAutoresizingMaskIntoConstraints = false
        addSubview(header); addSubview(scroll); addSubview(empty)
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: topAnchor, constant: 7), header.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10), header.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            scroll.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 4), scroll.leadingAnchor.constraint(equalTo: leadingAnchor), scroll.trailingAnchor.constraint(equalTo: trailingAnchor), scroll.bottomAnchor.constraint(equalTo: bottomAnchor), empty.centerXAnchor.constraint(equalTo: centerXAnchor), empty.centerYAnchor.constraint(equalTo: scroll.centerYAnchor)
        ])
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func update(_ locations: [DefinitionLocation]) {
        self.locations = locations; count.stringValue = "\(locations.count)"; table.reloadData(); empty.isHidden = !locations.isEmpty; scroll.isHidden = locations.isEmpty
    }
    func numberOfRows(in tableView: NSTableView) -> Int { locations.count }
    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = (tableView.makeView(withIdentifier: NSUserInterfaceItemIdentifier("reference-cell"), owner: self) as? NSTextField) ?? NSTextField(labelWithString: "")
        cell.identifier = NSUserInterfaceItemIdentifier("reference-cell")
        guard locations.indices.contains(row), let path = URL(string: locations[row].uri)?.path else { return cell }
        cell.stringValue = "\((path as NSString).lastPathComponent):\(locations[row].line + 1):\(locations[row].column + 1)"
        cell.font = .systemFont(ofSize: 11); cell.lineBreakMode = .byTruncatingTail; return cell
    }
    @objc private func clicked(_ sender: Any?) { guard table.clickedRow >= 0, locations.indices.contains(table.clickedRow) else { return }; onSelect?(locations[table.clickedRow]) }
}
