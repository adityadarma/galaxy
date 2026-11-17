import AppKit

@MainActor
final class GitBranchPicker: NSView, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    static let actionRowHeight: CGFloat = 32
    static let branchRowHeight: CGFloat = 50
    static let maxVisibleBranches = 6

    private enum Item {
        case create
        case createFrom
        case checkoutDetached
        case branch(GitBranchInfo, GitCommitInfo?)

        var searchText: String {
            switch self {
            case .create: "create new branch"
            case .createFrom: "create new branch from"
            case .checkoutDetached: "checkout detached"
            case .branch(let branch, let commit):
                [branch.name, commit?.author, commit?.hash, commit?.subject].compactMap { $0 }.joined(separator: " ")
            }
        }
    }

    private let field = NSTextField()
    private let table = NSTableView()
    private let scroll = NSScrollView()
    private let message = NSTextField(labelWithString: "No matching branches or tags")
    private var allBranches: [GitBranchInfo] = []
    private var commitsByBranch: [String: GitCommitInfo] = [:]
    private var items: [Item] = []
    private var heightConstraint: NSLayoutConstraint!
    var onCheckout: ((String) -> Void)?
    var onClose: (() -> Void)?
    var onCreate: (() -> Void)?
    var onCreateFrom: (() -> Void)?
    var onCheckoutDetached: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        shadow = NSShadow()
        layer?.cornerRadius = 10
        layer?.borderWidth = 1
        layer?.shadowOpacity = 0.32
        layer?.shadowRadius = 16
        layer?.shadowOffset = NSSize(width: 0, height: -5)
        translatesAutoresizingMaskIntoConstraints = false
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Git branches")
        setAccessibilityIdentifier("git-branch-picker")

        field.placeholderString = "Select a branch or tag to checkout"
        field.font = .systemFont(ofSize: 14)
        field.focusRingType = .none
        field.bezelStyle = .roundedBezel
        field.delegate = self
        field.setAccessibilityIdentifier("git-branch-picker-field")
        field.translatesAutoresizingMaskIntoConstraints = false

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("branch"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.style = .plain
        table.rowSizeStyle = .custom
        table.rowHeight = Self.actionRowHeight
        table.intercellSpacing = .zero
        table.backgroundColor = .clear
        table.refusesFirstResponder = true
        table.delegate = self
        table.dataSource = self
        table.target = self
        table.action = #selector(rowClicked(_:))
        table.setAccessibilityIdentifier("git-branch-picker-results")

        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.borderType = .noBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false

        message.font = .systemFont(ofSize: 12)
        message.textColor = .secondaryLabelColor
        message.isHidden = true
        message.translatesAutoresizingMaskIntoConstraints = false

        for view in [field, scroll, message] { addSubview(view) }
        heightConstraint = heightAnchor.constraint(equalToConstant: 80)
        NSLayoutConstraint.activate([
            heightConstraint,
            field.topAnchor.constraint(equalTo: topAnchor, constant: 10),
            field.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            field.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            field.heightAnchor.constraint(equalToConstant: 34),
            scroll.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 7),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 6),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -6),
            message.topAnchor.constraint(equalTo: field.bottomAnchor, constant: 14),
            message.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 16),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        layer?.backgroundColor = Theme.sidebar.cgColor
        layer?.borderColor = NSColor.separatorColor.cgColor
    }

    func reset(branches: [GitBranchInfo], commits: [GitCommitInfo]) {
        allBranches = branches
        commitsByBranch = [:]
        for commit in commits {
            // The checked-out branch is reported as a `.head` ref.
            for ref in commit.refs where ref.kind != .tag && commitsByBranch[ref.name] == nil {
                commitsByBranch[ref.name] = commit
            }
        }
        field.stringValue = ""
        filter()
    }

    func focus() { window?.makeFirstResponder(field) }

    func controlTextDidChange(_ obj: Notification) { filter() }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.moveUp(_:)): move(by: -1)
        case #selector(NSResponder.moveDown(_:)): move(by: 1)
        case #selector(NSResponder.insertNewline(_:)): runSelected()
        case #selector(NSResponder.cancelOperation(_:)): onClose?()
        default: return false
        }
        return true
    }

    private func filter() {
        let query = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let branchItems = allBranches.map { Item.branch($0, commitsByBranch[$0.name]) }
        items = [.create, .createFrom, .checkoutDetached] + branchItems
        if !query.isEmpty { items = items.filter { $0.searchText.lowercased().contains(query) } }
        table.reloadData()
        if !items.isEmpty {
            table.selectRowIndexes([0], byExtendingSelection: false)
            table.scrollRowToVisible(0)
        }
        let empty = items.isEmpty
        message.isHidden = !empty
        scroll.isHidden = empty
        let visible = Array(items.prefix(3 + Self.maxVisibleBranches))
        let listHeight = visible.reduce(CGFloat.zero) { $0 + height(for: $1) }
        heightConstraint.constant = empty ? 88 : 10 + 34 + 7 + listHeight + 6
        setAccessibilityValue(empty ? message.stringValue : "\(items.count) choices")
    }

    private func move(by delta: Int) {
        guard !items.isEmpty else { return }
        let current = table.selectedRow >= 0 ? table.selectedRow : 0
        let next = (current + delta + items.count) % items.count
        table.selectRowIndexes([next], byExtendingSelection: false)
        table.scrollRowToVisible(next)
    }

    private func runSelected() {
        guard items.indices.contains(table.selectedRow) else { return NSSound.beep() }
        switch items[table.selectedRow] {
        case .create: onCreate?()
        case .createFrom: onCreateFrom?()
        case .checkoutDetached: onCheckoutDetached?()
        case .branch(let branch, _): onCheckout?(branch.name)
        }
    }

    @objc private func rowClicked(_ sender: Any?) {
        guard items.indices.contains(table.clickedRow) else { return }
        table.selectRowIndexes([table.clickedRow], byExtendingSelection: false)
        runSelected()
    }

    func numberOfRows(in tableView: NSTableView) -> Int { items.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = tableView.makeView(withIdentifier: GitBranchPickerCell.reuseID, owner: self) as? GitBranchPickerCell
            ?? GitBranchPickerCell()
        switch items[row] {
        case .create:
            cell.showAction(symbol: "plus", title: "Create new branch…")
        case .createFrom:
            cell.showAction(symbol: "plus.square.on.square", title: "Create new branch from…")
        case .checkoutDetached:
            cell.showAction(symbol: "scope", title: "Checkout detached…")
        case .branch(let branch, let commit):
            let age = commit.map { BlameFormat.relative($0.timestamp) } ?? ""
            let metadata = commit.map { "\($0.author)  ·  \(String($0.hash.prefix(7)))  ·  \($0.subject)" } ?? ""
            cell.showBranch(name: branch.name, age: age, metadata: metadata,
                            category: branch.current ? "current" : (branch.remote ? "remote" : "branch"),
                            symbol: branch.remote ? "cloud" : "arrow.triangle.branch",
                            current: branch.current,
                            separated: row == 0 || !isBranch(items[row - 1]))
        }
        return cell
    }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        items.indices.contains(row) ? height(for: items[row]) : Self.actionRowHeight
    }

    private func height(for item: Item) -> CGFloat {
        // Branches without a known commit have no second line.
        if case .branch(_, let commit) = item, commit != nil { return Self.branchRowHeight }
        return Self.actionRowHeight
    }

    private func isBranch(_ item: Item) -> Bool {
        if case .branch = item { return true }
        return false
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        tableView.makeView(withIdentifier: QuickOpenRowView.reuseID, owner: self) as? QuickOpenRowView
            ?? QuickOpenRowView()
    }
}

@MainActor
private final class GitBranchPickerCell: NSTableCellView {
    static let reuseID = NSUserInterfaceItemIdentifier("git-branch-picker-cell")

    private let icon = NSImageView()
    private let title = NSTextField(labelWithString: "")
    private let age = NSTextField(labelWithString: "")
    private let category = NSTextField(labelWithString: "")
    private let metadata = NSTextField(labelWithString: "")
    private let separator = NSBox()
    private let titleRow: NSStackView
    /// Centers single-line rows; two-line rows pin the title to the top.
    private var titleCenterY: NSLayoutConstraint!
    private var titleTop: NSLayoutConstraint!

    override init(frame frameRect: NSRect) {
        titleRow = NSStackView(views: [title, age])
        super.init(frame: frameRect)
        identifier = Self.reuseID

        icon.imageScaling = .scaleProportionallyDown
        icon.contentTintColor = .secondaryLabelColor
        icon.translatesAutoresizingMaskIntoConstraints = false

        title.font = .systemFont(ofSize: 13)
        title.lineBreakMode = .byTruncatingTail
        title.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        age.font = .systemFont(ofSize: 11)
        age.textColor = .tertiaryLabelColor
        age.setContentCompressionResistancePriority(.defaultHigh, for: .horizontal)

        category.font = .systemFont(ofSize: 10, weight: .medium)
        category.textColor = .secondaryLabelColor
        category.alignment = .right
        category.setContentCompressionResistancePriority(.required, for: .horizontal)
        category.setContentHuggingPriority(.required, for: .horizontal)
        category.translatesAutoresizingMaskIntoConstraints = false

        metadata.font = .systemFont(ofSize: 11)
        metadata.textColor = .secondaryLabelColor
        metadata.lineBreakMode = .byTruncatingTail
        metadata.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        metadata.translatesAutoresizingMaskIntoConstraints = false

        titleRow.orientation = .horizontal
        titleRow.spacing = 8
        titleRow.alignment = .firstBaseline
        titleRow.translatesAutoresizingMaskIntoConstraints = false

        separator.boxType = .separator
        separator.translatesAutoresizingMaskIntoConstraints = false

        for view in [icon, titleRow, category, metadata, separator] { addSubview(view) }
        titleCenterY = titleRow.centerYAnchor.constraint(equalTo: centerYAnchor)
        titleTop = titleRow.topAnchor.constraint(equalTo: topAnchor, constant: 8)
        NSLayoutConstraint.activate([
            icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            icon.centerYAnchor.constraint(equalTo: titleRow.centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 16),
            icon.heightAnchor.constraint(equalToConstant: 16),

            titleRow.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 10),
            titleRow.trailingAnchor.constraint(lessThanOrEqualTo: category.leadingAnchor, constant: -10),
            titleCenterY,

            category.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -14),
            category.firstBaselineAnchor.constraint(equalTo: title.firstBaselineAnchor),

            metadata.leadingAnchor.constraint(equalTo: titleRow.leadingAnchor),
            metadata.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -14),
            metadata.topAnchor.constraint(equalTo: titleRow.bottomAnchor, constant: 3),

            separator.topAnchor.constraint(equalTo: topAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    private func setSymbol(_ name: String, tint: NSColor) {
        icon.image = NSImage(systemSymbolName: name, accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 12, weight: .medium))
        icon.contentTintColor = tint
    }

    private func setTwoLines(_ twoLines: Bool) {
        titleCenterY.isActive = !twoLines
        titleTop.isActive = twoLines
        metadata.isHidden = !twoLines
    }

    func showAction(symbol: String, title: String) {
        setSymbol(symbol, tint: .secondaryLabelColor)
        self.title.stringValue = title
        self.title.font = .systemFont(ofSize: 13)
        age.isHidden = true
        category.isHidden = true
        metadata.stringValue = ""
        setTwoLines(false)
        separator.isHidden = true
        setAccessibilityLabel(title)
    }

    func showBranch(name: String, age: String, metadata: String, category: String,
                    symbol: String, current: Bool, separated: Bool) {
        setSymbol(symbol, tint: current ? Theme.accent : .secondaryLabelColor)
        title.stringValue = name
        title.font = .systemFont(ofSize: 13, weight: current ? .semibold : .medium)
        self.age.stringValue = age
        self.age.isHidden = age.isEmpty
        self.category.stringValue = category.uppercased()
        self.category.textColor = current ? Theme.accent : .tertiaryLabelColor
        self.category.isHidden = false
        self.metadata.stringValue = metadata
        setTwoLines(!metadata.isEmpty)
        separator.isHidden = !separated
        setAccessibilityLabel([name, age, metadata, category].filter { !$0.isEmpty }.joined(separator: ", "))
    }
}
