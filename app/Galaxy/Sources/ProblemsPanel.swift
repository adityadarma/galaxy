import AppKit

@MainActor
final class ProblemsPanel: NSView, NSTableViewDataSource, NSTableViewDelegate {
    var onSelect: ((ProblemInfo) -> Void)?
    private var problems: [ProblemInfo] = []
    private let title = NSTextField(labelWithString: "Problems")
    private let count = NSTextField(labelWithString: "0")
    private let table = NSTableView()
    private let scroll = NSScrollView()
    private let empty = NSTextField(labelWithString: "No problems")

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.backgroundColor = Theme.sidebar.cgColor
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Problems")
        setAccessibilityIdentifier("problems-panel")

        title.font = .boldSystemFont(ofSize: 12)
        count.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        count.textColor = .secondaryLabelColor
        let header = NSStackView(views: [title, count])
        header.spacing = 6
        header.translatesAutoresizingMaskIntoConstraints = false

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("problem"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 25
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.action = #selector(clicked(_:))
        table.setAccessibilityIdentifier("problems-results")

        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.translatesAutoresizingMaskIntoConstraints = false
        empty.font = .systemFont(ofSize: 11)
        empty.textColor = .secondaryLabelColor
        empty.translatesAutoresizingMaskIntoConstraints = false

        addSubview(header)
        addSubview(scroll)
        addSubview(empty)
        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 150),
            header.topAnchor.constraint(equalTo: topAnchor, constant: 7),
            header.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            header.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            scroll.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 4),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            empty.centerXAnchor.constraint(equalTo: centerXAnchor),
            empty.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func update(_ problems: [ProblemInfo]) {
        self.problems = problems
        count.stringValue = "\(problems.count)"
        table.reloadData()
        empty.isHidden = !problems.isEmpty
        scroll.isHidden = problems.isEmpty
        setAccessibilityValue(problems.isEmpty ? "No problems" : "\(problems.count) problems")
    }

    func numberOfRows(in tableView: NSTableView) -> Int { problems.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = (tableView.makeView(withIdentifier: NSUserInterfaceItemIdentifier("problem-cell"), owner: self) as? NSTextField)
            ?? NSTextField(labelWithString: "")
        cell.identifier = NSUserInterfaceItemIdentifier("problem-cell")
        guard problems.indices.contains(row) else { return cell }
        let problem = problems[row]
        cell.stringValue = "\(problem.line + 1):\(problem.column + 1)  \(problem.message)"
        cell.font = .systemFont(ofSize: 11)
        cell.textColor = Theme.diagnostic(problem.severity)
        cell.lineBreakMode = .byTruncatingTail
        return cell
    }

    @objc private func clicked(_ sender: Any?) {
        guard table.clickedRow >= 0, problems.indices.contains(table.clickedRow) else { return }
        onSelect?(problems[table.clickedRow])
    }
}
