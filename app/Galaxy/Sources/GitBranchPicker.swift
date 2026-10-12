import AppKit

@MainActor
final class GitBranchPicker: NSView, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    private let field = NSTextField()
    private let createButton = NSButton(title: "＋ Create new branch…", target: nil, action: nil)
    private let table = NSTableView()
    private let scroll = NSScrollView()
    private var allBranches: [GitBranchInfo] = []
    private var results: [GitBranchInfo] = []
    private var heightConstraint: NSLayoutConstraint!
    var onCheckout: ((String) -> Void)?
    var onClose: (() -> Void)?
    var onCreate: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 8
        layer?.backgroundColor = Theme.chrome.cgColor
        layer?.borderColor = NSColor.separatorColor.cgColor
        layer?.borderWidth = 1
        translatesAutoresizingMaskIntoConstraints = false

        field.placeholderString = "Select a branch or tag to checkout"
        field.font = .systemFont(ofSize: 14)
        field.focusRingType = .none
        field.bezelStyle = .roundedBezel
        field.delegate = self
        field.translatesAutoresizingMaskIntoConstraints = false

        createButton.isBordered = false
        createButton.alignment = .left
        createButton.font = .systemFont(ofSize: 13)
        createButton.target = self
        createButton.action = #selector(createBranch(_:))
        createButton.translatesAutoresizingMaskIntoConstraints = false

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("branch"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 34
        table.delegate = self
        table.dataSource = self
        table.target = self
        table.doubleAction = #selector(choose(_:))
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.translatesAutoresizingMaskIntoConstraints = false

        addSubview(field)
        addSubview(createButton)
        addSubview(scroll)
        heightConstraint = heightAnchor.constraint(equalToConstant: 360)
        NSLayoutConstraint.activate([
            field.topAnchor.constraint(equalTo: topAnchor, constant: 12),
            field.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            field.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            field.heightAnchor.constraint(equalToConstant: 34),
            createButton.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 5),
            createButton.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            createButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            createButton.heightAnchor.constraint(equalToConstant: 30),
            scroll.topAnchor.constraint(equalTo: createButton.bottomAnchor, constant: 4),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -8),
            heightConstraint,
        ])
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    func reset(branches: [GitBranchInfo]) {
        allBranches = branches
        field.stringValue = ""
        filter()
        field.becomeFirstResponder()
    }

    func controlTextDidChange(_ obj: Notification) { filter() }

    private func filter() {
        let query = field.stringValue.lowercased()
        results = allBranches.filter { query.isEmpty || $0.name.lowercased().contains(query) }
        table.reloadData()
        if !results.isEmpty { table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false) }
    }

    func numberOfRows(in tableView: NSTableView) -> Int { results.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = (tableView.makeView(withIdentifier: NSUserInterfaceItemIdentifier("branch-cell"), owner: self) as? NSTableCellView) ?? {
            let view = NSTableCellView()
            view.identifier = NSUserInterfaceItemIdentifier("branch-cell")
            let label = NSTextField(labelWithString: "")
            label.translatesAutoresizingMaskIntoConstraints = false
            label.font = .systemFont(ofSize: 13)
            view.textField = label
            view.addSubview(label)
            NSLayoutConstraint.activate([
                label.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 10),
                label.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            ])
            return view
        }()
        let branch = results[row]
        cell.textField?.stringValue = "\(branch.current ? "✓ " : "")\(branch.name)\(branch.remote ? "  (remote)" : "")"
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {}

    @objc private func choose(_ sender: Any?) {
        guard table.selectedRow >= 0, table.selectedRow < results.count else { return }
        onCheckout?(results[table.selectedRow].name)
    }

    @objc private func createBranch(_ sender: Any?) { onCreate?() }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { onClose?(); return }
        if event.keyCode == 36 { choose(nil); return }
        super.keyDown(with: event)
    }
}
