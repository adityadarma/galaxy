import AppKit

@MainActor
final class SourceControlRootView: NSView {
    override func draw(_ dirtyRect: NSRect) {
        Theme.sidebar.setFill()
        dirtyRect.intersection(bounds).fill()
    }
}

@MainActor
final class SourceControlController: NSObject, NSTableViewDataSource, NSTableViewDelegate, NSTextFieldDelegate {
    private enum SectionType {
        case staged
        case unstaged
    }

    private enum Row {
        case header(type: SectionType, title: String, count: Int, isCollapsed: Bool)
        case change(GitChangeInfo, isStaged: Bool)
        case empty(String)
    }

    let view = SourceControlRootView()
    private let message = NSTextField()
    private let commitButton = NSButton(title: "Commit", target: nil, action: nil)
    private let refreshButton = NSButton(image: NSImage(systemSymbolName: "arrow.clockwise", accessibilityDescription: "Refresh Changes")!, target: nil, action: nil)
    private let table = SourceControlTableView()
    private let scroll = NSScrollView()

    private var rows: [Row] = []
    private var stagedChanges: [GitChangeInfo] = []
    private var unstagedChanges: [GitChangeInfo] = []
    private var isStagedCollapsed: Bool = false
    private var isChangesCollapsed: Bool = false

    var workspace: Workspace?
    var onOpen: ((String) -> Void)?
    var onRefresh: (() -> Void)?

    override init() {
        super.init()
        view.translatesAutoresizingMaskIntoConstraints = false

        message.placeholderString = "Message (⌘Enter to commit)"
        message.font = .systemFont(ofSize: 12)
        message.delegate = self
        message.translatesAutoresizingMaskIntoConstraints = false

        commitButton.bezelStyle = .rounded
        commitButton.target = self
        commitButton.action = #selector(commit(_:))
        commitButton.translatesAutoresizingMaskIntoConstraints = false
        commitButton.isEnabled = false

        refreshButton.isBordered = false
        refreshButton.target = self
        refreshButton.action = #selector(refreshClicked(_:))
        refreshButton.translatesAutoresizingMaskIntoConstraints = false
        refreshButton.toolTip = "Refresh Changes"

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("scm-changes"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 26
        table.dataSource = self
        table.delegate = self
        table.backgroundColor = .clear
        table.target = self
        table.action = #selector(tableClicked(_:))
        table.doubleAction = #selector(tableClicked(_:))
        table.autoresizingMask = [.width, .height]

        table.onContextMenu = { [weak self] row in
            self?.contextMenu(for: row)
        }
        setupKeyboardHandlers()

        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.translatesAutoresizingMaskIntoConstraints = false

        view.addSubview(message)
        view.addSubview(commitButton)
        view.addSubview(refreshButton)
        view.addSubview(scroll)

        let messageTrailing = message.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -10)
        messageTrailing.priority = NSLayoutConstraint.Priority(999)

        let commitTrailing = commitButton.trailingAnchor.constraint(equalTo: refreshButton.leadingAnchor, constant: -6)
        commitTrailing.priority = NSLayoutConstraint.Priority(999)

        let refreshTrailing = refreshButton.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -10)
        refreshTrailing.priority = NSLayoutConstraint.Priority(999)

        let scrollTrailing = scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor)
        scrollTrailing.priority = NSLayoutConstraint.Priority(999)

        NSLayoutConstraint.activate([
            message.topAnchor.constraint(equalTo: view.topAnchor, constant: 10),
            message.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 10),
            messageTrailing,
            message.heightAnchor.constraint(equalToConstant: 28),

            commitButton.topAnchor.constraint(equalTo: message.bottomAnchor, constant: 8),
            commitButton.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 10),
            commitTrailing,
            commitButton.heightAnchor.constraint(equalToConstant: 28),

            refreshButton.centerYAnchor.constraint(equalTo: commitButton.centerYAnchor),
            refreshTrailing,
            refreshButton.widthAnchor.constraint(equalToConstant: 28),
            refreshButton.heightAnchor.constraint(equalToConstant: 28),

            scroll.topAnchor.constraint(equalTo: commitButton.bottomAnchor, constant: 10),
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scrollTrailing,
            scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    func focusMessageInput() {
        view.window?.makeFirstResponder(message)
    }

    func refresh(status: GitStatusInfo?) {
        let changes = status?.changes ?? []
        stagedChanges = changes.filter { isStagedChange($0) }
        unstagedChanges = changes.filter { isUnstagedChange($0) }
        rebuildRows()
        updateCommitButtonState()
    }

    private func isStagedChange(_ change: GitChangeInfo) -> Bool {
        if change.code.count >= 1 {
            let first = change.code.first!
            return first != " " && first != "?"
        }
        return change.staged
    }

    private func isUnstagedChange(_ change: GitChangeInfo) -> Bool {
        if change.code.count >= 2 {
            let second = change.code[change.code.index(after: change.code.startIndex)]
            let first = change.code.first!
            return second != " " || first == "?"
        }
        return !change.staged
    }

    private func rebuildRows() {
        rows = []
        if !stagedChanges.isEmpty {
            rows.append(.header(type: .staged, title: "STAGED CHANGES", count: stagedChanges.count, isCollapsed: isStagedCollapsed))
            if !isStagedCollapsed {
                rows += stagedChanges.map { Row.change($0, isStaged: true) }
            }
        }
        if !unstagedChanges.isEmpty || stagedChanges.isEmpty {
            rows.append(.header(type: .unstaged, title: "CHANGES", count: unstagedChanges.count, isCollapsed: isChangesCollapsed))
            if !isChangesCollapsed {
                if unstagedChanges.isEmpty && stagedChanges.isEmpty {
                    rows.append(.empty("No changes"))
                } else {
                    rows += unstagedChanges.map { Row.change($0, isStaged: false) }
                }
            }
        }
        table.reloadData()
    }

    private func toggleSection(_ type: SectionType) {
        switch type {
        case .staged:
            isStagedCollapsed.toggle()
        case .unstaged:
            isChangesCollapsed.toggle()
        }
        rebuildRows()
    }

    private func updateCommitButtonState() {
        let hasMsg = !message.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let hasChanges = !stagedChanges.isEmpty || !unstagedChanges.isEmpty
        commitButton.isEnabled = hasMsg && hasChanges
    }

    func controlTextDidChange(_ obj: Notification) {
        updateCommitButtonState()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        if commandSelector == #selector(NSResponder.insertNewline(_:)) {
            if NSEvent.modifierFlags.contains(.command) {
                commit(nil)
                return true
            }
        }
        return false
    }

    // MARK: - NSTableViewDataSource & Delegate

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        guard row >= 0, row < rows.count else { return false }
        if case .change = rows[row] { return true }
        return false
    }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat { 26 }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard row >= 0, row < rows.count else { return nil }
        switch rows[row] {
        case .header(let type, let title, let count, let isCollapsed):
            let cell = (tableView.makeView(withIdentifier: SourceControlHeaderCellView.reuseIdentifier, owner: self) as? SourceControlHeaderCellView)
                ?? SourceControlHeaderCellView()
            cell.configure(title: title, count: count, isCollapsed: isCollapsed, isStaged: type == .staged)
            cell.onToggle = { [weak self] in
                self?.toggleSection(type)
            }
            cell.onAction = { [weak self] in
                if type == .staged {
                    self?.unstageAll()
                } else {
                    self?.stageAll()
                }
            }
            return cell

        case .change(let change, let isStaged):
            let cell = (tableView.makeView(withIdentifier: SourceControlItemCellView.reuseIdentifier, owner: self) as? SourceControlItemCellView)
                ?? SourceControlItemCellView()
            cell.configure(with: change, isStaged: isStaged)
            cell.onAction = { [weak self] in
                if isStaged {
                    self?.unstage(change: change)
                } else {
                    self?.stage(change: change)
                }
            }
            return cell

        case .empty(let text):
            let cell = (tableView.makeView(withIdentifier: SourceControlEmptyCellView.reuseIdentifier, owner: self) as? SourceControlEmptyCellView)
                ?? SourceControlEmptyCellView()
            cell.label.stringValue = text
            return cell
        }
    }

    // MARK: - Actions

    @objc private func tableClicked(_ sender: Any?) {
        let row = table.clickedRow
        guard row >= 0, row < rows.count else { return }
        switch rows[row] {
        case .header(let type, _, _, _):
            toggleSection(type)
        case .change(let change, _):
            onOpen?(change.path)
        case .empty:
            break
        }
    }

    func stage(change: GitChangeInfo) {
        guard let workspace else { return }
        _ = try? workspace.gitStage(path: change.path)
        onRefresh?()
    }

    func unstage(change: GitChangeInfo) {
        guard let workspace else { return }
        _ = try? workspace.gitUnstage(path: change.path)
        onRefresh?()
    }

    @objc func stageAll() {
        guard let workspace else { return }
        for change in unstagedChanges {
            _ = try? workspace.gitStage(path: change.path)
        }
        onRefresh?()
    }

    @objc func unstageAll() {
        guard let workspace else { return }
        for change in stagedChanges {
            _ = try? workspace.gitUnstage(path: change.path)
        }
        onRefresh?()
    }

    @objc private func refreshClicked(_ sender: Any?) { onRefresh?() }

    @objc private func commit(_ sender: Any?) {
        guard let workspace else { return }
        let msg = message.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !msg.isEmpty else { return }

        // If there are no staged changes, stage all changes first (like VS Code)
        if stagedChanges.isEmpty && !unstagedChanges.isEmpty {
            for change in unstagedChanges {
                _ = try? workspace.gitStage(path: change.path)
            }
        }

        guard (try? workspace.gitCommit(message: msg)) != nil else { return }
        message.stringValue = ""
        updateCommitButtonState()
        onRefresh?()
    }

    // MARK: - Context Menu & Keyboard

    private func contextMenu(for row: Int) -> NSMenu? {
        guard row >= 0, row < rows.count else { return nil }
        let menu = NSMenu()
        switch rows[row] {
        case .header(let type, _, let count, _):
            guard count > 0 else { return nil }
            if type == .staged {
                let item = menu.addItem(withTitle: "Unstage All Changes", action: #selector(contextUnstageAll(_:)), keyEquivalent: "")
                item.target = self
            } else {
                let item = menu.addItem(withTitle: "Stage All Changes", action: #selector(contextStageAll(_:)), keyEquivalent: "")
                item.target = self
            }
        case .change(let change, let isStaged):
            let stageTitle = isStaged ? "Unstage Changes" : "Stage Changes"
            let stageItem = menu.addItem(withTitle: stageTitle, action: #selector(contextToggleStage(_:)), keyEquivalent: "")
            stageItem.target = self
            stageItem.representedObject = change
            menu.addItem(.separator())
            let openItem = menu.addItem(withTitle: "Open File", action: #selector(contextOpenFile(_:)), keyEquivalent: "")
            openItem.target = self
            openItem.representedObject = change.path
            let copyItem = menu.addItem(withTitle: "Copy Relative Path", action: #selector(contextCopyPath(_:)), keyEquivalent: "")
            copyItem.target = self
            copyItem.representedObject = change.path
        case .empty:
            return nil
        }
        return menu
    }

    @objc private func contextUnstageAll(_ sender: Any?) { unstageAll() }
    @objc private func contextStageAll(_ sender: Any?) { stageAll() }
    @objc private func contextToggleStage(_ sender: NSMenuItem) {
        guard let change = sender.representedObject as? GitChangeInfo else { return }
        if isStagedChange(change) {
            unstage(change: change)
        } else {
            stage(change: change)
        }
    }
    @objc private func contextOpenFile(_ sender: NSMenuItem) {
        guard let path = sender.representedObject as? String else { return }
        onOpen?(path)
    }
    @objc private func contextCopyPath(_ sender: NSMenuItem) {
        guard let path = sender.representedObject as? String else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(path, forType: .string)
    }

    private func setupKeyboardHandlers() {
        table.onSpaceKey = { [weak self] in
            guard let self else { return }
            let row = self.table.selectedRow
            guard row >= 0, row < self.rows.count, case .change(let change, let isStaged) = self.rows[row] else { return }
            if isStaged {
                self.unstage(change: change)
            } else {
                self.stage(change: change)
            }
        }
        table.onReturnKey = { [weak self] in
            guard let self else { return }
            let row = self.table.selectedRow
            guard row >= 0, row < self.rows.count, case .change(let change, _) = self.rows[row] else { return }
            self.onOpen?(change.path)
        }
    }
}

// MARK: - Custom Views

@MainActor
final class SourceControlTableView: NSTableView {
    var onContextMenu: ((Int) -> NSMenu?)?
    var onSpaceKey: (() -> Void)?
    var onReturnKey: (() -> Void)?

    override func menu(for event: NSEvent) -> NSMenu? {
        let point = convert(event.locationInWindow, from: nil)
        let r = row(at: point)
        if r >= 0 {
            selectRowIndexes(IndexSet(integer: r), byExtendingSelection: false)
            if let menu = onContextMenu?(r) {
                return menu
            }
        }
        return super.menu(for: event)
    }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 49 { // Space
            onSpaceKey?()
            return
        } else if event.keyCode == 36 { // Return
            onReturnKey?()
            return
        }
        super.keyDown(with: event)
    }
}

@MainActor
final class SourceControlHeaderCellView: NSTableCellView {
    static let reuseIdentifier = NSUserInterfaceItemIdentifier("scm-header-cell")

    let chevronButton = NSButton()
    let titleLabel = NSTextField(labelWithString: "")
    let countBadge = NSTextField(labelWithString: "")
    let actionButton = NSButton()

    var onToggle: (() -> Void)?
    var onAction: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        identifier = Self.reuseIdentifier
        setupViews()
    }

    required init?(coder: NSCoder) { fatalError() }

    private func setupViews() {
        chevronButton.isBordered = false
        chevronButton.target = self
        chevronButton.action = #selector(toggleClicked(_:))
        chevronButton.translatesAutoresizingMaskIntoConstraints = false
        chevronButton.contentTintColor = .secondaryLabelColor

        titleLabel.font = .systemFont(ofSize: 11, weight: .bold)
        titleLabel.textColor = .secondaryLabelColor
        titleLabel.translatesAutoresizingMaskIntoConstraints = false

        countBadge.font = .systemFont(ofSize: 10, weight: .medium)
        countBadge.textColor = .tertiaryLabelColor
        countBadge.translatesAutoresizingMaskIntoConstraints = false

        actionButton.isBordered = false
        actionButton.target = self
        actionButton.action = #selector(actionClicked(_:))
        actionButton.translatesAutoresizingMaskIntoConstraints = false
        actionButton.contentTintColor = .secondaryLabelColor

        addSubview(chevronButton)
        addSubview(titleLabel)
        addSubview(countBadge)
        addSubview(actionButton)

        NSLayoutConstraint.activate([
            chevronButton.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            chevronButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            chevronButton.widthAnchor.constraint(equalToConstant: 16),
            chevronButton.heightAnchor.constraint(equalToConstant: 16),

            titleLabel.leadingAnchor.constraint(equalTo: chevronButton.trailingAnchor, constant: 4),
            titleLabel.centerYAnchor.constraint(equalTo: centerYAnchor),

            countBadge.leadingAnchor.constraint(equalTo: titleLabel.trailingAnchor, constant: 6),
            countBadge.centerYAnchor.constraint(equalTo: centerYAnchor),

            actionButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            actionButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            actionButton.widthAnchor.constraint(equalToConstant: 20),
            actionButton.heightAnchor.constraint(equalToConstant: 20),
        ])
    }

    func configure(title: String, count: Int, isCollapsed: Bool, isStaged: Bool) {
        let chevronImage = NSImage(
            systemSymbolName: isCollapsed ? "chevron.right" : "chevron.down",
            accessibilityDescription: isCollapsed ? "Expand section" : "Collapse section"
        )?.withSymbolConfiguration(.init(pointSize: 10, weight: .bold))
        chevronButton.image = chevronImage

        titleLabel.stringValue = title
        countBadge.stringValue = "\(count)"

        let actionImageName = isStaged ? "minus" : "plus"
        let tooltip = isStaged ? "Unstage All Changes" : "Stage All Changes"
        actionButton.image = NSImage(
            systemSymbolName: actionImageName,
            accessibilityDescription: tooltip
        )?.withSymbolConfiguration(.init(pointSize: 11, weight: .bold))
        actionButton.toolTip = tooltip
        actionButton.isHidden = count == 0
    }

    @objc private func toggleClicked(_ sender: Any?) { onToggle?() }
    @objc private func actionClicked(_ sender: Any?) { onAction?() }
}

@MainActor
final class SourceControlItemCellView: NSTableCellView {
    static let reuseIdentifier = NSUserInterfaceItemIdentifier("scm-item-cell")

    let iconView = NSImageView()
    let nameLabel = NSTextField(labelWithString: "")
    let dirLabel = NSTextField(labelWithString: "")
    let statusBadge = NSTextField(labelWithString: "")
    let actionButton = NSButton()

    var onAction: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        identifier = Self.reuseIdentifier
        setupViews()
    }

    required init?(coder: NSCoder) { fatalError() }

    private func setupViews() {
        iconView.translatesAutoresizingMaskIntoConstraints = false
        iconView.imageScaling = .scaleProportionallyDown
        iconView.contentTintColor = .secondaryLabelColor

        nameLabel.font = .systemFont(ofSize: 12, weight: .regular)
        nameLabel.textColor = .labelColor
        nameLabel.lineBreakMode = .byTruncatingTail
        nameLabel.translatesAutoresizingMaskIntoConstraints = false
        nameLabel.setContentCompressionResistancePriority(.defaultHigh, for: .horizontal)

        dirLabel.font = .systemFont(ofSize: 11, weight: .regular)
        dirLabel.textColor = .tertiaryLabelColor
        dirLabel.lineBreakMode = .byTruncatingMiddle
        dirLabel.translatesAutoresizingMaskIntoConstraints = false
        dirLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        statusBadge.font = .systemFont(ofSize: 11, weight: .bold)
        statusBadge.alignment = .right
        statusBadge.translatesAutoresizingMaskIntoConstraints = false

        actionButton.isBordered = false
        actionButton.target = self
        actionButton.action = #selector(actionClicked(_:))
        actionButton.translatesAutoresizingMaskIntoConstraints = false
        actionButton.contentTintColor = .secondaryLabelColor

        addSubview(iconView)
        addSubview(nameLabel)
        addSubview(dirLabel)
        addSubview(actionButton)
        addSubview(statusBadge)

        NSLayoutConstraint.activate([
            iconView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 18),
            iconView.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: 14),
            iconView.heightAnchor.constraint(equalToConstant: 14),

            nameLabel.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 6),
            nameLabel.centerYAnchor.constraint(equalTo: centerYAnchor),

            dirLabel.leadingAnchor.constraint(equalTo: nameLabel.trailingAnchor, constant: 4),
            dirLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
            dirLabel.trailingAnchor.constraint(lessThanOrEqualTo: actionButton.leadingAnchor, constant: -4),

            actionButton.trailingAnchor.constraint(equalTo: statusBadge.leadingAnchor, constant: -4),
            actionButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            actionButton.widthAnchor.constraint(equalToConstant: 18),
            actionButton.heightAnchor.constraint(equalToConstant: 18),

            statusBadge.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            statusBadge.centerYAnchor.constraint(equalTo: centerYAnchor),
            statusBadge.widthAnchor.constraint(equalToConstant: 16),
        ])
    }

    func configure(with change: GitChangeInfo, isStaged: Bool) {
        let name = (change.path as NSString).lastPathComponent
        let dir = (change.path as NSString).deletingLastPathComponent
        nameLabel.stringValue = name
        dirLabel.stringValue = dir.isEmpty ? "" : "\(dir)/"
        iconView.image = fileIcon(for: change.path)

        let badgeInfo = statusBadgeInfo(for: change, isStaged: isStaged)
        statusBadge.stringValue = badgeInfo.letter
        statusBadge.textColor = badgeInfo.color

        let actionSymbol = isStaged ? "minus" : "plus"
        let tooltip = isStaged ? "Unstage Changes" : "Stage Changes"
        actionButton.image = NSImage(
            systemSymbolName: actionSymbol,
            accessibilityDescription: tooltip
        )?.withSymbolConfiguration(.init(pointSize: 11, weight: .semibold))
        actionButton.toolTip = tooltip
    }

    private func fileIcon(for path: String) -> NSImage? {
        let ext = (path as NSString).pathExtension.lowercased()
        let name: String
        switch ext {
        case "rs", "swift", "c", "cpp", "h", "go", "py", "js", "ts", "jsx", "tsx", "php", "rb", "java":
            name = "chevron.left.forwardslash.chevron.right"
        case "json", "toml", "yaml", "yml", "xml":
            name = "curlybraces"
        case "md", "txt":
            name = "doc.text"
        case "png", "jpg", "jpeg", "svg", "gif", "webp", "pdf":
            name = "photo"
        default:
            name = "doc"
        }
        return NSImage(systemSymbolName: name, accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 12, weight: .regular))
    }

    private func statusBadgeInfo(for change: GitChangeInfo, isStaged: Bool) -> (letter: String, color: NSColor) {
        let code = change.code
        if isStaged {
            let first = code.first ?? "M"
            switch first {
            case "M": return ("M", .systemOrange)
            case "A": return ("A", .systemGreen)
            case "D": return ("D", .systemRed)
            case "R": return ("R", .systemBlue)
            default: return ("M", .systemOrange)
            }
        } else {
            if code.contains("?") {
                return ("U", .systemGreen)
            }
            let second = code.count >= 2 ? code[code.index(after: code.startIndex)] : (code.first ?? "M")
            switch second {
            case "M": return ("M", .systemOrange)
            case "D": return ("D", .systemRed)
            case "A": return ("A", .systemGreen)
            case "U": return ("U", .systemOrange)
            default:
                let trimmed = code.trimmingCharacters(in: .whitespaces)
                let letter = trimmed.isEmpty ? "M" : String(trimmed.prefix(1))
                return (letter, .systemOrange)
            }
        }
    }

    @objc private func actionClicked(_ sender: Any?) { onAction?() }
}

@MainActor
final class SourceControlEmptyCellView: NSTableCellView {
    static let reuseIdentifier = NSUserInterfaceItemIdentifier("scm-empty-cell")

    let label = NSTextField(labelWithString: "")

    override init(frame: NSRect) {
        super.init(frame: frame)
        identifier = Self.reuseIdentifier
        label.font = .systemFont(ofSize: 12)
        label.textColor = .secondaryLabelColor
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 20),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            label.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -10),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }
}
