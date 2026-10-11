import AppKit

struct ListeningPort: Equatable {
    let process: String
    let pid: Int
    let address: String
    let port: Int
}

enum ListeningPortParser {
    static func parse(_ output: String) -> [ListeningPort] {
        output.split(whereSeparator: \.isNewline).compactMap { line in
            let fields = line.split(whereSeparator: \.isWhitespace).map(String.init)
            guard fields.count >= 8, fields[0] != "COMMAND", fields.contains("TCP"),
                  let pid = Int(fields[1]) else { return nil }
            guard fields.last == "(LISTEN)" else { return nil }
            let endpoint = fields[fields.count - 2]
            guard let separator = endpoint.lastIndex(of: ":") else { return nil }
            guard let port = Int(endpoint[endpoint.index(after: separator)...]) else { return nil }
            return ListeningPort(process: fields[0], pid: pid, address: String(endpoint[..<separator]), port: port)
        }.sorted { $0.port == $1.port ? $0.process < $1.process : $0.port < $1.port }
    }
}

@MainActor
final class PortsPanel: NSView, NSTableViewDataSource, NSTableViewDelegate {
    private let table = NSTableView()
    private let scroll = NSScrollView()
    private let empty = NSTextField(labelWithString: "No listening ports")
    private let refreshButton = NSButton(title: "Refresh", target: nil, action: nil)
    private var ports: [ListeningPort] = []
    private var timer: Timer?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setAccessibilityRole(.table)
        setAccessibilityLabel("Listening ports")
        setAccessibilityIdentifier("ports-panel")
        for (identifier, width) in [("Port", 80), ("Address", 180), ("Process", 180), ("PID", 80)] {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(identifier))
            column.title = identifier
            column.width = CGFloat(width)
            table.addTableColumn(column)
        }
        table.headerView = NSTableHeaderView()
        table.rowHeight = 24
        table.intercellSpacing = .zero
        table.dataSource = self
        table.delegate = self
        table.backgroundColor = .clear
        table.setAccessibilityIdentifier("ports-results")
        refreshButton.bezelStyle = .texturedRounded
        refreshButton.target = self
        refreshButton.action = #selector(refreshClicked(_:))
        refreshButton.translatesAutoresizingMaskIntoConstraints = false
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.drawsBackground = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        empty.textColor = .secondaryLabelColor
        empty.translatesAutoresizingMaskIntoConstraints = false
        addSubview(refreshButton)
        addSubview(scroll)
        addSubview(empty)
        NSLayoutConstraint.activate([
            refreshButton.topAnchor.constraint(equalTo: topAnchor, constant: 6),
            refreshButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            scroll.topAnchor.constraint(equalTo: refreshButton.bottomAnchor, constant: 4),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            empty.centerXAnchor.constraint(equalTo: centerXAnchor),
            empty.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func start() {
        timer?.invalidate()
        refresh()
        timer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in self?.refresh() }
    }

    func stop() { timer?.invalidate(); timer = nil }

    @objc private func refreshClicked(_ sender: Any?) { refresh() }

    private func refresh() {
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let process = Process()
            let pipe = Pipe()
            process.executableURL = URL(fileURLWithPath: "/usr/sbin/lsof")
            process.arguments = ["-nP", "-iTCP", "-sTCP:LISTEN"]
            process.standardOutput = pipe
            guard (try? process.run()) != nil else { return }
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            let next = ListeningPortParser.parse(String(data: data, encoding: .utf8) ?? "")
            Task { @MainActor [weak self] in
                guard let self, self.ports != next else { return }
                self.ports = next
                self.table.reloadData()
                self.empty.isHidden = !next.isEmpty
                self.scroll.isHidden = next.isEmpty
            }
        }
    }

    func numberOfRows(in tableView: NSTableView) -> Int { ports.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = (tableView.makeView(withIdentifier: NSUserInterfaceItemIdentifier("port-cell"), owner: self) as? NSTextField) ?? NSTextField(labelWithString: "")
        cell.identifier = NSUserInterfaceItemIdentifier("port-cell")
        guard ports.indices.contains(row), let identifier = tableColumn?.identifier.rawValue else { return cell }
        let port = ports[row]
        cell.stringValue = switch identifier {
        case "Port": "\(port.port)"
        case "Address": port.address
        case "Process": port.process
        case "PID": "\(port.pid)"
        default: ""
        }
        cell.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        cell.textColor = .secondaryLabelColor
        cell.lineBreakMode = .byTruncatingTail
        return cell
    }
}
