import AppKit

@MainActor
final class SourceControlRootView: NSView {
    override func draw(_ dirtyRect: NSRect) {
        Theme.sidebar.setFill()
        dirtyRect.intersection(bounds).fill()
    }
}

// MARK: - Pill Badge

@MainActor
final class CountPillView: NSView {
    let label = NSTextField(labelWithString: "")

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 9
        layer?.masksToBounds = true
        layer?.backgroundColor = NSColor(white: 1.0, alpha: 0.16).cgColor

        label.font = .monospacedDigitSystemFont(ofSize: 11, weight: .medium)
        label.textColor = .white
        label.alignment = .center
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 18),
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 6),
            label.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    func setCount(_ count: Int) {
        label.stringValue = "\(count)"
        isHidden = count == 0
    }
}

// MARK: - Generate Commit Icon Button

@MainActor
final class GenerateCommitIconButton: NSButton {
    private var isHovered = false {
        didSet { updateAppearance() }
    }
    private var trackingArea: NSTrackingArea?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setup()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    private func setup() {
        wantsLayer = true
        isBordered = false
        imagePosition = .imageOnly
        focusRingType = .none
        translatesAutoresizingMaskIntoConstraints = false

        let config = NSImage.SymbolConfiguration(pointSize: 11, weight: .regular)
        let img = (NSImage(systemSymbolName: "wand.and.stars", accessibilityDescription: "Generate Commit Message")
            ?? NSImage(systemSymbolName: "sparkles", accessibilityDescription: "Generate Commit Message"))?
            .withSymbolConfiguration(config)
        img?.isTemplate = true
        self.image = img

        toolTip = "Generate Commit Message"
        setAccessibilityLabel("Generate Commit Message")
        setAccessibilityRole(.button)

        layer?.cornerRadius = 3
        layer?.masksToBounds = true
        updateAppearance()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let trackingArea { removeTrackingArea(trackingArea) }
        let area = NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeInActiveApp, .assumeInside], owner: self, userInfo: nil)
        addTrackingArea(area)
        self.trackingArea = area
    }

    override func mouseEntered(with event: NSEvent) { isHovered = true }
    override func mouseExited(with event: NSEvent) { isHovered = false }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .pointingHand)
    }

    private func updateAppearance() {
        contentTintColor = isHovered ? .labelColor : .secondaryLabelColor
        layer?.backgroundColor = isHovered
            ? NSColor.labelColor.withAlphaComponent(0.08).cgColor
            : NSColor.clear.cgColor
    }
}

// MARK: - Commit Message Input Container

@MainActor
final class CommitMessageInputContainer: NSView {
    let textField = NSTextField()
    let generateButton = GenerateCommitIconButton()

    var onGenerate: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 4
        layer?.masksToBounds = true
        layer?.borderWidth = 1

        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        layer?.borderColor = (isDark ? NSColor(white: 0.24, alpha: 1.0) : NSColor.separatorColor.withAlphaComponent(0.6)).cgColor
        layer?.backgroundColor = isDark
            ? (NSColor(hex: "#1E2227") ?? NSColor(white: 0.14, alpha: 1.0)).cgColor
            : NSColor.textBackgroundColor.cgColor

        textField.isBordered = false
        textField.drawsBackground = false
        textField.focusRingType = .none
        textField.font = .systemFont(ofSize: 12)
        textField.textColor = .labelColor
        textField.placeholderString = "Message (⌘Enter to commit)"
        textField.maximumNumberOfLines = 1
        textField.usesSingleLineMode = true
        textField.cell?.wraps = false
        textField.cell?.isScrollable = true
        textField.cell?.usesSingleLineMode = true
        textField.lineBreakMode = .byClipping
        textField.translatesAutoresizingMaskIntoConstraints = false

        generateButton.target = self
        generateButton.action = #selector(generateClicked(_:))

        addSubview(textField)
        addSubview(generateButton)

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 28),

            textField.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            textField.centerYAnchor.constraint(equalTo: centerYAnchor),
            textField.heightAnchor.constraint(equalToConstant: 20),
            textField.trailingAnchor.constraint(equalTo: generateButton.leadingAnchor, constant: -4),

            generateButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            generateButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            generateButton.widthAnchor.constraint(equalToConstant: 22),
            generateButton.heightAnchor.constraint(equalToConstant: 22),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    func setFocused(_ focused: Bool) {
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let accentColor = NSColor(srgbRed: 0/255.0, green: 120/255.0, blue: 212/255.0, alpha: 1.0)
        let normalBorder = isDark ? NSColor(white: 0.24, alpha: 1.0) : NSColor.separatorColor.withAlphaComponent(0.6)

        layer?.borderColor = focused ? accentColor.cgColor : normalBorder.cgColor
        layer?.borderWidth = focused ? 1.5 : 1.0
    }

    @objc private func generateClicked(_ sender: Any?) { onGenerate?() }
}

// MARK: - Non Interactive Stack View for Button Content

@MainActor
private final class NonInteractiveStackView: NSStackView {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

// MARK: - Commit Split Button

@MainActor
final class CommitSplitButton: NSView {
    let mainButton = NSButton()
    let dropdownButton = NSButton()
    private let divider = NSView()
    private let checkIcon = NSImageView()
    private let commitLabel = NSTextField(labelWithString: "Commit")

    var onCommit: (() -> Void)?
    var onDropdown: ((NSButton) -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 4
        layer?.masksToBounds = true
        layer?.backgroundColor = NSColor(srgbRed: 0/255.0, green: 120/255.0, blue: 212/255.0, alpha: 1.0).cgColor

        // Main button
        mainButton.title = ""
        mainButton.image = nil
        mainButton.isBordered = false
        mainButton.target = self
        mainButton.action = #selector(commitClicked(_:))
        mainButton.translatesAutoresizingMaskIntoConstraints = false

        checkIcon.image = NSImage(systemSymbolName: "checkmark", accessibilityDescription: "Commit")?
            .withSymbolConfiguration(.init(pointSize: 11, weight: .bold))
        checkIcon.contentTintColor = .white
        checkIcon.translatesAutoresizingMaskIntoConstraints = false

        commitLabel.font = .systemFont(ofSize: 12, weight: .medium)
        commitLabel.textColor = .white
        commitLabel.alignment = .center
        commitLabel.translatesAutoresizingMaskIntoConstraints = false

        let contentStack = NonInteractiveStackView(views: [checkIcon, commitLabel])
        contentStack.orientation = .horizontal
        contentStack.spacing = 6
        contentStack.alignment = .centerY
        contentStack.translatesAutoresizingMaskIntoConstraints = false
        mainButton.addSubview(contentStack)

        // Divider
        divider.wantsLayer = true
        divider.layer?.backgroundColor = NSColor(white: 1.0, alpha: 0.25).cgColor
        divider.translatesAutoresizingMaskIntoConstraints = false

        // Dropdown button
        dropdownButton.title = ""
        dropdownButton.image = NSImage(systemSymbolName: "chevron.down", accessibilityDescription: "More Commit Actions")?
            .withSymbolConfiguration(.init(pointSize: 10, weight: .bold))
        dropdownButton.imagePosition = .imageOnly
        dropdownButton.isBordered = false
        dropdownButton.contentTintColor = .white
        dropdownButton.target = self
        dropdownButton.action = #selector(dropdownClicked(_:))
        dropdownButton.translatesAutoresizingMaskIntoConstraints = false

        addSubview(mainButton)
        addSubview(divider)
        addSubview(dropdownButton)

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 28),

            mainButton.leadingAnchor.constraint(equalTo: leadingAnchor),
            mainButton.topAnchor.constraint(equalTo: topAnchor),
            mainButton.bottomAnchor.constraint(equalTo: bottomAnchor),
            mainButton.trailingAnchor.constraint(equalTo: divider.leadingAnchor),

            contentStack.centerXAnchor.constraint(equalTo: mainButton.centerXAnchor),
            contentStack.centerYAnchor.constraint(equalTo: mainButton.centerYAnchor),
            checkIcon.widthAnchor.constraint(equalToConstant: 12),
            checkIcon.heightAnchor.constraint(equalToConstant: 12),

            divider.topAnchor.constraint(equalTo: topAnchor),
            divider.bottomAnchor.constraint(equalTo: bottomAnchor),
            divider.widthAnchor.constraint(equalToConstant: 1),
            divider.trailingAnchor.constraint(equalTo: dropdownButton.leadingAnchor),

            dropdownButton.trailingAnchor.constraint(equalTo: trailingAnchor),
            dropdownButton.topAnchor.constraint(equalTo: topAnchor),
            dropdownButton.bottomAnchor.constraint(equalTo: bottomAnchor),
            dropdownButton.widthAnchor.constraint(equalToConstant: 28),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    @objc private func commitClicked(_ sender: Any?) { onCommit?() }
    @objc private func dropdownClicked(_ sender: Any?) { onDropdown?(dropdownButton) }

    var isEnabled: Bool = true {
        didSet {
            mainButton.isEnabled = isEnabled
            dropdownButton.isEnabled = isEnabled
            alphaValue = isEnabled ? 1.0 : 0.6
        }
    }
}

// MARK: - Main Source Control Controller

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
    private let messageContainer = CommitMessageInputContainer()
    let splitCommitButton = CommitSplitButton()
    private let table = SourceControlTableView()
    private let scroll = NSScrollView()

    private var rows: [Row] = []
    private var stagedChanges: [GitChangeInfo] = []
    private var unstagedChanges: [GitChangeInfo] = []
    private var isStagedCollapsed: Bool = false
    private var isChangesCollapsed: Bool = false
    private var currentBranch: String?

    var workspace: Workspace?
    var onOpen: ((String) -> Void)?
    var onRefresh: (() -> Void)?

    override init() {
        super.init()
        view.translatesAutoresizingMaskIntoConstraints = false

        messageContainer.textField.delegate = self
        messageContainer.textField.placeholderString = "Message (⌘Enter to commit)"
        messageContainer.translatesAutoresizingMaskIntoConstraints = false
        messageContainer.onGenerate = { [weak self] in
            self?.generateCommitMessage()
        }

        splitCommitButton.translatesAutoresizingMaskIntoConstraints = false
        splitCommitButton.isEnabled = false
        splitCommitButton.onCommit = { [weak self] in
            self?.commit(nil)
        }
        splitCommitButton.onDropdown = { [weak self] button in
            self?.showCommitDropdown(from: button)
        }

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

        view.addSubview(messageContainer)
        view.addSubview(splitCommitButton)
        view.addSubview(scroll)

        let msgTrailing = messageContainer.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -10)
        msgTrailing.priority = NSLayoutConstraint.Priority(999)

        let commitTrailing = splitCommitButton.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -10)
        commitTrailing.priority = NSLayoutConstraint.Priority(999)

        let scrollTrailing = scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor)
        scrollTrailing.priority = NSLayoutConstraint.Priority(999)

        NSLayoutConstraint.activate([
            messageContainer.topAnchor.constraint(equalTo: view.topAnchor, constant: 10),
            messageContainer.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 10),
            msgTrailing,

            splitCommitButton.topAnchor.constraint(equalTo: messageContainer.bottomAnchor, constant: 8),
            splitCommitButton.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 10),
            commitTrailing,

            scroll.topAnchor.constraint(equalTo: splitCommitButton.bottomAnchor, constant: 8),
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scrollTrailing,
            scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    func focusMessageInput() {
        view.window?.makeFirstResponder(messageContainer.textField)
    }

    func refresh(status: GitStatusInfo?) {
        currentBranch = status?.branch
        if let branch = currentBranch, !branch.isEmpty {
            messageContainer.textField.placeholderString = "Message (⌘Enter to commit on '\(branch)')"
        } else {
            messageContainer.textField.placeholderString = "Message (⌘Enter to commit)"
        }

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
            rows.append(.header(type: .staged, title: "Staged Changes", count: stagedChanges.count, isCollapsed: isStagedCollapsed))
            if !isStagedCollapsed {
                rows += stagedChanges.map { Row.change($0, isStaged: true) }
            }
        }
        if !unstagedChanges.isEmpty || stagedChanges.isEmpty {
            rows.append(.header(type: .unstaged, title: "Changes", count: unstagedChanges.count, isCollapsed: isChangesCollapsed))
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
        let hasChanges = !stagedChanges.isEmpty || !unstagedChanges.isEmpty
        splitCommitButton.isEnabled = hasChanges
    }

    func controlTextDidChange(_ obj: Notification) {
        updateCommitButtonState()
    }

    func controlTextDidBeginEditing(_ obj: Notification) {
        messageContainer.setFocused(true)
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        messageContainer.setFocused(false)
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
            cell.onDiscardAll = { [weak self] in
                self?.discardAll()
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
            cell.onDiscard = { [weak self] in
                self?.discard(change: change)
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

    func discard(change: GitChangeInfo) {
        guard let workspace else { return }
        let root = workspace.root()
        let path = change.path
        if change.code.contains("?") {
            let fileURL = URL(fileURLWithPath: root).appendingPathComponent(path)
            try? FileManager.default.removeItem(at: fileURL)
        } else {
            _ = runGit(args: ["restore", "--", path])
        }
        onRefresh?()
    }

    func discardAll() {
        guard workspace != nil else { return }
        _ = runGit(args: ["restore", "."])
        _ = runGit(args: ["clean", "-fd"])
        onRefresh?()
    }

    private func heuristicCommitMessage(for targets: [GitChangeInfo]) -> String {
        let fileNames = targets.map { ($0.path as NSString).lastPathComponent }
        if fileNames.count == 1 {
            return "Update \(fileNames[0])"
        } else if fileNames.count == 2 {
            return "Update \(fileNames[0]) and \(fileNames[1])"
        } else {
            return "Update \(fileNames[0]), \(fileNames[1]) and \(fileNames.count - 2) others"
        }
    }

    func generateCommitMessage() {
        let targets = !stagedChanges.isEmpty ? stagedChanges : unstagedChanges
        guard !targets.isEmpty else { return }

        let fallback = heuristicCommitMessage(for: targets)

        // If AI / LLM is not configured, immediately use Option 1
        guard AICommitService.shared.isConfigured else {
            messageContainer.textField.stringValue = fallback
            updateCommitButtonState()
            focusMessageInput()
            return
        }

        // Retrieve git diff (prefer staged changes; fallback to unstaged)
        var (_, diff) = runGit(args: ["diff", "--cached"])
        if diff.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            (_, diff) = runGit(args: ["diff"])
        }

        guard !diff.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            messageContainer.textField.stringValue = fallback
            updateCommitButtonState()
            focusMessageInput()
            return
        }

        // Show generating feedback
        messageContainer.textField.stringValue = ""
        messageContainer.textField.placeholderString = "Generating with AI…"
        messageContainer.generateButton.isEnabled = false

        Task { [weak self] in
            let generated = await AICommitService.shared.generateCommitMessage(diff: diff)
            await MainActor.run {
                guard let self else { return }
                self.messageContainer.generateButton.isEnabled = true
                if let branch = self.currentBranch, !branch.isEmpty {
                    self.messageContainer.textField.placeholderString = "Message (⌘Enter to commit on '\(branch)')"
                } else {
                    self.messageContainer.textField.placeholderString = "Message (⌘Enter to commit)"
                }

                if let generated, !generated.isEmpty {
                    self.messageContainer.textField.stringValue = generated
                } else {
                    // Graceful fallback to Option 1
                    self.messageContainer.textField.stringValue = fallback
                }
                self.updateCommitButtonState()
                self.focusMessageInput()
            }
        }
    }

    @objc func commit(_ sender: Any?) {
        guard let workspace else { return }
        var msg = messageContainer.textField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        if msg.isEmpty {
            // Auto-generate if message was left blank
            generateCommitMessage()
            msg = messageContainer.textField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !msg.isEmpty else {
                focusMessageInput()
                return
            }
        }

        if stagedChanges.isEmpty && !unstagedChanges.isEmpty {
            for change in unstagedChanges {
                _ = try? workspace.gitStage(path: change.path)
            }
        }

        guard (try? workspace.gitCommit(message: msg)) != nil else { return }
        messageContainer.textField.stringValue = ""
        updateCommitButtonState()
        onRefresh?()
    }

    func commitAmend() {
        guard workspace != nil else { return }
        if stagedChanges.isEmpty && !unstagedChanges.isEmpty {
            stageAll()
        }
        let msg = messageContainer.textField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        if msg.isEmpty {
            _ = runGit(args: ["commit", "--amend", "--no-edit"])
        } else {
            _ = runGit(args: ["commit", "--amend", "-m", msg])
            messageContainer.textField.stringValue = ""
        }
        updateCommitButtonState()
        onRefresh?()
    }

    func commitAndPush() {
        commit(nil)
        _ = runGit(args: ["push"])
        onRefresh?()
    }

    func commitAndSync() {
        commit(nil)
        _ = runGit(args: ["pull", "--rebase"])
        _ = runGit(args: ["push"])
        onRefresh?()
    }

    func pull() {
        _ = runGit(args: ["pull"])
        onRefresh?()
    }

    func push() {
        _ = runGit(args: ["push"])
        onRefresh?()
    }

    func stash() {
        _ = runGit(args: ["stash"])
        onRefresh?()
    }

    func stashPop() {
        _ = runGit(args: ["stash", "pop"])
        onRefresh?()
    }

    @discardableResult
    private func runGit(args: [String]) -> (status: Int32, output: String) {
        guard let root = workspace?.root() else { return (-1, "") }
        let process = Process()
        let pipe = Pipe()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        process.currentDirectoryURL = URL(fileURLWithPath: root)
        process.arguments = args
        process.standardOutput = pipe
        process.standardError = pipe
        do {
            try process.run()
            process.waitUntilExit()
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            let output = String(data: data, encoding: .utf8) ?? ""
            return (process.terminationStatus, output)
        } catch {
            return (-1, error.localizedDescription)
        }
    }

    // MARK: - Dropdown & Menus

    private func showCommitDropdown(from button: NSButton) {
        let menu = NSMenu()

        let commitItem = menu.addItem(withTitle: "Commit", action: #selector(menuCommit(_:)), keyEquivalent: "")
        commitItem.target = self

        let amendItem = menu.addItem(withTitle: "Commit (Amend)", action: #selector(menuCommitAmend(_:)), keyEquivalent: "")
        amendItem.target = self

        menu.addItem(.separator())

        let pushItem = menu.addItem(withTitle: "Commit & Push", action: #selector(menuCommitAndPush(_:)), keyEquivalent: "")
        pushItem.target = self

        let syncItem = menu.addItem(withTitle: "Commit & Sync", action: #selector(menuCommitAndSync(_:)), keyEquivalent: "")
        syncItem.target = self

        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: button.bounds.maxY + 4), in: button)
    }

    @objc private func menuCommit(_ sender: Any?) { commit(nil) }
    @objc private func menuCommitAmend(_ sender: Any?) { commitAmend() }
    @objc private func menuCommitAndPush(_ sender: Any?) { commitAndPush() }
    @objc private func menuCommitAndSync(_ sender: Any?) { commitAndSync() }

    func moreActionsMenu() -> NSMenu {
        let menu = NSMenu()
        let pullItem = menu.addItem(withTitle: "Pull", action: #selector(menuPull(_:)), keyEquivalent: "")
        pullItem.target = self
        let pushItem = menu.addItem(withTitle: "Push", action: #selector(menuPush(_:)), keyEquivalent: "")
        pushItem.target = self
        let syncItem = menu.addItem(withTitle: "Sync", action: #selector(menuCommitAndSync(_:)), keyEquivalent: "")
        syncItem.target = self

        menu.addItem(.separator())
        let stashItem = menu.addItem(withTitle: "Stash All Changes", action: #selector(menuStash(_:)), keyEquivalent: "")
        stashItem.target = self
        let popItem = menu.addItem(withTitle: "Pop Latest Stash", action: #selector(menuStashPop(_:)), keyEquivalent: "")
        popItem.target = self

        menu.addItem(.separator())
        let discardItem = menu.addItem(withTitle: "Discard All Changes", action: #selector(menuDiscardAll(_:)), keyEquivalent: "")
        discardItem.target = self

        return menu
    }

    @objc private func menuPull(_ sender: Any?) { pull() }
    @objc private func menuPush(_ sender: Any?) { push() }
    @objc private func menuStash(_ sender: Any?) { stash() }
    @objc private func menuStashPop(_ sender: Any?) { stashPop() }
    @objc private func menuDiscardAll(_ sender: Any?) { discardAll() }

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
                let discard = menu.addItem(withTitle: "Discard All Changes", action: #selector(menuDiscardAll(_:)), keyEquivalent: "")
                discard.target = self
            }
        case .change(let change, let isStaged):
            let stageTitle = isStaged ? "Unstage Changes" : "Stage Changes"
            let stageItem = menu.addItem(withTitle: stageTitle, action: #selector(contextToggleStage(_:)), keyEquivalent: "")
            stageItem.target = self
            stageItem.representedObject = change

            let discardItem = menu.addItem(withTitle: "Discard Changes", action: #selector(contextDiscardChange(_:)), keyEquivalent: "")
            discardItem.target = self
            discardItem.representedObject = change

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
    @objc private func contextDiscardChange(_ sender: NSMenuItem) {
        guard let change = sender.representedObject as? GitChangeInfo else { return }
        discard(change: change)
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
    let countPill = CountPillView()
    let discardAllButton = NSButton()
    let actionButton = NSButton()

    var onToggle: (() -> Void)?
    var onAction: (() -> Void)?
    var onDiscardAll: (() -> Void)?

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

        titleLabel.font = .systemFont(ofSize: 11, weight: .semibold)
        titleLabel.textColor = .labelColor
        titleLabel.translatesAutoresizingMaskIntoConstraints = false

        countPill.translatesAutoresizingMaskIntoConstraints = false

        discardAllButton.isBordered = false
        discardAllButton.target = self
        discardAllButton.action = #selector(discardAllClicked(_:))
        discardAllButton.translatesAutoresizingMaskIntoConstraints = false
        discardAllButton.contentTintColor = .secondaryLabelColor
        discardAllButton.image = NSImage(systemSymbolName: "arrow.uturn.backward", accessibilityDescription: "Discard All Changes")?
            .withSymbolConfiguration(.init(pointSize: 10, weight: .semibold))
        discardAllButton.toolTip = "Discard All Changes"

        actionButton.isBordered = false
        actionButton.target = self
        actionButton.action = #selector(actionClicked(_:))
        actionButton.translatesAutoresizingMaskIntoConstraints = false
        actionButton.contentTintColor = .secondaryLabelColor

        addSubview(chevronButton)
        addSubview(titleLabel)
        addSubview(countPill)
        addSubview(discardAllButton)
        addSubview(actionButton)

        NSLayoutConstraint.activate([
            chevronButton.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            chevronButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            chevronButton.widthAnchor.constraint(equalToConstant: 16),
            chevronButton.heightAnchor.constraint(equalToConstant: 16),

            titleLabel.leadingAnchor.constraint(equalTo: chevronButton.trailingAnchor, constant: 4),
            titleLabel.centerYAnchor.constraint(equalTo: centerYAnchor),

            countPill.leadingAnchor.constraint(equalTo: titleLabel.trailingAnchor, constant: 6),
            countPill.centerYAnchor.constraint(equalTo: centerYAnchor),

            actionButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            actionButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            actionButton.widthAnchor.constraint(equalToConstant: 18),
            actionButton.heightAnchor.constraint(equalToConstant: 18),

            discardAllButton.trailingAnchor.constraint(equalTo: actionButton.leadingAnchor, constant: -4),
            discardAllButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            discardAllButton.widthAnchor.constraint(equalToConstant: 18),
            discardAllButton.heightAnchor.constraint(equalToConstant: 18),
        ])
    }

    func configure(title: String, count: Int, isCollapsed: Bool, isStaged: Bool) {
        let chevronImage = NSImage(
            systemSymbolName: isCollapsed ? "chevron.right" : "chevron.down",
            accessibilityDescription: isCollapsed ? "Expand section" : "Collapse section"
        )?.withSymbolConfiguration(.init(pointSize: 10, weight: .bold))
        chevronButton.image = chevronImage

        titleLabel.stringValue = title
        countPill.setCount(count)

        discardAllButton.isHidden = isStaged || count == 0

        let actionSymbol = isStaged ? "minus" : "plus"
        let tooltip = isStaged ? "Unstage All Changes" : "Stage All Changes"
        actionButton.image = NSImage(
            systemSymbolName: actionSymbol,
            accessibilityDescription: tooltip
        )?.withSymbolConfiguration(.init(pointSize: 11, weight: .semibold))
        actionButton.toolTip = tooltip
        actionButton.isHidden = count == 0
    }

    @objc private func toggleClicked(_ sender: Any?) { onToggle?() }
    @objc private func actionClicked(_ sender: Any?) { onAction?() }
    @objc private func discardAllClicked(_ sender: Any?) { onDiscardAll?() }
}

@MainActor
final class SourceControlItemCellView: NSTableCellView {
    static let reuseIdentifier = NSUserInterfaceItemIdentifier("scm-item-cell")

    let iconView = NSImageView()
    let nameLabel = NSTextField(labelWithString: "")
    let dirLabel = NSTextField(labelWithString: "")
    let statusBadge = NSTextField(labelWithString: "")
    let discardButton = NSButton()
    let actionButton = NSButton()

    var onAction: (() -> Void)?
    var onDiscard: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        identifier = Self.reuseIdentifier
        setupViews()
    }

    required init?(coder: NSCoder) { fatalError() }

    private func setupViews() {
        iconView.translatesAutoresizingMaskIntoConstraints = false
        iconView.imageScaling = .scaleProportionallyDown

        nameLabel.font = .systemFont(ofSize: 12, weight: .regular)
        nameLabel.textColor = .labelColor
        nameLabel.lineBreakMode = .byTruncatingTail
        nameLabel.translatesAutoresizingMaskIntoConstraints = false
        nameLabel.setContentCompressionResistancePriority(.defaultHigh, for: .horizontal)

        dirLabel.font = .systemFont(ofSize: 11, weight: .regular)
        dirLabel.textColor = NSColor(white: 0.55, alpha: 1.0)
        dirLabel.lineBreakMode = .byTruncatingMiddle
        dirLabel.translatesAutoresizingMaskIntoConstraints = false
        dirLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        statusBadge.font = .systemFont(ofSize: 11, weight: .medium)
        statusBadge.alignment = .right
        statusBadge.translatesAutoresizingMaskIntoConstraints = false

        discardButton.isBordered = false
        discardButton.target = self
        discardButton.action = #selector(discardClicked(_:))
        discardButton.translatesAutoresizingMaskIntoConstraints = false
        discardButton.contentTintColor = .secondaryLabelColor
        discardButton.image = NSImage(systemSymbolName: "arrow.uturn.backward", accessibilityDescription: "Discard Changes")?
            .withSymbolConfiguration(.init(pointSize: 10, weight: .semibold))
        discardButton.toolTip = "Discard Changes"
        discardButton.isHidden = true

        actionButton.isBordered = false
        actionButton.target = self
        actionButton.action = #selector(actionClicked(_:))
        actionButton.translatesAutoresizingMaskIntoConstraints = false
        actionButton.contentTintColor = .secondaryLabelColor
        actionButton.isHidden = true

        addSubview(iconView)
        addSubview(nameLabel)
        addSubview(dirLabel)
        addSubview(discardButton)
        addSubview(actionButton)
        addSubview(statusBadge)

        NSLayoutConstraint.activate([
            iconView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 16),
            iconView.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: 16),
            iconView.heightAnchor.constraint(equalToConstant: 16),

            nameLabel.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 6),
            nameLabel.centerYAnchor.constraint(equalTo: centerYAnchor),

            dirLabel.leadingAnchor.constraint(equalTo: nameLabel.trailingAnchor, constant: 6),
            dirLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
            dirLabel.trailingAnchor.constraint(lessThanOrEqualTo: discardButton.leadingAnchor, constant: -4),

            discardButton.trailingAnchor.constraint(equalTo: actionButton.leadingAnchor, constant: -2),
            discardButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            discardButton.widthAnchor.constraint(equalToConstant: 16),
            discardButton.heightAnchor.constraint(equalToConstant: 16),

            actionButton.trailingAnchor.constraint(equalTo: statusBadge.leadingAnchor, constant: -4),
            actionButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            actionButton.widthAnchor.constraint(equalToConstant: 16),
            actionButton.heightAnchor.constraint(equalToConstant: 16),

            statusBadge.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            statusBadge.centerYAnchor.constraint(equalTo: centerYAnchor),
            statusBadge.widthAnchor.constraint(equalToConstant: 16),
        ])
    }

    func configure(with change: GitChangeInfo, isStaged: Bool) {
        let name = (change.path as NSString).lastPathComponent
        let dir = (change.path as NSString).deletingLastPathComponent
        nameLabel.stringValue = name
        dirLabel.stringValue = dir

        let glyph = FileGlyph.for(name: name)
        iconView.image = glyph.image
        iconView.contentTintColor = glyph.color

        let badgeInfo = statusBadgeInfo(for: change, isStaged: isStaged)
        statusBadge.stringValue = badgeInfo.letter
        statusBadge.textColor = badgeInfo.color

        let actionSymbol = isStaged ? "minus" : "plus"
        let tooltip = isStaged ? "Unstage Changes" : "Stage Changes"
        actionButton.image = NSImage(
            systemSymbolName: actionSymbol,
            accessibilityDescription: tooltip
        )?.withSymbolConfiguration(.init(pointSize: 10, weight: .semibold))
        actionButton.toolTip = tooltip
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInActiveApp, .inVisibleRect],
                                       owner: self, userInfo: nil))
    }

    override func mouseEntered(with event: NSEvent) {
        discardButton.isHidden = false
        actionButton.isHidden = false
    }

    override func mouseExited(with event: NSEvent) {
        discardButton.isHidden = true
        actionButton.isHidden = true
    }

    private func statusBadgeInfo(for change: GitChangeInfo, isStaged: Bool) -> (letter: String, color: NSColor) {
        let orangeColor = NSColor(srgbRed: 226/255.0, green: 192/255.0, blue: 141/255.0, alpha: 1.0) // VS Code Modified amber #e2c08d
        let greenColor = NSColor(srgbRed: 115/255.0, green: 201/255.0, blue: 145/255.0, alpha: 1.0) // VS Code Green #73c991
        let redColor = NSColor(srgbRed: 199/255.0, green: 78/255.0, blue: 57/255.0, alpha: 1.0)     // VS Code Red #c74e39

        let code = change.code
        if isStaged {
            let first = code.first ?? "M"
            switch first {
            case "M": return ("M", orangeColor)
            case "A": return ("A", greenColor)
            case "D": return ("D", redColor)
            case "R": return ("R", greenColor)
            default: return ("M", orangeColor)
            }
        } else {
            if code.contains("?") {
                return ("U", greenColor)
            }
            let second = code.count >= 2 ? code[code.index(after: code.startIndex)] : (code.first ?? "M")
            switch second {
            case "M": return ("M", orangeColor)
            case "D": return ("D", redColor)
            case "A": return ("A", greenColor)
            case "U": return ("U", greenColor)
            default:
                let trimmed = code.trimmingCharacters(in: .whitespaces)
                let letter = trimmed.isEmpty ? "M" : String(trimmed.prefix(1))
                return (letter, orangeColor)
            }
        }
    }

    @objc private func actionClicked(_ sender: Any?) { onAction?() }
    @objc private func discardClicked(_ sender: Any?) { onDiscard?() }
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
