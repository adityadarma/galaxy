import AppKit
import os

/// Main window, a mix of VSCode and the JetBrains New UI:
///
///     [ traffic lights                                            ]  title bar
///     [ activity | sidebar   | tabs                               ]
///     [   bar    | explorer  | breadcrumb                         ]
///     [          |           | editor                             ]
///     [ status bar                                                ]
///
/// The window's own title bar is transparent and hidden; `TitleBar` draws
/// in its place.
@MainActor
final class MainWindowController: NSWindowController, NSWindowDelegate {
    private let bridge: CoreBridge
    let statusBar = StatusBar()
    let pathBar = PathBar()
    let titleBar = TitleBar()
    let activityBar = ActivityBar()
    let tabBar = TabBar()
    /// Open documents, in tab order (M2-06).
    private(set) var tabs: [EditorTab] = []
    private(set) var activeIndex: Int?
    var activeTab: EditorTab? { activeIndex.map { tabs[$0] } }
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
    /// Sidebar, VSCode style: "EXPLORER" view title, a collapsible
    /// section named after the folder, then the tree or an empty state.
    let sidebar = SidebarView()
    let sectionHeader = SidebarSectionHeader()
    let emptyState = NSStackView()
    let explorer = ExplorerController()
    private(set) var workspace: Workspace?
    /// Go to File index of `workspace`, built in the background.
    private(set) var fileFinder: FileFinder?
    private var finderGeneration = 0
    /// Called when a (re)built index is installed (tests wait on it).
    var onFileFinderReady: (() -> Void)?
    /// Files opened in this workspace, most recent first, relative paths.
    private(set) var recentFiles: [String] = []
    private var loadedQuickOpen: QuickOpen?
    private var loadedGoToLine: GoToLine?
    private var loadedPalette: CommandPaletteView?
    /// Command registry (M2-09); kept for the window so recency survives
    /// closing the palette.
    let commands = CommandPalette()
    /// What had focus before the palette opened; commands run from there.
    private weak var paletteReturnResponder: NSResponder?
    /// Back/forward history (M2-08).
    let navHistory = NavigationHistory()
    /// Set while back/forward shows a location, so that move is not
    /// itself recorded as a jump.
    private var isNavigating = false
    private var keyMonitor: Any?
    /// Tabs in the order they were last shown, most recent first (⌃Tab).
    private var tabOrderByUse: [ObjectIdentifier] = []
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

    /// Default sidebar width.
    static let sidebarWidth: CGFloat = 240
    static let minSidebarWidth: CGFloat = 160
    /// The editor keeps at least this much room when the sidebar grows.
    static let minEditorWidth: CGFloat = 240

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
        // `TitleBar` draws it.
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
        editorView.onClickMove = { [weak self] before, after in
            guard let self, let tab = self.activeTab else { return }
            _ = self.navHistory.recordJump(
                from: NavLocation(key: tab.navKey, line: before.line, col: before.col),
                to: NavLocation(key: tab.navKey, line: after.line, col: after.col))
        }
        editorView.onCursorChange = { [weak self] primary, count in
            self?.statusBar.showCursor(primary: primary, count: count)
        }
        window.contentView?.layoutSubtreeIfNeeded()
        splitView.setPosition(Self.sidebarWidth, ofDividerAt: 0)
        if window.frameAutosaveName.isEmpty || !window.setFrameUsingName("MainWindow") {
            window.center()
        }

        installKeyMonitor()

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
        titleBar.translatesAutoresizingMaskIntoConstraints = false
        activityBar.translatesAutoresizingMaskIntoConstraints = false
        activityBar.setActive(.explorer)
        activityBar.onSelect = { [weak self] item in
            guard let self, item == .explorer else { return }
            // Clicking the active item collapses the sidebar, as in VSCode.
            self.setSidebarVisible(!self.isSidebarVisible)
        }

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

        tabBar.translatesAutoresizingMaskIntoConstraints = false
        tabBar.onSelect = { [weak self] i in self?.selectTab(i, fromTabBar: true) }
        tabBar.onClose = { [weak self] i in self?.closeTab(at: i) }
        tabBar.onMove = { [weak self] from, to in self?.moveTab(from: from, to: to) }
        tabBar.titleBar = titleBar
        pathBar.translatesAutoresizingMaskIntoConstraints = false
        pathBar.show([])
        findBarSlot.translatesAutoresizingMaskIntoConstraints = false
        findBarSlot.isHidden = true
        findBarHeight = findBarSlot.heightAnchor.constraint(equalToConstant: 0)
        detailView.addSubview(tabBar)
        detailView.addSubview(pathBar)
        detailView.addSubview(findBarSlot)
        detailView.addSubview(scrollView)
        NSLayoutConstraint.activate([
            tabBar.topAnchor.constraint(equalTo: detailView.topAnchor),
            tabBar.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
            tabBar.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
            pathBar.topAnchor.constraint(equalTo: tabBar.bottomAnchor),
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
        // Frame-based panes: NSSplitView sizes them itself, which keeps the
        // divider draggable. The delegate sets the limits and keeps the
        // sidebar's width when the window resizes.
        splitView.delegate = self
        sidebar.autoresizingMask = [.height]
        detailView.autoresizingMask = [.width, .height]
        splitView.addSubview(sidebar)
        splitView.addSubview(detailView)

        content.addSubview(titleBar)
        content.addSubview(activityBar)
        content.addSubview(splitView)
        content.addSubview(statusBar)
        NSLayoutConstraint.activate([
            titleBar.topAnchor.constraint(equalTo: content.topAnchor),
            titleBar.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            titleBar.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            activityBar.topAnchor.constraint(equalTo: titleBar.bottomAnchor),
            activityBar.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            activityBar.bottomAnchor.constraint(equalTo: statusBar.topAnchor),
            splitView.topAnchor.constraint(equalTo: titleBar.bottomAnchor),
            splitView.leadingAnchor.constraint(equalTo: activityBar.trailingAnchor),
            splitView.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            splitView.bottomAnchor.constraint(equalTo: statusBar.topAnchor),
            statusBar.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            statusBar.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            statusBar.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            // Only valid once the bar is in the window.
            titleBar.bottomAnchor.constraint(equalTo: titleBarBottom),
        ])
    }

    private func buildSidebar() {
        sidebar.setAccessibilityIdentifier("sidebar")

        // One header row, as in VSCode with a single folder open: the
        // folder name (or EXPLORER) and the explorer actions.
        sectionHeader.translatesAutoresizingMaskIntoConstraints = false
        sectionHeader.onNewFile = { [weak self] in self?.explorerNewFile(nil) }
        sectionHeader.onNewFolder = { [weak self] in self?.explorerNewFolder(nil) }
        sectionHeader.onRefresh = { [weak self] in self?.refreshExplorer(nil) }
        sectionHeader.onCollapseAll = { [weak self] in self?.collapseExplorerFolders(nil) }
        sectionHeader.moreMenu = { [weak self] in self?.explorerMoreMenu() ?? NSMenu() }
        sectionHeader.showsTools = false

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
            self?.openWorkspaceFile(workspace, entry, preview: true)
        }
        explorer.onPinFile = { [weak self] workspace, entry in
            self?.openWorkspaceFile(workspace, entry, preview: false)
        }

        sidebar.addSubview(sectionHeader)
        sidebar.addSubview(explorer.scrollView)
        sidebar.addSubview(emptyState)
        NSLayoutConstraint.activate([
            sectionHeader.topAnchor.constraint(equalTo: sidebar.topAnchor),
            sectionHeader.leadingAnchor.constraint(equalTo: sidebar.leadingAnchor),
            sectionHeader.trailingAnchor.constraint(equalTo: sidebar.trailingAnchor),
            explorer.scrollView.topAnchor.constraint(equalTo: sectionHeader.bottomAnchor),
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

    @objc func toggleSidebarVisibility(_ sender: Any?) {
        setSidebarVisible(!isSidebarVisible)
    }

    @objc func showExplorer(_ sender: Any?) {
        setSidebarVisible(true)
        if workspace != nil { window?.makeFirstResponder(explorer.outlineView) }
    }

    func setSidebarVisible(_ visible: Bool) {
        guard visible != isSidebarVisible else { return }
        sidebar.isHidden = !visible
        activityBar.setActive(visible ? .explorer : nil)
        if visible {
            splitView.layoutSubtreeIfNeeded()
            if sidebar.frame.width < Self.minSidebarWidth { splitView.setPosition(Self.sidebarWidth, ofDividerAt: 0) }
        }
    }

    /// Starts an empty "Untitled" buffer in a new tab, ready for typing.
    @objc func newDocument(_ sender: Any?) {
        let tab = EditorTab(doc: bridge.newDocument(), title: "Untitled", pathComponents: ["Untitled"],
                            windowTitle: "Untitled", isPreview: false,
                            status: "Untitled. Type here, or press ⌘O to open a file or folder.")
        add(tab, replacing: nil)
    }

    // MARK: Tabs (M2-06)

    /// Shows tab `index` in the editor. Coming from the tab bar, the
    /// explorer selection is cleared so clicking that file again opens it.
    func selectTab(_ index: Int, fromTabBar: Bool = false) {
        guard tabs.indices.contains(index) else { return }
        if let old = activeTab, old !== tabs[index] {
            recordFindOrigin()
            noteLeaving(old)
        }
        if let old = activeTab {
            old.scrollY = scrollView.contentView.bounds.origin.y
            old.status = statusLabel.stringValue
        }
        activeIndex = index
        let tab = tabs[index]
        let id = ObjectIdentifier(tab)
        tabOrderByUse.removeAll { $0 == id }
        tabOrderByUse.insert(id, at: 0)
        pathBar.show(tab.pathComponents)
        editorView.document = tab.doc
        if let y = tab.scrollY {
            scrollView.contentView.scroll(to: NSPoint(x: 0, y: y))
            scrollView.reflectScrolledClipView(scrollView.contentView)
        } else {
            scrollToTop()
        }
        statusLabel.stringValue = tab.status
        statusBar.show(document: tab.doc)
        if let rel = tab.relPath { noteRecent(rel) }
        refreshTabBar()
        updateEditedState()
        if fromTabBar { explorer.outlineView.deselectAll(nil) }
        if isFindVisible {
            applyFind(select: false)
        } else {
            window?.makeFirstResponder(editorView)
        }
    }

    /// ⌃Tab: the tab shown before this one. Pressed again, it goes back.
    @objc func selectLastUsedTab(_ sender: Any?) {
        let live = Set(tabs.map(ObjectIdentifier.init))
        tabOrderByUse.removeAll { !live.contains($0) }
        guard tabOrderByUse.count > 1,
              let i = tabs.firstIndex(where: { ObjectIdentifier($0) == tabOrderByUse[1] }) else {
            return NSSound.beep()
        }
        selectTab(i, fromTabBar: true)
    }

    @objc func showNextTab(_ sender: Any?) {
        guard let i = activeIndex, tabs.count > 1 else { return }
        selectTab((i + 1) % tabs.count, fromTabBar: true)
    }

    @objc func showPreviousTab(_ sender: Any?) {
        guard let i = activeIndex, tabs.count > 1 else { return }
        selectTab((i + tabs.count - 1) % tabs.count, fromTabBar: true)
    }

    /// ⌘W: closes the active tab, or the window when no tab is left.
    @objc func closeTab(_ sender: Any?) {
        if let i = activeIndex { closeTab(at: i) } else { window?.performClose(nil) }
    }

    /// Closes tab `index`, asking first if it has unsaved changes. `false`
    /// if the user cancelled.
    @discardableResult
    func closeTab(at index: Int) -> Bool {
        guard tabs.indices.contains(index) else { return false }
        editorView.flushPendingUpdates()
        if tabs[index].doc.isModified() {
            if activeIndex != index { selectTab(index) }
            guard confirmDiscardActive() else { return false }
        }
        let wasActive = activeIndex == index
        // A closed file can be reopened by Back; an Untitled buffer cannot.
        if tabs[index].fileKey == nil {
            navHistory.forget(key: tabs[index].navKey)
        } else if wasActive {
            noteLeaving(tabs[index])
        }
        tabs.remove(at: index)
        if let a = activeIndex {
            activeIndex = a > index ? a - 1 : (a == index ? nil : a)
        }
        if tabs.isEmpty {
            showNoDocument()
        } else if wasActive {
            selectTab(min(index, tabs.count - 1), fromTabBar: true)
        } else {
            refreshTabBar()
            updateEditedState()
        }
        return true
    }

    func moveTab(from: Int, to: Int) {
        guard tabs.indices.contains(from), tabs.indices.contains(to), from != to else { return }
        let active = activeTab
        tabs.insert(tabs.remove(at: from), at: to)
        activeIndex = active.flatMap { a in tabs.firstIndex { $0 === a } }
        refreshTabBar()
    }

    private func refreshTabBar() {
        tabBar.set(items: tabs.map { TabBar.Item(title: $0.title, isPreview: $0.isPreview, isEdited: $0.isEdited) },
                   selected: activeIndex)
    }

    /// Adds `tab` after the active one, or puts it in place of tab
    /// `replacing`, and shows it.
    private func add(_ tab: EditorTab, replacing: Int?) {
        if let r = replacing {
            if activeIndex == r {
                noteLeaving(tabs[r])
                activeIndex = nil
            }
            tabs[r] = tab
            selectTab(r)
        } else {
            let at = activeIndex.map { $0 + 1 } ?? tabs.count
            tabs.insert(tab, at: at)
            if let a = activeIndex, a >= at { activeIndex = a + 1 }
            selectTab(at)
        }
    }

    /// Where a newly opened file goes: over the current preview tab, or
    /// over an untouched empty "Untitled" tab, or (nil) a new tab.
    private func slotForOpening(preview: Bool) -> Int? {
        if preview, let i = tabs.firstIndex(where: { $0.isPreview && !$0.isEdited }) { return i }
        guard let i = activeIndex else { return nil }
        let doc = tabs[i].doc
        return doc.path().isEmpty && !doc.isModified() && doc.lenBytes() == 0 ? i : nil
    }

    /// Same file, however it was reached (symlinks, /private/var).
    private static func fileKey(_ path: String) -> String? {
        path.isEmpty ? nil : URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL.path
    }

    private func tabIndex(forPath path: String) -> Int? {
        guard let key = Self.fileKey(path) else { return nil }
        return tabs.firstIndex { $0.fileKey == key }
    }

    /// Editor with no document: after the last tab is closed.
    private func showNoDocument() {
        activeIndex = nil
        if isFindVisible { closeFind(nil) }
        editorView.document = nil
        pathBar.show([])
        statusBar.show(document: nil)
        statusLabel.stringValue = "No open files. ⌘N for a new one, ⌘O to open a file or folder."
        refreshTabBar()
        updateEditedState()
        window?.makeFirstResponder(workspace != nil ? explorer.outlineView : nil)
    }

    // MARK: Command palette (M2-09)

    var isCommandPaletteVisible: Bool { loadedPalette.map { !$0.isHidden } ?? false }

    var commandPalette: CommandPaletteView {
        if let panel = loadedPalette { return panel }
        let panel = CommandPaletteView()
        panel.palette = commands
        panel.isHidden = true
        panel.translatesAutoresizingMaskIntoConstraints = false
        panel.onRun = { [weak self] match in self?.runCommand(match.id) }
        panel.onClose = { [weak self] in self?.closeCommandPalette() }
        window?.contentView?.addSubview(panel)
        let preferred = panel.widthAnchor.constraint(equalToConstant: CommandPaletteView.width)
        preferred.priority = .defaultHigh
        NSLayoutConstraint.activate([
            panel.topAnchor.constraint(equalTo: tabBar.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: detailView.centerXAnchor),
            panel.widthAnchor.constraint(lessThanOrEqualTo: detailView.widthAnchor, constant: -24),
            preferred,
        ])
        loadedPalette = panel
        return panel
    }

    /// ⇧⌘P.
    @objc func showCommandPalette(_ sender: Any?) {
        if isCommandPaletteVisible { return commandPalette.move(by: 1) }
        closeOverlays()
        paletteReturnResponder = window?.firstResponder
        // Decided before the field takes focus, from what had it.
        let available = availableCommandIDs()
        let byAction = CommandPaletteView.shortcuts(in: NSApp.mainMenu ?? MainMenu.build())
        var shortcuts: [String: String] = [:]
        for (id, action) in CommandPaletteView.actions {
            if let text = byAction[action] { shortcuts[id] = text }
        }
        // Not menu key equivalents: the window's key monitor handles them.
        shortcuts["workbench.action.navigateBack"] = "⌃-"
        shortcuts["workbench.action.navigateForward"] = "⌃⇧-"
        shortcuts["workbench.action.openPreviousRecentlyUsedEditor"] = "⌃⇥"
        let panel = commandPalette
        panel.isHidden = false
        panel.reset(available: available, shortcuts: shortcuts)
        panel.focus()
    }

    func closeCommandPalette() {
        guard isCommandPaletteVisible else { return }
        commandPalette.isHidden = true
        restoreFocusAfterPalette()
    }

    private func restoreFocusAfterPalette() {
        let target = paletteReturnResponder ?? (activeTab != nil ? editorView : nil)
        window?.makeFirstResponder(target)
    }

    /// Ids whose action has a target that accepts it right now, asked the
    /// way AppKit asks before enabling a menu item. Go to File and Go to
    /// Line stay enabled in the menu (they explain what they need) but are
    /// not offered here when they could only explain.
    func availableCommandIDs() -> [String] {
        commands.commands().map(\.id).filter { id in
            guard let action = CommandPaletteView.actions[id], let target = target(for: action) else { return false }
            switch action {
            case #selector(showGoToFile(_:)) where workspace == nil: return false
            case #selector(showGoToLine(_:)) where editorView.document == nil: return false
            default: break
            }
            let item = NSMenuItem(title: "", action: action, keyEquivalent: "")
            return (target as? NSMenuItemValidation)?.validateMenuItem(item) ?? true
        }
    }

    /// First object that handles `action`, searched like AppKit does for a
    /// menu item: from the window's first responder up through the window
    /// and this controller, then the app and its delegate. Walked by hand
    /// so it also works while the window is not key (tests).
    private func target(for action: Selector) -> NSObject? {
        var responder: NSResponder? = window?.firstResponder ?? window
        while let r = responder {
            if r.responds(to: action) { return r }
            responder = r.nextResponder
        }
        for candidate in [window as NSObject?, self, NSApp, NSApp.delegate as? NSObject] {
            if let c = candidate, c.responds(to: action) { return c }
        }
        return nil
    }

    /// Runs command `id` from where focus was, as its menu item would.
    func runCommand(_ id: String) {
        commandPalette.isHidden = true
        restoreFocusAfterPalette()
        guard let action = CommandPaletteView.actions[id], let target = target(for: action) else {
            return NSSound.beep()
        }
        commands.noteUsed(id: id)
        _ = target.perform(action, with: self)
    }

    /// Closes Go to File, Go to Line and the palette, whichever is open.
    private func closeOverlays() {
        if isQuickOpenVisible { closeGoToFile() }
        if isGoToLineVisible { closeGoToLine() }
        if isCommandPaletteVisible {
            commandPalette.isHidden = true
        }
    }

    @objc func refreshExplorer(_ sender: Any?) {
        guard workspace != nil else { return NSSound.beep() }
        explorer.refresh()
    }

    @objc func collapseExplorerFolders(_ sender: Any?) {
        guard workspace != nil else { return NSSound.beep() }
        explorer.collapseAll()
    }

    /// Asks for a new item's name; `nil` cancels. Tests replace it.
    var askNewName: (_ title: String, _ folder: String) -> String? = { title, folder in
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = folder.isEmpty ? "In the workspace root." : "In \(folder)."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        field.placeholderString = "Name (use / for subfolders)"
        alert.accessoryView = field
        alert.addButton(withTitle: "Create")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        return alert.runModal() == .alertFirstButtonReturn ? field.stringValue : nil
    }

    @objc func explorerNewFile(_ sender: Any?) { createExplorerItem(folder: false) }
    @objc func explorerNewFolder(_ sender: Any?) { createExplorerItem(folder: true) }

    /// Creates a file or folder in the selected folder (or the selected
    /// file's folder, else the root), shows it, and opens a new file.
    private func createExplorerItem(folder: Bool) {
        guard let workspace else { return NSSound.beep() }
        let parent = explorer.targetFolder
        guard let name = askNewName(folder ? "New Folder" : "New File", parent),
              !name.trimmingCharacters(in: .whitespaces).isEmpty else { return }
        do {
            let rel = folder
                ? try workspace.createFolder(parentRelPath: parent, name: name)
                : try workspace.createFile(parentRelPath: parent, name: name)
            explorer.refresh()
            explorer.reveal(relPath: rel)
            rebuildFileFinder()
            if !folder {
                let entry = FileEntry(name: (rel as NSString).lastPathComponent, relPath: rel, isDir: false)
                openWorkspaceFile(workspace, entry, preview: false)
            }
        } catch {
            report(error, while: folder ? "create folder" : "create file")
        }
    }

    /// The selected item's absolute URL, else the workspace root.
    private var explorerSelectionURL: URL? {
        guard let workspace else { return nil }
        let root = URL(fileURLWithPath: workspace.root())
        guard let rel = explorer.selectedEntry?.relPath else { return root }
        return root.appendingPathComponent(rel)
    }

    @objc func revealInFinder(_ sender: Any?) {
        guard let url = explorerSelectionURL else { return NSSound.beep() }
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    @objc func copyExplorerPath(_ sender: Any?) {
        guard let url = explorerSelectionURL else { return NSSound.beep() }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(url.path, forType: .string)
    }

    /// The header's "…" menu.
    func explorerMoreMenu() -> NSMenu {
        let menu = NSMenu()
        func add(_ title: String, _ action: Selector) {
            menu.addItem(withTitle: title, action: action, keyEquivalent: "").target = self
        }
        add("New File…", #selector(explorerNewFile(_:)))
        add("New Folder…", #selector(explorerNewFolder(_:)))
        add("Open Folder…", #selector(openFolder(_:)))
        menu.addItem(.separator())
        add("Refresh Explorer", #selector(refreshExplorer(_:)))
        add("Collapse Folders in Explorer", #selector(collapseExplorerFolders(_:)))
        menu.addItem(.separator())
        add("Reveal in Finder", #selector(revealInFinder(_:)))
        add("Copy Path", #selector(copyExplorerPath(_:)))
        return menu
    }

    // MARK: Go to Line and back/forward (M2-08)

    var isGoToLineVisible: Bool { loadedGoToLine.map { !$0.isHidden } ?? false }

    /// Built on first ⌃G, like the other panels.
    var goToLine: GoToLine {
        if let panel = loadedGoToLine { return panel }
        let panel = GoToLine()
        panel.isHidden = true
        panel.translatesAutoresizingMaskIntoConstraints = false
        panel.onGo = { [weak self] target in self?.goTo(target) }
        panel.onClose = { [weak self] in self?.closeGoToLine() }
        window?.contentView?.addSubview(panel)
        let preferred = panel.widthAnchor.constraint(equalToConstant: GoToLine.width)
        preferred.priority = .defaultHigh
        NSLayoutConstraint.activate([
            panel.topAnchor.constraint(equalTo: tabBar.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: detailView.centerXAnchor),
            panel.widthAnchor.constraint(lessThanOrEqualTo: detailView.widthAnchor, constant: -24),
            preferred,
        ])
        loadedGoToLine = panel
        return panel
    }

    /// ⌃G.
    @objc func showGoToLine(_ sender: Any?) {
        guard let doc = editorView.document else {
            statusLabel.stringValue = "Open a file to use Go to Line."
            return NSSound.beep()
        }
        closeOverlays()
        editorView.flushPendingUpdates()
        let panel = goToLine
        panel.reset(lineCount: doc.lineCount(), current: doc.viewport(firstLine: 0, count: 0).primary.line)
        panel.isHidden = false
        panel.focus()
    }

    func closeGoToLine() {
        guard isGoToLineVisible else { return }
        goToLine.isHidden = true
        window?.makeFirstResponder(editorView)
    }

    private func goTo(_ target: LineTarget) {
        closeGoToLine()
        guard let tab = activeTab else { return }
        let from = location(of: tab)
        _ = navHistory.recordJump(from: from, to: NavLocation(key: tab.navKey, line: target.line, col: target.col ?? 0))
        move(tab, toLine: target.line, col: target.col)
    }

    /// ⌃-.
    @objc func navigateBack(_ sender: Any?) { navigate(forward: false) }

    /// ⌃⇧-.
    @objc func navigateForward(_ sender: Any?) { navigate(forward: true) }

    /// Steps through the history, skipping places that can no longer be
    /// shown (a closed Untitled buffer, a deleted file).
    private func navigate(forward: Bool) {
        closeOverlays()
        let current = activeTab.map(location(of:))
        while let loc = forward ? navHistory.forward(current: current) : navHistory.back(current: current) {
            if show(loc) { return }
            navHistory.forget(key: loc.key)
        }
        NSSound.beep()
    }

    /// Shows `loc`, reopening its file if its tab was closed.
    private func show(_ loc: NavLocation) -> Bool {
        isNavigating = true
        defer { isNavigating = false }
        if let i = tabs.firstIndex(where: { $0.navKey == loc.key }) {
            if i != activeIndex { selectTab(i) }
        } else {
            guard !loc.key.hasPrefix("untitled:"), FileManager.default.fileExists(atPath: loc.key) else { return false }
            if let workspace, let rel = relativePath(loc.key) {
                let name = (rel as NSString).lastPathComponent
                openWorkspaceFile(workspace, FileEntry(name: name, relPath: rel, isDir: false), preview: false)
            } else {
                try? openFile(at: URL(fileURLWithPath: loc.key))
            }
        }
        guard let tab = activeTab, tab.navKey == loc.key else { return false }
        move(tab, toLine: loc.line, col: loc.col)
        return true
    }

    private func move(_ tab: EditorTab, toLine line: UInt64, col: UInt64?) {
        tab.doc.goToLine(line: line, col: col)
        editorView.didChangeSelection()
        editorView.revealLine(Int(min(line, tab.doc.lineCount() - 1)))
        window?.makeFirstResponder(editorView)
    }

    /// Records where the caret was before this find session, once, if find
    /// has since moved it far away.
    private func recordFindOrigin() {
        guard let origin = findOrigin, let tab = activeTab else { return }
        findOrigin = nil
        guard origin.key == tab.navKey else { return }
        _ = navHistory.recordJump(from: origin, to: location(of: tab))
    }

    /// Runs `move` and records where the caret was if it went far.
    private func recordingJump(_ move: () -> Void) {
        guard let tab = activeTab else { return move() }
        let from = location(of: tab)
        move()
        _ = navHistory.recordJump(from: from, to: location(of: tab))
    }

    func location(of tab: EditorTab) -> NavLocation {
        tab.doc.caretLocation(key: tab.navKey)
    }

    /// Records the place being left, unless back/forward is moving.
    private func noteLeaving(_ tab: EditorTab) {
        guard !isNavigating else { return }
        navHistory.record(from: location(of: tab))
    }

    /// ⌃- and ⌃⇧- go to the menu items, whatever the Shift key does to
    /// the character: Shift turns `-` into `_` on US keyboards, which a
    /// menu key equivalent of `-` would not match.
    private func installKeyMonitor() {
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            // Only plain values cross into the main-actor closure.
            // kVK_ANSI_Minus, kVK_Tab
            guard event.keyCode == 27 || event.keyCode == 48 else { return event }
            let isTab = event.keyCode == 48
            let mods = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
                .subtracting([.capsLock, .numericPad, .function])
            let windowNumber = event.windowNumber
            let handled = MainActor.assumeIsolated { () -> Bool in
                guard let self, windowNumber == self.window?.windowNumber else { return false }
                switch (isTab, mods) {
                case (false, [.control]): self.navigateBack(nil)
                case (false, [.control, .shift]): self.navigateForward(nil)
                case (true, [.control]): self.selectLastUsedTab(nil)
                default: return false
                }
                return true
            }
            return handled ? nil : event
        }
    }

    // MARK: Go to File (M2-07)

    var isQuickOpenVisible: Bool { loadedQuickOpen.map { !$0.isHidden } ?? false }

    /// Built on first ⌘P, like the find bar, to keep it out of startup.
    var quickOpen: QuickOpen {
        if let panel = loadedQuickOpen { return panel }
        let panel = QuickOpen()
        panel.isHidden = true
        panel.translatesAutoresizingMaskIntoConstraints = false
        panel.onOpen = { [weak self] match in self?.openQuickOpenMatch(match) }
        panel.onClose = { [weak self] in self?.closeGoToFile() }
        window?.contentView?.addSubview(panel)
        let preferred = panel.widthAnchor.constraint(equalToConstant: QuickOpen.width)
        preferred.priority = .defaultHigh
        NSLayoutConstraint.activate([
            panel.topAnchor.constraint(equalTo: tabBar.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: detailView.centerXAnchor),
            panel.widthAnchor.constraint(lessThanOrEqualTo: detailView.widthAnchor, constant: -24),
            preferred,
        ])
        loadedQuickOpen = panel
        return panel
    }

    /// ⌘P. Pressed again while open, it moves down the list, as in VSCode.
    @objc func showGoToFile(_ sender: Any?) {
        guard workspace != nil else {
            statusLabel.stringValue = "Open a folder to use Go to File (⇧⌘O)."
            NSSound.beep()
            return
        }
        if isQuickOpenVisible { return quickOpen.move(by: 1) }
        closeOverlays()
        let panel = quickOpen
        fileFinder?.setContext(current: activeTab.flatMap { relativePath($0.doc.path()) }, recent: recentFiles)
        panel.finder = fileFinder
        panel.isHidden = false
        panel.reset()
        panel.focus()
        // Files may have been added since the last index; there is no file
        // watcher yet (M2-02), so refresh it each time the panel opens.
        rebuildFileFinder()
    }

    func closeGoToFile() {
        guard isQuickOpenVisible else { return }
        quickOpen.isHidden = true
        window?.makeFirstResponder(activeTab != nil ? editorView : explorer.outlineView)
    }

    private func openQuickOpenMatch(_ match: FileMatch) {
        closeGoToFile()
        guard let workspace else { return }
        let name = (match.relPath as NSString).lastPathComponent
        openWorkspaceFile(workspace, FileEntry(name: name, relPath: match.relPath, isDir: false), preview: false)
    }

    /// Walks the workspace off the main thread and installs the result if
    /// the workspace is still the same.
    private func rebuildFileFinder() {
        guard let workspace else { return }
        finderGeneration += 1
        let token = finderGeneration
        Task.detached(priority: .userInitiated) { [weak self] in
            let finder = workspace.buildFileFinder()
            await MainActor.run {
                guard let self, self.finderGeneration == token else { return }
                finder.setContext(current: self.activeTab.flatMap { self.relativePath($0.doc.path()) },
                                  recent: self.recentFiles)
                self.fileFinder = finder
                self.loadedQuickOpen?.placeholderMessage = "Indexing files…"
                self.loadedQuickOpen?.finder = finder
                self.onFileFinderReady?()
            }
        }
    }

    private func noteRecent(_ rel: String) {
        recentFiles.removeAll { $0 == rel }
        recentFiles.insert(rel, at: 0)
        if recentFiles.count > 50 { recentFiles.removeLast(recentFiles.count - 50) }
    }

    /// `path` relative to the workspace root, if it is inside it.
    private func relativePath(_ path: String) -> String? {
        guard let workspace, let file = Self.fileKey(path), let root = Self.fileKey(workspace.root()) else {
            return nil
        }
        let prefix = root.hasSuffix("/") ? root : root + "/"
        return file.hasPrefix(prefix) ? String(file.dropFirst(prefix.count)) : nil
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
    /// Caret when the find bar opened. Typing in the field moves the caret
    /// to the nearest match, so this is the place Back should return to.
    private var findOrigin: NavLocation?
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
        if !isFindVisible { findOrigin = activeTab.map(location(of:)) }
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
        recordFindOrigin()
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
        recordFindOrigin()
        recordingJump { _ = doc.findNext() }
        finishFindMove()
    }

    @objc func findPrevious(_ sender: Any?) {
        guard let doc = editorView.document else { return }
        if !isFindVisible || findBar.pattern.isEmpty { return presentFind(replace: false) }
        recordFindOrigin()
        recordingJump { _ = doc.findPrevious() }
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

    /// Window title, and the unsaved dots on the tab, the path bar and the
    /// window's close button. Runs at most once per frame while typing.
    /// The first edit keeps a preview tab open.
    func updateEditedState() {
        let title = activeTab?.windowTitle ?? workspace?.name() ?? "RustCode"
        if window?.title != title { window?.title = title }
        if let i = activeIndex {
            let tab = tabs[i]
            let edited = tab.doc.isModified()
            if edited != tab.isEdited || (edited && tab.isPreview) {
                tab.isEdited = edited
                if edited { tab.isPreview = false }
                refreshTabBar()
            }
            pathBar.setEdited(edited)
        } else {
            pathBar.setEdited(false)
        }
        window?.isDocumentEdited = tabs.contains { $0.isEdited }
        explorer.dirtyPaths = Set(tabs.compactMap { $0.isEdited ? $0.relPath : nil })
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
        if let tab = activeTab {
            tab.title = url.lastPathComponent
            tab.windowTitle = url.lastPathComponent
            tab.pathComponents = [url.lastPathComponent]
            tab.fileKey = Self.fileKey(doc.path())
            tab.relPath = relativePath(doc.path())
            pathBar.show(tab.pathComponents)
            refreshTabBar()
        }
        savedStatus(doc)
        return true
    }

    private func savedStatus(_ doc: DocumentHandle) {
        statusLabel.stringValue = "\(documentName): saved, \(doc.lineCount()) lines, \(doc.lenBytes()) bytes"
        updateEditedState()
    }

    /// `true` when it is safe to close the window or quit: every tab with
    /// unsaved changes was saved or discarded. Stops at the first Cancel,
    /// with that tab shown.
    func confirmDiscardChanges() -> Bool {
        editorView.flushPendingUpdates()
        for i in tabs.indices where tabs[i].doc.isModified() {
            if activeIndex != i { selectTab(i) }
            guard confirmDiscardActive() else { return false }
        }
        return true
    }

    /// Asks about the active tab only.
    private func confirmDiscardActive() -> Bool {
        guard let doc = editorView.document, doc.isModified() else { return true }
        switch askSaveChoice(documentName) {
        case .save: return save()
        case .discard: return true
        case .cancel: return false
        }
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        confirmDiscardChanges()
    }

    func windowWillClose(_ notification: Notification) {
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
        keyMonitor = nil
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
        fileFinder = nil
        recentFiles = []
        // Paths are relative to the new root (or outside it).
        for tab in tabs { tab.relPath = relativePath(tab.doc.path()) }
        if isQuickOpenVisible { closeGoToFile() }
        loadedQuickOpen?.finder = nil
        // Ready by the time ⌘P is pressed in all but huge folders.
        rebuildFileFinder()
        explorer.setWorkspace(ws)
        explorer.scrollView.isHidden = false
        emptyState.isHidden = true
        sectionHeader.title = ws.name()
        sectionHeader.showsTools = true
        setSidebarVisible(true)
        if tabs.isEmpty { pathBar.show([]) }
        updateEditedState()
        statusLabel.stringValue = "\(ws.name()): choose a file in the sidebar."
    }

    /// Opens a file in its own tab (or shows its tab if already open).
    func openFile(at url: URL) throws {
        if let i = tabIndex(forPath: url.path) {
            tabs[i].isPreview = false
            return selectTab(i)
        }
        let start = ContinuousClock.now
        let doc = try bridge.openDocument(path: url.path)
        let name = url.lastPathComponent
        let tab = EditorTab(doc: doc, title: name, pathComponents: [name], windowTitle: name,
                            isPreview: false, status: describe(doc, name: name, since: start))
        tab.fileKey = Self.fileKey(doc.path())
        tab.relPath = relativePath(doc.path())
        add(tab, replacing: slotForOpening(preview: false))
    }

    /// Single click in the explorer opens a preview tab, which the next
    /// single click replaces; a double click or an edit keeps it.
    func openWorkspaceFile(_ workspace: Workspace, _ entry: FileEntry, preview: Bool) {
        let path = URL(fileURLWithPath: workspace.root()).appendingPathComponent(entry.relPath).path
        if let i = tabIndex(forPath: path) {
            if !preview { tabs[i].isPreview = false }
            return selectTab(i)
        }
        do {
            let start = ContinuousClock.now
            let doc = try workspace.openFile(relPath: entry.relPath)
            let tab = EditorTab(
                doc: doc, title: entry.name,
                pathComponents: entry.relPath.split(separator: "/").map(String.init),
                windowTitle: "\(entry.name) — \(workspace.name())", isPreview: preview,
                status: describe(doc, name: entry.relPath, since: start))
            tab.fileKey = Self.fileKey(doc.path())
            tab.relPath = entry.relPath
            add(tab, replacing: slotForOpening(preview: preview))
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

/// Solid sidebar background (no vibrancy): matches the flat chrome.
@MainActor
final class SidebarView: NSView {
    override func draw(_ dirtyRect: NSRect) {
        Theme.sidebar.setFill()
        dirtyRect.intersection(bounds).fill()
    }
}

/// Sidebar header, as in VSCode with one folder open: the folder name in
/// caps (EXPLORER before a folder is open) and, on the right, New File,
/// New Folder, Refresh, Collapse Folders and a "…" menu. Same height as the
/// tab strip, with a rule underneath.
@MainActor
final class SidebarSectionHeader: NSView {
    static var height: CGFloat { Theme.tabHeight }

    let label = NSTextField(labelWithString: "EXPLORER")
    let newFileButton = SidebarSectionHeader.tool("doc.badge.plus", "New File…")
    let newFolderButton = SidebarSectionHeader.tool("folder.badge.plus", "New Folder…")
    let refreshButton = SidebarSectionHeader.tool("arrow.clockwise", "Refresh Explorer")
    let collapseButton = SidebarSectionHeader.tool("rectangle.stack.badge.minus", "Collapse Folders in Explorer")
    let moreButton = SidebarSectionHeader.tool("ellipsis", "More Actions…")
    var tools: [NSButton] { [newFileButton, newFolderButton, refreshButton, collapseButton, moreButton] }

    var onNewFile: (() -> Void)?
    var onNewFolder: (() -> Void)?
    var onRefresh: (() -> Void)?
    var onCollapseAll: (() -> Void)?
    var moreMenu: (() -> NSMenu)?

    /// Folder name; empty shows EXPLORER.
    var title = "" {
        didSet {
            label.stringValue = title.isEmpty ? "EXPLORER" : title.uppercased()
            setAccessibilityLabel(title.isEmpty ? "Explorer" : title)
        }
    }

    /// The buttons only make sense with a folder open.
    var showsTools = true {
        didSet { tools.forEach { $0.isHidden = !showsTools } }
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Explorer")
        setAccessibilityIdentifier("folder-title")

        label.font = .systemFont(ofSize: 12, weight: .medium)
        label.textColor = .labelColor
        label.lineBreakMode = .byTruncatingTail
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        label.setContentHuggingPriority(.init(1), for: .horizontal)
        for (button, action) in [(newFileButton, #selector(newFile(_:))), (newFolderButton, #selector(newFolder(_:))),
                                 (refreshButton, #selector(refresh(_:))), (collapseButton, #selector(collapseAll(_:))),
                                 (moreButton, #selector(more(_:)))] {
            button.target = self
            button.action = action
        }

        let row = NSStackView(views: [label] + tools)
        row.spacing = 6
        row.setCustomSpacing(10, after: label)
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: Self.height),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 20),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.separatorColor.setFill()
        NSRect(x: 0, y: 0, width: bounds.width, height: 1).intersection(dirtyRect).fill()
    }

    private static func tool(_ symbol: String, _ title: String) -> NSButton {
        let b = NSButton()
        b.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)?
            .withSymbolConfiguration(.init(pointSize: 14, weight: .regular))
        b.isBordered = false
        b.imagePosition = .imageOnly
        b.contentTintColor = .secondaryLabelColor
        b.toolTip = title
        b.setAccessibilityLabel(title)
        b.widthAnchor.constraint(equalToConstant: 24).isActive = true
        b.heightAnchor.constraint(equalToConstant: 24).isActive = true
        return b
    }

    @objc private func newFile(_ sender: Any?) { onNewFile?() }
    @objc private func newFolder(_ sender: Any?) { onNewFolder?() }
    @objc private func refresh(_ sender: Any?) { onRefresh?() }
    @objc private func collapseAll(_ sender: Any?) { onCollapseAll?() }

    @objc private func more(_ sender: Any?) {
        guard let menu = moreMenu?() else { return }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: moreButton.bounds.maxY + 4), in: moreButton)
    }
}

extension MainWindowController: NSSplitViewDelegate {
    func splitView(_ splitView: NSSplitView, constrainMinCoordinate proposedMinimumPosition: CGFloat,
                   ofSubviewAt dividerIndex: Int) -> CGFloat {
        Self.minSidebarWidth
    }

    func splitView(_ splitView: NSSplitView, constrainMaxCoordinate proposedMaximumPosition: CGFloat,
                   ofSubviewAt dividerIndex: Int) -> CGFloat {
        max(Self.minSidebarWidth, splitView.bounds.width - Self.minEditorWidth)
    }

    /// Window resizes go to the editor; the sidebar keeps its width.
    func splitView(_ splitView: NSSplitView, shouldAdjustSizeOfSubview view: NSView) -> Bool {
        view !== sidebar
    }

    func splitView(_ splitView: NSSplitView, canCollapseSubview subview: NSView) -> Bool { false }
}

extension MainWindowController: NSMenuItemValidation {
    /// Disables menu items (and palette commands) that cannot do anything
    /// right now.
    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        let hasDocument = editorView.document != nil
        switch item.action {
        case #selector(saveDocument(_:)), #selector(saveDocumentAs(_:)),
             #selector(showFind(_:)), #selector(showFindAndReplace(_:)),
             #selector(findNext(_:)), #selector(findPrevious(_:)), #selector(useSelectionForFind(_:)):
            return hasDocument
        case #selector(refreshExplorer(_:)), #selector(collapseExplorerFolders(_:)),
             #selector(explorerNewFile(_:)), #selector(explorerNewFolder(_:)),
             #selector(revealInFinder(_:)), #selector(copyExplorerPath(_:)):
            return workspace != nil
        case #selector(closeTab(_:)):
            return true
        case #selector(showNextTab(_:)), #selector(showPreviousTab(_:)), #selector(selectLastUsedTab(_:)):
            return tabs.count > 1
        case #selector(navigateBack(_:)): return navHistory.canGoBack()
        case #selector(navigateForward(_:)): return navHistory.canGoForward()
        default: return true
        }
    }
}
