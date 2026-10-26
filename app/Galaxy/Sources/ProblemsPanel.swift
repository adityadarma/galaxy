import AppKit

@MainActor
final class ProblemsPanel: NSView, NSTableViewDataSource, NSTableViewDelegate {
    private enum Row {
        case file(FileGroup)
        case problem(ProblemInfo)
    }

    private struct FileGroup {
        let uri: String
        let name: String
        let directory: String
        let problems: [ProblemInfo]
    }

    var onSelect: ((ProblemInfo) -> Void)?
    private var groups: [FileGroup] = []
    private var rows: [Row] = []
    private var collapsed: Set<String> = []
    private var problemsSignature = ""
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

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("problem"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 22
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.selectionHighlightStyle = .regular
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
        empty.font = .systemFont(ofSize: 12)
        empty.textColor = .secondaryLabelColor
        empty.translatesAutoresizingMaskIntoConstraints = false

        addSubview(scroll)
        addSubview(empty)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: topAnchor),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            empty.centerXAnchor.constraint(equalTo: centerXAnchor),
            empty.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func update(_ problems: [ProblemInfo]) {
        let signature = problems.map {
            "\($0.uri)\u{1F}\($0.line)\u{1F}\($0.column)\u{1F}\($0.severity)\u{1F}\($0.message)"
        }.joined(separator: "\u{1E}")
        guard signature != problemsSignature else { return }
        problemsSignature = signature
        let grouped = Dictionary(grouping: problems, by: \.uri)
        groups = grouped.map { uri, problems in
            let path = uri.replacingOccurrences(of: "file://", with: "")
            let url = URL(fileURLWithPath: path)
            return FileGroup(
                uri: uri,
                name: url.lastPathComponent,
                directory: url.deletingLastPathComponent().path,
                problems: problems.sorted { ($0.line, $0.column, $0.message) < ($1.line, $1.column, $1.message) }
            )
        }.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        rebuildRows()
        empty.isHidden = !problems.isEmpty
        scroll.isHidden = problems.isEmpty
        setAccessibilityValue(problems.isEmpty ? "No problems" : "\(problems.count) problems")
    }

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        guard rows.indices.contains(row) else { return 24 }
        if case .file = rows[row] { return 24 }
        return 22
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard rows.indices.contains(row) else { return nil }
        switch rows[row] {
        case let .file(group):
            let cell = reusable(FileHeaderCell.self, id: "problem-file-cell")
            cell.configure(
                name: group.name,
                directory: group.directory,
                count: group.problems.count,
                expanded: !collapsed.contains(group.uri)
            )
            return cell
        case let .problem(problem):
            let cell = reusable(ProblemRowCell.self, id: "problem-cell")
            cell.configure(problem)
            return cell
        }
    }

    @objc private func clicked(_ sender: Any?) {
        guard table.clickedRow >= 0, rows.indices.contains(table.clickedRow) else { return }
        switch rows[table.clickedRow] {
        case let .file(group):
            if collapsed.contains(group.uri) { collapsed.remove(group.uri) } else { collapsed.insert(group.uri) }
            rebuildRows()
        case let .problem(problem): onSelect?(problem)
        }
    }

    private func rebuildRows() {
        rows = groups.flatMap { group in
            var result: [Row] = [.file(group)]
            if !collapsed.contains(group.uri) { result.append(contentsOf: group.problems.map(Row.problem)) }
            return result
        }
        table.reloadData()
    }

    private func reusable<T: NSView>(_ type: T.Type, id: String) -> T {
        let identifier = NSUserInterfaceItemIdentifier(id)
        if let view = table.makeView(withIdentifier: identifier, owner: self) as? T { return view }
        let view = T(frame: .zero)
        view.identifier = identifier
        return view
    }
}

private final class FileHeaderCell: NSTableCellView {
    private let chevron = NSImageView()
    private let icon = NSImageView()
    private let name = NSTextField(labelWithString: "")
    private let directory = NSTextField(labelWithString: "")
    private let badge = NSTextField(labelWithString: "")

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        for view in [chevron, icon, name, directory, badge] { view.translatesAutoresizingMaskIntoConstraints = false; addSubview(view) }
        chevron.contentTintColor = .secondaryLabelColor
        icon.image = NSImage(systemSymbolName: "doc.text.fill", accessibilityDescription: nil)
        icon.contentTintColor = .systemPurple
        name.font = .boldSystemFont(ofSize: 11)
        directory.font = .systemFont(ofSize: 10)
        directory.textColor = .secondaryLabelColor
        badge.font = .monospacedDigitSystemFont(ofSize: 11, weight: .medium)
        badge.textColor = .white
        badge.alignment = .center
        badge.wantsLayer = true
        badge.layer?.cornerRadius = 8
        badge.layer?.backgroundColor = Theme.accent.withAlphaComponent(0.38).cgColor
        NSLayoutConstraint.activate([
            chevron.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8), chevron.centerYAnchor.constraint(equalTo: centerYAnchor), chevron.widthAnchor.constraint(equalToConstant: 11),
            icon.leadingAnchor.constraint(equalTo: chevron.trailingAnchor, constant: 5), icon.centerYAnchor.constraint(equalTo: centerYAnchor), icon.widthAnchor.constraint(equalToConstant: 13),
            name.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 5), name.centerYAnchor.constraint(equalTo: centerYAnchor),
            directory.leadingAnchor.constraint(equalTo: name.trailingAnchor, constant: 6), directory.centerYAnchor.constraint(equalTo: centerYAnchor), directory.trailingAnchor.constraint(lessThanOrEqualTo: badge.leadingAnchor, constant: -5),
            badge.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8), badge.centerYAnchor.constraint(equalTo: centerYAnchor), badge.widthAnchor.constraint(greaterThanOrEqualToConstant: 20), badge.heightAnchor.constraint(equalToConstant: 16),
        ])
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func configure(name: String, directory: String, count: Int, expanded: Bool) {
        chevron.image = NSImage(systemSymbolName: expanded ? "chevron.down" : "chevron.right", accessibilityDescription: nil)
        self.name.stringValue = name
        self.directory.stringValue = directory
        badge.stringValue = "\(count)"
    }
}

private final class ProblemRowCell: NSTableCellView {
    private let icon = NSImageView()
    private let message = NSTextField(labelWithString: "")
    private let metadata = NSTextField(labelWithString: "")

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        for view in [icon, message, metadata] { view.translatesAutoresizingMaskIntoConstraints = false; addSubview(view) }
        message.font = .systemFont(ofSize: 11)
        message.lineBreakMode = .byTruncatingTail
        metadata.font = .systemFont(ofSize: 10)
        metadata.textColor = .secondaryLabelColor
        NSLayoutConstraint.activate([
            icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 34), icon.centerYAnchor.constraint(equalTo: centerYAnchor), icon.widthAnchor.constraint(equalToConstant: 14), icon.heightAnchor.constraint(equalToConstant: 14),
            message.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 6), message.centerYAnchor.constraint(equalTo: centerYAnchor),
            metadata.leadingAnchor.constraint(equalTo: message.trailingAnchor, constant: 6), metadata.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8), metadata.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func configure(_ problem: ProblemInfo) {
        let symbol = problem.severity == 1 ? "xmark.circle" : "exclamationmark.triangle"
        icon.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)
        icon.contentTintColor = Theme.diagnostic(problem.severity)
        message.stringValue = problem.message
        let source = problem.source ?? ""
        let code = problem.code ?? ""
        let origin = source.isEmpty ? "" : code.isEmpty ? source : "\(source)(\(code))"
        metadata.stringValue = "\(origin)  [Ln \(problem.line + 1), Col \(problem.column + 1)]"
    }
}
