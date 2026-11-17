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
        // VS Code-style badge: muted blue chip with light blue digits.
        layer?.backgroundColor = NSColor(srgbRed: 0.30, green: 0.45, blue: 0.62, alpha: 0.45).cgColor

        label.font = .monospacedDigitSystemFont(ofSize: 11, weight: .medium)
        label.textColor = NSColor(srgbRed: 0.62, green: 0.80, blue: 1.0, alpha: 1.0)
        label.alignment = .center
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 18),
            widthAnchor.constraint(greaterThanOrEqualToConstant: 18),
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 5),
            label.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -5),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    func setCount(_ count: Int) {
        label.stringValue = CountFormat.compact(count)
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

    var isGenerating: Bool = false {
        didSet {
            updateIcon()
            updateAppearance()
            if isGenerating != oldValue { onGeneratingChanged?(isGenerating) }
        }
    }
    /// Lets other controls (the sidebar header button) mirror the state.
    var onGeneratingChanged: ((Bool) -> Void)?

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

        layer?.cornerRadius = 4
        layer?.masksToBounds = true
        updateIcon()
        updateAppearance()
    }

    private func updateIcon() {
        let config = NSImage.SymbolConfiguration(pointSize: isGenerating ? 10 : 12, weight: .medium)
        let symbolName = isGenerating ? "stop.fill" : "wand.and.sparkles"
        let fallbackName = isGenerating ? "stop" : "wand.and.stars"
        let description = isGenerating ? "Stop Generating Commit Message" : "Generate Commit Message"

        let img = (NSImage(systemSymbolName: symbolName, accessibilityDescription: description)
            ?? NSImage(systemSymbolName: fallbackName, accessibilityDescription: description))?
            .withSymbolConfiguration(config)
        img?.isTemplate = true
        self.image = img

        toolTip = description
        setAccessibilityLabel(description)
        setAccessibilityRole(.button)
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
        if isGenerating {
            // Stop state: accent-tinted square on a soft accent chip.
            contentTintColor = Theme.accent
            layer?.backgroundColor = Theme.accent.withAlphaComponent(isHovered ? 0.28 : 0.18).cgColor
        } else {
            contentTintColor = isHovered ? .labelColor : .secondaryLabelColor
            layer?.backgroundColor = isHovered
                ? NSColor.labelColor.withAlphaComponent(0.08).cgColor
                : NSColor.clear.cgColor
        }
    }
}

// MARK: - Commit Message Text View

/// Multi-line message editor. Return inserts a newline (a commit message has
/// a body); ⌘Return commits.
@MainActor
final class CommitMessageTextView: NSTextView {
    var placeholder = "" { didSet { needsDisplay = true } }
    var onCommandReturn: (() -> Void)?
    var onFocusChange: ((Bool) -> Void)?
    var onTextChange: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        let isReturn = event.keyCode == 36 || event.keyCode == 76
        if isReturn, event.modifierFlags.contains(.command) {
            onCommandReturn?()
            return
        }
        super.keyDown(with: event)
    }

    override func didChangeText() {
        super.didChangeText()
        needsDisplay = true
        onTextChange?()
    }

    override func becomeFirstResponder() -> Bool {
        let accepted = super.becomeFirstResponder()
        if accepted { onFocusChange?(true) }
        return accepted
    }

    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned { onFocusChange?(false) }
        return resigned
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        guard string.isEmpty, !placeholder.isEmpty else { return }
        let attributes: [NSAttributedString.Key: Any] = [
            .font: font ?? NSFont.systemFont(ofSize: 13),
            .foregroundColor: NSColor.placeholderTextColor,
        ]
        let padding = textContainer?.lineFragmentPadding ?? 0
        NSAttributedString(string: placeholder, attributes: attributes)
            .draw(at: NSPoint(x: textContainerOrigin.x + padding, y: textContainerOrigin.y))
    }
}

// MARK: - Commit Message Input Container

/// The commit message box: one line until the text needs more, then grows to `maxLines`
/// (then scrolls), with the generate button in the top right corner.
@MainActor
final class CommitMessageInputContainer: NSView {
    static let minLines = 1
    static let maxLines = 8
    static let padding = NSEdgeInsets(top: 8, left: 10, bottom: 8, right: 32)

    let textView = CommitMessageTextView()
    let generateButton = GenerateCommitIconButton()
    private let scrollView = NSScrollView()
    private(set) var heightConstraint: NSLayoutConstraint!
    private var isFocused = false
    private var lastWidth: CGFloat = 0

    var onGenerate: (() -> Void)?
    var onTextChange: (() -> Void)?
    var onCommandReturn: (() -> Void)? {
        get { textView.onCommandReturn }
        set { textView.onCommandReturn = newValue }
    }

    var text: String {
        get { textView.string }
        set {
            textView.string = newValue
            textView.needsDisplay = true
            updateHeight()
        }
    }

    var placeholder: String {
        get { textView.placeholder }
        set { textView.placeholder = newValue }
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 6
        layer?.masksToBounds = true
        layer?.borderWidth = 1

        textView.drawsBackground = false
        textView.isRichText = false
        textView.allowsUndo = true
        textView.font = .systemFont(ofSize: 13)
        textView.textColor = .labelColor
        textView.insertionPointColor = .labelColor
        textView.textContainerInset = .zero
        textView.textContainer?.lineFragmentPadding = 0
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticSpellingCorrectionEnabled = false
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.textContainer?.widthTracksTextView = true
        textView.placeholder = "Message (⌘Enter to commit)"
        textView.setAccessibilityLabel("Commit message")
        textView.setAccessibilityIdentifier("commit-message")
        textView.onFocusChange = { [weak self] focused in self?.setFocused(focused) }
        textView.onTextChange = { [weak self] in
            self?.updateHeight()
            self?.onTextChange?()
        }

        scrollView.documentView = textView
        scrollView.drawsBackground = false
        scrollView.borderType = .noBorder
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.scrollerStyle = .overlay
        scrollView.translatesAutoresizingMaskIntoConstraints = false

        generateButton.target = self
        generateButton.action = #selector(generateClicked(_:))

        addSubview(scrollView)
        addSubview(generateButton)

        let pad = Self.padding
        heightConstraint = heightAnchor.constraint(equalToConstant: Self.height(forContent: 0, lineHeight: lineHeight))
        NSLayoutConstraint.activate([
            heightConstraint,
            scrollView.topAnchor.constraint(equalTo: topAnchor, constant: pad.top),
            scrollView.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -pad.bottom),
            scrollView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: pad.left),
            scrollView.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -pad.right),

            generateButton.topAnchor.constraint(equalTo: topAnchor, constant: 6),
            generateButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            generateButton.widthAnchor.constraint(equalToConstant: 22),
            generateButton.heightAnchor.constraint(equalToConstant: 22),
        ])
        applyColors()
    }

    required init?(coder: NSCoder) { fatalError() }

    private var lineHeight: CGFloat {
        textView.layoutManager?.defaultLineHeight(for: textView.font ?? .systemFont(ofSize: 13)) ?? 16
    }

    /// Box height for text `content` points tall: clamped to the line limits.
    static func height(forContent content: CGFloat, lineHeight: CGFloat) -> CGFloat {
        let clamped = min(max(content, lineHeight * CGFloat(minLines)), lineHeight * CGFloat(maxLines))
        return ceil(clamped) + padding.top + padding.bottom
    }

    /// Fits the box to its text. Safe to call whenever the text or width changes.
    func updateHeight() {
        guard let layout = textView.layoutManager, let container = textView.textContainer else { return }
        let width = bounds.width - Self.padding.left - Self.padding.right
        if width > 0 { container.containerSize = NSSize(width: width, height: .greatestFiniteMagnitude) }
        layout.ensureLayout(for: container)
        let target = Self.height(forContent: layout.usedRect(for: container).height, lineHeight: lineHeight)
        if abs(heightConstraint.constant - target) > 0.5 { heightConstraint.constant = target }
    }

    override func layout() {
        super.layout()
        // The text wraps at the width it is laid out with; refit once it changes.
        guard abs(bounds.width - lastWidth) > 0.5 else { return }
        lastWidth = bounds.width
        DispatchQueue.main.async { [weak self] in self?.updateHeight() }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyColors()
    }

    func setFocused(_ focused: Bool) {
        isFocused = focused
        applyColors()
    }

    private func applyColors() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            let normalBorder = isDark ? NSColor(white: 0.24, alpha: 1.0) : NSColor.separatorColor.withAlphaComponent(0.6)
            layer?.borderColor = (isFocused ? Theme.accent : normalBorder).cgColor
            layer?.borderWidth = isFocused ? 1.5 : 1.0
            layer?.backgroundColor = Theme.editorBackground.resolved(for: effectiveAppearance).cgColor
        }
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
    private let progress = NSProgressIndicator()
    private let commitLabel = NSTextField(labelWithString: "Commit")

    var onCommit: (() -> Void)?
    var onDropdown: ((NSButton) -> Void)?

    enum Mode { case commit, publish }

    private var dropdownWidth: NSLayoutConstraint?
    private var dividerWidth: NSLayoutConstraint?

    private(set) var isLoading = false

    func setLoading(_ loading: Bool, title: String? = nil) {
        isLoading = loading
        commitLabel.stringValue = title ?? (mode == .publish ? "Publish Branch" : "Commit")
        checkIcon.isHidden = loading
        progress.isHidden = !loading
        if loading { progress.startAnimation(nil) } else { progress.stopAnimation(nil) }
        mainButton.setAccessibilityLabel(commitLabel.stringValue)
    }

    /// `.publish` swaps the label for "Publish Branch" and hides the dropdown.
    var mode: Mode = .commit {
        didSet {
            guard mode != oldValue else { return }
            let publish = mode == .publish
            commitLabel.stringValue = publish ? "Publish Branch" : "Commit"
            let symbol = publish ? "icloud.and.arrow.up" : "checkmark"
            checkIcon.image = NSImage(systemSymbolName: symbol, accessibilityDescription: commitLabel.stringValue)?
                .withSymbolConfiguration(.init(pointSize: 11, weight: .bold))
            dropdownButton.isHidden = publish
            divider.isHidden = publish
            // Let the main button fill the whole bar when the dropdown is gone.
            dropdownWidth?.constant = publish ? 0 : 30
            dividerWidth?.constant = publish ? 0 : 1
            mainButton.setAccessibilityLabel(commitLabel.stringValue)
        }
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        // Same corner radius as the message field above it.
        layer?.cornerRadius = 6
        layer?.masksToBounds = true
        applyColor()

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

        progress.style = .spinning
        progress.controlSize = .small
        progress.isIndeterminate = true
        progress.isDisplayedWhenStopped = false
        progress.isHidden = true
        progress.translatesAutoresizingMaskIntoConstraints = false
        progress.setAccessibilityLabel("Git operation in progress")

        commitLabel.font = .systemFont(ofSize: 12, weight: .semibold)
        commitLabel.textColor = .white
        commitLabel.alignment = .center
        commitLabel.translatesAutoresizingMaskIntoConstraints = false

        let contentStack = NonInteractiveStackView(views: [checkIcon, progress, commitLabel])
        contentStack.orientation = .horizontal
        contentStack.spacing = 3
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

        let dropdownWidth = dropdownButton.widthAnchor.constraint(equalToConstant: 30)
        let dividerWidth = divider.widthAnchor.constraint(equalToConstant: 1)
        self.dropdownWidth = dropdownWidth
        self.dividerWidth = dividerWidth

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 30),

            mainButton.leadingAnchor.constraint(equalTo: leadingAnchor),
            mainButton.topAnchor.constraint(equalTo: topAnchor),
            mainButton.bottomAnchor.constraint(equalTo: bottomAnchor),
            mainButton.trailingAnchor.constraint(equalTo: divider.leadingAnchor),

            contentStack.centerXAnchor.constraint(equalTo: mainButton.centerXAnchor),
            contentStack.centerYAnchor.constraint(equalTo: mainButton.centerYAnchor),
            checkIcon.widthAnchor.constraint(equalToConstant: 14),
            checkIcon.heightAnchor.constraint(equalToConstant: 14),
            progress.widthAnchor.constraint(equalToConstant: 14),
            progress.heightAnchor.constraint(equalToConstant: 14),

            divider.topAnchor.constraint(equalTo: topAnchor),
            divider.bottomAnchor.constraint(equalTo: bottomAnchor),
            dividerWidth,
            divider.trailingAnchor.constraint(equalTo: dropdownButton.leadingAnchor),

            dropdownButton.trailingAnchor.constraint(equalTo: trailingAnchor),
            dropdownButton.topAnchor.constraint(equalTo: topAnchor),
            dropdownButton.bottomAnchor.constraint(equalTo: bottomAnchor),
            dropdownWidth,
        ])
        divider.layer?.backgroundColor = NSColor(white: 1.0, alpha: 0.3).cgColor
    }

    required init?(coder: NSCoder) { fatalError() }

    /// Same colour as the focused message box border above it.
    private func applyColor() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = Theme.accent.cgColor
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyColor()
    }

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
final class SourceControlController: NSObject, NSTableViewDataSource, NSTableViewDelegate {
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
    private var generateTask: Task<Void, Never>?
    private var generationID = 0
    private var draftBeforeGeneration: String?
    /// True when HEAD is on a branch with no upstream and a remote exists.
    private var isBranchUnpublished = false
    private var publishCheckTask: Task<Void, Never>?
    private(set) var isPerformingGitOperation = false

    var workspace: Workspace?
    var onOpen: ((String) -> Void)?
    /// Opens the file beside its committed version. Only changes that have one
    /// (not new or deleted files) use it; the rest go to `onOpen`.
    var onOpenDiff: ((String) -> Void)?
    var onRefresh: (() -> Void)?
    var onMessage: ((String) -> Void)?

    override init() {
        super.init()
        view.translatesAutoresizingMaskIntoConstraints = false
        messageContainer.placeholder = "Message (⌘Enter to commit)"
        messageContainer.onTextChange = { [weak self] in
            guard let self else { return }
            if self.messageContainer.generateButton.isGenerating {
                self.cancelCommitMessageGeneration(restoreDraft: false)
            }
            self.updateCommitButtonState()
        }
        messageContainer.onCommandReturn = { [weak self] in self?.commit(nil) }
        messageContainer.translatesAutoresizingMaskIntoConstraints = false
        messageContainer.onGenerate = { [weak self] in
            self?.generateCommitMessage()
        }

        splitCommitButton.translatesAutoresizingMaskIntoConstraints = false
        splitCommitButton.isEnabled = false
        splitCommitButton.onCommit = { [weak self] in
            guard let self else { return }
            if self.splitCommitButton.mode == .publish { self.publishBranch() } else { self.commit(nil) }
        }
        splitCommitButton.onDropdown = { [weak self] button in
            self?.showCommitDropdown(from: button)
        }

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("scm-changes"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        // Plain style: no automatic sidebar inset or extra row gaps.
        table.style = .plain
        table.intercellSpacing = .zero
        table.rowHeight = 24
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

            scroll.topAnchor.constraint(equalTo: splitCommitButton.bottomAnchor, constant: 6),
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scrollTrailing,
            scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }

    func focusMessageInput() {
        view.window?.makeFirstResponder(messageContainer.textView)
    }

    func refresh(status: GitStatusInfo?) {
        currentBranch = status?.branch
        updatePlaceholderString()
        checkPublishState()

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
        let hasMessage = !messageContainer.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        // Like VS Code: an unpublished branch with nothing staged and no
        // message offers "Publish Branch" in place of "Commit".
        let publish = isBranchUnpublished && stagedChanges.isEmpty && !hasMessage
        splitCommitButton.mode = publish ? .publish : .commit
        splitCommitButton.isEnabled = !isPerformingGitOperation && (publish || hasChanges)
    }

    /// Checks off the main thread whether the branch lacks an upstream.
    private func checkPublishState() {
        publishCheckTask?.cancel()
        guard let root = workspace?.root(), let branch = currentBranch, !branch.isEmpty else {
            isBranchUnpublished = false
            updateCommitButtonState()
            return
        }
        publishCheckTask = Task { [weak self] in
            let unpublished = await Task.detached(priority: .utility) {
                Self.runGit(root: root, args: ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).status != 0
                    && !Self.runGit(root: root, args: ["remote"]).output
                        .trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            }.value
            guard !Task.isCancelled, let self else { return }
            self.isBranchUnpublished = unpublished
            self.updateCommitButtonState()
        }
    }

    func publishBranch() {
        guard let root = workspace?.root(), let branch = currentBranch, !branch.isEmpty else { return }
        performGitOperation("Publishing…", success: "Published branch \(branch)") { _ in
            let remotes = Self.runGit(root: root, args: ["remote"]).output
                .split(whereSeparator: \.isNewline).map(String.init)
            guard let remote = remotes.contains("origin") ? "origin" : remotes.first else {
                return "No Git remote is configured"
            }
            return Self.gitFailure(Self.runGit(root: root, args: ["push", "-u", remote, branch]))
        }
    }


    // MARK: - NSTableViewDataSource & Delegate

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        guard row >= 0, row < rows.count else { return false }
        if case .change = rows[row] { return true }
        return false
    }

    nonisolated func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        24
    }

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
            cell.onOpenFile = { [weak self] in
                // The file itself, even when a click on the row opens its diff.
                self?.onOpen?(change.path)
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
            open(change)
        case .empty:
            break
        }
    }

    /// Changes with a committed version open as a diff; new, untracked and
    /// deleted files have nothing to compare against and open normally.
    private func open(_ change: GitChangeInfo) {
        let hasCommittedVersion = !change.code.contains("?") && !change.code.contains("A") && !change.code.contains("D")
        if hasCommittedVersion, let onOpenDiff { onOpenDiff(change.path) } else { onOpen?(change.path) }
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

    private func updatePlaceholderString() {
        // Short placeholder so it fits a narrow sidebar; the branch is in the tooltip.
        messageContainer.placeholder = messageContainer.generateButton.isGenerating
            ? "Generating with AI…"
            : "Message (⌘Enter to commit)"
        if let branch = currentBranch, !branch.isEmpty {
            messageContainer.textView.toolTip = "⌘Enter to commit on '\(branch)'"
        } else {
            messageContainer.textView.toolTip = "⌘Enter to commit"
        }
    }

    func generateCommitMessage() {
        if messageContainer.generateButton.isGenerating {
            cancelCommitMessageGeneration(restoreDraft: true)
            focusMessageInput()
            return
        }

        let targets = !stagedChanges.isEmpty ? stagedChanges : unstagedChanges
        guard !targets.isEmpty else { return }

        let fallback = heuristicCommitMessage(for: targets)

        // If AI / LLM is not configured, immediately use Option 1
        guard AICommitService.shared.isConfigured else {
            messageContainer.text = fallback
            updateCommitButtonState()
            focusMessageInput()
            return
        }

        guard let root = workspace?.root() else { return }

        // Show generating feedback
        draftBeforeGeneration = messageContainer.text
        messageContainer.text = ""
        messageContainer.generateButton.isGenerating = true
        messageContainer.generateButton.isEnabled = true
        updatePlaceholderString()

        generateTask?.cancel()
        generationID += 1
        let id = generationID
        generateTask = Task { [weak self] in
            // Git runs off the main thread: a big diff must not freeze the UI.
            let diff = await Task.detached {
                // Prefer staged changes; fall back to unstaged.
                var (_, diff) = Self.runGit(root: root, args: ["diff", "--cached"])
                if diff.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    (_, diff) = Self.runGit(root: root, args: ["diff"])
                }
                return diff
            }.value
            let hasDiff = !diff.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            let generated = hasDiff ? await AICommitService.shared.generateCommitMessage(diff: diff) : nil
            guard !Task.isCancelled, let self, self.generationID == id else { return }
            self.draftBeforeGeneration = nil
            self.messageContainer.generateButton.isGenerating = false
            self.generateTask = nil
            if let generated, !generated.isEmpty {
                self.messageContainer.text = generated
            } else {
                // Graceful fallback to the heuristic message.
                self.messageContainer.text = fallback
            }
            self.updatePlaceholderString()
            self.updateCommitButtonState()
            self.focusMessageInput()
        }
    }

    private func cancelCommitMessageGeneration(restoreDraft: Bool) {
        generationID += 1
        generateTask?.cancel()
        generateTask = nil
        messageContainer.generateButton.isGenerating = false
        if restoreDraft, let draftBeforeGeneration {
            messageContainer.text = draftBeforeGeneration
        }
        draftBeforeGeneration = nil
        updatePlaceholderString()
        updateCommitButtonState()
    }

    @objc func commit(_ sender: Any?) {
        guard workspace != nil else { return }
        if messageContainer.generateButton.isGenerating {
            cancelCommitMessageGeneration(restoreDraft: true)
        }
        var msg = messageContainer.text.trimmingCharacters(in: .whitespacesAndNewlines)
        if msg.isEmpty {
            // Auto-generate if message was left blank
            generateCommitMessage()
            msg = messageContainer.text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !msg.isEmpty else {
                focusMessageInput()
                return
            }
        }
        let message = msg

        let paths = stagedChanges.isEmpty ? unstagedChanges.map(\.path) : []
        performGitOperation("Committing…", success: "Commit created", clearMessage: true) { workspace in
            do {
                for path in paths { try workspace.gitStage(path: path) }
                try workspace.gitCommit(message: message)
                return nil
            } catch {
                return Self.describe(error)
            }
        }
    }

    func commitAmend() {
        guard workspace != nil else { return }
        if stagedChanges.isEmpty && !unstagedChanges.isEmpty {
            stageAll()
        }
        let msg = messageContainer.text.trimmingCharacters(in: .whitespacesAndNewlines)
        if msg.isEmpty {
            _ = runGit(args: ["commit", "--amend", "--no-edit"])
        } else {
            _ = runGit(args: ["commit", "--amend", "-m", msg])
            messageContainer.text = ""
        }
        updateCommitButtonState()
        onRefresh?()
    }

    func commitAndPush() {
        commitThenNetwork(sync: false)
    }

    func commitAndSync() {
        commitThenNetwork(sync: true)
    }

    func pull() {
        guard let root = workspace?.root() else { return }
        performGitOperation("Pulling…", success: "Pull complete") { _ in
            Self.gitFailure(Self.runGit(root: root, args: ["pull"]))
        }
    }

    func push() {
        guard let root = workspace?.root() else { return }
        performGitOperation("Pushing…", success: "Push complete") { _ in
            Self.gitFailure(Self.runGit(root: root, args: ["push"]))
        }
    }

    private func commitThenNetwork(sync: Bool) {
        guard let root = workspace?.root() else { return }
        let message = messageContainer.text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !message.isEmpty else { generateCommitMessage(); return }
        let paths = stagedChanges.isEmpty ? unstagedChanges.map(\.path) : []
        performGitOperation(
            sync ? "Committing & syncing…" : "Committing & pushing…",
            success: sync ? "Commit and sync complete" : "Commit and push complete",
            clearMessage: true
        ) { workspace in
            do {
                for path in paths { try workspace.gitStage(path: path) }
                try workspace.gitCommit(message: message)
            } catch {
                return Self.describe(error)
            }
            if sync,
               let failure = Self.gitFailure(Self.runGit(root: root, args: ["pull", "--rebase"])) {
                return failure
            }
            return Self.gitFailure(Self.runGit(root: root, args: ["push"]))
        }
    }

    private func performGitOperation(
        _ title: String,
        success: String,
        clearMessage: Bool = false,
        _ operation: @escaping @Sendable (Workspace) -> String?
    ) {
        guard let workspace, !isPerformingGitOperation else { return }
        isPerformingGitOperation = true
        splitCommitButton.setLoading(true, title: title)
        updateCommitButtonState()
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let failure = operation(workspace)
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.isPerformingGitOperation = false
                self.splitCommitButton.setLoading(false)
                if failure == nil, clearMessage { self.messageContainer.text = "" }
                self.onMessage?(failure.map { "Git: \($0)" } ?? success)
                self.updateCommitButtonState()
                self.onRefresh?()
            }
        }
    }

    private nonisolated static func gitFailure(_ result: (status: Int32, output: String)) -> String? {
        guard result.status != 0 else { return nil }
        let message = result.output.trimmingCharacters(in: .whitespacesAndNewlines)
        return message.isEmpty ? "Git command failed" : message
    }

    private nonisolated static func describe(_ error: Error) -> String {
        if case CoreError.Io(let message) = error { return message }
        return "\(error)"
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
        return Self.runGit(root: root, args: args)
    }

    @discardableResult
    private nonisolated static func runGit(root: String, args: [String]) -> (status: Int32, output: String) {
        let process = Process()
        let pipe = Pipe()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        process.currentDirectoryURL = URL(fileURLWithPath: root)
        process.arguments = args
        process.standardOutput = pipe
        process.standardError = pipe
        do {
            try process.run()
            // Drain the pipe before waiting: a child that fills the pipe
            // buffer (a large diff) blocks on write and never exits.
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
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
            self.open(change)
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

        titleLabel.font = .systemFont(ofSize: 13, weight: .regular)
        titleLabel.textColor = .labelColor
        titleLabel.lineBreakMode = .byTruncatingTail
        titleLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
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

        // Section actions appear on hover only, like VS Code.
        discardAllButton.alphaValue = 0
        actionButton.alphaValue = 0

        NSLayoutConstraint.activate([
            chevronButton.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 6),
            chevronButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            chevronButton.widthAnchor.constraint(equalToConstant: 14),
            chevronButton.heightAnchor.constraint(equalToConstant: 14),

            titleLabel.leadingAnchor.constraint(equalTo: chevronButton.trailingAnchor, constant: 4),
            titleLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
            titleLabel.trailingAnchor.constraint(lessThanOrEqualTo: discardAllButton.leadingAnchor, constant: -6),

            // Count sits on the right edge; hover actions appear left of it.
            countPill.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            countPill.centerYAnchor.constraint(equalTo: centerYAnchor),

            actionButton.trailingAnchor.constraint(equalTo: countPill.leadingAnchor, constant: -6),
            actionButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            actionButton.widthAnchor.constraint(equalToConstant: 18),
            actionButton.heightAnchor.constraint(equalToConstant: 18),

            discardAllButton.trailingAnchor.constraint(equalTo: actionButton.leadingAnchor, constant: -2),
            discardAllButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            discardAllButton.widthAnchor.constraint(equalToConstant: 18),
            discardAllButton.heightAnchor.constraint(equalToConstant: 18),
        ])
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInActiveApp, .inVisibleRect],
                                       owner: self, userInfo: nil))
    }

    override func mouseEntered(with event: NSEvent) { setActionsVisible(true) }
    override func mouseExited(with event: NSEvent) { setActionsVisible(false) }

    private func setActionsVisible(_ visible: Bool) {
        discardAllButton.alphaValue = visible ? 1 : 0
        actionButton.alphaValue = visible ? 1 : 0
    }

    func configure(title: String, count: Int, isCollapsed: Bool, isStaged: Bool) {
        let chevronImage = NSImage(
            systemSymbolName: isCollapsed ? "chevron.right" : "chevron.down",
            accessibilityDescription: isCollapsed ? "Expand section" : "Collapse section"
        )?.withSymbolConfiguration(.init(pointSize: 9, weight: .semibold))
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

    let indentGuide = NSView()
    let iconView = NSImageView()
    let nameLabel = NSTextField(labelWithString: "")
    let dirLabel = NSTextField(labelWithString: "")
    let statusBadge = NSTextField(labelWithString: "")
    let openFileButton = NSButton()
    let discardButton = NSButton()
    let actionButton = NSButton()

    var onAction: (() -> Void)?
    var onDiscard: (() -> Void)?
    var onOpenFile: (() -> Void)?
    /// A deleted file has nothing to open.
    private var canOpenFile = true
    private var isHovered = false
    /// The directory fills the row up to the status letter until the
    /// hover buttons appear, then gives them room.
    private var dirTrailingIdle: NSLayoutConstraint!
    private var dirTrailingHovered: NSLayoutConstraint!

    override init(frame: NSRect) {
        super.init(frame: frame)
        identifier = Self.reuseIdentifier
        setupViews()
    }

    required init?(coder: NSCoder) { fatalError() }

    private func setupViews() {
        iconView.translatesAutoresizingMaskIntoConstraints = false
        iconView.imageScaling = .scaleProportionallyDown

        indentGuide.wantsLayer = true
        indentGuide.layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(0.14).cgColor
        indentGuide.translatesAutoresizingMaskIntoConstraints = false

        nameLabel.font = .systemFont(ofSize: 13, weight: .regular)
        nameLabel.textColor = .labelColor
        nameLabel.lineBreakMode = .byTruncatingTail
        nameLabel.translatesAutoresizingMaskIntoConstraints = false
        nameLabel.setContentCompressionResistancePriority(.defaultHigh, for: .horizontal)

        dirLabel.font = .systemFont(ofSize: 12, weight: .regular)
        dirLabel.textColor = NSColor(white: 0.55, alpha: 1.0)
        dirLabel.lineBreakMode = .byTruncatingTail
        dirLabel.translatesAutoresizingMaskIntoConstraints = false
        dirLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        statusBadge.font = .monospacedSystemFont(ofSize: 11, weight: .semibold)
        statusBadge.alignment = .right
        statusBadge.translatesAutoresizingMaskIntoConstraints = false

        openFileButton.isBordered = false
        openFileButton.target = self
        openFileButton.action = #selector(openFileClicked(_:))
        openFileButton.translatesAutoresizingMaskIntoConstraints = false
        openFileButton.contentTintColor = .secondaryLabelColor
        openFileButton.image = (NSImage(systemSymbolName: "doc.badge.arrow.up", accessibilityDescription: "Open File")
            ?? NSImage(systemSymbolName: "doc.text", accessibilityDescription: "Open File"))?
            .withSymbolConfiguration(.init(pointSize: 10, weight: .semibold))
        openFileButton.toolTip = "Open File"
        openFileButton.setAccessibilityLabel("Open File")
        openFileButton.isHidden = true

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

        addSubview(indentGuide)
        addSubview(iconView)
        addSubview(nameLabel)
        addSubview(dirLabel)
        addSubview(openFileButton)
        addSubview(discardButton)
        addSubview(actionButton)
        addSubview(statusBadge)

        dirTrailingIdle = dirLabel.trailingAnchor.constraint(lessThanOrEqualTo: statusBadge.leadingAnchor, constant: -6)
        dirTrailingHovered = dirLabel.trailingAnchor.constraint(lessThanOrEqualTo: openFileButton.leadingAnchor, constant: -4)
        dirTrailingIdle.isActive = true

        NSLayoutConstraint.activate([
            // Vertical guide under the section chevron (x = 6 + 14 / 2).
            indentGuide.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            indentGuide.topAnchor.constraint(equalTo: topAnchor),
            indentGuide.bottomAnchor.constraint(equalTo: bottomAnchor),
            indentGuide.widthAnchor.constraint(equalToConstant: 1),

            // Aligned under the section title (chevron 6 + 14 + 4).
            iconView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 24),
            iconView.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: 16),
            iconView.heightAnchor.constraint(equalToConstant: 16),

            nameLabel.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 6),
            nameLabel.centerYAnchor.constraint(equalTo: centerYAnchor),

            dirLabel.leadingAnchor.constraint(equalTo: nameLabel.trailingAnchor, constant: 6),
            dirLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
            openFileButton.trailingAnchor.constraint(equalTo: discardButton.leadingAnchor, constant: -2),
            openFileButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            openFileButton.widthAnchor.constraint(equalToConstant: 16),
            openFileButton.heightAnchor.constraint(equalToConstant: 16),

            discardButton.trailingAnchor.constraint(equalTo: actionButton.leadingAnchor, constant: -2),
            discardButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            discardButton.widthAnchor.constraint(equalToConstant: 16),
            discardButton.heightAnchor.constraint(equalToConstant: 16),

            actionButton.trailingAnchor.constraint(equalTo: statusBadge.leadingAnchor, constant: -4),
            actionButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            actionButton.widthAnchor.constraint(equalToConstant: 16),
            actionButton.heightAnchor.constraint(equalToConstant: 16),

            statusBadge.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            statusBadge.centerYAnchor.constraint(equalTo: centerYAnchor),
            statusBadge.widthAnchor.constraint(equalToConstant: 14),
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

        canOpenFile = !change.code.contains("D")
        updateButtonVisibility()
    }

    private func updateButtonVisibility() {
        discardButton.isHidden = !isHovered
        actionButton.isHidden = !isHovered
        openFileButton.isHidden = !isHovered || !canOpenFile
        dirTrailingIdle.isActive = !isHovered
        dirTrailingHovered.isActive = isHovered
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        isHovered = false
        updateButtonVisibility()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInActiveApp, .inVisibleRect],
                                       owner: self, userInfo: nil))
    }

    override func mouseEntered(with event: NSEvent) {
        isHovered = true
        updateButtonVisibility()
    }

    override func mouseExited(with event: NSEvent) {
        isHovered = false
        updateButtonVisibility()
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
    @objc private func openFileClicked(_ sender: Any?) { onOpenFile?() }
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
