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
    let bridge: CoreBridge
    let statusBar = StatusBar()
    let pathBar = PathBar()
    private let breadcrumbBar = BreadcrumbBar()
    private let stickyScopeView = StickyScopeView()
    private var documentSymbols: [OutlineSymbol] = []
    private var lastSemanticRequest = Date.distantPast
    private var lastDiagnosticsRevision: UInt64?
    let titleBar = TitleBar()
    let activityBar = ActivityBar()
    private let sourceControl = SourceControlController()
    private let searchSidebar = SearchSidebarController()
    let extensions = ExtensionsController()
    private var extensionsGeneration = 0
    private(set) var activeSidebarTab: ActivityBar.Item = .explorer
    let tabBar = TabBar()
    /// Open documents, in tab order (M2-06).
    private(set) var tabs: [EditorTab] = []
    private(set) var activeIndex: Int?
    var activeTab: EditorTab? { activeIndex.map { tabs[$0] } }
    /// Left-hand message in the status bar.
    var statusLabel: NSTextField { statusBar.message }
    private let scrollView = DiagnosticScrollView()
    private let secondaryScrollView = DiagnosticScrollView()
    private let markdownPreview = MarkdownPreviewView()
    /// User asked for the Markdown preview; it shows whenever the active tab is Markdown.
    private(set) var isMarkdownPreviewOn = false
    var isMarkdownPreviewShown: Bool { markdownPreview.superview != nil }
    private let editorSplitView = NSSplitView()
    private let problemsPanel = ProblemsPanel()
    private let referencesPanel = ReferencesPanel()
    private let terminalView = TerminalView()
    /// Shows the active tab's terminal when that tab is a terminal (VS Code
    /// "Move Terminal into Editor Area"). Covers the path bar and editor.
    let editorTerminalView = TerminalView()
    private let outputPanel = OutputPanel()
    private let portsPanel = PortsPanel()
    private let gitGraphPanel = GitGraphPanel()
    private let bottomPanelHeader = BottomPanelHeader()
    private let bottomPanelContent = BottomPanelContentView()
    private var bottomPanelHeight: NSLayoutConstraint!
    private var bottomPanelHeaderHeight: NSLayoutConstraint!
    private var bottomPanelTab = BottomPanelHeader.Tab.terminal
    var isBottomPanelVisible: Bool { bottomPanelHeight.constant > 0 }
    /// Panel content height when open, and whether it fills the editor area.
    private var bottomPanelOpenHeight: CGFloat = 240
    private(set) var isBottomPanelMaximized = false
    private let bottomPanelPlaceholder = NSTextField(labelWithString: "Debug console is not connected")
    private var terminalHandles: [TerminalHandle] = []
    private var activeTerminalIndex = 0
    let hoverPopover = HoverPopover()
    let blamePopover = BlamePopover()
    /// `editor.gitBlame`, or the View > Toggle Git Blame override of it.
    private(set) var gitBlameEnabled = true
    /// Commit behind the annotated line; what the blame card shows.
    private var blameCommit: BlameCommitInfo?
    private var blameRefreshItem: DispatchWorkItem?
    private var blameHideItem: DispatchWorkItem?
    /// Quiet time after the last edit before blame is recomputed.
    static let blameRefreshDelay: TimeInterval = 0.5
    private let completionPopup = CompletionPopup()
    private let signaturePopup = SignaturePopup()
    private let codeActionPopup = CodeActionPopup()
    private var completionQuery = ""
    private var localCompletionItems: [CompletionInfo]?
    private var snippetCompletionItems: [CompletionInfo] = []
    private var snippetSession: SnippetSession?
    private var snippetDocumentKey: String?
    private var completionDismissed = false
    private var completionPosition = TextPos(line: 0, colUtf16: 0)
    private var hoverPosition: TextPos?
    private var hoverPoint = NSPoint.zero
    private var hoverDocument: DocumentHandle?
    private var hoverRequestPosition: TextPos?
    private var hoverRequestTime = Date.distantPast
    /// Word the visible card describes (server range, single line).
    private var hoverShownRange: (line: UInt64, start: UInt64, end: UInt64)?
    private var hoverShownContents: String?
    private var hoverShownPosition: TextPos?
    private let splitView = CardSplitView()
    private let detailView = CardView()
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
    var onWindowClosed: (() -> Void)?
    /// Go to File index of `workspace`, built in the background.
    private(set) var fileFinder: FileFinder?
    private var finderGeneration = 0
    /// Watches the open folder (M2-02).
    private var workspaceWatch: WorkspaceWatch?
    /// Bumped per workspace, so a late batch from the previous folder is
    /// dropped.
    private var watchGeneration = 0
    /// Called after a watcher batch has been applied (tests wait on it).
    var onWorkspaceChangesApplied: ((WatchBatch) -> Void)?
    /// Called when a (re)built index is installed (tests wait on it).
    var onFileFinderReady: (() -> Void)?
    /// Files opened in this workspace, most recent first, relative paths.
    private(set) var recentFiles: [String] = []
    private var loadedQuickOpen: QuickOpen?
    private var loadedWorkspaceSearch: WorkspaceSearch?
    private var searchGeneration = 0
    private var loadedGoToLine: GoToLine?
    private var loadedOutlinePicker: OutlinePicker?
    private var loadedPalette: CommandPaletteView?
    private var loadedGitPicker: GitBranchPicker?
    private var syntaxEvictionTimer: Timer?
    private var lspTimer: Timer?
    private var memoryPressureSource: DispatchSourceMemoryPressure?
    var globalSettingsURL = GalaxySettings.globalURL
    private var settingsModified: Date?
    /// `editor.autoClosingTags` and `editor.linkedEditing`, applied to every tab.
    private var autoClosingTags = true
    private var linkedEditing = true
    private var workspaceSettingsModified: Date?
    private(set) var settingsProblems: [String] = []
    private var autoSaveMode: AutoSaveMode = .off
    private var autoSaveDelay: TimeInterval = 1.0
    private var autoSaveWorkItem: DispatchWorkItem?
    static let syntaxEvictionInterval: TimeInterval = 5 * 60
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
    private var definitionRequest: (document: DocumentHandle, position: TextPos)?
    private var referencesRequest: DocumentHandle?
    private var renameRequest: DocumentHandle?
    private var codeActionRequest: DocumentHandle?
    private var formattingRequest: DocumentHandle?
    private var outlineRequest: DocumentHandle?
    private var workspaceSymbolRoot: String?
    private var keyMonitor: Any?
    /// Tabs in the order they were last shown, most recent first (⌃Tab).
    private var tabOrderByUse: [ObjectIdentifier] = []
    let editorView = EditorView(frame: .zero)
    let secondaryEditorView = EditorView(frame: .zero)
    private var isEditorSplit = false
    /// Orientation of the user's own split, restored after a diff tab.
    private var userSplitVertical = true
    /// The left pane shows the committed file of a diff tab.
    private(set) var isDiffPaneShown = false
    private(set) var diffMap = DiffLineMap()
    /// Scroll to the first change once the diff has been computed.
    private var diffRevealPending = false
    private var isSyncingDiffScroll = false
    /// Set while the preview drives the editor, so that does not echo back.
    private var isSyncingPreviewScroll = false

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

    enum DiskConflictChoice { case reload, keepMine }

    /// Asks what to do when a file with unsaved edits changed on disk
    /// (M2-03). `saving`: asked by Save, where keeping overwrites the file.
    /// Tests replace it.
    var askDiskConflict: (_ documentName: String, _ saving: Bool) -> DiskConflictChoice = { name, saving in
        let alert = NSAlert()
        alert.messageText = "\(name) changed on disk."
        alert.informativeText = saving
            ? "Another program changed the file since it was opened. Saving replaces its changes with yours."
            : "Another program changed the file, and you have unsaved changes. Reload discards your changes; you can undo it."
        alert.addButton(withTitle: saving ? "Save Anyway" : "Keep My Changes")
        alert.addButton(withTitle: "Reload from Disk")
        return alert.runModal() == .alertSecondButtonReturn ? .reload : .keepMine
    }

    /// Conflict prompts wait until the window is key, so a background
    /// change does not pop an alert over another app. Tests turn it off.
    var promptsOnlyWhenKey = true
    private var isResolvingDiskConflict = false

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

    @objc func splitEditorRight(_ sender: Any?) { setEditorSplit(vertical: true) }
    @objc func splitEditorDown(_ sender: Any?) { setEditorSplit(vertical: false) }
    @objc func closeEditorSplit(_ sender: Any?) {
        isEditorSplit = false
        guard !isDiffPaneShown else { return }
        secondaryScrollView.removeFromSuperview()
        editorSplitView.isVertical = true
        editorSplitView.adjustSubviews()
        secondaryEditorView.document = nil
    }

    private func setEditorSplit(vertical: Bool) {
        // The left pane belongs to the diff while a diff tab is shown.
        guard !isDiffPaneShown else { return NSSound.beep() }
        if isMarkdownPreviewShown {
            isMarkdownPreviewOn = false
            updateMarkdownPreview()
        }
        isEditorSplit = true
        userSplitVertical = vertical
        editorSplitView.isVertical = vertical
        if secondaryScrollView.superview == nil {
            editorSplitView.addSubview(secondaryScrollView)
        }
        secondaryEditorView.document = activeTab?.doc
        editorSplitView.layoutSubtreeIfNeeded()
        editorSplitView.adjustSubviews()
        secondaryEditorView.requestDocumentRefresh(scrollCaret: false)
    }

    /// Shows or hides the rendered Markdown beside the editor (VS Code
    /// "Open Preview to the Side"). Only Markdown files can be previewed.
    @objc func toggleMarkdownPreview(_ sender: Any?) {
        guard isMarkdownPreviewOn || canPreviewMarkdown else { return NSSound.beep() }
        isMarkdownPreviewOn.toggle()
        updateMarkdownPreview()
    }

    private var canPreviewMarkdown: Bool {
        guard let tab = activeTab, tab.isDocument else { return false }
        return MarkdownPreviewView.isMarkdown(path: tab.doc.path())
    }

    /// Brings the preview pane in line with the active tab: shown for a
    /// Markdown file while the preview is on, hidden otherwise. Takes the
    /// secondary pane's place, and gives it back when it closes.
    private func updateMarkdownPreview() {
        defer { tabBar.setPreviewButton(visible: canPreviewMarkdown, active: isMarkdownPreviewShown) }
        let wanted = isMarkdownPreviewOn && !isDiffPaneShown && canPreviewMarkdown
        if wanted, let tab = activeTab {
            if !isMarkdownPreviewShown {
                secondaryScrollView.removeFromSuperview()
                editorSplitView.isVertical = true
                editorSplitView.addSubview(markdownPreview)
                editorSplitView.layoutSubtreeIfNeeded()
                if markdownPreview.frame.width < 1 {
                    editorSplitView.setPosition(editorSplitView.bounds.width / 2, ofDividerAt: 0)
                }
                editorSplitView.adjustSubviews()
            }
            markdownPreview.show(markdown: tab.doc.text())
            syncPreviewToEditor()
        } else if isMarkdownPreviewShown {
            markdownPreview.removeFromSuperview()
            if isDiffPaneShown {
                editorSplitView.isVertical = true
            } else {
                editorSplitView.isVertical = isEditorSplit ? userSplitVertical : true
                if isEditorSplit { editorSplitView.addSubview(secondaryScrollView) }
            }
            editorSplitView.adjustSubviews()
        }
    }

    /// Opens the Git Graph as an editor tab, or closes it when it is the
    /// tab showing. One graph tab at a time.
    @objc func toggleGitGraph(_ sender: Any?) {
        if let i = tabs.firstIndex(where: \.isGitGraph) {
            if activeIndex == i { closeTab(at: i) } else { selectTab(i) }
            return
        }
        let tab = EditorTab(doc: bridge.newDocument(), title: "Git Graph", pathComponents: ["Git Graph"],
                            windowTitle: "Git Graph", isPreview: false, status: "", isGitGraph: true)
        add(tab, replacing: nil)
    }

    var isGitGraphTabShown: Bool { activeTab?.isGitGraph == true }

    /// Shows or hides the Git Graph for the active tab, and keeps it
    /// refreshing only while it is showing.
    private func showEditorGitGraph(for tab: EditorTab?) {
        if tab?.isGitGraph == true {
            gitGraphPanel.workspace = workspace
            gitGraphPanel.isHidden = false
            gitGraphPanel.start()
        } else if !gitGraphPanel.isHidden {
            if (window?.firstResponder as? NSView)?.isDescendant(of: gitGraphPanel) == true {
                window?.makeFirstResponder(editorView)
            }
            gitGraphPanel.stop()
            gitGraphPanel.isHidden = true
        }
    }

    @objc func toggleTerminal(_ sender: Any?) {
        setBottomPanelOpen(!isBottomPanelVisible)
        if isBottomPanelVisible { showBottomPanel(tab: bottomPanelTab) }
    }

    /// Opens or closes the panel by resizing it. Panels stay in the view
    /// hierarchy: adding and removing views while AppKit updates tracking
    /// areas crashed (`_NSViewSubviewMutationSafeApply`).
    private func setBottomPanelOpen(_ open: Bool) {
        if !open {
            isBottomPanelMaximized = false
            bottomPanelHeader.setMaximized(false)
            // Typing goes back to the editor, as in VS Code.
            if window?.firstResponder === terminalView { window?.makeFirstResponder(editorView) }
            outputPanel.stop()
            portsPanel.stop()
        }
        bottomPanelHeight.constant = open ? currentBottomPanelHeight() : 0
        bottomPanelHeaderHeight.constant = open ? BottomPanelHeader.height : 0
        bottomPanelHeader.isHidden = !open
        bottomPanelContent.isHidden = !open
        titleBar.setPanelVisible(open)
        detailView.needsLayout = true
    }

    private func currentBottomPanelHeight() -> CGFloat {
        guard isBottomPanelMaximized else { return bottomPanelOpenHeight }
        // Leave the tab strip and a sliver of editor, like VS Code.
        let available = detailView.bounds.height - Theme.tabHeight - BottomPanelHeader.height - 40
        return max(bottomPanelOpenHeight, available)
    }

    private var resizeStartHeight: CGFloat = 0
    var bottomPanelHeaderForTesting: BottomPanelHeader { bottomPanelHeader }
    var bottomPanelContentHeight: CGFloat { bottomPanelHeight.constant }

    /// Drag on the panel's top edge. The size is kept for next time.
    private func resizeBottomPanel(began: Bool, deltaY: CGFloat) {
        if began { resizeStartHeight = bottomPanelHeight.constant; return }
        setBottomPanelHeight(resizeStartHeight + deltaY)
    }

    /// Sets the open panel's content height, leaving the editor some room.
    func setBottomPanelHeight(_ height: CGFloat) {
        let maximum = max(Self.minBottomPanelHeight,
                          detailView.bounds.height - Theme.tabHeight - BottomPanelHeader.height - 40)
        let clamped = min(max(height, Self.minBottomPanelHeight), maximum)
        isBottomPanelMaximized = false
        bottomPanelHeader.setMaximized(false)
        bottomPanelOpenHeight = clamped
        UserDefaults.standard.set(Double(clamped), forKey: "BottomPanelHeight")
        if isBottomPanelVisible { bottomPanelHeight.constant = clamped }
        detailView.needsLayout = true
    }

    /// Panel children are laid out at least this tall; a shorter panel
    /// would cut off the terminal's bottom rows.
    static let minBottomPanelHeight: CGFloat = BottomPanelContentView.minimumChildHeight

    private func toggleBottomPanelMaximized() {
        isBottomPanelMaximized.toggle()
        bottomPanelHeader.setMaximized(isBottomPanelMaximized)
        bottomPanelHeight.constant = currentBottomPanelHeight()
        detailView.needsLayout = true
    }

    private func showBottomPanel(tab: BottomPanelHeader.Tab) {
        bottomPanelTab = tab
        bottomPanelHeader.select(tab)
        terminalView.isHidden = tab != .terminal
        problemsPanel.isHidden = tab != .problems
        outputPanel.isHidden = tab != .output
        portsPanel.isHidden = tab != .ports
        bottomPanelPlaceholder.isHidden = tab != .debugConsole
        if tab == .terminal {
            ensureTerminal()
            window?.makeFirstResponder(terminalView)
        }
        if tab == .output { outputPanel.start() } else { outputPanel.stop() }
        if tab == .ports { portsPanel.start() } else { portsPanel.stop() }
    }

    private func ensureTerminal() {
        guard let workspace else {
            bottomPanelHeader.setTerminals(names: [], selected: 0)
            return
        }
        if terminalHandles.isEmpty,
           let handle = bridge.terminal(workspace: workspace, cols: 100, rows: 24) {
            terminalHandles = [handle]
            activeTerminalIndex = 0
        }
        refreshTerminalPicker()
        guard terminalHandles.indices.contains(activeTerminalIndex) else { return }
        terminalView.start(handle: terminalHandles[activeTerminalIndex])
    }

    private func refreshTerminalPicker() {
        bottomPanelHeader.setTerminals(names: terminalHandles.map { $0.shellName() }, selected: activeTerminalIndex)
    }

    private func newTerminal() {
        guard let workspace,
              let handle = bridge.terminal(workspace: workspace, cols: 100, rows: 24) else { return NSSound.beep() }
        terminalHandles.append(handle)
        activeTerminalIndex = terminalHandles.count - 1
        if !isBottomPanelVisible { setBottomPanelOpen(true) }
        showBottomPanel(tab: .terminal)
    }

    /// Kills the active terminal; the panel closes with the last one.
    private func killActiveTerminal() {
        guard terminalHandles.indices.contains(activeTerminalIndex) else { return }
        terminalHandles.remove(at: activeTerminalIndex).kill()
        removedTerminal()
    }

    /// The shell in the active terminal exited (`exit`, ⌃D).
    private func terminalExited() {
        guard terminalHandles.indices.contains(activeTerminalIndex),
              !terminalHandles[activeTerminalIndex].isRunning() else { return }
        terminalHandles.remove(at: activeTerminalIndex)
        removedTerminal()
    }

    private func removedTerminal() {
        terminalView.stop()
        activeTerminalIndex = max(0, min(activeTerminalIndex, terminalHandles.count - 1))
        if terminalHandles.isEmpty {
            refreshTerminalPicker()
            if bottomPanelTab == .terminal { setBottomPanelOpen(false) }
        } else {
            ensureTerminal()
        }
    }

    // MARK: Terminals in the editor area

    /// Terminal tabs, in tab order.
    var terminalTabs: [EditorTab] { tabs.filter { $0.terminal != nil } }

    /// Moves the panel's active terminal into a new editor tab, keeping the
    /// shell running. With no panel terminal, starts a new one there.
    @objc func moveTerminalToEditor(_ sender: Any?) {
        guard let workspace else { return NSSound.beep() }
        if terminalHandles.indices.contains(activeTerminalIndex) {
            let handle = terminalHandles.remove(at: activeTerminalIndex)
            removedTerminal()
            openTerminalTab(handle)
        } else if let handle = bridge.terminal(workspace: workspace, cols: 100, rows: 24) {
            openTerminalTab(handle)
        } else {
            NSSound.beep()
        }
    }

    /// Opens a new terminal as an editor tab.
    @objc func createTerminalEditor(_ sender: Any?) {
        guard let workspace, let handle = bridge.terminal(workspace: workspace, cols: 100, rows: 24) else {
            return NSSound.beep()
        }
        openTerminalTab(handle)
    }

    /// Moves a terminal tab (the clicked one from the tab menu, else the
    /// active one) back into the bottom panel.
    @objc func moveTerminalToPanel(_ sender: Any?) {
        let target = ((sender as? NSMenuItem)?.representedObject as? EditorTab) ?? activeTab
        guard let tab = target, let handle = tab.terminal,
              let index = tabs.firstIndex(where: { $0 === tab }) else { return NSSound.beep() }
        closeTab(at: index, keepTerminal: true)
        terminalHandles.append(handle)
        activeTerminalIndex = terminalHandles.count - 1
        if !isBottomPanelVisible { setBottomPanelOpen(true) }
        showBottomPanel(tab: .terminal)
    }

    private func openTerminalTab(_ handle: TerminalHandle) {
        let name = handle.shellName()
        let tab = EditorTab(doc: bridge.newDocument(), title: name, pathComponents: [name], windowTitle: name,
                            isPreview: false, status: "Terminal: \(name)", terminal: handle)
        add(tab, replacing: nil)
    }

    /// Shows or hides the editor-area terminal for the active tab.
    private func showEditorTerminal(for tab: EditorTab?) {
        if let handle = tab?.terminal {
            editorTerminalView.isHidden = false
            editorTerminalView.start(handle: handle)
        } else if !editorTerminalView.isHidden {
            if window?.firstResponder === editorTerminalView { window?.makeFirstResponder(editorView) }
            editorTerminalView.stop()
            editorTerminalView.isHidden = true
        }
    }

    /// The shell in the active terminal tab exited: close the tab.
    private func editorTerminalExited() {
        guard let index = activeIndex, let handle = tabs[index].terminal, !handle.isRunning() else { return }
        closeTab(at: index, keepTerminal: true)
    }

    private func menu(forTab index: Int) -> NSMenu? {
        guard tabs.indices.contains(index), tabs[index].terminal != nil else { return nil }
        let menu = NSMenu()
        let move = menu.addItem(withTitle: "Move Terminal into Panel", action: #selector(moveTerminalToPanel(_:)), keyEquivalent: "")
        move.target = self
        move.representedObject = tabs[index]
        return menu
    }

    private func panelTerminalMenu() -> NSMenu {
        let menu = NSMenu()
        menu.addItem(withTitle: "Move Terminal into Editor Area", action: #selector(moveTerminalToEditor(_:)), keyEquivalent: "")
            .target = self
        menu.addItem(withTitle: "New Terminal in Editor Area", action: #selector(createTerminalEditor(_:)), keyEquivalent: "")
            .target = self
        return menu
    }

    init(bridge: CoreBridge) {
        self.bridge = bridge
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 960, height: 640),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Galaxy"
        // The title stays set (Window menu, Mission Control, VoiceOver) but
        // `TitleBar` draws it.
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.setFrameAutosaveName("MainWindow")
        window.tabbingMode = .disallowed
        super.init(window: window)
        window.delegate = self
        buildContent()
        installMemoryPressureHandler()
        editorView.onChange = { [weak self] in
            if let document = self?.activeTab?.doc,
               var session = self?.snippetSession {
                session.noteDocumentChange(length: document.lenUtf16())
                self?.snippetSession = session
            }
            self?.updateEditedState()
            self?.scheduleAutoSave()
            self?.refreshFindStatus()
            if let document = self?.activeTab?.doc { self?.bridge.syncLSP(document: document) }
            self?.bridge.pollLSP()
            self?.requestCompletionIfNeeded()
            self?.refreshGitDiff()
            if self?.isMarkdownPreviewShown == true { self?.updateMarkdownPreview() }
            self?.updateBlameAnnotation()
            self?.scheduleBlameRefresh()
        }
        editorView.onSnippetInvalidated = { [weak self] in
            self?.snippetSession = nil
            self?.snippetDocumentKey = nil
        }
        editorView.onClickMove = { [weak self] before, after in
            guard let self, let tab = self.activeTab else { return }
            _ = self.navHistory.recordJump(
                from: NavLocation(key: tab.navKey, line: before.line, col: before.col),
                to: NavLocation(key: tab.navKey, line: after.line, col: after.col))
            self.refreshNavigationButtons()
        }
        editorView.onHover = { [weak self] position, point in
            self?.beginHover(at: position, point: point)
        }
        editorView.onDefinitionClick = { [weak self] position in
            self?.requestDefinition(at: position) ?? false
        }
        editorView.onCursorChange = { [weak self] primary, count in
            if let self,
               let document = self.activeTab?.doc,
               let session = self.snippetSession,
               let span = session.span {
                let caret = document.utf16Offset(pos: TextPos(line: primary.line, colUtf16: primary.colUtf16))
                if caret < span.start || caret > span.end {
                    self.snippetSession = nil
                    self.snippetDocumentKey = nil
                }
            }
            self?.statusBar.showCursor(primary: primary, count: count)
            self?.breadcrumbBar.show(symbols: self?.documentSymbols ?? [], atLine: primary.line)
            self?.updateBlameAnnotation()
        }
        breadcrumbBar.onSelect = { [weak self] symbol in
            guard let self, let tab = self.activeTab else { return }
            self.move(tab, toLine: symbol.line, col: symbol.column)
        }
        window.contentView?.layoutSubtreeIfNeeded()
        splitView.setPosition(Self.sidebarWidth, ofDividerAt: 0)
        if window.frameAutosaveName.isEmpty || !window.setFrameUsingName("MainWindow") {
            window.center()
        }

        installKeyMonitor()
        reloadKeybindings(force: true)

        bridge.onEvent = { [weak self] event in
            if case let .pong(message) = event {
                // Don't overwrite the status of an open document.
                if self?.editorView.document == nil {
                    self?.statusLabel.stringValue = "Core \(bridge.version) connected (\(message))"
                }
            }
        }
        statusBar.onGitClick = { [weak self] in self?.showGitBranchPicker() }
        statusBar.onGitGraphClick = { [weak self] in self?.toggleGitGraph(nil) }
    }

    private func installMemoryPressureHandler() {
        let source = DispatchSource.makeMemoryPressureSource(eventMask: [.warning, .critical], queue: .main)
        source.setEventHandler { [weak self] in
            self?.handleMemoryPressure(source.data)
        }
        source.resume()
        memoryPressureSource = source
    }

    private func handleMemoryPressure(_ event: DispatchSource.MemoryPressureEvent) {
        editorView.releaseNonEssentialCaches()
        secondaryEditorView.releaseNonEssentialCaches()
        loadedQuickOpen?.finder = nil
        fileFinder = nil
        finderGeneration += 1
        if event.contains(.critical) {
            evictInactiveSyntax(now: .now, includeRecentlyInactive: true)
        } else {
            evictInactiveSyntax(now: .now)
        }
        statusLabel.stringValue = event.contains(.critical)
            ? "Memory pressure: released inactive editor caches."
            : "Memory pressure: released editor caches."
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    private func buildContent() {
        guard let content = window?.contentView else { return }

        statusLabel.stringValue = "Core \(bridge.version) starting…"
        statusBar.translatesAutoresizingMaskIntoConstraints = false
        titleBar.translatesAutoresizingMaskIntoConstraints = false
        activityBar.translatesAutoresizingMaskIntoConstraints = false
        activityBar.setActive(.explorer)
        activityBar.onSelect = { [weak self] item in
            guard let self else { return }
            switch item {
            case .explorer:
                if self.isSidebarVisible && self.activeSidebarTab == .explorer {
                    self.setSidebarVisible(false)
                } else {
                    self.showExplorerTab()
                }
            case .sourceControl:
                if self.isSidebarVisible && self.activeSidebarTab == .sourceControl {
                    self.setSidebarVisible(false)
                } else {
                    self.showSourceControlTab()
                }
            case .search:
                if self.isSidebarVisible && self.activeSidebarTab == .search {
                    self.setSidebarVisible(false)
                } else {
                    self.showSearchTab()
                }
            case .extensions:
                self.showExtensionsAction(nil)
            }
        }
        titleBar.onToggleSidebar = { [weak self] in self?.toggleSidebarVisibility(nil) }
        titleBar.onTogglePanel = { [weak self] in self?.toggleTerminal(nil) }
        titleBar.onBack = { [weak self] in self?.navigateBack(nil) }
        titleBar.onForward = { [weak self] in self?.navigateForward(nil) }
        titleBar.onSearch = { [weak self] in self?.showQuickAccess(nil) }
        gitGraphPanel.onMessage = { [weak self] message in self?.statusLabel.stringValue = message }
        gitGraphPanel.onRepositoryChanged = { [weak self] in self?.refreshGitStatus() }
        titleBar.setSidebarVisible(true)
        titleBar.setPanelVisible(false)
        sourceControl.onOpen = { [weak self] relPath in
            guard let self, let workspace = self.workspace else { return }
            self.openWorkspaceFile(workspace, FileEntry(name: (relPath as NSString).lastPathComponent, relPath: relPath, isDir: false), preview: false)
        }
        sourceControl.onOpenDiff = { [weak self] relPath in
            guard let self, let workspace = self.workspace else { return }
            self.openWorkspaceDiff(workspace, relPath: relPath)
        }
        sourceControl.onRefresh = { [weak self] in self?.refreshGitStatus() }
        extensions.onInstallSpec = { [weak self] spec, sha in self?.installPlugin(spec: spec, sha256: sha) }
        extensions.onUpdate = { [weak self] plugin in
            guard let source = plugin.source else { return }
            self?.installPlugin(spec: source, sha256: nil)
        }
        extensions.onUninstall = { [weak self] id in self?.uninstallPlugin(id: id) }
        extensions.onSetEnabled = { [weak self] id, enabled in self?.setPluginEnabled(id: id, enabled: enabled) }
        searchSidebar.onSearch = { [weak self] query, options in self?.searchSidebarWorkspace(query, options: options) }
        searchSidebar.onOpen = { [weak self] match in self?.openSearchSidebarMatch(match) }
        searchSidebar.onReplaceAll = { [weak self] query, replacement, options in
            self?.searchSidebarReplaceAll(query: query, replacement: replacement, options: options)
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
        scrollView.backgroundColor = Theme.editorBackground
        scrollView.borderType = .noBorder
        scrollView.translatesAutoresizingMaskIntoConstraints = true
        scrollView.autoresizingMask = [.width, .height]
        editorView.autoresizingMask = [.width]
        scrollView.documentView = editorView
        secondaryScrollView.hasVerticalScroller = true
        secondaryScrollView.hasHorizontalScroller = false
        secondaryScrollView.scrollerStyle = .overlay
        secondaryScrollView.automaticallyAdjustsContentInsets = false
        secondaryScrollView.contentInsets = scrollView.contentInsets
        secondaryScrollView.drawsBackground = true
        secondaryScrollView.backgroundColor = Theme.editorBackground
        secondaryScrollView.translatesAutoresizingMaskIntoConstraints = true
        secondaryScrollView.autoresizingMask = [.width, .height]
        secondaryEditorView.autoresizingMask = [.width]
        secondaryScrollView.documentView = secondaryEditorView
        installDiffScrollSync()
        markdownPreview.onUserScroll = { [weak self] line in self?.syncEditorToPreview(line: line) }
        editorSplitView.isVertical = true
        editorSplitView.dividerStyle = .thin
        editorSplitView.translatesAutoresizingMaskIntoConstraints = false
        editorSplitView.addSubview(scrollView)

        tabBar.translatesAutoresizingMaskIntoConstraints = false
        tabBar.setContentHuggingPriority(.required, for: .vertical)
        tabBar.setContentCompressionResistancePriority(.required, for: .vertical)
        tabBar.onSelect = { [weak self] i in self?.selectTab(i, fromTabBar: true) }
        tabBar.onClose = { [weak self] i in self?.closeTab(at: i) }
        tabBar.onMove = { [weak self] from, to in self?.moveTab(from: from, to: to) }
        tabBar.onTogglePreview = { [weak self] in self?.toggleMarkdownPreview(nil) }
        syntaxEvictionTimer = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
            self?.evictInactiveSyntax(now: .now)
        }
        lspTimer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.bridge.pollLSP()
                if let self { self.statusBar.showLSP(self.bridge.lspStatus()) }
                self?.refreshHover()
            self?.refreshCompletion()
                self?.refreshSignatureHelp()
                self?.refreshDefinition()
                self?.refreshReferences()
                self?.refreshRename()
                self?.refreshCodeActions()
                self?.refreshFormatting()
                self?.refreshDocumentSymbols()
                if let document = self?.activeTab?.doc {
                    if Date().timeIntervalSince(self?.lastSemanticRequest ?? .distantPast) > 0.5 {
                        self?.lastSemanticRequest = .now
                        self?.bridge.requestSemanticTokens(document: document)
                        self?.bridge.requestInlayHints(document: document, startLine: 0, endLine: document.lineCount())
                    }
                    self?.editorView.semanticTokens = self?.bridge.semanticTokens(document: document) ?? []
                    self?.editorView.inlayHints = self?.bridge.inlayHints(document: document) ?? []
                }
                self?.updateStickyScope()
                self?.reloadSettings()
                let problems = self?.bridge.core.problems() ?? []
                self?.problemsPanel.update(problems)
                self?.problemsPanel.isHidden = self?.bottomPanelTab != .problems
                self?.bottomPanelHeader.setProblemCount(problems.count)
                if let document = self?.activeTab?.doc {
                    let markers = problems.filter { $0.uri == "file://\(document.path())" }
                        .map { ($0.line, $0.severity) }
                    self?.scrollView.diagnosticScroller.update(markers: markers, lineCount: document.lineCount())
                    // Redraw only when diagnostics change, not every 100 ms.
                    let revision = document.diagnosticsRevision()
                    if revision != self?.lastDiagnosticsRevision {
                        self?.lastDiagnosticsRevision = revision
                        self?.editorView.needsDisplay = true
                    }
                }
            }
        }
        tabBar.titleBar = titleBar
        pathBar.translatesAutoresizingMaskIntoConstraints = false
        pathBar.show([])
        breadcrumbBar.translatesAutoresizingMaskIntoConstraints = false
        breadcrumbBar.clear()
        findBarSlot.translatesAutoresizingMaskIntoConstraints = false
        stickyScopeView.translatesAutoresizingMaskIntoConstraints = false
        // `BottomPanelContentView.layout()` sizes these (never below its
        // minimum). No autoresizing mask: one tied to the collapsed,
        // zero-height container conflicts with the panels' own constraints.
        for panel in [terminalView, outputPanel, portsPanel, problemsPanel] as [NSView] {
            panel.translatesAutoresizingMaskIntoConstraints = true
            panel.autoresizingMask = []
            // A zero frame would become a `height == 0` constraint before
            // the first layout.
            panel.frame = NSRect(x: 0, y: 0, width: 800, height: BottomPanelContentView.minimumChildHeight)
        }
        bottomPanelHeader.translatesAutoresizingMaskIntoConstraints = false
        bottomPanelHeader.clipsToBounds = true
        bottomPanelContent.translatesAutoresizingMaskIntoConstraints = false
        bottomPanelHeight = bottomPanelContent.heightAnchor.constraint(equalToConstant: 0)
        bottomPanelHeaderHeight = bottomPanelHeader.heightAnchor.constraint(equalToConstant: 0)
        bottomPanelHeader.isHidden = true
        referencesPanel.translatesAutoresizingMaskIntoConstraints = true
        referencesPanel.autoresizingMask = [.width]
        // Positioned by frame in `hoverFrame`; without constraints Auto Layout
        // would collapse it to zero size.
        hoverPopover.translatesAutoresizingMaskIntoConstraints = true
        completionPopup.translatesAutoresizingMaskIntoConstraints = false
        signaturePopup.translatesAutoresizingMaskIntoConstraints = false
        findBarSlot.isHidden = true
        findBarHeight = findBarSlot.heightAnchor.constraint(equalToConstant: 0)
        detailView.addSubview(tabBar)
        detailView.addSubview(pathBar)
        detailView.addSubview(breadcrumbBar)
        detailView.addSubview(findBarSlot)
        detailView.addSubview(editorSplitView)
        detailView.addSubview(stickyScopeView)
        editorTerminalView.translatesAutoresizingMaskIntoConstraints = false
        editorTerminalView.isHidden = true
        detailView.addSubview(editorTerminalView)
        gitGraphPanel.translatesAutoresizingMaskIntoConstraints = false
        detailView.addSubview(gitGraphPanel)
        // `TerminalView` is layer-backed. Without a layer here, it was
        // composited above every non-layer sibling and blanked the tab bar
        // and path bar while a terminal tab was active.
        detailView.wantsLayer = true
        detailView.addSubview(bottomPanelHeader)
        detailView.addSubview(bottomPanelContent)
        // Every panel is added once and switched with `isHidden`; see
        // `setBottomPanelOpen`.
        bottomPanelPlaceholder.textColor = .secondaryLabelColor
        bottomPanelPlaceholder.alignment = .center
        bottomPanelPlaceholder.isHidden = true
        for panel in [terminalView, problemsPanel, outputPanel, portsPanel, bottomPanelPlaceholder] as [NSView] {
            bottomPanelContent.addSubview(panel)
        }
        bottomPanelContent.isHidden = true
        detailView.addSubview(hoverPopover)
        detailView.addSubview(blamePopover)
        detailView.addSubview(completionPopup)
        detailView.addSubview(signaturePopup)
        NSLayoutConstraint.activate([
            tabBar.topAnchor.constraint(equalTo: detailView.topAnchor),
            tabBar.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
            tabBar.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
            tabBar.heightAnchor.constraint(equalToConstant: Theme.tabHeight),
            pathBar.topAnchor.constraint(equalTo: tabBar.bottomAnchor),
            pathBar.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
            pathBar.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
            breadcrumbBar.topAnchor.constraint(equalTo: pathBar.bottomAnchor),
            breadcrumbBar.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
            breadcrumbBar.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
            findBarSlot.topAnchor.constraint(equalTo: breadcrumbBar.bottomAnchor),
            findBarSlot.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
            findBarSlot.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
            findBarHeight,
             editorSplitView.topAnchor.constraint(equalTo: findBarSlot.bottomAnchor),
             editorSplitView.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
             editorSplitView.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
             stickyScopeView.topAnchor.constraint(equalTo: editorSplitView.topAnchor),
             stickyScopeView.leadingAnchor.constraint(equalTo: editorSplitView.leadingAnchor),
             stickyScopeView.trailingAnchor.constraint(equalTo: editorSplitView.trailingAnchor),
             // VS Code order: editor, panel title row, panel content.
             editorSplitView.bottomAnchor.constraint(equalTo: bottomPanelHeader.topAnchor),
             // A terminal tab covers the path bar, breadcrumbs and editor.
             editorTerminalView.topAnchor.constraint(equalTo: tabBar.bottomAnchor),
             editorTerminalView.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
             editorTerminalView.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
             editorTerminalView.bottomAnchor.constraint(equalTo: bottomPanelHeader.topAnchor),
             // The Git Graph tab covers the same area.
             gitGraphPanel.topAnchor.constraint(equalTo: tabBar.bottomAnchor),
             gitGraphPanel.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
             gitGraphPanel.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
             gitGraphPanel.bottomAnchor.constraint(equalTo: bottomPanelHeader.topAnchor),
             bottomPanelHeader.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
             bottomPanelHeader.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
             bottomPanelHeaderHeight,
             bottomPanelContent.topAnchor.constraint(equalTo: bottomPanelHeader.bottomAnchor),
             bottomPanelContent.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
             bottomPanelContent.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
             bottomPanelContent.bottomAnchor.constraint(equalTo: detailView.bottomAnchor),
             bottomPanelHeight,
          ])
        problemsPanel.onSelect = { [weak self] problem in self?.showProblem(problem) }
        referencesPanel.onSelect = { [weak self] location in self?.showReference(location) }
        problemsPanel.isHidden = true
        outputPanel.isHidden = true
        portsPanel.isHidden = true
        gitGraphPanel.isHidden = true
        referencesPanel.removeFromSuperview()
        terminalView.isHidden = true
        problemsPanel.isHidden = true
        bottomPanelHeader.onSelect = { [weak self] tab in
            guard let self else { return }
            if !self.isBottomPanelVisible { self.setBottomPanelOpen(true) }
            self.showBottomPanel(tab: tab)
        }
        bottomPanelHeader.onNewTerminal = { [weak self] in self?.newTerminal() }
        bottomPanelHeader.onKillTerminal = { [weak self] in self?.killActiveTerminal() }
        bottomPanelHeader.onToggleMaximize = { [weak self] in self?.toggleBottomPanelMaximized() }
        bottomPanelHeader.onResize = { [weak self] began, delta in self?.resizeBottomPanel(began: began, deltaY: delta) }
        if let saved = UserDefaults.standard.object(forKey: "BottomPanelHeight") as? Double, saved >= Double(Self.minBottomPanelHeight) {
            bottomPanelOpenHeight = CGFloat(saved)
        }
        bottomPanelHeader.onClose = { [weak self] in self?.setBottomPanelOpen(false) }
        bottomPanelHeader.onSelectTerminal = { [weak self] index in
            guard let self, self.terminalHandles.indices.contains(index) else { return }
            self.activeTerminalIndex = index
            self.ensureTerminal()
            self.window?.makeFirstResponder(self.terminalView)
        }
        stickyScopeView.isHidden = true
        terminalView.onOpenLink = { [weak self] path, line, col in
            self?.openTerminalLink(path: path, line: line, col: col)
        }
        terminalView.onExit = { [weak self] in self?.terminalExited() }
        terminalView.contextMenu = { [weak self] in self?.panelTerminalMenu() }
        editorTerminalView.onOpenLink = { [weak self] path, line, col in
            self?.openTerminalLink(path: path, line: line, col: col)
        }
        editorTerminalView.onExit = { [weak self] in self?.editorTerminalExited() }
        tabBar.menuForTab = { [weak self] index in self?.menu(forTab: index) }
        hoverPopover.isHidden = true
        blamePopover.isHidden = true
        blamePopover.translatesAutoresizingMaskIntoConstraints = true
        blamePopover.onPointerEntered = { [weak self] in self?.cancelBlameHide() }
        blamePopover.onPointerExited = { [weak self] in self?.scheduleBlameHide() }
        editorView.onMouseExit = { [weak self] in self?.scheduleBlameHide() }
        completionPopup.isHidden = true
        signaturePopup.isHidden = true
        completionPopup.onClose = { [weak self] in self?.closeCompletion() }
        completionPopup.onSelect = { [weak self] item in self?.acceptCompletion(item) }
        editorView.onCompletionKey = { [weak self] event in self?.handleCompletionKey(event) ?? false }
        editorView.onTextInput = { [weak self] text in
            self?.completionDismissed = false
            self?.requestOnTypeFormatting(trigger: text)
        }

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
            splitView.leadingAnchor.constraint(equalTo: activityBar.trailingAnchor, constant: Theme.cardGap / 2),
            splitView.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -Theme.cardGap),
            splitView.bottomAnchor.constraint(equalTo: statusBar.topAnchor, constant: -Theme.cardGap),
            statusBar.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            statusBar.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            statusBar.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            // Own height, not the system title bar's: that one is gone in
            // full screen, and shorter than this in a window.
            titleBar.heightAnchor.constraint(equalToConstant: TitleBar.minimumHeight),
        ])
    }

    private func showProblem(_ problem: ProblemInfo) {
        guard let path = URL(string: problem.uri)?.path else { return }
        guard let index = tabs.firstIndex(where: { $0.doc.path() == path }) else { return }
        if activeIndex != index { selectTab(index) }
        activeTab?.doc.goToLine(line: problem.line, col: problem.column)
        editorView.window?.makeFirstResponder(editorView)
    }

    private func openTerminalLink(path: String, line: UInt64, col: UInt64) {
        guard let workspace else { return }
        let url: URL
        if path.hasPrefix("/") {
            url = URL(fileURLWithPath: path)
        } else {
            url = URL(fileURLWithPath: workspace.root()).appendingPathComponent(path)
        }
        do {
            try openFile(at: url)
            activeTab?.doc.goToLine(line: max(1, line) - 1, col: max(1, col) - 1)
            editorView.revealLine(Int(max(1, line) - 1))
        } catch {
            statusLabel.stringValue = "Cannot open terminal link: \(path)"
        }
    }

    // MARK: Git blame

    @objc func toggleGitBlame(_ sender: Any?) {
        setGitBlame(!gitBlameEnabled)
    }

    private func setGitBlame(_ on: Bool) {
        gitBlameEnabled = on
        if on {
            refreshBlame()
        } else {
            blameRefreshItem?.cancel()
            updateBlameAnnotation()
        }
    }

    /// Recomputes blame for the active file in the background. The text is
    /// passed in, so unsaved edits show as uncommitted. A slower, older
    /// answer is dropped by the per-tab generation.
    private func refreshBlame() {
        blameRefreshItem?.cancel()
        guard gitBlameEnabled, let workspace, let tab = activeTab, tab.isDocument,
              let path = tab.relPath else {
            updateBlameAnnotation()
            return
        }
        tab.blameGeneration += 1
        let generation = tab.blameGeneration
        let text = tab.doc.text()
        let lines = tab.doc.lineCount()
        DispatchQueue.global(qos: .utility).async { [weak self, workspace, path, text] in
            let blame = try? workspace.gitBlame(relPath: path, text: text)
            Task { @MainActor [weak self] in
                guard let self,
                      let tab = self.tabs.first(where: { $0.relPath == path }),
                      tab.blameGeneration == generation else { return }
                tab.blame = blame
                tab.blameLineCount = lines
                if self.activeTab === tab { self.updateBlameAnnotation() }
            }
        }
    }

    private func scheduleBlameRefresh() {
        guard gitBlameEnabled else { return }
        blameRefreshItem?.cancel()
        let item = DispatchWorkItem { [weak self] in self?.refreshBlame() }
        blameRefreshItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.blameRefreshDelay, execute: item)
    }

    /// Shows the commit of the caret line after its text. Hidden when blame
    /// is off, unknown, or stale (an edit added or removed lines since).
    func updateBlameAnnotation() {
        defer {
            if editorView.blameAnnotation == nil { hideBlamePopover() }
        }
        blameCommit = nil
        guard gitBlameEnabled, let tab = activeTab, tab.isDocument, let blame = tab.blame,
              tab.doc.lineCount() == tab.blameLineCount else {
            editorView.blameAnnotation = nil
            return
        }
        let line = tab.doc.primaryCursor().line
        guard Int(line) < blame.lineCommit.count else {
            editorView.blameAnnotation = nil
            return
        }
        let index = Int(blame.lineCommit[Int(line)])
        guard blame.commits.indices.contains(index) else {
            editorView.blameAnnotation = nil
            return
        }
        let commit = blame.commits[index]
        blameCommit = commit
        editorView.blameAnnotation = EditorView.BlameAnnotation(line: line, text: BlameFormat.annotation(for: commit))
    }

    /// Opens the card when `point` (editor coordinates) is on the blame text.
    /// Returns `true` when it is, so the LSP hover stays out of the way.
    private func handleBlameHover(point: NSPoint) -> Bool {
        guard let commit = blameCommit, let rect = editorView.blameAnnotationRect(), rect.contains(point) else {
            if !blamePopover.isHidden { scheduleBlameHide() }
            return false
        }
        cancelBlameHide()
        hoverPopover.isHidden = true
        if blamePopover.isHidden || blamePopover.sha != commit.sha {
            blamePopover.show(commit)
            let anchor = detailView.convert(rect, from: editorView)
            let visible = detailView.convert(editorView.visibleRect, from: editorView).insetBy(dx: 8, dy: 4)
            blamePopover.frame = HoverPopover.placement(size: blamePopover.fittingSize(), anchor: anchor, in: visible, gap: 2)
            blamePopover.isHidden = false
            blamePopover.needsLayout = true
        }
        return true
    }

    /// Closes the card after a short grace period, so the pointer can cross
    /// the gap between the annotation and the card to reach `Copy SHA`.
    private func scheduleBlameHide() {
        guard !blamePopover.isHidden else { return }
        blameHideItem?.cancel()
        let item = DispatchWorkItem { [weak self] in self?.hideBlamePopover() }
        blameHideItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: item)
    }

    private func cancelBlameHide() {
        blameHideItem?.cancel()
        blameHideItem = nil
    }

    private func hideBlamePopover() {
        cancelBlameHide()
        blamePopover.isHidden = true
    }

    private func beginHover(at position: TextPos, point: NSPoint) {
        if handleBlameHover(point: point) { return }
        guard position.line != UInt64.max, let document = activeTab?.doc else {
            hoverPopover.isHidden = true
            return
        }
        // Same spot: skip unless the last answer was empty (the server may
        // still be indexing), then retry at most once a second.
        if hoverDocument === document, hoverRequestPosition?.line == position.line,
           hoverRequestPosition?.colUtf16 == position.colUtf16,
           !hoverPopover.isHidden || Date().timeIntervalSince(hoverRequestTime) < 1 { return }
        // Leaving the hovered word hides the card; moving within it keeps it.
        if !hoverPopover.isHidden, !(hoverShownRange.map { range in
            position.line == range.line && position.colUtf16 >= range.start && position.colUtf16 <= range.end
        } ?? false) {
            hoverPopover.isHidden = true
        }
        hoverRequestTime = Date()
        hoverDocument = document
        hoverPosition = position
        hoverPoint = point
        hoverRequestPosition = position
        // Only remember the position once a request actually went out, so
        // hovering the same spot again retries after the server is ready.
        if !bridge.requestHover(document: document, line: position.line, character: position.colUtf16) {
            hoverRequestPosition = nil
        }
    }

    private func refreshHover() {
        guard let document = hoverDocument, let position = hoverRequestPosition,
              let hover = bridge.hover(document: document),
              hover.startLine == nil || (position.line >= (hover.startLine ?? 0) && position.line <= (hover.endLine ?? position.line)) else { return }
        // The poll timer calls this every 100 ms; only re-layout on new content.
        if !hoverPopover.isHidden, hoverShownContents == hover.contents,
           hoverShownPosition?.line == position.line, hoverShownPosition?.colUtf16 == position.colUtf16 { return }
        hoverShownContents = hover.contents
        hoverShownPosition = position
        if let line = hover.startLine, hover.endLine == line,
           let start = hover.startCharacter, let end = hover.endCharacter {
            hoverShownRange = (line, start, end)
        } else {
            hoverShownRange = (position.line, position.colUtf16, position.colUtf16)
        }
        hoverPopover.show(hover.contents)
        hoverPopover.frame = hoverFrame(for: hover, at: position)
        hoverPopover.isHidden = false
        hoverPopover.needsLayout = true
    }

    /// Places the card above the hovered word (VS Code default), or below,
    /// or on whichever side of the line has more room, clamped to the
    /// visible editor so it never spills off the window edges.
    private func hoverFrame(for hover: HoverInfo, at position: TextPos) -> NSRect {
        let editor = editorView
        let line = position.line
        let text = activeTab?.doc.displayLine(line: line) ?? ""
        let column = hover.startLine == line ? (hover.startCharacter ?? position.colUtf16) : position.colUtf16
        let wordX = editor.textOriginX + editor.xOffset(in: text, column: column)
        let lineTop = CGFloat(line) * editor.metrics.lineHeight
        let wordRect = NSRect(x: wordX, y: lineTop, width: 1, height: editor.metrics.lineHeight)
        let anchor = detailView.convert(wordRect, from: editor)
        let visible = detailView.convert(editor.visibleRect, from: editor).insetBy(dx: 8, dy: 4)
        let size = hoverPopover.fittingSize(maxHeight: min(HoverPopover.maxHeight, max(60, visible.height / 2)))
        return HoverPopover.placement(size: size, anchor: anchor, in: visible)
    }

    private func requestCompletionIfNeeded() {
        guard let document = activeTab?.doc else { return }
        let position = document.primaryCursor()
        completionPosition = TextPos(line: position.line, colUtf16: position.colUtf16)
        let line = document.displayLine(line: position.line)
        let column = Int(position.colUtf16)
        let prefix = String(line.prefix(max(0, min(column, line.count))))
            .split(whereSeparator: { !$0.isLetter && !$0.isNumber && $0 != "_" }).last.map(String.init) ?? ""
        completionQuery = prefix
        let beforePrefix = String(line.prefix(max(0, min(column, line.count))))
        let lspTrigger = beforePrefix.hasSuffix(".") || beforePrefix.hasSuffix("::") || beforePrefix.hasSuffix("->")
        guard !prefix.isEmpty || lspTrigger else {
            snippetCompletionItems = []
            completionPopup.isHidden = true
            return
        }
        if isSettingsDocument(document) {
            localCompletionItems = GalaxySettings.completionItems()
            refreshCompletion()
            return
        }
        localCompletionItems = nil
        bridge.requestCompletion(document: document, line: position.line, character: position.colUtf16)
        snippetCompletionItems = prefix.isEmpty ? [] : bridge.snippetCompletions(document: document)
            .filter { $0.label.lowercased().hasPrefix(prefix.lowercased()) }
        refreshCompletion()
    }

    private func refreshCompletion() {
        guard let document = activeTab?.doc else { return }
        guard !completionDismissed else { return }
        let position = document.primaryCursor()
        guard position.line == completionPosition.line,
              position.colUtf16 == completionPosition.colUtf16 else {
            completionPopup.isHidden = true
            return
        }
        let lspItems = bridge.completions(document: document)
        let items = localCompletionItems ?? (lspItems + snippetCompletionItems)
        guard !items.isEmpty else {
            completionPopup.isHidden = true
            return
        }
        completionPopup.update(items, query: completionQuery)
        if completionPopup.isHidden { return }
        completionPopup.frame = NSRect(x: editorView.textOriginX + 8, y: CGFloat(completionPosition.line + 1) * editorView.metrics.lineHeight + 8, width: 420, height: 192)
    }

    private func closeCompletion() {
        completionPopup.isHidden = true
        completionDismissed = true
        snippetCompletionItems = []
    }

    private func isSettingsDocument(_ document: DocumentHandle) -> Bool {
        let path = Self.fileKey(document.path())
        let global = Self.fileKey(globalSettingsURL.path)
        let workspace = self.workspace.map { Self.fileKey(GalaxySettings.workspaceURL(root: $0.root()).path) }
        return path == global || path == workspace
    }

    private func refreshSignatureHelp() {
        guard let document = activeTab?.doc else { signaturePopup.isHidden = true; return }
        bridge.requestSignatureHelp(document: document, line: completionPosition.line, character: completionPosition.colUtf16)
        guard let signature = bridge.signatureHelp(document: document) else { signaturePopup.isHidden = true; return }
        signaturePopup.show(signature)
        signaturePopup.isHidden = false
        signaturePopup.frame = NSRect(x: editorView.textOriginX + 8, y: CGFloat(completionPosition.line + 1) * editorView.metrics.lineHeight + 8, width: 520, height: signature.documentation == nil ? 48 : 78)
    }

    private func requestDefinition(at position: TextPos) -> Bool {
        guard let document = activeTab?.doc else { return false }
        // laravel-lsp has no log channel provider; the core resolves it.
        if let channel = editorView.logChannelName(at: position) {
            if let found = laravelLogChannelLocation(file: document.path(), channel: channel) {
                jump(to: found)
            } else {
                NSSound.beep()
            }
            return true
        }
        // A relative import is a file on disk: open it without the server.
        // Packages (`node:fs`, `react`) go to the server below.
        if let specifier = editorView.importSpecifier(at: position),
           let target = EditorView.resolveImport(specifier.path, from: document.path()) {
            jump(to: DefinitionLocation(uri: URL(fileURLWithPath: target).absoluteString, line: 0, column: 0))
            return true
        }
        definitionRequest = (document, position)
        bridge.requestDefinition(document: document, line: position.line, character: position.colUtf16)
        return true
    }

    private func refreshDefinition() {
        guard let request = definitionRequest,
              let location = bridge.definitionLocations(document: request.document).first else { return }
        definitionRequest = nil
        jump(to: location)
    }

    /// Opens `location` and records the jump for back/forward.
    private func jump(to location: DefinitionLocation) {
        guard let path = URL(string: location.uri)?.path else { return }
        let target = NavLocation(key: path, line: location.line, col: location.column)
        if let current = activeTab.map(location(of:)) { _ = navHistory.recordJump(from: current, to: target); refreshNavigationButtons() }
        _ = show(target)
    }

    @objc func findReferences(_ sender: Any?) {
        guard let document = activeTab?.doc else { NSSound.beep(); return }
        let position = document.primaryCursor()
        referencesRequest = document
        bridge.requestReferences(document: document, line: position.line, character: position.colUtf16)
    }

    private func refreshReferences() {
        guard let document = referencesRequest else { return }
        let locations = bridge.references(document: document)
        guard !locations.isEmpty else { return }
        referencesRequest = nil
        problemsPanel.isHidden = true
        referencesPanel.update(locations)
        if referencesPanel.superview == nil { detailView.addSubview(referencesPanel) }
        referencesPanel.frame = NSRect(x: 0, y: 0, width: detailView.bounds.width, height: 150)
        referencesPanel.isHidden = false
    }

    private func showReference(_ location: DefinitionLocation) {
        guard URL(string: location.uri) != nil else { return }
        referencesPanel.removeFromSuperview()
        jump(to: location)
    }

    @objc func renameSymbol(_ sender: Any?) {
        guard let document = activeTab?.doc else { NSSound.beep(); return }
        let current = document.primaryCursor()
        let line = document.displayLine(line: current.line)
        let column = min(Int(current.colUtf16), line.utf16.count)
        let prefix = String(line.prefix(column))
        let suffix = String(line.dropFirst(column))
        let left = prefix.lastIndex(where: { !$0.isLetter && !$0.isNumber && $0 != "_" }).map { prefix.index(after: $0) } ?? prefix.startIndex
        let right = suffix.firstIndex(where: { !$0.isLetter && !$0.isNumber && $0 != "_" }) ?? suffix.endIndex
        let field = NSTextField(string: String(prefix[left...]) + String(suffix[..<right]))
        field.frame = NSRect(x: 0, y: 0, width: 280, height: 24)
        let alert = NSAlert(); alert.messageText = "Rename Symbol"; alert.informativeText = "Enter the new symbol name."; alert.accessoryView = field
        alert.addButton(withTitle: "Rename"); alert.addButton(withTitle: "Cancel"); alert.window.initialFirstResponder = field
        guard alert.runModal() == .alertFirstButtonReturn, !field.stringValue.isEmpty else { return }
        renameRequest = document
        bridge.requestRename(document: document, line: current.line, character: current.colUtf16, newName: field.stringValue)
    }

    private func refreshRename() {
        guard let document = renameRequest else { return }
        let edits = bridge.renameEdits(document: document)
        guard !edits.isEmpty else { return }
        renameRequest = nil
        snippetSession = nil
        snippetDocumentKey = nil
        var grouped: [String: [RenameEdit]] = [:]
        for edit in edits { grouped[edit.uri, default: []].append(edit) }
        for (uri, fileEdits) in grouped {
            guard let path = URL(string: uri)?.path else { continue }
            let target: DocumentHandle
            let wasOpen = tabs.contains { $0.doc.path() == path }
            if let tab = tabs.first(where: { $0.doc.path() == path }) { target = tab.doc }
            else { guard let opened = try? bridge.openDocument(path: path) else { continue }; target = opened }
            _ = bridge.applyRenameEdits(document: target, edits: fileEdits)
            if !wasOpen { try? bridge.save(document: target) }
            bridge.syncLSP(document: target)
        }
        updateEditedState()
    }

    @objc func showCodeActions(_ sender: Any?) {
        guard let document = activeTab?.doc else { NSSound.beep(); return }
        let position = document.primaryCursor()
        codeActionRequest = document
        bridge.requestCodeActions(document: document, line: position.line, character: position.colUtf16)
    }

    private func refreshCodeActions() {
        guard let document = codeActionRequest else { return }
        let actions = bridge.codeActions(document: document)
        guard !actions.isEmpty else { return }
        codeActionRequest = nil
        codeActionPopup.update(actions)
        codeActionPopup.isHidden = false
        codeActionPopup.frame = NSRect(x: editorView.textOriginX + 8, y: editorView.metrics.lineHeight + 8, width: 420, height: 156)
        window?.makeFirstResponder(editorView)
    }

    private func applyCodeAction(_ action: CodeActionInfo) {
        codeActionPopup.isHidden = true
        snippetSession = nil
        snippetDocumentKey = nil
        var grouped: [String: [RenameEdit]] = [:]
        for edit in action.edits { grouped[edit.uri, default: []].append(edit) }
        for (uri, edits) in grouped {
            guard let path = URL(string: uri)?.path else { continue }
            let wasOpen = tabs.contains { $0.doc.path() == path }
            let target: DocumentHandle
            if let tab = tabs.first(where: { $0.doc.path() == path }) {
                target = tab.doc
            } else {
                guard let opened = try? bridge.openDocument(path: path) else { continue }
                target = opened
            }
            _ = bridge.applyRenameEdits(document: target, edits: edits)
            if !wasOpen { try? bridge.save(document: target) }
            bridge.syncLSP(document: target)
        }
        updateEditedState()
        editorView.needsDisplay = true
    }

    @objc func formatDocument(_ sender: Any?) {
        guard let document = activeTab?.doc else { NSSound.beep(); return }
        let selected = document.selectedRange()
        formattingRequest = document
        if selected.length > 0 {
        let indent = document.indentSettings()
        bridge.requestRangeFormatting(document: document, range: selected, tabSize: UInt64(indent.tabSize), insertSpaces: !indent.useTabs)
        } else {
        let indent = document.indentSettings()
        bridge.requestFormatting(document: document, tabSize: UInt64(indent.tabSize), insertSpaces: !indent.useTabs)
        }
    }

    private func requestOnTypeFormatting(trigger: String) {
        guard trigger == ";" || trigger == "}" || trigger == ")" || trigger == "\n",
              let document = activeTab?.doc else { return }
        let position = document.selectedRange().location
        let cursor = document.positionAt(offset: position)
        formattingRequest = document
        let indent = document.indentSettings()
        bridge.requestOnTypeFormatting(document: document, line: cursor.line, character: cursor.colUtf16, trigger: trigger, tabSize: UInt64(indent.tabSize), insertSpaces: !indent.useTabs)
    }

    private func refreshFormatting() {
        guard let document = formattingRequest else { return }
        let edits = bridge.formattingEdits(document: document)
        guard !edits.isEmpty else { return }
        formattingRequest = nil
        snippetSession = nil
        snippetDocumentKey = nil
        _ = bridge.applyFormattingEdits(document: document, edits: edits)
        bridge.syncLSP(document: document)
        updateEditedState()
        editorView.needsDisplay = true
    }

    @objc func goToDefinition(_ sender: Any?) {
        guard let document = activeTab?.doc else { NSSound.beep(); return }
        let position = document.primaryCursor()
        _ = requestDefinition(at: TextPos(line: position.line, colUtf16: position.colUtf16))
    }

    @objc func goToTypeDefinition(_ sender: Any?) {
        guard let document = activeTab?.doc else { NSSound.beep(); return }
        let position = document.primaryCursor()
        definitionRequest = (document, TextPos(line: position.line, colUtf16: position.colUtf16))
        bridge.requestTypeDefinition(document: document, line: position.line, character: position.colUtf16)
    }

    @objc func goToImplementation(_ sender: Any?) {
        guard let document = activeTab?.doc else { NSSound.beep(); return }
        let position = document.primaryCursor()
        definitionRequest = (document, TextPos(line: position.line, colUtf16: position.colUtf16))
        bridge.requestImplementation(document: document, line: position.line, character: position.colUtf16)
    }

    private func handleCompletionKey(_ event: NSEvent) -> Bool {
        if completionPopup.isHidden, let session = snippetSession {
            switch event.keyCode {
            case 48: // Tab
                return moveSnippetStop(direction: event.modifierFlags.contains(.shift) ? -1 : 1)
            case 53: // Escape
                snippetSession = nil
                return true
            default: break
            }
            _ = session
        }
        guard !completionPopup.isHidden else { return false }
        switch event.keyCode {
        case 125: completionPopup.move(1); return true
        case 126: completionPopup.move(-1); return true
        case 36, 48, 76: completionPopup.accept(); return true
        case 53: completionPopup.cancel(); return true
        default: return false
        }
    }

    private func acceptCompletion(_ item: CompletionInfo) {
        guard let document = activeTab?.doc else { return }
        let start = completionPosition.colUtf16 >= UInt64(completionQuery.utf16.count) ? completionPosition.colUtf16 - UInt64(completionQuery.utf16.count) : 0
        let offset = document.utf16Offset(pos: TextPos(line: completionPosition.line, colUtf16: start))
        let text: String
        var firstStopIndex: UInt32?
        var firstStops: [SnippetStopInfo] = []
        if item.isSnippet {
            let line = document.displayLine(line: completionPosition.line)
            let indent = String(line.prefix { $0 == " " || $0 == "\t" })
            let settings = document.indentSettings()
            let unit = settings.useTabs ? "\t" : String(repeating: " ", count: Int(settings.tabSize))
            let selectedText = document.textInRange(range: document.selectedRange())
            let year = String(Calendar.current.component(.year, from: Date()))
            let expansion = bridge.expandedSnippet(body: item.insertText, baseIndent: indent, indentUnit: unit,
                                                   filename: (document.path() as NSString).lastPathComponent,
                                                   selectedText: selectedText, currentYear: year)
            text = expansion.text
            firstStopIndex = expansion.stops.first(where: { $0.index != 0 })?.index
        if let firstStopIndex {
                firstStops = expansion.stops.filter { $0.index == firstStopIndex }
            }
            snippetSession = makeSnippetSession(expansion.stops, insertionOffset: offset,
                                                documentLength: document.lenUtf16() + UInt64(text.utf16.count) - UInt64(completionQuery.utf16.count))
            snippetDocumentKey = Self.fileKey(document.path())
        } else {
            text = item.insertText
        }
        document.commitText(text: text, replacement: Utf16Range(location: offset, length: UInt64(completionQuery.utf16.count)))
        if firstStopIndex != nil {
            let selections = firstStops.map { stop in
                let absolute = Utf16Range(location: offset + stop.range.location, length: stop.range.length)
                return TextSelection(anchor: document.positionAt(offset: absolute.location),
                                     head: document.positionAt(offset: absolute.location + absolute.length))
            }
            document.setSelections(selections: selections, primary: 0)
        }
        closeCompletion()
        editorView.didChangeDocument()
    }

    private func makeSnippetSession(_ stops: [SnippetStopInfo], insertionOffset: UInt64,
                                    documentLength: UInt64) -> SnippetSession? {
        let groups = Dictionary(grouping: stops, by: \ .index)
            .sorted { lhs, rhs in
                if lhs.key == 0 { return false }
                if rhs.key == 0 { return true }
                return lhs.key < rhs.key
            }
        let ranges = groups.map { index, values in
            values.map { stop in
                SnippetSession.Stop(index: index,
                                    location: insertionOffset + stop.range.location,
                                    length: stop.range.length)
            }
        }
        guard !ranges.isEmpty else { return nil }
        return SnippetSession(stops: ranges, documentLength: documentLength)
    }

    private func moveSnippetStop(direction: Int) -> Bool {
        guard var session = snippetSession,
              let document = activeTab?.doc,
              snippetDocumentKey == Self.fileKey(document.path()) else {
            snippetSession = nil
            snippetDocumentKey = nil
            return false
        }
        session.noteDocumentChange(length: document.lenUtf16())
        guard let stop = session.move(direction) else {
            snippetSession = nil
            return true
        }
        let selections = stop.map { range in
            TextSelection(anchor: document.positionAt(offset: range.location),
                          head: document.positionAt(offset: range.location + range.length))
        }
        document.setSelections(selections: selections, primary: 0)
        snippetSession = session
        editorView.didChangeSelection()
        return true
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

        sectionHeader.onScmPull = { [weak self] in self?.sourceControl.pull() }
        sectionHeader.onScmBranch = { [weak self] in self?.showGitBranchPicker() }
        sectionHeader.onScmCommit = { [weak self] in self?.sourceControl.commit(nil) }
        sectionHeader.onScmRefresh = { [weak self] in self?.refreshGitStatus() }
        sectionHeader.scmMoreMenu = { [weak self] in self?.sourceControl.moreActionsMenu() ?? NSMenu() }
        sectionHeader.onExtensionsRefresh = { [weak self] in self?.refreshExtensions(network: true) }
        sectionHeader.onExtensionsAddRegistry = { [weak self] in self?.addPluginRegistry() }
        sectionHeader.onSearchRefresh = { [weak self] in self?.searchSidebar.refresh() }
        sectionHeader.onSearchClear = { [weak self] in self?.searchSidebar.clearQuery() }
        sectionHeader.onSearchToggleReplace = { [weak self] in self?.searchSidebar.toggleReplace() }
        sectionHeader.onSearchToggleDetails = { [weak self] in self?.searchSidebar.toggleDetails() }
        sectionHeader.searchMoreMenu = { [weak self] in self?.searchSidebar.moreMenu() ?? NSMenu() }

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
        explorer.onRename = { [weak self] entry, name in self?.renameEntry(entry, to: name) }
        explorer.contextMenu = { [weak self] in self?.explorerContextMenu() }
        explorer.outlineView.onKey = { [weak self] event in self?.explorerKey(event) ?? false }

        sidebar.addSubview(sectionHeader)
        sidebar.addSubview(explorer.scrollView)
        sidebar.addSubview(sourceControl.view)
        sidebar.addSubview(searchSidebar.view)
        sidebar.addSubview(extensions.view)
        sidebar.addSubview(emptyState)
        NSLayoutConstraint.activate([
            sectionHeader.topAnchor.constraint(equalTo: sidebar.topAnchor),
            sectionHeader.leadingAnchor.constraint(equalTo: sidebar.leadingAnchor),
            sectionHeader.trailingAnchor.constraint(equalTo: sidebar.trailingAnchor),
            explorer.scrollView.topAnchor.constraint(equalTo: sectionHeader.bottomAnchor),
            explorer.scrollView.leadingAnchor.constraint(equalTo: sidebar.leadingAnchor),
            explorer.scrollView.trailingAnchor.constraint(equalTo: sidebar.trailingAnchor),
            explorer.scrollView.bottomAnchor.constraint(equalTo: sidebar.bottomAnchor),
            sourceControl.view.topAnchor.constraint(equalTo: sectionHeader.bottomAnchor),
            sourceControl.view.leadingAnchor.constraint(equalTo: sidebar.leadingAnchor),
            sourceControl.view.trailingAnchor.constraint(equalTo: sidebar.trailingAnchor),
            sourceControl.view.bottomAnchor.constraint(equalTo: sidebar.bottomAnchor),
            searchSidebar.view.topAnchor.constraint(equalTo: sectionHeader.bottomAnchor),
            searchSidebar.view.leadingAnchor.constraint(equalTo: sidebar.leadingAnchor),
            searchSidebar.view.trailingAnchor.constraint(equalTo: sidebar.trailingAnchor),
            searchSidebar.view.bottomAnchor.constraint(equalTo: sidebar.bottomAnchor),
            extensions.view.topAnchor.constraint(equalTo: sectionHeader.bottomAnchor),
            extensions.view.leadingAnchor.constraint(equalTo: sidebar.leadingAnchor),
            extensions.view.trailingAnchor.constraint(equalTo: sidebar.trailingAnchor),
            extensions.view.bottomAnchor.constraint(equalTo: sidebar.bottomAnchor),
            emptyState.centerXAnchor.constraint(equalTo: sidebar.centerXAnchor).withPriority(.defaultHigh),
            emptyState.centerYAnchor.constraint(equalTo: sidebar.centerYAnchor, constant: -20),
            emptyState.leadingAnchor.constraint(greaterThanOrEqualTo: sidebar.leadingAnchor, constant: 16).withPriority(.defaultLow),
            emptyState.trailingAnchor.constraint(lessThanOrEqualTo: sidebar.trailingAnchor, constant: -16).withPriority(.defaultLow),
        ])
        sourceControl.view.isHidden = true
        searchSidebar.view.isHidden = true
        extensions.view.isHidden = true
    }

    // MARK: Sidebar

    @objc func toggleSidebarVisibility(_ sender: Any?) {
        setSidebarVisible(!isSidebarVisible)
    }

    @objc func showExplorer(_ sender: Any?) {
        if isSidebarVisible && activeSidebarTab == .explorer {
            if workspace != nil { window?.makeFirstResponder(explorer.outlineView) }
        } else {
            showExplorerTab()
        }
    }

    @objc func showSourceControlAction(_ sender: Any?) {
        if isSidebarVisible && activeSidebarTab == .sourceControl {
            setSidebarVisible(false)
        } else {
            showSourceControlTab()
        }
    }

    @objc func toggleSourceControl(_ sender: Any?) {
        showSourceControlAction(sender)
    }

    func setSidebarVisible(_ visible: Bool) {
        guard visible != isSidebarVisible else { return }
        activityBar.setActive(visible ? activeSidebarTab : nil)
        if visible {
            sidebar.isHidden = false
            updateSidebarContent()
            splitView.setPosition(Self.sidebarWidth, ofDividerAt: 0)
        } else {
            splitView.setPosition(0, ofDividerAt: 0)
            sidebar.isHidden = true
        }
        titleBar.setSidebarVisible(visible)
        splitView.layoutSubtreeIfNeeded()
        editorView.needsLayout = true
        editorView.needsDisplay = true
    }

    func applyPreferences() {
        editorView.setFont(Theme.editorFont)
    }

    func reloadSettings(force: Bool = false) {
        let globalDate = (try? globalSettingsURL.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
        let workspaceURL = workspace.map { GalaxySettings.workspaceURL(root: $0.root()) }
        let workspaceDate = workspaceURL.flatMap {
            (try? $0.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
        }
        guard force || globalDate != settingsModified || workspaceDate != workspaceSettingsModified else { return }
        settingsModified = globalDate
        workspaceSettingsModified = workspaceDate
        do {
            let global = FileManager.default.fileExists(atPath: globalSettingsURL.path)
                ? try GalaxySettings.load(from: globalSettingsURL) : .empty
            let workspace = workspaceURL.flatMap { url in
                FileManager.default.fileExists(atPath: url.path) ? try? GalaxySettings.load(from: url) : nil
            } ?? .empty
            settingsProblems = []
            let settings = global.merged(with: workspace)
            autoClosingTags = settings.effectiveAutoClosingTags
            linkedEditing = settings.effectiveLinkedEditing
        setGitBlame(settings.effectiveGitBlame)
            autoSaveMode = settings.effectiveAutoSave
            autoSaveDelay = settings.effectiveAutoSaveDelay
            if autoSaveMode != .afterDelay { autoSaveWorkItem?.cancel(); autoSaveWorkItem = nil }
            for tab in tabs {
                bridge.setIndent(document: tab.doc, useTabs: settings.useTabs, tabSize: settings.effectiveTabSize)
                applyTagEditing(to: tab)
            }
            editorView.renderWhitespace = settings.effectiveRenderWhitespace
            secondaryEditorView.renderWhitespace = settings.effectiveRenderWhitespace
            statusBar.show(document: activeTab?.doc, path: activeTab?.doc.path())
        } catch {
            settingsProblems = ["settings.json could not be read: \(error.localizedDescription)"]
            statusLabel.stringValue = settingsProblems[0]
            Log.ui.error("settings: \(String(describing: error), privacy: .public)")
        }
    }

    private func refreshGitStatus() {
        guard let workspace, let status = bridge.gitStatus(workspace: workspace) else {
            statusBar.showGit(branch: nil, changedCount: 0)
            activityBar.setBadge(0, for: .sourceControl)
            explorer.gitStatusMap = [:]
            explorer.gitChangedPaths = []
            sourceControl.refresh(status: nil)
            refreshTabBar()
            return
        }
        statusBar.showGit(branch: status.branch, changedCount: status.changedPaths.count)
        activityBar.setBadge(status.changedPaths.count, for: .sourceControl)

        var map: [String: GitFileStatus] = [:]
        for change in status.changes {
            map[change.path] = GitFileStatus.from(code: change.code)
        }
        explorer.gitStatusMap = map
        explorer.gitChangedPaths = Set(status.changedPaths)
        sourceControl.refresh(status: status)
        refreshTabBar()
        refreshGitDiff()
        refreshBlame()
    }

    func updateSidebarContent() {
        switch activeSidebarTab {
        case .explorer:
            sourceControl.view.isHidden = true
            searchSidebar.view.isHidden = true
            extensions.view.isHidden = true
            let hasWorkspace = workspace != nil
            explorer.scrollView.isHidden = !hasWorkspace
            emptyState.isHidden = hasWorkspace
            sectionHeader.mode = .explorer
            sectionHeader.title = ""
            sectionHeader.showsTools = hasWorkspace
            activityBar.setActive(isSidebarVisible ? .explorer : nil)
        case .sourceControl:
            explorer.scrollView.isHidden = true
            emptyState.isHidden = true
            searchSidebar.view.isHidden = true
            extensions.view.isHidden = true
            sourceControl.view.isHidden = false
            sourceControl.workspace = workspace
            sourceControl.refresh(status: workspace.flatMap { bridge.gitStatus(workspace: $0) })
            sectionHeader.mode = .sourceControl
            sectionHeader.title = "SOURCE CONTROL"
            activityBar.setActive(isSidebarVisible ? .sourceControl : nil)
        case .search:
            explorer.scrollView.isHidden = true
            sourceControl.view.isHidden = true
            emptyState.isHidden = true
            searchSidebar.view.isHidden = false
            extensions.view.isHidden = true
            sectionHeader.mode = .search
            sectionHeader.title = "SEARCH"
            activityBar.setActive(isSidebarVisible ? .search : nil)
        case .extensions:
            explorer.scrollView.isHidden = true
            sourceControl.view.isHidden = true
            emptyState.isHidden = true
            searchSidebar.view.isHidden = true
            extensions.view.isHidden = false
            sectionHeader.mode = .extensions
            sectionHeader.title = "EXTENSIONS"
            activityBar.setActive(isSidebarVisible ? .extensions : nil)
        }
    }

    func showExplorerTab() {
        activeSidebarTab = .explorer
        updateSidebarContent()
        setSidebarVisible(true)
        if workspace != nil { window?.makeFirstResponder(explorer.outlineView) }
    }

    func showSourceControlTab() {
        activeSidebarTab = .sourceControl
        updateSidebarContent()
        setSidebarVisible(true)
        sourceControl.focusMessageInput()
    }

    @objc func showExtensionsAction(_ sender: Any?) {
        if isSidebarVisible && activeSidebarTab == .extensions {
            setSidebarVisible(false)
        } else {
            showExtensionsTab()
        }
    }

    func showExtensionsTab() {
        activeSidebarTab = .extensions
        updateSidebarContent()
        setSidebarVisible(true)
        refreshExtensions(network: false)
    }

    func showSearchTab() {
        activeSidebarTab = .search
        updateSidebarContent()
        setSidebarVisible(true)
        searchSidebar.reset()
        searchSidebar.focus()
    }

    private func searchSidebarWorkspace(_ query: String, options: WorkspaceSearchOptions) {
        guard let workspace else { return }
        searchGeneration += 1
        let generation = searchGeneration
        guard !query.isEmpty else { searchSidebar.show([], message: "Type to search the workspace"); return }
        searchSidebar.show([], message: "Searching…")
        Task.detached(priority: .userInitiated) { [weak self, workspace] in
            let results = (try? workspace.search(query: query, options: options, limit: 10_000)) ?? []
            await MainActor.run {
                guard let self, self.searchGeneration == generation, self.activeSidebarTab == .search else { return }
                self.searchSidebar.show(results)
            }
        }
    }

    private func openSearchSidebarMatch(_ match: WorkspaceSearchMatch) {
        guard let workspace else { return }
        openWorkspaceFile(workspace, FileEntry(name: (match.relPath as NSString).lastPathComponent, relPath: match.relPath, isDir: false), preview: false)
        activeTab?.doc.goToLine(line: match.line, col: match.column)
    }

    private func searchSidebarReplaceAll(query: String, replacement: String, options: WorkspaceSearchOptions) {
        guard !query.isEmpty else { return }
        var totalReplaced = 0
        for tab in tabs {
            _ = tab.doc.setFind(pattern: query, config: FindConfig(caseSensitive: options.caseSensitive, wholeWord: options.wholeWord, regex: options.regex))
            let count = tab.doc.replaceAll(replacement: replacement)
            if count > 0 {
                totalReplaced += Int(count)
                if tab === activeTab {
                    editorView.didChangeDocument()
                }
            }
        }
        if totalReplaced > 0 {
            statusLabel.stringValue = "Replaced \(totalReplaced) occurrence\(totalReplaced == 1 ? "" : "s") in open editors"
        }
        searchSidebarWorkspace(query, options: options)
    }

    private func showSourceControl(_ visible: Bool) {
        if visible {
            showSourceControlTab()
        } else {
            showExplorerTab()
        }
    }

    private func showGitBranchPicker() {
        guard let workspace else { return }
        let picker = loadedGitPicker ?? GitBranchPicker()
        if loadedGitPicker == nil {
            picker.onClose = { [weak self] in self?.closeGitBranchPicker() }
            picker.onCheckout = { [weak self] name in self?.checkoutGitBranch(name) }
            picker.onCreate = { [weak self] in self?.createGitBranch() }
            window?.contentView?.addSubview(picker)
            NSLayoutConstraint.activate([
                picker.topAnchor.constraint(equalTo: tabBar.bottomAnchor, constant: 8),
                picker.centerXAnchor.constraint(equalTo: detailView.centerXAnchor),
                picker.widthAnchor.constraint(equalToConstant: 620),
            ])
            loadedGitPicker = picker
        }
        picker.reset(branches: bridge.gitBranches(workspace: workspace))
        picker.isHidden = false
        window?.makeFirstResponder(picker)
    }

    private func closeGitBranchPicker() {
        loadedGitPicker?.isHidden = true
        window?.makeFirstResponder(activeTab == nil ? explorer.outlineView : editorView)
    }

    private func checkoutGitBranch(_ name: String) {
        guard let workspace else { return }
        guard bridge.gitCheckout(workspace: workspace, name: name) else {
            statusLabel.stringValue = "Could not checkout \(name)."
            return
        }
        closeGitBranchPicker()
        refreshGitStatus()
        statusLabel.stringValue = ""
    }

    private func createGitBranch() {
        let alert = NSAlert()
        alert.messageText = "Create new branch"
        let field = NSTextField(string: "")
        field.placeholderString = "Branch name"
        field.frame = NSRect(x: 0, y: 0, width: 320, height: 24)
        alert.accessoryView = field
        alert.addButton(withTitle: "Create")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn, let workspace else { return }
        guard bridge.gitCreateBranch(workspace: workspace, name: field.stringValue) else {
            statusLabel.stringValue = "Could not create branch."
            return
        }
        closeGitBranchPicker()
        refreshGitStatus()
        statusLabel.stringValue = ""
    }

    private func refreshGitDiff() {
        guard let workspace, let tab = activeTab, let path = tab.relPath else {
            editorView.gitLineChanges = [:]
            return
        }
        let text = tab.doc.text()
        let wantsHunks = tab.diffBase != nil
        DispatchQueue.global(qos: .utility).async { [weak self, workspace, path, text] in
            let changes = (try? workspace.gitDiffLines(relPath: path, text: text)) ?? []
            let mapped = Self.gitLineMarkers(changes)
            let map = wantsHunks ? DiffLineMap((try? workspace.gitDiffHunks(relPath: path, text: text)) ?? []) : nil
            Task { @MainActor [weak self] in
                guard let self, self.activeTab?.relPath == path else { return }
                self.editorView.gitLineChanges = mapped
                if let map, self.isDiffPaneShown { self.applyDiff(map) }
            }
        }
    }

    // MARK: Diff tab

    /// Opens `relPath` beside its committed version (left, read-only) and the
    /// working copy (right, editable). A file with no committed version opens
    /// as a normal tab. A tab already open for the file turns into the diff.
    func openWorkspaceDiff(_ workspace: Workspace, relPath: String) {
        let name = (relPath as NSString).lastPathComponent
        guard let base = (try? workspace.gitBaseDocument(relPath: relPath)) ?? nil else {
            return openWorkspaceFile(workspace, FileEntry(name: name, relPath: relPath, isDir: false), preview: false)
        }
        diffRevealPending = true
        let path = URL(fileURLWithPath: workspace.root()).appendingPathComponent(relPath).path
        if let i = tabIndex(forPath: path) {
            tabs[i].isPreview = false
            tabs[i].diffBase = base
            if activeIndex == i {
                applyDiffPane(for: tabs[i])
                refreshGitDiff()
                refreshTabBar()
            } else {
                selectTab(i)
            }
            window?.makeFirstResponder(editorView)
            return
        }
        do {
            let doc = try workspace.openFile(relPath: relPath)
            let tab = EditorTab(
                doc: doc, title: name,
                pathComponents: relPath.split(separator: "/").map(String.init),
                windowTitle: "\(name) — \(workspace.name())", isPreview: false, status: "")
            tab.fileKey = Self.fileKey(doc.path())
            tab.relPath = relPath
            tab.diffBase = base
            add(tab, replacing: slotForOpening(preview: false))
            window?.makeFirstResponder(editorView)
        } catch {
            report(error, while: "open \(relPath)")
        }
    }

    /// Shows the committed text on the left for a diff tab; restores the
    /// plain editor (or the user's own split) for any other tab.
    private func applyDiffPane(for tab: EditorTab) {
        editorView.lineTints = [:]
        secondaryEditorView.lineTints = [:]
        guard let base = tab.diffBase else {
            if isDiffPaneShown { leaveDiffPane() }
            secondaryEditorView.document = isEditorSplit ? tab.doc : nil
            return
        }
        if !isDiffPaneShown {
            isDiffPaneShown = true
            markdownPreview.removeFromSuperview()
            secondaryScrollView.removeFromSuperview()
            editorSplitView.isVertical = true
            editorSplitView.addSubview(secondaryScrollView, positioned: .below, relativeTo: scrollView)
        }
        secondaryEditorView.isReadOnly = true
        secondaryEditorView.document = base
        editorSplitView.layoutSubtreeIfNeeded()
        // A pane added to a split view starts with no width; give each side half.
        if secondaryScrollView.frame.width < 1 {
            editorSplitView.setPosition(editorSplitView.bounds.width / 2, ofDividerAt: 0)
        }
        editorSplitView.adjustSubviews()
        secondaryEditorView.requestDocumentRefresh(scrollCaret: false)
    }

    private func leaveDiffPane() {
        isDiffPaneShown = false
        diffMap = DiffLineMap()
        secondaryEditorView.isReadOnly = false
        secondaryEditorView.lineTints = [:]
        editorView.lineTints = [:]
        secondaryScrollView.removeFromSuperview()
        editorSplitView.isVertical = isEditorSplit ? userSplitVertical : true
        if isEditorSplit { editorSplitView.addSubview(secondaryScrollView) }
        editorSplitView.adjustSubviews()
    }

    /// Tints the changed lines of both panes and, the first time, scrolls to
    /// the first change.
    private func applyDiff(_ map: DiffLineMap) {
        diffMap = map
        editorView.lineTints = Dictionary(map.newChangedLines.map { ($0, Theme.diffAdded) }, uniquingKeysWith: { a, _ in a })
        secondaryEditorView.lineTints = Dictionary(map.oldChangedLines.map { ($0, Theme.diffRemoved) }, uniquingKeysWith: { a, _ in a })
        guard diffRevealPending else { return }
        diffRevealPending = false
        let lineHeight = editorView.metrics.lineHeight
        let first = max(0, (map.hunks.first?.newStart ?? 0) - 3)
        scrollView.contentView.scroll(to: NSPoint(x: 0, y: CGFloat(first) * lineHeight - scrollView.contentInsets.top))
        scrollView.reflectScrolledClipView(scrollView.contentView)
        syncDiffScroll(fromPrimary: true)
    }

    /// Keeps both panes on the same lines while either one scrolls.
    private func installDiffScrollSync() {
        for (view, isPrimary) in [(scrollView, true), (secondaryScrollView, false)] {
            view.contentView.postsBoundsChangedNotifications = true
            NotificationCenter.default.addObserver(
                forName: NSView.boundsDidChangeNotification, object: view.contentView, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.syncDiffScroll(fromPrimary: isPrimary)
                    if isPrimary { self?.syncPreviewToEditor() }
                }
            }
        }
    }

    private func syncDiffScroll(fromPrimary: Bool) {
        guard isDiffPaneShown, !isSyncingDiffScroll else { return }
        let source = fromPrimary ? scrollView : secondaryScrollView
        let target = fromPrimary ? secondaryScrollView : scrollView
        let lineHeight = editorView.metrics.lineHeight
        guard lineHeight > 0 else { return }
        let inset = scrollView.contentInsets.top
        let y = max(0, source.contentView.bounds.origin.y + inset)
        let line = Int(y / lineHeight)
        let fraction = y - CGFloat(line) * lineHeight
        let mapped = fromPrimary ? diffMap.oldLine(forNew: line) : diffMap.newLine(forOld: line)
        isSyncingDiffScroll = true
        defer { isSyncingDiffScroll = false }
        target.contentView.scroll(to: NSPoint(x: 0, y: CGFloat(mapped) * lineHeight + fraction - inset))
        target.reflectScrolledClipView(target.contentView)
    }

    /// Moves the Markdown preview to the source line at the top of the editor.
    private func syncPreviewToEditor() {
        guard isMarkdownPreviewShown, !isSyncingPreviewScroll else { return }
        let lineHeight = editorView.metrics.lineHeight
        guard lineHeight > 0 else { return }
        let y = max(0, scrollView.contentView.bounds.origin.y + scrollView.contentInsets.top)
        markdownPreview.scroll(toSourceLine: Double(y / lineHeight))
    }

    /// Moves the editor to `line`, the source line at the top of the preview.
    private func syncEditorToPreview(line: Double) {
        let lineHeight = editorView.metrics.lineHeight
        guard isMarkdownPreviewShown, lineHeight > 0 else { return }
        let clip = scrollView.contentView
        let wanted = NSPoint(x: clip.bounds.origin.x, y: CGFloat(line) * lineHeight - scrollView.contentInsets.top)
        let target = clip.constrainBoundsRect(NSRect(origin: wanted, size: clip.bounds.size)).origin
        isSyncingPreviewScroll = true
        defer { isSyncingPreviewScroll = false }
        clip.scroll(to: target)
        scrollView.reflectScrolledClipView(clip)
    }

    /// Pure helper called from the utility queue in `refreshGitDiff()`, so it
    /// must not inherit the controller's main-actor isolation.
    nonisolated static func gitLineMarkers(_ changes: [GitLineChangeInfo]) -> [UInt64: GitLineChangeKind] {
        changes.reduce(into: [:]) { markers, change in
            guard let current = markers[change.line] else {
                markers[change.line] = change.kind
                return
            }
            let priority: [GitLineChangeKind: Int] = [.added: 0, .modified: 1, .deleted: 2]
            if priority[change.kind, default: 0] > priority[current, default: 0] {
                markers[change.line] = change.kind
            }
        }
    }

    /// Starts an empty "Untitled" buffer in a new tab, ready for typing.
    @objc func newDocument(_ sender: Any?) {
        let tab = EditorTab(doc: bridge.newDocument(), title: "Untitled", pathComponents: ["Untitled"],
                            windowTitle: "Untitled", isPreview: false,
                            status: "Untitled. Type here, or press ⌘O to open a file or folder.")
        add(tab, replacing: nil)
    }

    @objc func restartLanguageServer(_ sender: Any?) {
        guard let tab = activeTab, let workspace else { NSSound.beep(); return }
        let language = tab.doc.syntaxLanguage() ?? StatusBar.language(for: tab.doc.path())
        guard bridge.restartLSP(document: tab.doc, language: language, root: workspace.root()) else {
            statusLabel.stringValue = "Language server could not be restarted."
            return
        }
        statusLabel.stringValue = "Language server restarting."
    }

    @objc func stopLanguageServer(_ sender: Any?) {
        guard let document = activeTab?.doc, bridge.stopLSP(document: document) else {
            statusLabel.stringValue = "No language server is running."
            return
        }
        statusLabel.stringValue = "Language server stopped."
    }

    // MARK: Tabs (M2-06)

    /// Shows tab `index` in the editor. Coming from the tab bar, the
    /// explorer selection is cleared so clicking that file again opens it.
    func selectTab(_ index: Int, fromTabBar: Bool = false) {
        guard tabs.indices.contains(index) else { return }
        if activeIndex != index {
            snippetSession = nil
            snippetDocumentKey = nil
        }
        if let old = activeTab, old !== tabs[index] {
            recordFindOrigin()
            noteLeaving(old)
            if autoSaveMode == .onFocusChange { autoSave(tab: old) }
        }
        if let old = activeTab {
            old.scrollY = scrollView.contentView.bounds.origin.y
            old.status = statusLabel.stringValue
        }
        activeIndex = index
        let tab = tabs[index]
        refreshGitDiff()
        refreshBlame()
        tab.lastActiveAt = .now
        _ = tab.doc.resumeSyntax()
        let id = ObjectIdentifier(tab)
        tabOrderByUse.removeAll { $0 == id }
        tabOrderByUse.insert(id, at: 0)
        pathBar.show(tab.pathComponents)
        editorView.document = tab.doc
        applyDiffPane(for: tab)
        updateMarkdownPreview()
        let root = workspace?.root() ?? (tab.doc.path() as NSString).deletingLastPathComponent
        if tab.isDocument {
            _ = bridge.startLSP(document: tab.doc, language: tab.doc.syntaxLanguage() ?? StatusBar.language(for: tab.doc.path()), root: root)
        }
        if let y = tab.scrollY {
            scrollView.contentView.scroll(to: NSPoint(x: 0, y: y))
            scrollView.reflectScrolledClipView(scrollView.contentView)
        } else {
            scrollToTop()
        }
        statusLabel.stringValue = tab.status
        statusBar.show(document: tab.doc, path: tab.doc.path())
        if let rel = tab.relPath { noteRecent(rel) }
        refreshTabBar()
        updateEditedState()
        if let rel = tab.relPath {
            explorer.reveal(relPath: rel)
        }
        showEditorTerminal(for: tab)
        showEditorGitGraph(for: tab)
        if tab.terminal != nil {
            if isFindVisible { closeFind(nil) }
            window?.makeFirstResponder(editorTerminalView)
        } else if tab.isGitGraph {
            if isFindVisible { closeFind(nil) }
            gitGraphPanel.focus()
        } else if isFindVisible {
            applyFind(select: false)
        } else {
            window?.makeFirstResponder(editorView)
        }
        resolveDiskConflictIfNeeded()
    }

    /// Releases parser workers/trees for tabs that stayed hidden long enough.
    /// Called by the timer and directly by tests with a deterministic clock.
    func evictInactiveSyntax(now: Date) {
        evictInactiveSyntax(now: now, includeRecentlyInactive: false)
    }

    private func evictInactiveSyntax(now: Date, includeRecentlyInactive: Bool) {
        for tab in tabs where tab !== activeTab && (includeRecentlyInactive || now.timeIntervalSince(tab.lastActiveAt) >= Self.syntaxEvictionInterval) {
            _ = tab.doc.suspendSyntax()
        }
    }

    // MARK: Changes on disk (M2-03)

    /// Compares open files with the disk: those under `changed` (paths
    /// relative to the workspace, from the watcher), or all of them when
    /// `nil`. Unedited tabs reload; edited ones are asked about when shown;
    /// deleted ones keep their text and are marked "(deleted)".
    func checkOpenFiles(changed: [String]? = nil) {
        let touched: (EditorTab) -> Bool = { tab in
            guard let changed else { return true }
            guard let rel = tab.relPath else { return false }
            return changed.contains { rel == $0 || rel.hasPrefix($0 + "/") }
        }
        var tabBarChanged = false
        for tab in tabs where tab.fileKey != nil && touched(tab) {
            let wasDeleted = tab.isDeletedOnDisk
            switch tab.doc.checkDisk() {
            case .unchanged:
                tab.isDeletedOnDisk = false
            case .deleted:
                tab.isDeletedOnDisk = true
                tab.hasDiskConflict = false
            case .changed:
                tab.isDeletedOnDisk = false
                if tab === activeTab { editorView.flushPendingUpdates() }
                if tab.doc.isModified() {
                    tab.hasDiskConflict = true
                } else {
                    reloadFromDisk(tab)
                }
            }
            let edited = tab.doc.isModified()
            if wasDeleted != tab.isDeletedOnDisk || edited != tab.isEdited {
                tab.isEdited = edited
                tabBarChanged = true
            }
        }
        if tabBarChanged { refreshTabBar() }
        updateEditedState()
        resolveDiskConflictIfNeeded()
    }

    /// Asks about the active tab's conflict, once the window is key.
    private func resolveDiskConflictIfNeeded() {
        guard let tab = activeTab, tab.hasDiskConflict, !isResolvingDiskConflict,
              !promptsOnlyWhenKey || window?.isKeyWindow == true else { return }
        isResolvingDiskConflict = true
        defer { isResolvingDiskConflict = false }
        tab.hasDiskConflict = false
        switch askDiskConflict(tab.title, false) {
        case .reload: reloadFromDisk(tab)
        case .keepMine: tab.doc.keepBufferOverDisk()
        }
        updateEditedState()
    }

    /// Reloads `tab` from disk as one undoable step.
    private func reloadFromDisk(_ tab: EditorTab) {
        do {
            _ = try tab.doc.reloadFromDisk()
        } catch {
            return report(error, while: "reload \(tab.title)")
        }
        tab.hasDiskConflict = false
        tab.isDeletedOnDisk = false
        tab.isEdited = tab.doc.isModified()
        let message = "\(tab.title): reloaded from disk. ⌘Z restores the previous text."
        if tab === activeTab {
            editorView.requestDocumentRefresh(scrollCaret: false)
            editorView.scheduleChangeNotification()
            statusBar.show(document: tab.doc, path: tab.doc.path())
            statusLabel.stringValue = message
        } else {
            tab.status = message
        }
        refreshTabBar()
    }

    private static let fullScreenKey = "MainWindowFullScreen"

    /// Remembered so the next launch opens full screen again, as it was left.
    func windowDidEnterFullScreen(_ notification: Notification) {
        UserDefaults.standard.set(true, forKey: Self.fullScreenKey)
    }

    func windowDidExitFullScreen(_ notification: Notification) {
        UserDefaults.standard.set(false, forKey: Self.fullScreenKey)
        centerTrafficLights()
    }

    func windowDidResize(_ notification: Notification) { centerTrafficLights() }

    /// The system title bar is made as tall as `TitleBar`, and the close,
    /// minimise and zoom buttons are centred in it.
    func centerTrafficLights() {
        guard let window, !window.styleMask.contains(.fullScreen),
              let close = window.standardWindowButton(.closeButton),
              let container = close.superview?.superview else { return }
        let height = TitleBar.minimumHeight
        let frame = NSRect(x: 0, y: window.frame.height - height, width: window.frame.width, height: height)
        if container.frame != frame { container.frame = frame }
        for kind in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
            guard let button = window.standardWindowButton(kind), let bar = button.superview else { continue }
            let y = ((bar.bounds.height - button.frame.height) / 2).rounded()
            if button.frame.origin.y != y { button.setFrameOrigin(NSPoint(x: button.frame.origin.x, y: y)) }
        }
    }

    func windowDidBecomeKey(_ notification: Notification) {
        centerTrafficLights()
        // Files outside the workspace are not watched; this catches them,
        // and a conflict that waited for the window.
        checkOpenFiles()
        reloadKeybindings()
    }

    func windowDidResignKey(_ notification: Notification) {
        guard autoSaveMode == .onFocusChange else { return }
        autoSaveModifiedDocuments()
    }

    private func scheduleAutoSave() {
        guard autoSaveMode == .afterDelay else { return }
        autoSaveWorkItem?.cancel()
        let item = DispatchWorkItem { [weak self] in
            Task { @MainActor [weak self] in self?.autoSaveModifiedDocuments() }
        }
        autoSaveWorkItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + autoSaveDelay, execute: item)
    }

    private func autoSaveModifiedDocuments() {
        autoSaveWorkItem = nil
        editorView.flushPendingUpdates()
        let selected = activeIndex
        for index in tabs.indices where tabs[index].doc.isModified() && !tabs[index].doc.path().isEmpty {
            autoSave(tab: tabs[index])
        }
        if let selected, tabs.indices.contains(selected), activeIndex != selected { selectTab(selected) }
    }

    private func autoSave(tab: EditorTab) {
        guard tab.doc.isModified(), !tab.doc.path().isEmpty else { return }
        do {
            try tab.doc.save()
            bridge.saveLSP(document: tab.doc)
            tab.isEdited = false
        } catch {
            Log.ui.error("auto-save failed for \(tab.doc.path(), privacy: .public): \(String(describing: error), privacy: .public)")
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
    /// Closing a terminal tab kills its shell, as in VS Code, unless
    /// `keepTerminal` (moved back to the panel, or already exited).
    @discardableResult
    func closeTab(at index: Int, keepTerminal: Bool = false) -> Bool {
        guard tabs.indices.contains(index) else { return false }
        if let handle = tabs[index].terminal {
            if activeIndex == index { showEditorTerminal(for: nil) }
            if !keepTerminal { handle.kill() }
        }
        if tabs[index].isGitGraph, activeIndex == index { showEditorGitGraph(for: nil) }
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
        tabBar.setPreviewButton(visible: canPreviewMarkdown, active: isMarkdownPreviewShown)
        let statusMap = explorer.gitStatusMap
        let gitPaths = explorer.gitChangedPaths
        tabBar.set(items: tabs.map { tab in
            let rel = tab.relPath
            var status = rel.flatMap { statusMap[$0] } ?? .none
            if status == .none && tab.isDeletedOnDisk {
                status = .deleted
            }
            let isChanged = rel.map(gitPaths.contains) ?? (status != .none)
            return TabBar.Item(
                title: tab.displayTitle,
                isPreview: tab.isPreview,
                isEdited: tab.isEdited,
                isGitChanged: isChanged,
                gitStatus: status,
                isTerminal: tab.terminal != nil,
                glyphName: tab.diffBase == nil ? nil : tab.title,
                isGitGraph: tab.isGitGraph
            )
        },
                   selected: activeIndex)
        statusBar.setGitGraphVisible(isGitGraphTabShown)
    }

    /// Adds `tab` after the active one, or puts it in place of tab
    /// `replacing`, and shows it.
    private func applyTagEditing(to tab: EditorTab) {
        bridge.setTagEditing(document: tab.doc, autoClose: autoClosingTags, autoRename: linkedEditing)
    }

    private func add(_ tab: EditorTab, replacing: Int?) {
        applyTagEditing(to: tab)
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
        guard let i = activeIndex, tabs[i].isDocument else { return nil }
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
        showEditorTerminal(for: nil)
        showEditorGitGraph(for: nil)
        if isDiffPaneShown { leaveDiffPane() }
        editorView.document = nil
        updateMarkdownPreview()
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
            panel.topAnchor.constraint(equalTo: titleBar.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: titleBar.centerXAnchor),
            panel.widthAnchor.constraint(lessThanOrEqualTo: titleBar.widthAnchor, constant: -24),
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
        let shortcuts = keybindings.shortcutTexts
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

    /// Closes Go to File, Go to Line, Go to Symbol and the palette, whichever is open.
    private func closeOverlays() {
        if isWorkspaceSearchVisible { closeWorkspaceSearch() }
        if isQuickOpenVisible { closeGoToFile() }
        if isOutlinePickerVisible { closeGoToSymbol() }
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

    // MARK: Extensions (language server plugins)

    /// Shows what an install will run and asks before anything executes.
    /// Tests replace it.
    var askConfirmPluginInstall: (_ preview: PluginPreview) -> Bool = { preview in
        let alert = NSAlert()
        let verb = preview.previousCommit == nil ? "Install" : "Update"
        alert.messageText = "\(verb) \"\(preview.name)\" \(preview.version)?"
        var lines = [
            "Source: \(preview.source)",
            "Commit: \(preview.commit.prefix(12))" + (preview.verified ? " (checksum verified)" : ""),
            "Server: \(preview.server)",
            "",
        ]
        if preview.commands.isEmpty {
            lines.append("No install commands.")
        } else {
            lines.append("These commands run with your permissions:")
            lines += preview.commands.map { "  \($0)" }
        }
        lines += ["", "Only continue if you trust this source."]
        alert.informativeText = lines.joined(separator: "\n")
        alert.addButton(withTitle: verb)
        alert.addButton(withTitle: "Cancel")
        return alert.runModal() == .alertFirstButtonReturn
    }

    var askConfirmPluginUninstall: (_ id: String) -> Bool = { id in
        let alert = NSAlert()
        alert.messageText = "Uninstall \"\(id)\"?"
        alert.informativeText = "Its folder is deleted. Built-in plugins are not affected."
        alert.addButton(withTitle: "Uninstall")
        alert.addButton(withTitle: "Cancel")
        return alert.runModal() == .alertFirstButtonReturn
    }

    var askRegistryURL: () -> String? = {
        let alert = NSAlert()
        alert.messageText = "Add Plugin Registry"
        alert.informativeText = "A git URL (URL#REF to pin a branch or tag) or an absolute folder containing index.json."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 320, height: 24))
        field.placeholderString = "https://host/plugins.git#main"
        alert.accessoryView = field
        alert.addButton(withTitle: "Add")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        return alert.runModal() == .alertFirstButtonReturn ? field.stringValue : nil
    }

    /// Reloads the list from disk; with `network`, also reads registries and
    /// checks installed plugins for newer commits.
    func refreshExtensions(network: Bool) {
        extensions.show(plugins: pluginList(), hasRegistries: !pluginRegistries().isEmpty)
        guard network else { return }
        extensionsGeneration += 1
        let generation = extensionsGeneration
        extensions.setStatus("Checking registries and updates…")
        Task.detached(priority: .utility) { [weak self] in
            let listing = pluginRegistryEntries()
            var updates: [String: String] = [:]
            for plugin in pluginList() where !plugin.builtin {
                if let latest = try? pluginCheckUpdate(id: plugin.id) { updates[plugin.id] = latest }
            }
            await MainActor.run {
                guard let self, self.extensionsGeneration == generation else { return }
                self.extensions.show(plugins: pluginList(), available: listing.entries,
                                     registryErrors: listing.errors, hasRegistries: !pluginRegistries().isEmpty,
                                     updates: updates)
                self.extensions.setStatus(updates.isEmpty ? "Up to date." : "\(updates.count) update(s) available.")
            }
        }
    }

    func installPlugin(spec: String, sha256: String?) {
        extensions.setStatus("Fetching \(spec)…")
        Task.detached(priority: .userInitiated) { [weak self] in
            let result = Result { try pluginFetch(spec: spec, sha256: sha256) }
            await MainActor.run {
                guard let self else { return }
                switch result {
                case .failure(let error):
                    self.extensions.setStatus("Could not fetch: \(Self.describe(error))")
                case .success(let preview):
                    guard self.askConfirmPluginInstall(preview) else {
                        Task.detached { try? pluginDiscard(token: preview.token) }
                        self.extensions.setStatus("Cancelled.")
                        return
                    }
                    self.extensions.setStatus("Installing \(preview.name)…")
                    Task.detached(priority: .userInitiated) { [weak self] in
                        let installed = Result { try pluginInstall(token: preview.token) }
                        await MainActor.run {
                            guard let self else { return }
                            switch installed {
                            case .success:
                                self.extensions.setStatus("Installed \(preview.name) \(preview.version).")
                                self.pluginsChanged()
                            case .failure(let error):
                                self.extensions.setStatus("Install failed: \(Self.describe(error))")
                                if let window = self.window {
                                    let alert = NSAlert()
                                    alert.messageText = "Could not install \(preview.name)"
                                    alert.informativeText = Self.describe(error)
                                    alert.beginSheetModal(for: window)
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    func uninstallPlugin(id: String) {
        guard askConfirmPluginUninstall(id) else { return }
        do {
            try pluginUninstall(id: id)
            extensions.setStatus("Uninstalled \(id).")
            pluginsChanged()
        } catch {
            extensions.setStatus("Uninstall failed: \(Self.describe(error))")
        }
    }

    func setPluginEnabled(id: String, enabled: Bool) {
        do {
            try pluginSetEnabled(id: id, enabled: enabled)
            extensions.setStatus("\(enabled ? "Enabled" : "Disabled") \(id).")
            pluginsChanged()
        } catch {
            extensions.setStatus("Could not change \(id): \(Self.describe(error))")
        }
    }

    func addPluginRegistry() {
        guard let url = askRegistryURL()?.trimmingCharacters(in: .whitespacesAndNewlines), !url.isEmpty else { return }
        do {
            try pluginAddRegistry(url: url)
            refreshExtensions(network: true)
        } catch {
            extensions.setStatus("Could not add registry: \(Self.describe(error))")
        }
    }

    /// Refreshes the list and restarts the active document's server so it
    /// runs the plugin set that is now installed.
    private func pluginsChanged() {
        refreshExtensions(network: false)
        guard activeTab != nil, workspace != nil else { return }
        restartLanguageServer(nil)
    }

    private static func describe(_ error: Error) -> String {
        if case CoreError.Io(let message) = error { return message }
        return String(describing: error)
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
            // The watcher reports it too, a moment later.
            updateFileFinder(changed: [rel])
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

    @objc func copyExplorerRelativePath(_ sender: Any?) {
        guard workspace != nil else { return NSSound.beep() }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(explorer.selectedEntry?.relPath ?? ".", forType: .string)
    }

    // MARK: Rename and delete (M2-05)

    /// Asks before moving `name` to the Trash. Tests replace it.
    var askConfirmDelete: (_ name: String, _ isFolder: Bool) -> Bool = { name, isFolder in
        let alert = NSAlert()
        alert.messageText = "Move \"\(name)\" to the Trash?"
        alert.informativeText = isFolder
            ? "The folder and everything in it can be restored from the Trash."
            : "You can restore it from the Trash."
        alert.addButton(withTitle: "Move to Trash")
        alert.addButton(withTitle: "Cancel")
        return alert.runModal() == .alertFirstButtonReturn
    }

    /// Moves a file to the Trash. Tests replace it so they do not fill the
    /// user's Trash.
    var moveToTrash: (URL) throws -> Void = { url in
        try FileManager.default.trashItem(at: url, resultingItemURL: nil)
    }

    /// F2 / Return in the explorer: rename the selected row in place.
    @objc func renameExplorerItem(_ sender: Any?) {
        guard let entry = explorer.selectedEntry else { return NSSound.beep() }
        setSidebarVisible(true)
        explorer.beginRename(relPath: entry.relPath)
    }

    /// ⌘⌫ in the explorer: move the selected row to the Trash.
    @objc func deleteExplorerItem(_ sender: Any?) {
        guard let workspace, let entry = explorer.selectedEntry else { return NSSound.beep() }
        guard askConfirmDelete(entry.name, entry.isDir) else { return }
        let url = URL(fileURLWithPath: workspace.root()).appendingPathComponent(entry.relPath)
        do {
            try moveToTrash(url)
        } catch {
            return report(error, while: "move \(entry.name) to the Trash")
        }
        Log.ui.info("moved \(entry.relPath, privacy: .public) to the Trash")
        // Clean tabs of what was deleted close; edited ones stay, marked
        // "(deleted)", so nothing typed is lost.
        let under = { (tab: EditorTab) in
            tab.relPath.map { $0 == entry.relPath || $0.hasPrefix(entry.relPath + "/") } ?? false
        }
        for tab in tabs.reversed() where under(tab) {
            guard let i = tabs.firstIndex(where: { $0 === tab }) else { continue }
            if tab.doc.isModified() {
                tab.isDeletedOnDisk = true
                _ = tab.doc.checkDisk()
            } else {
                closeTab(at: i)
            }
        }
        recentFiles.removeAll { $0 == entry.relPath || $0.hasPrefix(entry.relPath + "/") }
        explorer.refresh()
        refreshTabBar()
        updateEditedState()
        updateFileFinder(changed: [entry.relPath])
    }

    /// Renames `entry` to `name` (within its folder) and follows it in the
    /// open tabs, the recent list and the Go to File index.
    func renameEntry(_ entry: FileEntry, to name: String) {
        guard let workspace else { return }
        let newRel: String
        do {
            newRel = try workspace.renameEntry(relPath: entry.relPath, newName: name)
        } catch {
            return report(error, while: "rename \(entry.name)")
        }
        guard newRel != entry.relPath else { return }
        let root = URL(fileURLWithPath: workspace.root())
        let moved = { (rel: String) -> String? in
            if rel == entry.relPath { return newRel }
            if rel.hasPrefix(entry.relPath + "/") { return newRel + rel.dropFirst(entry.relPath.count) }
            return nil
        }
        for tab in tabs {
            guard let old = tab.relPath, let rel = moved(old) else { continue }
            let path = root.appendingPathComponent(rel).path
            navHistory.forget(key: tab.navKey)
            tab.doc.setPath(path: path)
            tab.relPath = rel
            tab.fileKey = Self.fileKey(path)
            tab.title = (rel as NSString).lastPathComponent
            tab.windowTitle = "\(tab.title) — \(workspace.name())"
            tab.pathComponents = rel.split(separator: "/").map(String.init)
            tab.isDeletedOnDisk = false
            if tab === activeTab { pathBar.show(tab.pathComponents) }
        }
        recentFiles = recentFiles.map { moved($0) ?? $0 }
        explorer.refresh()
        explorer.reveal(relPath: newRel)
        refreshTabBar()
        updateEditedState()
        updateFileFinder(changed: [entry.relPath, newRel])
        statusLabel.stringValue = "Renamed \(entry.relPath) to \(newRel)."
    }

    /// Return renames, ⌘⌫ deletes, as in VSCode on macOS.
    private func explorerKey(_ event: NSEvent) -> Bool {
        let mods = event.modifierFlags.intersection([.command, .shift, .control, .option])
        guard explorer.selectedEntry != nil else { return false }
        switch (event.keyCode, mods) {
        case (36, []), (76, []): // Return, keypad Enter
            renameExplorerItem(nil)
            return true
        case (51, [.command]), (117, [.command]): // ⌘⌫, ⌘⌦
            deleteExplorerItem(nil)
            return true
        default:
            return false
        }
    }

    /// Right-click menu for the selected row, or the root below the rows.
    func explorerContextMenu() -> NSMenu {
        let menu = NSMenu()
        func add(_ title: String, _ action: Selector, key: String = "", mods: NSEvent.ModifierFlags = []) {
            let item = menu.addItem(withTitle: title, action: action, keyEquivalent: key)
            item.keyEquivalentModifierMask = mods
            item.target = self
        }
        add("New File…", #selector(explorerNewFile(_:)))
        add("New Folder…", #selector(explorerNewFolder(_:)))
        menu.addItem(.separator())
        add("Reveal in Finder", #selector(revealInFinder(_:)))
        menu.addItem(.separator())
        add("Copy Path", #selector(copyExplorerPath(_:)))
        add("Copy Relative Path", #selector(copyExplorerRelativePath(_:)))
        if explorer.selectedEntry != nil {
            menu.addItem(.separator())
            add("Rename…", #selector(renameExplorerItem(_:)), key: "\r")
            add("Move to Trash", #selector(deleteExplorerItem(_:)), key: "\u{8}", mods: .command)
        }
        return menu
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
        panel.reset(lineCount: doc.lineCount(), current: doc.primaryCursor().line)
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
        refreshNavigationButtons()
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
            if show(loc) { return refreshNavigationButtons() }
            navHistory.forget(key: loc.key)
        }
        refreshNavigationButtons()
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

    // MARK: Go to Symbol (M3-10)

    var isOutlinePickerVisible: Bool { loadedOutlinePicker.map { !$0.isHidden } ?? false }

    var outlinePicker: OutlinePicker {
        if let panel = loadedOutlinePicker { return panel }
        let panel = OutlinePicker()
        panel.isHidden = true
        panel.translatesAutoresizingMaskIntoConstraints = false
        panel.onGo = { [weak self] symbol in self?.goTo(symbol) }
        panel.onQueryChange = { [weak self] query in self?.requestWorkspaceSymbols(query: query) }
        panel.onClose = { [weak self] in self?.closeGoToSymbol() }
        window?.contentView?.addSubview(panel)
        let preferred = panel.widthAnchor.constraint(equalToConstant: OutlinePicker.width)
        preferred.priority = .defaultHigh
        NSLayoutConstraint.activate([
            panel.topAnchor.constraint(equalTo: tabBar.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: detailView.centerXAnchor),
            panel.widthAnchor.constraint(lessThanOrEqualTo: detailView.widthAnchor, constant: -24),
            preferred,
        ])
        loadedOutlinePicker = panel
        return panel
    }

    /// Shift-Command-O.
    @objc func showGoToSymbol(_ sender: Any?) {
        guard let doc = editorView.document else {
            statusLabel.stringValue = "Open a file to use Go to Symbol."
            return NSSound.beep()
        }
        if isOutlinePickerVisible { return outlinePicker.move(by: 1) }
        closeOverlays()
        let panel = outlinePicker
        let fallback = doc.outline()
        panel.reset(symbols: fallback)
        workspaceSymbolRoot = workspace?.root()
        outlineRequest = doc
        if let root = workspaceSymbolRoot {
            bridge.requestWorkspaceSymbols(root: root, query: "")
        } else {
            bridge.requestDocumentSymbols(document: doc)
        }
        panel.isHidden = false
        panel.focus()
    }

    /// Command-T: searches symbols across the active workspace.
    @objc func showGoToWorkspaceSymbol(_ sender: Any?) {
        guard let root = workspace?.root() else {
            statusLabel.stringValue = "Open a workspace to use Go to Workspace Symbol."
            return NSSound.beep()
        }
        if isOutlinePickerVisible { return outlinePicker.move(by: 1) }
        closeOverlays()
        let panel = outlinePicker
        panel.reset(symbols: [])
        workspaceSymbolRoot = root
        outlineRequest = nil
        bridge.requestWorkspaceSymbols(root: root, query: "")
        panel.isHidden = false
        panel.focus()
    }

    private func refreshDocumentSymbols() {
        if let document = activeTab?.doc {
            let symbols = bridge.documentSymbols(document: document)
            documentSymbols = symbols.map { OutlineSymbol(uri: $0.uri, name: $0.name, kind: $0.kind, line: $0.line, column: $0.column) }
            let line = editorView.document?.primaryCursor().line ?? 0
            breadcrumbBar.show(symbols: documentSymbols, atLine: line)
        } else {
            documentSymbols = []
            breadcrumbBar.clear()
        }
        if let root = workspaceSymbolRoot {
            let symbols = bridge.workspaceSymbols(root: root)
            if !symbols.isEmpty {
                let mapped = symbols.map { OutlineSymbol(uri: $0.uri, name: $0.name, kind: $0.kind, line: $0.line, column: $0.column) }
                outlinePicker.reset(symbols: mapped)
            }
            return
        }
        guard let document = outlineRequest else { return }
        let symbols = bridge.documentSymbols(document: document)
        guard !symbols.isEmpty else { return }
        outlineRequest = nil
        outlinePicker.reset(symbols: symbols.map { OutlineSymbol(uri: $0.uri, name: $0.name, kind: $0.kind, line: $0.line, column: $0.column) })
    }

    private func updateStickyScope() {
        guard activeTab != nil, !documentSymbols.isEmpty else {
            stickyScopeView.show([])
            return
        }
        let lineHeight = editorView.metrics.lineHeight
        let firstVisible = UInt64(max(0, Int(floor(scrollView.contentView.bounds.origin.y / lineHeight))))
        guard firstVisible > 0 else {
            stickyScopeView.show([])
            return
        }
        let scopes = documentSymbols
            .filter { $0.line < firstVisible }
            .sorted { $0.line == $1.line ? $0.column < $1.column : $0.line < $1.line }
        stickyScopeView.show(Array(scopes.suffix(3)))
    }

    private func requestWorkspaceSymbols(query: String) {
        guard let root = workspaceSymbolRoot else { return }
        bridge.requestWorkspaceSymbols(root: root, query: query)
    }

    func closeGoToSymbol() {
        guard isOutlinePickerVisible else { return }
        outlinePicker.isHidden = true
        window?.makeFirstResponder(editorView)
    }

    private func goTo(_ symbol: OutlineSymbol) {
        closeGoToSymbol()
        if let uri = symbol.uri, let path = URL(string: uri)?.path,
           let workspace, let rel = relativePath(path) {
            openWorkspaceFile(workspace, FileEntry(name: URL(fileURLWithPath: path).lastPathComponent, relPath: rel, isDir: false), preview: false)
        }
        guard let tab = activeTab else { return }
        let from = location(of: tab)
        _ = navHistory.recordJump(
            from: from,
            to: NavLocation(key: tab.navKey, line: symbol.line, col: symbol.column))
        refreshNavigationButtons()
        move(tab, toLine: symbol.line, col: symbol.column)
    }

    /// Records where the caret was before this find session, once, if find
    /// has since moved it far away.
    private func recordFindOrigin() {
        guard let origin = findOrigin, let tab = activeTab else { return }
        findOrigin = nil
        guard origin.key == tab.navKey else { return }
        _ = navHistory.recordJump(from: origin, to: location(of: tab))
        refreshNavigationButtons()
    }

    /// Runs `move` and records where the caret was if it went far.
    private func recordingJump(_ move: () -> Void) {
        guard let tab = activeTab else { return move() }
        let from = location(of: tab)
        move()
        _ = navHistory.recordJump(from: from, to: location(of: tab))
        refreshNavigationButtons()
    }

    func location(of tab: EditorTab) -> NavLocation {
        tab.doc.caretLocation(key: tab.navKey)
    }

    /// Records the place being left, unless back/forward is moving.
    private func noteLeaving(_ tab: EditorTab) {
        guard !isNavigating else { return }
        navHistory.record(from: location(of: tab))
        refreshNavigationButtons()
    }

    /// Greys the title bar's back and forward buttons when there is nowhere to go.
    func refreshNavigationButtons() {
        titleBar.setNavigation(canGoBack: navHistory.canGoBack(), canGoForward: navHistory.canGoForward())
    }

    /// Every keyboard shortcut goes through here (M2-10), before menus
    /// and the focused view see the key. That covers keys a menu key
    /// equivalent cannot match (⌃⇧- arrives as `_`), and extra keys from
    /// `keybindings.json`.
    private func installKeyMonitor() {
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            // Only plain values cross into the main-actor closure.
            guard let id = Keybindings.keyID(for: event) else { return event }
            let windowNumber = event.windowNumber
            let handled = MainActor.assumeIsolated { () -> Bool in
                guard let self, windowNumber == self.window?.windowNumber else { return false }
                if event.keyCode == 0x6F, event.modifierFlags.intersection(.deviceIndependentFlagsMask).isEmpty {
                    self.goToDefinition(self)
                    return true
                }
                return self.performKeyBinding(id)
            }
            return handled ? nil : event
        }
    }

    // MARK: Keyboard shortcuts (M2-10)

    /// Runs the command bound to key `id`, from the focused view, if its
    /// menu item would be enabled. `false` lets the key go on as usual
    /// (typing, a text field's own ⌘A, a disabled command).
    @discardableResult
    func performKeyBinding(_ id: String) -> Bool {
        guard let command = keybindings.commandByKey[id],
              let action = CommandPaletteView.actions[command],
              let target = target(for: action) else { return false }
        let item = NSMenuItem(title: "", action: action, keyEquivalent: "")
        if let validator = target as? NSMenuItemValidation, !validator.validateMenuItem(item) { return false }
        _ = target.perform(action, with: self)
        return true
    }

    /// Where `keybindings.json` lives. Tests point it elsewhere.
    var keybindingsURL = Keybindings.defaultURL
    private(set) var keybindings = Keybindings.defaults
    private var keybindingsModified: Date?

    /// Reads `keybindings.json` again if it changed, and updates the menu.
    /// Problems are logged and shown in the status line.
    func reloadKeybindings(force: Bool = false) {
        let modified = (try? keybindingsURL.resourceValues(forKeys: [.contentModificationDateKey]))?
            .contentModificationDate
        guard force || modified != keybindingsModified else { return }
        keybindingsModified = modified
        keybindings = Keybindings.load(from: keybindingsURL)
        if let menu = NSApp.mainMenu { keybindings.apply(to: menu) }
        for problem in keybindings.problems {
            Log.ui.error("keybindings: \(problem, privacy: .public)")
        }
        if let first = keybindings.problems.first {
            let more = keybindings.problems.count > 1 ? " (+\(keybindings.problems.count - 1) more)" : ""
            statusLabel.stringValue = "Keyboard shortcuts: \(first)\(more)"
        }
    }

    /// Opens `keybindings.json`, created with every default listed in
    /// comments if it does not exist. Saving it applies it.
    @objc func openKeybindingsFile(_ sender: Any?) {
        let url = keybindingsURL
        do {
            if !FileManager.default.fileExists(atPath: url.path) {
                try FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                        withIntermediateDirectories: true)
                try keybindingsTemplate().write(to: url, atomically: true, encoding: .utf8)
            }
            try openFile(at: url)
        } catch {
            report(error, while: "open keybindings.json")
        }
    }

    private func openJSONSettings(at url: URL, action: String) {
        do {
            if !FileManager.default.fileExists(atPath: url.path) {
                try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
                try "{\n  \"editor\": {\n    \"insertSpaces\": true,\n    \"tabSize\": 4\n  }\n}\n".write(to: url, atomically: true, encoding: .utf8)
            }
            try openFile(at: url)
        } catch {
            report(error, while: action)
        }
    }

    @objc func openUserSettingsJson(_ sender: Any?) {
        openJSONSettings(at: globalSettingsURL, action: "open user settings")
    }

    @objc func openWorkspaceSettingsJson(_ sender: Any?) {
        guard let workspace else {
            statusLabel.stringValue = "Open a workspace to use workspace settings."
            return NSSound.beep()
        }
        openJSONSettings(at: GalaxySettings.workspaceURL(root: workspace.root()), action: "open workspace settings")
    }

    // MARK: Go to File (M2-07)

    var isWorkspaceSearchVisible: Bool { loadedWorkspaceSearch.map { !$0.isHidden } ?? false }

    private var workspaceSearch: WorkspaceSearch {
        if let panel = loadedWorkspaceSearch { return panel }
        let panel = WorkspaceSearch()
        panel.isHidden = true
        panel.translatesAutoresizingMaskIntoConstraints = false
        panel.onClose = { [weak self] in self?.closeWorkspaceSearch() }
        panel.onSearch = { [weak self] query, options in self?.searchWorkspace(query, options: options) }
        panel.onOpen = { [weak self] match in self?.openWorkspaceSearchMatch(match) }
        window?.contentView?.addSubview(panel)
        NSLayoutConstraint.activate([
            panel.topAnchor.constraint(equalTo: tabBar.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: detailView.centerXAnchor),
            panel.widthAnchor.constraint(equalToConstant: WorkspaceSearch.width),
            panel.widthAnchor.constraint(lessThanOrEqualTo: detailView.widthAnchor, constant: -24),
        ])
        loadedWorkspaceSearch = panel
        return panel
    }

    @objc func showWorkspaceSearch(_ sender: Any?) {
        guard workspace != nil else { NSSound.beep(); statusLabel.stringValue = "Open a folder to search the workspace."; return }
        closeOverlays()
        let panel = workspaceSearch
        panel.isHidden = false
        panel.reset()
        panel.focus()
    }

    private func searchWorkspace(_ query: String, options: WorkspaceSearchOptions) {
        guard let workspace else { return }
        searchGeneration += 1
        let generation = searchGeneration
        guard !query.isEmpty else { workspaceSearch.show([], message: "Type to search the workspace"); return }
        workspaceSearch.show([], message: "Searching…")
        Task.detached(priority: .userInitiated) { [weak self, workspace] in
            let results = (try? workspace.search(query: query, options: options, limit: 10_000)) ?? []
            await MainActor.run {
                guard let self, self.searchGeneration == generation, self.isWorkspaceSearchVisible else { return }
                self.workspaceSearch.show(results, message: results.isEmpty ? "No results" : nil)
            }
        }
    }

    private func openWorkspaceSearchMatch(_ match: WorkspaceSearchMatch) {
        closeWorkspaceSearch()
        guard let workspace else { return }
        openWorkspaceFile(workspace, FileEntry(name: (match.relPath as NSString).lastPathComponent,
                                               relPath: match.relPath, isDir: false), preview: false)
        activeTab?.doc.goToLine(line: match.line, col: match.column)
    }

    private func closeWorkspaceSearch() {
        guard isWorkspaceSearchVisible else { return }
        workspaceSearch.isHidden = true
        window?.makeFirstResponder(activeTab != nil ? editorView : explorer.outlineView)
    }

    var isQuickOpenVisible: Bool { loadedQuickOpen.map { !$0.isHidden } ?? false }

    /// Built on first ⌘P, like the find bar, to keep it out of startup.
    var quickOpen: QuickOpen {
        if let panel = loadedQuickOpen { return panel }
        let panel = QuickOpen()
        panel.isHidden = true
        panel.translatesAutoresizingMaskIntoConstraints = false
        panel.onOpen = { [weak self] match in self?.openQuickOpenMatch(match) }
        panel.onClose = { [weak self] in self?.closeGoToFile() }
        panel.onMode = { [weak self] text in self?.switchQuickAccess(to: text) }
        window?.contentView?.addSubview(panel)
        let preferred = panel.widthAnchor.constraint(equalToConstant: QuickOpen.width)
        preferred.priority = .defaultHigh
        NSLayoutConstraint.activate([
            panel.topAnchor.constraint(equalTo: titleBar.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: titleBar.centerXAnchor),
            panel.widthAnchor.constraint(lessThanOrEqualTo: titleBar.widthAnchor, constant: -24),
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
        panel.hintShortcuts = keybindings.shortcutTexts
        panel.isHidden = false
        panel.reset()
        panel.focus()
    }

    /// The title bar's search box: Go to File with the mode list, or the
    /// command palette when no folder is open.
    @objc func showQuickAccess(_ sender: Any?) {
        guard workspace != nil else { return showCommandPalette(nil) }
        if isQuickOpenVisible { return closeGoToFile() }
        showGoToFile(nil)
    }

    /// Hands the quick-access field over to the panel for its prefix:
    /// `>` commands, `@` symbols, `:` line, `%` text. The rest of the text
    /// is carried over.
    private func switchQuickAccess(to text: String) {
        guard let prefix = text.first else { return }
        let rest = String(text.dropFirst())
        closeGoToFile()
        switch prefix {
        case ">":
            showCommandPalette(nil)
            commandPalette.setQuery(rest)
            Self.caretToEnd(commandPalette.field)
        case "@":
            showGoToSymbol(nil)
            guard isOutlinePickerVisible else { return }
            outlinePicker.setQuery(rest)
            Self.caretToEnd(outlinePicker.field)
        case ":":
            showGoToLine(nil)
            guard isGoToLineVisible else { return }
            goToLine.setInput(rest)
            Self.caretToEnd(goToLine.field)
        case "%":
            showWorkspaceSearch(nil)
            guard isWorkspaceSearchVisible, !rest.isEmpty else { return }
            workspaceSearch.field.stringValue = rest
            workspaceSearch.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
            Self.caretToEnd(workspaceSearch.field)
        default: break
        }
    }

    private static func caretToEnd(_ field: NSTextField) {
        field.currentEditor()?.selectedRange = NSRange(location: field.stringValue.utf16.count, length: 0)
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

    /// Updates the Go to File index for changed paths, off the main thread.
    private func updateFileFinder(changed paths: [String], then done: (@MainActor @Sendable () -> Void)? = nil) {
        guard let workspace else { done?(); return }
        // Still being built: the walk will see the change, or a restart will.
        guard let finder = fileFinder else {
            rebuildFileFinder()
            done?()
            return
        }
        let token = finderGeneration
        Task.detached(priority: .utility) { [weak self] in
            let updated = finder.applyChanges(workspace: workspace, changed: paths)
            await MainActor.run {
                defer { done?() }
                guard let self, self.finderGeneration == token, updated, self.isQuickOpenVisible else { return }
                self.quickOpen.runSearch(keepSelection: true)
            }
        }
    }

    /// Starts watching `workspace`; batches come back on the main actor.
    private func startWatching(_ workspace: Workspace) {
        workspaceWatch?.stop()
        workspaceWatch = nil
        watchGeneration += 1
        let sink = WatchSink(controller: self, generation: watchGeneration)
        do {
            workspaceWatch = try workspace.watch(listener: sink)
        } catch {
            // Not fatal: the explorer and ⌘P still work, refreshed by hand.
            Log.ui.error("watch failed: \(String(describing: error), privacy: .public)")
        }
    }

    /// A watcher batch (M2-02): update the explorer and the file index.
    fileprivate func workspaceChanged(_ batch: WatchBatch, generation: Int) {
        guard generation == watchGeneration, workspace != nil else { return }
        if batch.rescan {
            explorer.refresh()
            checkOpenFiles()
            rebuildFileFinder()
            refreshGitStatus()
            onWorkspaceChangesApplied?(batch)
            return
        }
        explorer.refresh(changed: batch.paths)
        refreshGitStatus()
        checkOpenFiles(changed: batch.paths)
        updateFileFinder(changed: batch.paths) { [weak self] in
            self?.onWorkspaceChangesApplied?(batch)
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
        let title = activeTab?.windowTitle ?? workspace?.name() ?? "Galaxy"
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
        guard activeTab?.isDocument != false, let doc = editorView.document else { return true }
        if doc.path().isEmpty { return saveAs() }
        if doc.checkDisk() == .changed, let tab = activeTab {
            tab.hasDiskConflict = false
            if askDiskConflict(documentName, true) == .reload {
                reloadFromDisk(tab)
                updateEditedState()
                return false
            }
        }
        do {
            try doc.save()
        } catch {
            report(error, while: "save \(documentName)")
            return false
        }
        bridge.saveLSP(document: doc)
        if let tab = activeTab, tab.isDeletedOnDisk {
            tab.isDeletedOnDisk = false
            refreshTabBar()
        }
        savedStatus(doc)
        return true
    }

    @discardableResult
    func saveAs() -> Bool {
        guard activeTab?.isDocument != false, let doc = editorView.document, let url = askSaveLocation(documentName) else { return false }
        do {
            try doc.saveAs(path: url.path)
        } catch {
            report(error, while: "save \(url.lastPathComponent)")
            return false
        }
        bridge.saveLSP(document: doc)
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
        // Saving keybindings.json applies it right away.
        if Self.fileKey(doc.path()) == Self.fileKey(keybindingsURL.path) {
            reloadKeybindings(force: true)
            if keybindings.problems.isEmpty { statusLabel.stringValue = "Keyboard shortcuts updated." }
        }
        if isSettingsDocument(doc) {
            reloadSettings(force: true)
            if settingsProblems.isEmpty { statusLabel.stringValue = "Settings updated." }
        }
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

    // MARK: Session (M2-11)

    /// Where the session is saved. Tests point it elsewhere.
    var sessionURL = Session.defaultURL
    private var didCloseWindow = false
    private var isTerminatingApplication = false
    var isClosingForApplicationTermination: Bool { isTerminatingApplication }

    /// The folder, the file tabs with caret and scroll, and the active tab.
    func currentSession() -> Session {
        var session = Session()
        session.workspace = workspace?.root()
        session.sidebarVisible = isSidebarVisible
        for tab in tabs {
            let path = tab.doc.path()
            guard !path.isEmpty, !tab.isDeletedOnDisk else { continue }
            let loc = location(of: tab)
            let y = tab === activeTab ? scrollView.contentView.bounds.origin.y : tab.scrollY
            if tab === activeTab { session.active = session.tabs.count }
            session.tabs.append(Session.Tab(path: path, isPreview: tab.isPreview, line: loc.line, col: loc.col,
                                            scrollY: y.map(Double.init)))
        }
        return session
    }

    /// Saves the session; failures are logged, never shown.
    func saveSession() {
        do {
            try currentSession().save(to: sessionURL)
        } catch {
            Log.app.error("saving session failed: \(String(describing: error), privacy: .public)")
        }
    }

    private func saveWorkspaceSession() {
        guard let workspace else { return }
        do {
            try currentSession().save(to: Session.workspaceURL(for: workspace.root()))
        } catch {
            Log.app.error("saving workspace session failed: \(String(describing: error), privacy: .public)")
        }
    }

    func saveWorkspaceSessionForQuit() { saveWorkspaceSession() }

    private func restoreTabs(from session: Session) {
        var restored: [(EditorTab, CGFloat?)] = []
        var target: EditorTab?
        for (i, saved) in session.tabs.enumerated() {
            guard FileManager.default.fileExists(atPath: saved.path) else { continue }
            if let workspace, let rel = relativePath(saved.path) {
                let entry = FileEntry(name: (rel as NSString).lastPathComponent, relPath: rel, isDir: false)
                openWorkspaceFile(workspace, entry, preview: saved.isPreview)
            } else {
                try? openFile(at: URL(fileURLWithPath: saved.path))
            }
            guard let index = tabIndex(forPath: saved.path) else { continue }
            let tab = tabs[index]
            tab.doc.goToLine(line: saved.line, col: saved.col)
            restored.append((tab, saved.scrollY.map { CGFloat($0) }))
            if i == session.active { target = tab }
        }
        for (tab, y) in restored { tab.scrollY = y }
        if let current = activeTab {
            scrollView.contentView.scroll(to: NSPoint(x: 0, y: current.scrollY ?? 0))
            scrollView.reflectScrolledClipView(scrollView.contentView)
            if let target, target !== current, let i = tabs.firstIndex(where: { $0 === target }) {
                selectTab(i)
            }
            editorView.didChangeSelection()
        }
    }

    func saveSessionForQuit() {
        if !didCloseWindow {
            saveSession()
            if let window { UserDefaults.standard.set(window.styleMask.contains(.fullScreen), forKey: Self.fullScreenKey) }
        }
    }

    func prepareForApplicationQuit() { isTerminatingApplication = true }

    /// Reopens what `session` had: the folder if it still exists, and the
    /// tabs whose files still exist, with caret and scroll. Returns
    /// `false` if nothing could be restored.
    @discardableResult
    func restore(_ session: Session) -> Bool {
        isNavigating = true
        defer { isNavigating = false }
        var restoredWorkspace = false
        if let root = session.workspace {
            var isDir: ObjCBool = false
            if FileManager.default.fileExists(atPath: root, isDirectory: &isDir), isDir.boolValue {
                do {
                    try openWorkspace(at: URL(fileURLWithPath: root))
                    restoredWorkspace = true
                } catch {
                    Log.app.error("restoring workspace failed: \(String(describing: error), privacy: .public)")
                }
            }
        }
        restoreTabs(from: session)
        if restoredWorkspace { setSidebarVisible(session.sidebarVisible) }
        Log.app.info("restored session: workspace \(restoredWorkspace), \(self.tabs.count) of \(session.tabs.count) tabs")
        return restoredWorkspace || !tabs.isEmpty
    }

    func windowWillClose(_ notification: Notification) {
        autoSaveWorkItem?.cancel()
        autoSaveWorkItem = nil
        // Closing the window is an intentional fresh start. If closing the
        // last window also terminates the app, `saveSessionForQuit` sees this.
        didCloseWindow = true
        syntaxEvictionTimer?.invalidate()
        syntaxEvictionTimer = nil
        lspTimer?.invalidate()
        lspTimer = nil
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
        keyMonitor = nil
        workspaceWatch?.stop()
        workspaceWatch = nil
        onWindowClosed?()
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
        // A new folder is a new project session. Do not carry files from the
        // previous workspace into the new tab bar.
        editorView.flushPendingUpdates()
        for tab in tabs where tab.doc.isModified() {
            if activeTab !== tab { selectTab(tabs.firstIndex { $0 === tab } ?? 0) }
            guard confirmDiscardActive() else { return }
        }
        saveWorkspaceSession()
        tabs.removeAll()
        activeIndex = nil
        snippetSession = nil
        snippetDocumentKey = nil
        showNoDocument()
        let ws = try bridge.core.openWorkspace(path: url.path)
        workspace = ws
        titleBar.setProjectTitle(ws.name())
        Self.rememberProject(url.path)
        workspaceSettingsModified = nil
        fileFinder = nil
        recentFiles = []
        if isQuickOpenVisible { closeGoToFile() }
        loadedQuickOpen?.finder = nil
        // Ready by the time ⌘P is pressed in all but huge folders.
        rebuildFileFinder()
        startWatching(ws)
        refreshGitStatus()
        explorer.setWorkspace(ws)
        updateSidebarContent()
        setSidebarVisible(true)
        if tabs.isEmpty { pathBar.show([]) }
        updateEditedState()
        statusLabel.stringValue = ""
        if let session = Session.load(from: Session.workspaceURL(for: ws.root())) {
            restoreTabs(from: session)
        }
    }

    @objc func openRecentProject(_ sender: Any?) {
        guard let path = (sender as? NSMenuItem)?.representedObject as? String else { return }
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory), isDirectory.boolValue else {
            statusLabel.stringValue = "Recent project no longer exists: \(path)"
            return
        }
        open(url: URL(fileURLWithPath: path))
    }

    static func recentProjectPaths() -> [String] {
        UserDefaults.standard.stringArray(forKey: "RecentProjectPaths") ?? []
    }

    private static func rememberProject(_ path: String) {
        let key = fileKey(path) ?? path
        var paths = recentProjectPaths().filter { fileKey($0) != key }
        paths.insert(path, at: 0)
        UserDefaults.standard.set(Array(paths.prefix(10)), forKey: "RecentProjectPaths")
        MainMenu.refreshRecentProjects()
    }

    /// Opens a file in its own tab (or shows its tab if already open).
    func openFile(at url: URL) throws {
        if let i = tabIndex(forPath: url.path) {
            tabs[i].isPreview = false
            return selectTab(i)
        }
        let doc = try bridge.openDocument(path: url.path)
        let name = url.lastPathComponent
        let branch = workspace.flatMap { bridge.gitStatus(workspace: $0)?.branch }
        let tab = EditorTab(doc: doc, title: name, pathComponents: [name], windowTitle: name,
                            isPreview: false, status: describe(branch: branch))
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
            selectTab(i)
            window?.makeFirstResponder(editorView)
            return
        }
        do {
            let doc = try workspace.openFile(relPath: entry.relPath)
            let branch = bridge.gitStatus(workspace: workspace)?.branch
            let tab = EditorTab(
                doc: doc, title: entry.name,
                pathComponents: entry.relPath.split(separator: "/").map(String.init),
                windowTitle: "\(entry.name) — \(workspace.name())", isPreview: preview,
                status: describe(branch: branch))
            tab.fileKey = Self.fileKey(doc.path())
            tab.relPath = entry.relPath
            add(tab, replacing: slotForOpening(preview: preview))
            window?.makeFirstResponder(editorView)
        } catch {
            report(error, while: "open \(entry.relPath)")
        }
    }

    private func describe(branch: String?) -> String {
        // The branch is shown once in the left status-bar Git label.
        return branch == nil ? "No Git branch" : ""
    }

    private func report(_ error: Error, while action: String) {
        Log.ui.error("\(action, privacy: .public) failed: \(String(describing: error), privacy: .public)")
        if let window { NSAlert(error: error).beginSheetModal(for: window) }
    }
}

/// A rounded panel with a hairline border, clipping what it holds to its
/// corners. The sidebar and the editor area are cards on the chrome colour.
@MainActor
class CardView: NSView {
    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = Theme.cardRadius
        layer?.cornerCurve = .continuous
        layer?.masksToBounds = true
        layer?.borderWidth = 1
        refreshBorder()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        refreshBorder()
    }

    /// Dynamic colours do not follow appearance changes on a layer by themselves.
    private func refreshBorder() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.borderColor = NSColor.separatorColor.cgColor
        }
    }
}

/// Holds the two cards: draws the chrome colour in the gap between them and
/// no divider line, but keeps the gap draggable.
@MainActor
final class CardSplitView: NSSplitView {
    override var dividerThickness: CGFloat { Theme.cardGap }
    override func drawDivider(in rect: NSRect) {}

    override func draw(_ dirtyRect: NSRect) {
        Theme.chrome.setFill()
        dirtyRect.intersection(bounds).fill()
        super.draw(dirtyRect)
    }
}

/// Solid sidebar background (no vibrancy): matches the flat chrome.
@MainActor
final class SidebarView: CardView {
    override func draw(_ dirtyRect: NSRect) {
        Theme.sidebar.setFill()
        dirtyRect.intersection(bounds).fill()
    }
}

private extension NSLayoutConstraint {
    func withPriority(_ priority: NSLayoutConstraint.Priority) -> NSLayoutConstraint {
        self.priority = priority
        return self
    }
}

/// Sidebar header, as in VSCode with one folder open: the folder name in
/// caps (EXPLORER before a folder is open) and, on the right, New File,
/// New Folder, Refresh, Collapse Folders and a "…" menu. Same height as the
/// tab strip, with a rule underneath.
@MainActor
final class SidebarSectionHeader: NSView {
    enum Mode {
        case explorer
        case sourceControl
        case search
        case extensions
    }

    static var height: CGFloat { Theme.tabHeight }

    var mode: Mode = .explorer {
        didSet { updateMode() }
    }

    let label = NSTextField(labelWithString: "EXPLORER")

    // Explorer tools
    let newFileButton = SidebarSectionHeader.tool("doc.badge.plus", "New File…")
    let newFolderButton = SidebarSectionHeader.tool("folder.badge.plus", "New Folder…")
    let refreshButton = SidebarSectionHeader.tool("arrow.clockwise", "Refresh Explorer")
    let collapseButton = SidebarSectionHeader.tool("rectangle.stack.badge.minus", "Collapse Folders in Explorer")
    let moreButton = SidebarSectionHeader.tool("ellipsis", "More Actions…")
    var tools: [NSButton] { [newFileButton, newFolderButton, refreshButton, collapseButton, moreButton] }

    // Source Control tools
    let scmPullButton = SidebarSectionHeader.tool("arrow.down.to.line", "Pull")
    let scmBranchButton = SidebarSectionHeader.tool("arrow.triangle.branch", "Branches")
    let scmCommitButton = SidebarSectionHeader.tool("checkmark", "Commit")
    let scmRefreshButton = SidebarSectionHeader.tool("arrow.clockwise", "Refresh")
    let scmMoreButton = SidebarSectionHeader.tool("ellipsis", "More Actions…")
    var scmTools: [NSButton] {
        [scmPullButton, scmBranchButton, scmCommitButton, scmRefreshButton, scmMoreButton]
    }

    let searchRefreshButton = SidebarSectionHeader.tool("arrow.clockwise", "Refresh Search")
    let searchClearButton = SidebarSectionHeader.tool("line.3.horizontal.decrease.circle", "Clear Search")
    let searchReplaceButton = SidebarSectionHeader.tool("doc.badge.plus", "Toggle Replace")
    let searchFilterButton = SidebarSectionHeader.tool("line.3.horizontal.decrease", "Search Filters")
    let searchMoreButton = SidebarSectionHeader.tool("ellipsis", "More Search Actions…")
    let extensionsAddRegistryButton = SidebarSectionHeader.tool("plus", "Add Registry…")
    let extensionsRefreshButton = SidebarSectionHeader.tool("arrow.clockwise", "Refresh and Check for Updates")
    var extensionsTools: [NSButton] { [extensionsAddRegistryButton, extensionsRefreshButton] }
    var searchTools: [NSButton] { [searchRefreshButton, searchClearButton, searchReplaceButton, searchFilterButton, searchMoreButton] }

    var onNewFile: (() -> Void)?
    var onNewFolder: (() -> Void)?
    var onRefresh: (() -> Void)?
    var onCollapseAll: (() -> Void)?
    var moreMenu: (() -> NSMenu)?

    var onScmPull: (() -> Void)?
    var onScmBranch: (() -> Void)?
    var onScmCommit: (() -> Void)?
    var onScmRefresh: (() -> Void)?
    var scmMoreMenu: (() -> NSMenu)?
    var onSearchRefresh: (() -> Void)?
    var onExtensionsRefresh: (() -> Void)?
    var onExtensionsAddRegistry: (() -> Void)?
    var onSearchClear: (() -> Void)?
    var onSearchToggleReplace: (() -> Void)?
    var onSearchToggleDetails: (() -> Void)?
    var searchMoreMenu: (() -> NSMenu)?

    /// Folder name; empty shows EXPLORER.
    var title = "" {
        didSet {
            if mode == .explorer {
                label.stringValue = title.isEmpty ? "EXPLORER" : title.uppercased()
            }
            setAccessibilityLabel(title.isEmpty ? "Explorer" : title)
        }
    }

    /// The buttons only make sense with a folder open.
    var showsTools = true {
        didSet {
            if mode == .explorer {
                tools.forEach { $0.isHidden = !showsTools }
            }
        }
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

        for (button, action) in [
            (scmPullButton, #selector(scmPullAction(_:))),
            (scmBranchButton, #selector(scmBranchAction(_:))),
            (scmCommitButton, #selector(scmCommitAction(_:))),
            (scmRefreshButton, #selector(scmRefreshAction(_:))),
            (scmMoreButton, #selector(scmMoreAction(_:))),
        ] {
            button.target = self
            button.action = action
            button.isHidden = true
        }

        for (button, action) in [
            (searchRefreshButton, #selector(searchRefreshAction(_:))),
            (searchClearButton, #selector(searchClearAction(_:))),
            (searchReplaceButton, #selector(searchReplaceAction(_:))),
            (searchFilterButton, #selector(searchFilterAction(_:))),
            (searchMoreButton, #selector(searchMoreAction(_:))),
        ] {
            button.target = self
            button.action = action
            button.isHidden = true
        }

        for (button, action) in [
            (extensionsAddRegistryButton, #selector(extensionsAddRegistryAction(_:))),
            (extensionsRefreshButton, #selector(extensionsRefreshAction(_:))),
        ] {
            button.target = self
            button.action = action
            button.isHidden = true
        }

        let row = NSStackView(views: [label] + tools + scmTools + searchTools + extensionsTools)
        row.spacing = 3
        row.setCustomSpacing(6, after: label)
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: Self.height),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14).withPriority(.defaultHigh),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8).withPriority(.defaultHigh),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    private func updateMode() {
        switch mode {
        case .explorer:
            scmTools.forEach { $0.isHidden = true }
            searchTools.forEach { $0.isHidden = true }
            tools.forEach { $0.isHidden = !showsTools }
            extensionsTools.forEach { $0.isHidden = true }
            label.stringValue = title.isEmpty ? "EXPLORER" : title.uppercased()
        case .sourceControl:
            tools.forEach { $0.isHidden = true }
            scmTools.forEach { $0.isHidden = false }
            searchTools.forEach { $0.isHidden = true }
            extensionsTools.forEach { $0.isHidden = true }
            label.stringValue = "SOURCE CONTROL"
        case .search:
            tools.forEach { $0.isHidden = true }
            scmTools.forEach { $0.isHidden = true }
            searchTools.forEach { $0.isHidden = false }
            extensionsTools.forEach { $0.isHidden = true }
            label.stringValue = "SEARCH"
        case .extensions:
            tools.forEach { $0.isHidden = true }
            scmTools.forEach { $0.isHidden = true }
            searchTools.forEach { $0.isHidden = true }
            extensionsTools.forEach { $0.isHidden = false }
            label.stringValue = "EXTENSIONS"
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.separatorColor.setFill()
        NSRect(x: 0, y: 0, width: bounds.width, height: 1).intersection(dirtyRect).fill()
    }

    private static func tool(_ symbol: String, _ title: String) -> NSButton {
        let b = NSButton()
        b.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)?
            .withSymbolConfiguration(.init(pointSize: 12, weight: .regular))
        b.isBordered = false
        b.imagePosition = .imageOnly
        b.contentTintColor = .secondaryLabelColor
        b.toolTip = title
        b.setAccessibilityLabel(title)
        b.widthAnchor.constraint(equalToConstant: 20).isActive = true
        b.heightAnchor.constraint(equalToConstant: 20).isActive = true
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

    @objc private func scmPullAction(_ sender: Any?) { onScmPull?() }
    @objc private func scmBranchAction(_ sender: Any?) { onScmBranch?() }
    @objc private func scmCommitAction(_ sender: Any?) { onScmCommit?() }
    @objc private func scmRefreshAction(_ sender: Any?) { onScmRefresh?() }

    @objc private func scmMoreAction(_ sender: Any?) {
        guard let menu = scmMoreMenu?() else { return }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: scmMoreButton.bounds.maxY + 4), in: scmMoreButton)
    }
    @objc private func searchRefreshAction(_ sender: Any?) { onSearchRefresh?() }
    @objc private func extensionsRefreshAction(_ sender: Any?) { onExtensionsRefresh?() }
    @objc private func extensionsAddRegistryAction(_ sender: Any?) { onExtensionsAddRegistry?() }
    @objc private func searchClearAction(_ sender: Any?) { onSearchClear?() }
    @objc private func searchReplaceAction(_ sender: Any?) { onSearchToggleReplace?() }
    @objc private func searchFilterAction(_ sender: Any?) { onSearchToggleDetails?() }
    @objc private func searchMoreAction(_ sender: Any?) {
        guard let menu = searchMoreMenu?() else { return }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: searchMoreButton.bounds.maxY + 4), in: searchMoreButton)
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

    func splitView(_ splitView: NSSplitView, canCollapseSubview subview: NSView) -> Bool { subview === sidebar }

    /// No gap to drag while the sidebar is hidden.
    func splitView(_ splitView: NSSplitView, shouldHideDividerAt dividerIndex: Int) -> Bool {
        splitView.isSubviewCollapsed(sidebar)
    }
}

extension MainWindowController: NSMenuItemValidation {
    /// Disables menu items (and palette commands) that cannot do anything
    /// right now.
    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        let hasDocument = editorView.document != nil && activeTab?.isDocument != false
        switch item.action {
        case #selector(saveDocument(_:)), #selector(saveDocumentAs(_:)),
             #selector(showFind(_:)), #selector(showFindAndReplace(_:)),
             #selector(findNext(_:)), #selector(findPrevious(_:)), #selector(useSelectionForFind(_:)):
            return hasDocument
        case #selector(refreshExplorer(_:)), #selector(collapseExplorerFolders(_:)),
             #selector(explorerNewFile(_:)), #selector(explorerNewFolder(_:)),
             #selector(revealInFinder(_:)), #selector(copyExplorerPath(_:)),
             #selector(copyExplorerRelativePath(_:)):
            return workspace != nil
        case #selector(renameExplorerItem(_:)), #selector(deleteExplorerItem(_:)):
            return workspace != nil && explorer.selectedEntry != nil
        case #selector(closeTab(_:)):
            return true
        case #selector(moveTerminalToEditor(_:)), #selector(createTerminalEditor(_:)):
            return workspace != nil
        case #selector(moveTerminalToPanel(_:)):
            return ((item.representedObject as? EditorTab) ?? activeTab)?.terminal != nil
        case #selector(showNextTab(_:)), #selector(showPreviousTab(_:)), #selector(selectLastUsedTab(_:)):
            return tabs.count > 1
        case #selector(toggleGitBlame(_:)):
            item.state = gitBlameEnabled ? .on : .off
            return true
        case #selector(toggleMarkdownPreview(_:)): return isMarkdownPreviewOn || canPreviewMarkdown
        case #selector(navigateBack(_:)): return navHistory.canGoBack()
        case #selector(navigateForward(_:)): return navHistory.canGoForward()
        default: return true
        }
    }
}

/// Receives watcher batches on the watcher thread and hands them to the
/// window on the main actor. Holds the window weakly.
/// `@unchecked Sendable`: the state is set once in `init`; the weak
/// reference is only dereferenced on the main actor.
private final class WatchSink: WorkspaceListener, @unchecked Sendable {
    private weak var controller: MainWindowController?
    private let generation: Int

    init(controller: MainWindowController, generation: Int) {
        self.controller = controller
        self.generation = generation
    }

    func onChanges(batch: WatchBatch) {
        let generation = generation
        Task { @MainActor [weak controller] in
            controller?.workspaceChanged(batch, generation: generation)
        }
    }
}
