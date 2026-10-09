import AppKit
import os

/// Main window: sidebar (with a navigator switcher on top) | editor, and a
/// status bar along the bottom. The title bar is transparent: the sidebar
/// runs up under the traffic lights and a path bar takes the title's place
/// above the editor.
@MainActor
final class MainWindowController: NSWindowController, NSWindowDelegate {
    private let bridge: CoreBridge
    let statusBar = StatusBar()
    let pathBar = PathBar()
    /// Left-hand message in the status bar.
    var statusLabel: NSTextField { statusBar.message }
    private let scrollView = NSScrollView()
    private let splitView = NSSplitView()
    private let detailView = NSView()
    /// Holds the find bar's place in the layout. The bar itself is built on
    /// first use (⌘F): it costs about 20 ms, a tenth of the startup budget.
    private let findBarSlot = NSView()
    private var loadedFindBar: FindBar?
    var findBar: FindBar {
        if let bar = loadedFindBar { return bar }
        let bar = FindBar()
        bar.translatesAutoresizingMaskIntoConstraints = false
        findBarSlot.addSubview(bar)
        NSLayoutConstraint.activate([
            bar.topAnchor.constraint(equalTo: findBarSlot.topAnchor),
            bar.bottomAnchor.constraint(equalTo: findBarSlot.bottomAnchor),
            bar.leadingAnchor.constraint(equalTo: findBarSlot.leadingAnchor),
            bar.trailingAnchor.constraint(equalTo: findBarSlot.trailingAnchor),
        ])
        loadedFindBar = bar
        wireFindBar()
        return bar
    }
    /// Shown for files over 20 MB (M1-14).
    var largeFileBanner: NSTextField { statusBar.largeFileBadge }
    private var findBarHeight: NSLayoutConstraint!
    /// Sidebar: navigator switcher, folder name, then the explorer or an
    /// empty state.
    let sidebar = NSVisualEffectView()
    let navigator = NavigatorBar()
    private let folderTitle = NSTextField(labelWithString: "")
    let emptyState = NSStackView()
    let explorer = ExplorerController()
    private(set) var workspace: Workspace?
    let editorView = EditorView(frame: .zero)

    enum SaveChoice { case save, discard, cancel }

    /// Asks what to do with unsaved changes. Tests replace it.
    var askSaveChoice: (_ documentName: String) -> SaveChoice = { name in
        let alert = NSAlert()
        alert.messageText = "Do you want to save the changes you made to \(name)?"
        alert.informativeText = "Your changes will be lost if you don't save them."
        alert.addButton(withTitle: "Save")
        alert.addButton(withTitle: "Don't Save")
        alert.addButton(withTitle: "Cancel")
        switch alert.runModal() {
        case .alertFirstButtonReturn: return .save
        case .alertSecondButtonReturn: return .discard
        default: return .cancel
        }
    }

    /// Asks where to save an untitled document. Tests replace it.
    var askSaveLocation: (_ suggestedName: String) -> URL? = { name in
        let panel = NSSavePanel()
        panel.nameFieldStringValue = name
        panel.canCreateDirectories = true
        return panel.runModal() == .OK ? panel.url : nil
    }

    /// Title without the workspace suffix or edited marker.
    private var baseTitle = "RustCode"

    /// Default sidebar width.
    static let sidebarWidth: CGFloat = 240

    var isSidebarVisible: Bool { !sidebar.isHidden }

    init(bridge: CoreBridge) {
        self.bridge = bridge
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 960, height: 640),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "RustCode"
        // The title stays set (Window menu, Mission Control, VoiceOver) but
        // is drawn by the path bar instead.
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.setFrameAutosaveName("MainWindow")
        window.tabbingMode = .disallowed
        super.init(window: window)
        window.delegate = self
        buildContent()
        editorView.onChange = { [weak self] in
            self?.updateEditedState()
            self?.refreshFindStatus()
        }
        editorView.onCursorChange = { [weak self] primary, count in
            self?.statusBar.showCursor(primary: primary, count: count)
        }
        window.contentView?.layoutSubtreeIfNeeded()
        splitView.setPosition(Self.sidebarWidth, ofDividerAt: 0)
        if window.frameAutosaveName.isEmpty || !window.setFrameUsingName("MainWindow") {
            window.center()
        }

        bridge.onEvent = { [weak self] event in
            if case let .pong(message) = event {
                // Don't overwrite the status of an open document.
                if self?.editorView.document == nil {
                    self?.statusLabel.stringValue = "Core \(bridge.version) connected (\(message))"
                }
            }
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Bottom edge of the (transparent) title bar, in content coordinates.
    private var titleBarBottom: NSLayoutYAxisAnchor {
        (window?.contentLayoutGuide as? NSLayoutGuide)?.topAnchor ?? detailView.topAnchor
    }

    private func buildContent() {
        guard let content = window?.contentView else { return }

        statusLabel.stringValue = "Core \(bridge.version) starting…"
        statusBar.translatesAutoresizingMaskIntoConstraints = false
        statusBar.onToggleSidebar = { [weak self] in self?.toggleSidebar(nil) }

        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        // Thin scroller over the text instead of a gutter on the right.
        scrollView.scrollerStyle = .overlay
        scrollView.autohidesScrollers = true
        // A little air above line 1.
        scrollView.automaticallyAdjustsContentInsets = false
        scrollView.contentInsets = NSEdgeInsets(top: Theme.editorTopInset, left: 0, bottom: 0, right: 0)
        scrollView.drawsBackground = true
        scrollView.backgroundColor = .textBackgroundColor
        scrollView.borderType = .noBorder
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        editorView.autoresizingMask = [.width]
        scrollView.documentView = editorView

        detailView.translatesAutoresizingMaskIntoConstraints = false
        pathBar.translatesAutoresizingMaskIntoConstraints = false
        pathBar.show(["RustCode"])
        findBarSlot.translatesAutoresizingMaskIntoConstraints = false
        findBarSlot.isHidden = true
        findBarHeight = findBarSlot.heightAnchor.constraint(equalToConstant: 0)
        detailView.addSubview(pathBar)
        detailView.addSubview(findBarSlot)
        detailView.addSubview(scrollView)
        NSLayoutConstraint.activate([
            pathBar.topAnchor.constraint(equalTo: detailView.topAnchor),
            pathBar.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
            pathBar.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
            findBarSlot.topAnchor.constraint(equalTo: pathBar.bottomAnchor),
            findBarSlot.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
            findBarSlot.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
            findBarHeight,
            scrollView.topAnchor.constraint(equalTo: findBarSlot.bottomAnchor),
            scrollView.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
            scrollView.bottomAnchor.constraint(equalTo: detailView.bottomAnchor),
        ])

        buildSidebar()

        splitView.isVertical = true
        splitView.dividerStyle = .thin
        splitView.translatesAutoresizingMaskIntoConstraints = false
        splitView.addArrangedSubview(sidebar)
        splitView.addArrangedSubview(detailView)
        splitView.setHoldingPriority(.defaultHigh, forSubviewAt: 0)

        content.addSubview(splitView)
        content.addSubview(statusBar)
        NSLayoutConstraint.activate([
            splitView.topAnchor.constraint(equalTo: content.topAnchor),
            splitView.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            splitView.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            splitView.bottomAnchor.constraint(equalTo: statusBar.topAnchor),
            statusBar.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            statusBar.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            statusBar.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            // Only valid once both views are in the window.
            pathBar.bottomAnchor.constraint(equalTo: titleBarBottom),
            navigator.topAnchor.constraint(equalTo: titleBarBottom, constant: 4),
        ])
    }

    private func buildSidebar() {
        sidebar.setAccessibilityIdentifier("sidebar")
        sidebar.material = .sidebar
        sidebar.blendingMode = .behindWindow
        sidebar.state = .followsWindowActiveState
        sidebar.translatesAutoresizingMaskIntoConstraints = false
        sidebar.widthAnchor.constraint(greaterThanOrEqualToConstant: 160).isActive = true

        navigator.translatesAutoresizingMaskIntoConstraints = false
        navigator.onSelect = { [weak self] item in
            if item == .explorer { self?.setSidebarVisible(true) }
        }
        let navigatorRule = NSBox()
        navigatorRule.boxType = .separator
        navigatorRule.translatesAutoresizingMaskIntoConstraints = false

        folderTitle.font = .systemFont(ofSize: 12, weight: .semibold)
        folderTitle.textColor = .labelColor
        folderTitle.lineBreakMode = .byTruncatingMiddle
        folderTitle.isHidden = true
        folderTitle.setAccessibilityIdentifier("folder-title")
        let header = NSStackView(views: [folderTitle])
        header.orientation = .vertical
        header.alignment = .leading
        header.translatesAutoresizingMaskIntoConstraints = false

        // Shown until a folder is opened.
        let message = NSTextField(wrappingLabelWithString: "No folder open")
        message.font = .systemFont(ofSize: 12)
        message.textColor = .secondaryLabelColor
        message.alignment = .center
        let button = NSButton(title: "Open Folder…", target: self, action: #selector(openFolder(_:)))
        button.bezelStyle = .rounded
        button.controlSize = .large
        emptyState.setViews([message, button], in: .leading)
        emptyState.orientation = .vertical
        emptyState.alignment = .centerX
        emptyState.spacing = 12
        emptyState.translatesAutoresizingMaskIntoConstraints = false

        explorer.scrollView.isHidden = true
        explorer.scrollView.translatesAutoresizingMaskIntoConstraints = false
        explorer.onOpenFile = { [weak self] workspace, entry in
            self?.openWorkspaceFile(workspace, entry)
        }

        sidebar.addSubview(navigator)
        sidebar.addSubview(navigatorRule)
        sidebar.addSubview(header)
        sidebar.addSubview(explorer.scrollView)
        sidebar.addSubview(emptyState)
        NSLayoutConstraint.activate([
            navigator.leadingAnchor.constraint(equalTo: sidebar.leadingAnchor),
            navigator.trailingAnchor.constraint(equalTo: sidebar.trailingAnchor),
            navigatorRule.topAnchor.constraint(equalTo: navigator.bottomAnchor, constant: 4),
            navigatorRule.leadingAnchor.constraint(equalTo: sidebar.leadingAnchor),
            navigatorRule.trailingAnchor.constraint(equalTo: sidebar.trailingAnchor),
            header.topAnchor.constraint(equalTo: navigatorRule.bottomAnchor, constant: 8),
            header.leadingAnchor.constraint(equalTo: sidebar.leadingAnchor, constant: 14),
            header.trailingAnchor.constraint(lessThanOrEqualTo: sidebar.trailingAnchor, constant: -8),
            explorer.scrollView.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 6),
            explorer.scrollView.leadingAnchor.constraint(equalTo: sidebar.leadingAnchor),
            explorer.scrollView.trailingAnchor.constraint(equalTo: sidebar.trailingAnchor),
            explorer.scrollView.bottomAnchor.constraint(equalTo: sidebar.bottomAnchor),
            emptyState.centerXAnchor.constraint(equalTo: sidebar.centerXAnchor),
            emptyState.centerYAnchor.constraint(equalTo: sidebar.centerYAnchor, constant: -20),
            emptyState.leadingAnchor.constraint(greaterThanOrEqualTo: sidebar.leadingAnchor, constant: 16),
            emptyState.trailingAnchor.constraint(lessThanOrEqualTo: sidebar.trailingAnchor, constant: -16),
        ])
    }

    // MARK: Sidebar

    @objc func toggleSidebar(_ sender: Any?) {
        setSidebarVisible(!isSidebarVisible)
    }

    @objc func showExplorer(_ sender: Any?) {
        setSidebarVisible(true)
        if workspace != nil { window?.makeFirstResponder(explorer.outlineView) }
    }

    func setSidebarVisible(_ visible: Bool) {
        guard visible != isSidebarVisible else { return }
        sidebar.isHidden = !visible
        pathBar.clearsTrafficLights = !visible
        if visible {
            splitView.layoutSubtreeIfNeeded()
            if sidebar.frame.width < 160 { splitView.setPosition(Self.sidebarWidth, ofDividerAt: 0) }
        }
    }

    /// Starts an empty "Untitled" buffer, ready for typing.
    @objc func newDocument(_ sender: Any?) {
        guard confirmDiscardChanges() else { return }
        show(bridge.newDocument(), title: "Untitled", path: ["Untitled"],
             status: "Untitled. Type here, or press ⌘O to open a file or folder.")
    }

    private func show(_ doc: DocumentHandle, title: String, path: [String], status: String) {
        pathBar.show(path)
        editorView.document = doc
        scrollToTop()
        statusLabel.stringValue = status
        statusBar.show(document: doc)
        baseTitle = title
        updateEditedState()
        if isFindVisible {
            applyFind(select: false)
        } else {
            window?.makeFirstResponder(editorView)
        }
    }

    /// Top of the document, inset included.
    func scrollToTop() {
        scrollView.contentView.scroll(to: NSPoint(x: 0, y: -scrollView.contentInsets.top))
        scrollView.reflectScrolledClipView(scrollView.contentView)
    }

    // MARK: Latency (M1-16)

    /// Shows keypress-to-render latency for the last keys typed here.
    @objc func showTypingLatency(_ sender: Any?) {
        let alert = NSAlert()
        alert.messageText = "Typing latency"
        if let s = LatencyProbe.shared.summary {
            alert.informativeText = String(
                format: "Last %d keys, from the key press to the end of drawing:\n\nmedian %.1f ms, 95th percentile %.1f ms, worst %.1f ms\nDrawing alone: median %.1f ms\n\nTarget: under 8 ms. Putting the frame on screen adds up to one display refresh.",
                s.count, s.p50, s.p95, s.max, s.drawP50)
        } else {
            alert.informativeText = "Type something in the editor first."
        }
        if let window { alert.beginSheetModal(for: window) } else { alert.runModal() }
    }

    // MARK: Find and replace (M1-13)

    /// Does not build the bar: this is checked on every edit.
    var isFindVisible: Bool { loadedFindBar != nil && !findBarSlot.isHidden }

    /// Background match count in flight, if any. Only the newest one may
    /// update the bar; older ones are cancelled.
    private var countJob: FindCountJob?
    var isCountingFind: Bool { countJob != nil }
    /// Called after a background count updated the bar (tests wait on it).
    var onFindCountFinished: (() -> Void)?

    private func wireFindBar() {
        findBar.onPatternChange = { [weak self] in self?.applyFind(select: true) }
        findBar.onNext = { [weak self] in self?.findNext(nil) }
        findBar.onPrevious = { [weak self] in self?.findPrevious(nil) }
        findBar.onSelectAll = { [weak self] in self?.selectAllMatches() }
        findBar.onReplace = { [weak self] in self?.replaceCurrent() }
        findBar.onReplaceAll = { [weak self] in self?.replaceAll() }
        findBar.onClose = { [weak self] in self?.closeFind(nil) }
    }

    @objc func showFind(_ sender: Any?) { presentFind(replace: false) }

    @objc func showFindAndReplace(_ sender: Any?) { presentFind(replace: true) }

    /// ⌘E: searches for the selected text without moving focus.
    @objc func useSelectionForFind(_ sender: Any?) {
        guard editorView.document != nil, let seed = selectionSeed() else { return }
        findBar.setPattern(findBar.config.regex ? Self.escapeRegex(seed) : seed)
    }

    private func presentFind(replace: Bool) {
        guard editorView.document != nil else { return }
        if let seed = selectionSeed() {
            findBar.findField.stringValue = findBar.config.regex ? Self.escapeRegex(seed) : seed
        }
        if replace { findBar.setReplaceVisible(true) }
        setFindVisible(true)
        applyFind(select: true)
        findBar.focusFindField()
    }

    func setFindVisible(_ visible: Bool) {
        findBarSlot.isHidden = !visible
        findBarHeight.constant = visible ? findBar.preferredHeight : 0
    }

    @objc func closeFind(_ sender: Any?) {
        guard isFindVisible else { return }
        setFindVisible(false)
        countJob?.cancel()
        countJob = nil
        editorView.document?.clearFind()
        editorView.didChangeSelection()
        window?.makeFirstResponder(editorView)
    }

    @objc func findNext(_ sender: Any?) {
        guard let doc = editorView.document else { return }
        if !isFindVisible || findBar.pattern.isEmpty { return presentFind(replace: false) }
        _ = doc.findNext()
        finishFindMove()
    }

    @objc func findPrevious(_ sender: Any?) {
        guard let doc = editorView.document else { return }
        if !isFindVisible || findBar.pattern.isEmpty { return presentFind(replace: false) }
        _ = doc.findPrevious()
        finishFindMove()
    }

    /// Re-reads pattern and options from the bar. `select` jumps to the
    /// nearest match, as typing in the field does.
    func applyFind(select: Bool) {
        guard let doc = editorView.document else { return }
        _ = doc.setFind(pattern: findBar.pattern, config: findBar.config)
        if select { _ = doc.findNearest() }
        finishFindMove()
    }

    private func finishFindMove() {
        updateFindStatus()
        editorView.didChangeSelection()
    }

    /// Shows the match count. Small documents are counted right here; big
    /// ones on a background thread from a snapshot, and the result is
    /// dropped if the text or the search changed in the meantime.
    func updateFindStatus() {
        countJob?.cancel()
        countJob = nil
        guard let doc = editorView.document else { return }
        let status = doc.findStatus()
        findBar.show(status: status)
        guard !status.counted, let job = doc.startFindCount() else { return }
        countJob = job
        let revision = job.revision()
        Task.detached(priority: .userInitiated) { [weak self] in
            let result = job.run()
            await MainActor.run {
                guard let self, self.countJob === job else { return }
                self.countJob = nil
                guard let result, self.editorView.document === doc,
                      doc.revision() == revision, self.isFindVisible else { return }
                self.findBar.show(status: result)
                self.onFindCountFinished?()
            }
        }
    }

    /// Keeps the "3 of 12" label right while the text changes.
    func refreshFindStatus() {
        guard isFindVisible else { return }
        updateFindStatus()
    }

    private func selectAllMatches() {
        guard let doc = editorView.document, doc.selectAllMatches() else { return }
        updateFindStatus()
        editorView.didChangeSelection()
        window?.makeFirstResponder(editorView)
    }

    private func replaceCurrent() {
        guard let doc = editorView.document else { return }
        _ = doc.replaceCurrent(replacement: findBar.replacement)
        editorView.didChangeDocument()
        finishFindMove()
    }

    private func replaceAll() {
        guard let doc = editorView.document else { return }
        let count = doc.replaceAll(replacement: findBar.replacement)
        editorView.didChangeDocument()
        finishFindMove()
        if count > 0 {
            statusLabel.stringValue = "Replaced \(count) occurrence\(count == 1 ? "" : "s")"
        }
    }

    /// The selected text if it is a sensible search seed: one line, short.
    private func selectionSeed() -> String? {
        guard let doc = editorView.document else { return nil }
        let range = doc.selectedRange()
        guard range.length > 0, range.length <= 200 else { return nil }
        let text = doc.textInRange(range: range)
        return text.contains("\n") ? nil : text
    }

    private static func escapeRegex(_ text: String) -> String {
        let special: Set<Character> = ["\\", ".", "+", "*", "?", "(", ")", "|", "[", "]", "{", "}", "^", "$"]
        return text.reduce(into: "") { out, c in
            if special.contains(c) { out.append("\\") }
            out.append(c)
        }
    }

    // MARK: Saving

    /// Shows the title and the "edited" dot in the close button.
    func updateEditedState() {
        window?.title = baseTitle
        let edited = editorView.document?.isModified() ?? false
        window?.isDocumentEdited = edited
        pathBar.setEdited(edited)
    }

    private var documentName: String {
        guard let doc = editorView.document else { return "Untitled" }
        let path = doc.path()
        return path.isEmpty ? "Untitled" : URL(fileURLWithPath: path).lastPathComponent
    }

    @objc func saveDocument(_ sender: Any?) {
        _ = save()
    }

    @objc func saveDocumentAs(_ sender: Any?) {
        _ = saveAs()
    }

    /// Saves to the current path, or asks for one. `false` if it failed or
    /// was cancelled.
    @discardableResult
    func save() -> Bool {
        guard let doc = editorView.document else { return true }
        if doc.path().isEmpty { return saveAs() }
        do {
            try doc.save()
        } catch {
            report(error, while: "save \(documentName)")
            return false
        }
        savedStatus(doc)
        return true
    }

    @discardableResult
    func saveAs() -> Bool {
        guard let doc = editorView.document, let url = askSaveLocation(documentName) else { return false }
        do {
            try doc.saveAs(path: url.path)
        } catch {
            report(error, while: "save \(url.lastPathComponent)")
            return false
        }
        baseTitle = url.lastPathComponent
        pathBar.show([url.lastPathComponent])
        savedStatus(doc)
        return true
    }

    private func savedStatus(_ doc: DocumentHandle) {
        statusLabel.stringValue = "\(documentName): saved, \(doc.lineCount()) lines, \(doc.lenBytes()) bytes"
        updateEditedState()
    }

    /// `true` when it is safe to replace or close the current document:
    /// nothing unsaved, or the user saved or chose to discard.
    func confirmDiscardChanges() -> Bool {
        guard let doc = editorView.document, doc.isModified() else { return true }
        switch askSaveChoice(documentName) {
        case .save: return save()
        case .discard: return true
        case .cancel: return false
        }
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        editorView.flushPendingUpdates()
        return confirmDiscardChanges()
    }

    /// ⌘O: pick a file or a folder.
    @objc func openDocument(_ sender: Any?) {
        runOpenPanel(files: true)
    }

    /// ⇧⌘O and the sidebar button: pick a folder only.
    @objc func openFolder(_ sender: Any?) {
        runOpenPanel(files: false)
    }

    private func runOpenPanel(files: Bool) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = files
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Open"
        guard window != nil, panel.runModal() == .OK, let url = panel.url else { return }
        open(url: url)
    }

    /// Opens a folder as the workspace, or a single file in the editor.
    func open(url: URL) {
        do {
            var isDirectory: ObjCBool = false
            guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory) else {
                throw CocoaError(.fileNoSuchFile)
            }
            if isDirectory.boolValue {
                try openWorkspace(at: url)
            } else {
                try openFile(at: url)
            }
        } catch {
            report(error, while: "open \(url.lastPathComponent)")
        }
    }

    func openWorkspace(at url: URL) throws {
        let ws = try bridge.core.openWorkspace(path: url.path)
        workspace = ws
        explorer.setWorkspace(ws)
        explorer.scrollView.isHidden = false
        emptyState.isHidden = true
        folderTitle.stringValue = ws.name()
        folderTitle.isHidden = false
        setSidebarVisible(true)
        if editorView.document == nil {
            baseTitle = ws.name()
            pathBar.show([ws.name()])
        }
        updateEditedState()
        statusLabel.stringValue = "\(ws.name()): choose a file in the sidebar."
    }

    func openFile(at url: URL) throws {
        guard confirmDiscardChanges() else { return }
        let start = ContinuousClock.now
        let doc = try bridge.openDocument(path: url.path)
        show(doc, title: url.lastPathComponent, path: [url.lastPathComponent],
             status: describe(doc, name: url.lastPathComponent, since: start))
    }

    private func openWorkspaceFile(_ workspace: Workspace, _ entry: FileEntry) {
        guard confirmDiscardChanges() else { return }
        do {
            let start = ContinuousClock.now
            let doc = try workspace.openFile(relPath: entry.relPath)
            show(doc, title: "\(entry.name) — \(workspace.name())",
                 path: [workspace.name()] + entry.relPath.split(separator: "/").map(String.init),
                 status: describe(doc, name: entry.relPath, since: start))
        } catch {
            report(error, while: "open \(entry.relPath)")
        }
    }

    private func describe(_ doc: DocumentHandle, name: String, since start: ContinuousClock.Instant) -> String {
        let parts = (ContinuousClock.now - start).components
        let ms = Double(parts.seconds) * 1000 + Double(parts.attoseconds) / 1e15
        return "\(name): \(doc.lineCount()) lines, \(doc.lenBytes()) bytes, opened in \(String(format: "%.1f", ms)) ms"
    }

    private func report(_ error: Error, while action: String) {
        Log.ui.error("\(action, privacy: .public) failed: \(String(describing: error), privacy: .public)")
        if let window { NSAlert(error: error).beginSheetModal(for: window) }
    }
}
