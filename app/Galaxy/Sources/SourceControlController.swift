import AppKit

@MainActor
final class SourceControlController: NSObject, NSTableViewDataSource, NSTableViewDelegate, NSTextFieldDelegate {
    private enum Row { case header(String), change(GitChangeInfo) }

    let view = NSView()
    private let message = NSTextField()
    private let commitButton = NSButton(title: "Commit", target: nil, action: nil)
    private let refreshButton = NSButton(image: NSImage(systemSymbolName: "arrow.clockwise", accessibilityDescription: "Refresh Changes")!, target: nil, action: nil)
    private let table = NSTableView()
    private let scroll = NSScrollView()
    private var rows: [Row] = []
    var workspace: Workspace?
    var onOpen: ((String) -> Void)?
    var onRefresh: (() -> Void)?

    override init() {
        super.init()
        view.translatesAutoresizingMaskIntoConstraints = false
        message.placeholderString = "Message (⌘Enter to commit)"
        message.font = .systemFont(ofSize: 13)
        message.delegate = self
        message.translatesAutoresizingMaskIntoConstraints = false
        commitButton.bezelStyle = .rounded
        commitButton.target = self
        commitButton.action = #selector(commit(_:))
        commitButton.translatesAutoresizingMaskIntoConstraints = false
        refreshButton.isBordered = false
        refreshButton.target = self
        refreshButton.action = #selector(refresh(_:))
        refreshButton.translatesAutoresizingMaskIntoConstraints = false

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("changes"))
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 27
        table.dataSource = self
        table.delegate = self
        table.backgroundColor = .clear
        table.target = self
        table.doubleAction = #selector(openSelected(_:))
        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.translatesAutoresizingMaskIntoConstraints = false

        view.addSubview(message); view.addSubview(commitButton); view.addSubview(refreshButton); view.addSubview(scroll)
        NSLayoutConstraint.activate([
            message.topAnchor.constraint(equalTo: view.topAnchor, constant: 10),
            message.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 10),
            message.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -10),
            message.heightAnchor.constraint(equalToConstant: 30),
            commitButton.topAnchor.constraint(equalTo: message.bottomAnchor, constant: 8),
            commitButton.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 10),
            commitButton.widthAnchor.constraint(equalToConstant: 110),
            commitButton.heightAnchor.constraint(equalToConstant: 28),
            refreshButton.centerYAnchor.constraint(equalTo: commitButton.centerYAnchor),
            refreshButton.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -10),
            refreshButton.widthAnchor.constraint(equalToConstant: 28),
            refreshButton.heightAnchor.constraint(equalToConstant: 28),
            scroll.topAnchor.constraint(equalTo: commitButton.bottomAnchor, constant: 8),
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    func refresh(status: GitStatusInfo?) {
        let changes = status?.changes ?? []
        let staged = changes.filter(\.staged)
        let unstaged = changes.filter { !$0.staged }
        rows = []
        if !staged.isEmpty { rows.append(.header("STAGED CHANGES  \(staged.count)")); rows += staged.map(Row.change) }
        rows.append(.header("CHANGES  \(unstaged.count)"))
        rows += unstaged.map(Row.change)
        if changes.isEmpty { rows.append(.header("No changes")) }
        commitButton.isEnabled = !staged.isEmpty && !message.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        table.reloadData()
    }

    func controlTextDidChange(_ obj: Notification) {
        commitButton.isEnabled = rows.contains { if case .change(let change) = $0 { return change.staged }; return false }
            && !message.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }
    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        if case .header = rows[row] { return false }
        return true
    }
    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        switch rows[row] {
        case .header(let text):
            let id = NSUserInterfaceItemIdentifier("change-header")
            let label = (tableView.makeView(withIdentifier: id, owner: self) as? NSTextField) ?? NSTextField(labelWithString: "")
            label.identifier = id; label.font = .systemFont(ofSize: 12, weight: .semibold); label.textColor = .secondaryLabelColor
            label.stringValue = text
            return label
        case .change(let change):
            let id = NSUserInterfaceItemIdentifier("change-cell")
            let cell = (tableView.makeView(withIdentifier: id, owner: self) as? NSTableCellView) ?? {
                let cell = NSTableCellView(); cell.identifier = id
                let label = NSTextField(labelWithString: "")
                label.translatesAutoresizingMaskIntoConstraints = false; label.lineBreakMode = .byTruncatingMiddle
                cell.textField = label; cell.addSubview(label)
                NSLayoutConstraint.activate([
                    label.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 12),
                    label.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -8),
                    label.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                ])
                return cell
            }()
            let symbol = change.staged ? "✓" : change.code.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: "?", with: "U")
            cell.textField?.stringValue = "\(change.path)                                      \(symbol)"
            return cell
        }
    }

    @objc private func openSelected(_ sender: Any?) {
        guard table.selectedRow >= 0, case .change(let change) = rows[table.selectedRow] else { return }
        onOpen?(change.path)
    }
    @objc private func refresh(_ sender: Any?) { onRefresh?() }
    @objc private func commit(_ sender: Any?) {
        guard let workspace, !message.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              (try? workspace.gitCommit(message: message.stringValue)) != nil else { return }
        message.stringValue = ""; onRefresh?()
    }
}
