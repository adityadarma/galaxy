import AppKit

@MainActor
final class SearchSidebarController: NSObject, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    let view = NSView()
    private let query = NSTextField()
    private let replace = NSTextField()
    private let regex = NSButton(checkboxWithTitle: ".*", target: nil, action: nil)
    private let caseSensitive = NSButton(checkboxWithTitle: "Aa", target: nil, action: nil)
    private let wholeWord = NSButton(checkboxWithTitle: "ab", target: nil, action: nil)
    private let table = NSTableView()
    private let scroll = NSScrollView()
    private let message = NSTextField(labelWithString: "Type to search the workspace")
    private var results: [WorkspaceSearchMatch] = []
    var onSearch: ((String, WorkspaceSearchOptions) -> Void)?
    var onOpen: ((WorkspaceSearchMatch) -> Void)?

    override init() {
        super.init()
        view.translatesAutoresizingMaskIntoConstraints = false
        query.placeholderString = "Search"
        replace.placeholderString = "Replace"
        for field in [query, replace] {
            field.font = .systemFont(ofSize: 13)
            field.delegate = self
            field.translatesAutoresizingMaskIntoConstraints = false
        }
        for button in [regex, caseSensitive, wholeWord] {
            button.setButtonType(.switch); button.isBordered = false; button.target = self; button.action = #selector(optionsChanged(_:))
        }
        let options = NSStackView(views: [regex, caseSensitive, wholeWord])
        options.spacing = 4; options.translatesAutoresizingMaskIntoConstraints = false
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("search-results"))
        table.addTableColumn(column); table.headerView = nil; table.rowHeight = 48
        table.dataSource = self; table.delegate = self; table.target = self; table.doubleAction = #selector(openSelected(_:)); table.backgroundColor = .clear
        scroll.documentView = table; scroll.drawsBackground = false; scroll.hasVerticalScroller = true; scroll.translatesAutoresizingMaskIntoConstraints = false
        message.font = .systemFont(ofSize: 12); message.textColor = .secondaryLabelColor; message.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(query); view.addSubview(replace); view.addSubview(options); view.addSubview(scroll); view.addSubview(message)
        NSLayoutConstraint.activate([
            query.topAnchor.constraint(equalTo: view.topAnchor, constant: 10), query.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 10), query.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -10), query.heightAnchor.constraint(equalToConstant: 28),
            replace.topAnchor.constraint(equalTo: query.bottomAnchor, constant: 6), replace.leadingAnchor.constraint(equalTo: query.leadingAnchor), replace.trailingAnchor.constraint(equalTo: query.trailingAnchor), replace.heightAnchor.constraint(equalToConstant: 28),
            options.topAnchor.constraint(equalTo: replace.bottomAnchor, constant: 5), options.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 10),
            scroll.topAnchor.constraint(equalTo: options.bottomAnchor, constant: 6), scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor), scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor), scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            message.topAnchor.constraint(equalTo: scroll.topAnchor, constant: 10), message.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 12),
        ])
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
    func focus() { view.window?.makeFirstResponder(query) }
    func reset() { query.stringValue = ""; replace.stringValue = ""; show([], message: "Type to search the workspace") }
    func show(_ matches: [WorkspaceSearchMatch], message text: String? = nil) { results = matches; table.reloadData(); message.stringValue = text ?? (matches.isEmpty ? "No results" : ""); message.isHidden = !matches.isEmpty }
    func controlTextDidChange(_ obj: Notification) { runSearch() }
    @objc private func optionsChanged(_ sender: Any?) { runSearch() }
    private func runSearch() { onSearch?(query.stringValue, WorkspaceSearchOptions(regex: regex.state == .on, caseSensitive: caseSensitive.state == .on, wholeWord: wholeWord.state == .on, include: "", exclude: "")) }
    func numberOfRows(in tableView: NSTableView) -> Int { results.count }
    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let id = NSUserInterfaceItemIdentifier("search-result-cell")
        let cell = (tableView.makeView(withIdentifier: id, owner: self) as? NSTableCellView) ?? {
            let cell = NSTableCellView(); cell.identifier = id
            let label = NSTextField(labelWithString: ""); label.translatesAutoresizingMaskIntoConstraints = false; label.lineBreakMode = .byTruncatingTail; cell.textField = label; cell.addSubview(label)
            NSLayoutConstraint.activate([label.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 12), label.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -8), label.centerYAnchor.constraint(equalTo: cell.centerYAnchor)])
            return cell
        }()
        let match = results[row]
        cell.textField?.stringValue = "\(match.relPath)\n\(match.line + 1):\(match.column + 1)  \(match.text.trimmingCharacters(in: .whitespacesAndNewlines))"
        return cell
    }
    @objc private func openSelected(_ sender: Any?) { guard table.selectedRow >= 0, table.selectedRow < results.count else { return }; onOpen?(results[table.selectedRow]) }
}
