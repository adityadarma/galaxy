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
    let breadcrumbBar = BreadcrumbBar()
    let stickyScopeView = StickyScopeView()
    /// Right edge of the pinned rows. Pulled in by the minimap's width so the
    /// map runs the full height of the editor instead of under the rows.
    var stickyTrailing: NSLayoutConstraint!
    var documentSymbols: [OutlineSymbol] = []
    var lastDiagnosticsRevision: UInt64?
    var lastSignatureRequest: (path: String, revision: UInt64, line: UInt64, column: UInt64)?
    var rustGeneratedSourceRefresh: (root: String, id: UUID, automatic: Bool)?
    var rustCompilerRefreshWorkItem: DispatchWorkItem?
    var rustCompilerRefreshGeneration = 0
    /// The document in which the user typed since signature help was last
    /// dismissed. Help follows typing, not the caret: a click elsewhere in a
    /// call must not pop it up.
    var signatureHelpDocument: ObjectIdentifier?
    let titleBar = TitleBar()
    let activityBar = ActivityBar()
    let sourceControl = SourceControlController()
    let searchSidebar = SearchSidebarController()
    var activeSidebarTab: ActivityBar.Item = .explorer
    let tabBar = TabBar()
    /// Editor groups, left to right (or top to bottom). The tabs below are
    /// those of the active group: each group keeps its own tab list.
    var groups: [EditorGroup] = [EditorGroup()]
    var activeGroupIndex = 0
    var activeGroup: EditorGroup { groups[activeGroupIndex] }
    /// Open documents of the active group, in tab order (M2-06).
    var tabs: [EditorTab] {
        get { activeGroup.tabs }
        set { activeGroup.tabs = newValue }
    }
    var activeIndex: Int? {
        get { activeGroup.activeIndex }
        set { activeGroup.activeIndex = newValue }
    }
    var activeTab: EditorTab? { activeIndex.map { tabs[$0] } }
    /// Tabs of every group.
    var allTabs: [EditorTab] { groups.flatMap(\.tabs) }
    let groupsSplitView = EditorGroupsSplitView()
    /// The live editor stack (tab bar, path bar, editor, terminal...). It
    /// is placed over the active group's slot; other groups show a pane.
    let groupChrome = NSView()
    var chromeSlotConstraints: [NSLayoutConstraint] = []
    /// The tab the live editor currently shows (not necessarily `activeTab`
    /// while groups are being switched).
    weak var shownTab: EditorTab?
    /// Left-hand message in the status bar.
    var statusLabel: NSTextField { statusBar.message }
    let scrollView = DiagnosticScrollView()
    let secondaryScrollView = DiagnosticScrollView()
    private var loadedMarkdownPreview: MarkdownPreviewView?
    var markdownPreview: MarkdownPreviewView {
        if let preview = loadedMarkdownPreview { return preview }
        let preview = MarkdownPreviewView()
        preview.onUserScroll = { [weak self] line in self?.syncEditorToPreview(line: line) }
        loadedMarkdownPreview = preview
        return preview
    }
    /// User asked for the Markdown preview; it shows whenever the active tab is Markdown.
    var isMarkdownPreviewOn = false
    var isMarkdownPreviewShown: Bool { loadedMarkdownPreview?.superview != nil }

    func removeMarkdownPreviewIfLoaded() {
        loadedMarkdownPreview?.removeFromSuperview()
    }
    let editorSplitView = NSSplitView()
    let problemsPanel = ProblemsPanel()
    var loadedReferencesPanel: ReferencesPanel?
    var referencesPanel: ReferencesPanel {
        if let panel = loadedReferencesPanel { return panel }
        let panel = ReferencesPanel()
        panel.translatesAutoresizingMaskIntoConstraints = true
        panel.onSelect = { [weak self] location in self?.showReference(location) }
        panel.onPreview = { [weak self] location in self?.previewReference(location) }
        panel.onClose = { [weak self] in self?.closeReferencesPeek() }
        loadedReferencesPanel = panel
        return panel
    }
    let terminalPanel = TerminalPanelView()
    /// Shows the active tab's terminal when that tab is a terminal (VS Code
    /// "Move Terminal into Editor Area"). Covers the path bar and editor.
    let editorTerminalView = TerminalView()
    let outputPanel = OutputPanel()
    let portsPanel = PortsPanel()
    let gitGraphPanel = GitGraphPanel()
    /// Shows the active tab when it is a picture or a PDF.
    let mediaViewer = MediaViewerView()
    /// Covers the editor while no tab is open.
    let emptyEditor = EmptyEditorView()
    let bottomPanelHeader = BottomPanelHeader()
    let bottomPanelContent = BottomPanelContentView()
    var bottomPanelHeight: NSLayoutConstraint!
    var bottomPanelHeaderHeight: NSLayoutConstraint!
    var bottomPanelTab = BottomPanelHeader.Tab.terminal
    var isBottomPanelVisible: Bool { bottomPanelHeight.constant > 0 }
    /// Panel content height when open, and whether it fills the editor area.
    var bottomPanelOpenHeight: CGFloat = 240
    var isBottomPanelMaximized = false
    let debugConsole = DebugConsolePanel()
    let debugger = DebugCoordinator()
    var terminals: [PanelTerminal] = []
    var activeTerminalIndex = 0
    var nextTerminalGroup = 0
    /// Set by the app delegate: opens a window that takes over a terminal.
    var openTerminalInNewWindow: ((PanelTerminal, URL) -> Void)?
    /// The app delegate can activate an existing window for this workspace.
    /// `true` means the open request was handled elsewhere.
    var openExistingWorkspace: ((URL) -> Bool)?
    let hoverPopover = HoverPopover()
    let blamePopover = BlamePopover()
    /// `editor.gitBlame`, or the View > Toggle Git Blame override of it.
    var gitBlameEnabled = true
    /// Commit behind the annotated line; what the blame card shows.
    var blameCommit: BlameCommitInfo?
    var blameRefreshItem: DispatchWorkItem?
    /// `editor.inlineParameterHints`.
    var inlineParameterHintsEnabled = true
    var parameterHintsItem: DispatchWorkItem?
    /// Tests found in the shown file, and the runs started from their gutter icons.
    var detectedTests: [TestCase] = []
    var testScanItem: DispatchWorkItem?
    let testRunner = TestRunner()
    let testResultsPanel = TestResultsPanel()
    /// Last request: document, revision and lines asked for, and when.
    var parameterHintsRequest: (doc: ObjectIdentifier, revision: UInt64, lines: Range<Int>, at: Date, empty: Bool)?
    var parameterHintsGeneration = 0
    var parameterHintsInFlight = false
    /// One delayed retry after an empty answer (the engine may be indexing).
    var parameterHintsRetried = false
    var blameShowItem: DispatchWorkItem?
    var blameHideItem: DispatchWorkItem?
    /// Scroll notifications can arrive much faster than display refresh.
    /// Minimap and sticky scope updates are decorative, so coalesce them.
    var scrollDecorationUpdatePending = false
    var lastScrollDecorationUpdate = Date.distantPast
    /// Quiet time after the last edit before blame is recomputed.
    static let blameRefreshDelay: TimeInterval = 0.5
    let completionPopup = CompletionPopup()
    let signaturePopup = SignaturePopup()
    let codeActionPopup = CodeActionPopup()
    var completionQuery = ""
    var localCompletionItems: [CompletionInfo]?
    var snippetCompletionItems: [CompletionInfo] = []
    var snippetSession: SnippetSession?
    var snippetDocumentKey: String?
    var completionDismissed = false
    /// Coalesces typing bursts before asking synchronous native completion.
    var completionRequestItem: DispatchWorkItem?
    /// After Ctrl+Space the request is repeated until the server answers with
    /// something (it may still be starting or indexing), then this is cleared.
    var suggestRetryUntil: Date?
    var lastSuggestRequest = Date.distantPast
    var completionPosition = TextPos(line: 0, colUtf16: 0)
    var hoverPosition: TextPos?
    var hoverPoint = NSPoint.zero
    var hoverDocument: DocumentHandle?
    var hoverRequestPosition: TextPos?
    var hoverRequestTime = Date.distantPast
    /// Word the visible card describes (server range, single line).
    var hoverShownRange: (line: UInt64, start: UInt64, end: UInt64)?
    var hoverShownContents: String?
    var hoverShownPosition: TextPos?
    let splitView = CardSplitView()
    /// Holds the editor card and, under it, the bottom panel card. It draws
    /// nothing, so the chrome colour shows in the gap between them.
    let detailView = NSView()
    let editorCard = CardView()
    let bottomPanelCard = CardView()
    /// Space between the two cards: zero while the panel is closed.
    var bottomPanelGap: NSLayoutConstraint!
    /// Holds the find bar's place in the layout. The bar itself is built on
    /// first use (⌘F): it costs about 20 ms, a tenth of the startup budget.
    let findBarSlot = NSView()
    var loadedFindBar: FindBar?
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
    var findBarHeight: NSLayoutConstraint!
    /// Sidebar, VSCode style: "EXPLORER" view title, a collapsible
    /// section named after the folder, then the tree or an empty state.
    let sidebar = SidebarView()
    let sectionHeader = SidebarSectionHeader()
    let emptyState = NSStackView()
    let explorer = ExplorerController()
    var workspace: Workspace?
    var onWindowClosed: (() -> Void)?
    /// Go to File index of `workspace`, built in the background.
    var fileFinder: FileFinder?
    var finderGeneration = 0
    /// Watches the open folder (M2-02).
    var workspaceWatch: WorkspaceWatch?
    /// Bumped per workspace, so a late batch from the previous folder is
    /// dropped.
    var watchGeneration = 0
    /// Called after a watcher batch has been applied (tests wait on it).
    var onWorkspaceChangesApplied: ((WatchBatch) -> Void)?
    /// Called when a (re)built index is installed (tests wait on it).
    var onFileFinderReady: (() -> Void)?
    /// Files opened in this workspace, most recent first, relative paths.
    var recentFiles: [String] = []
    var loadedQuickOpen: QuickOpen?
    var loadedWorkspaceSearch: WorkspaceSearch?
    var searchGeneration = 0
    var loadedGoToLine: GoToLine?
    var loadedOutlinePicker: OutlinePicker?
    var loadedPalette: CommandPaletteView?
    var loadedGitPicker: GitBranchPicker?
    var loadedPHPVersionPicker: PHPVersionPicker?
    var loadedIndentationPicker: IndentationPicker?
    var syntaxEvictionTimer: Timer?
    var lspTimer: Timer?
    private var lastProblemsRefresh = Date.distantPast
    private var memoryPressureSource: DispatchSourceMemoryPressure?
    var globalSettingsURL = GalaxySettings.globalURL
    var settingsModified: Date?
    /// `editor.autoClosingTags` and `editor.linkedEditing`, applied to every tab.
    var autoClosingTags = true
    var linkedEditing = true
    var workspaceSettingsModified: Date?
    var settingsProblems: [String] = []
    var autoSaveMode: AutoSaveMode = .off
    var autoSaveDelay: TimeInterval = 1.0
    var autoSaveWorkItem: DispatchWorkItem?
    static let syntaxEvictionInterval: TimeInterval = 5 * 60
    /// Command registry (M2-09); kept for the window so recency survives
    /// closing the palette.
    let commands = CommandPalette()
    /// What had focus before the palette opened; commands run from there.
    weak var paletteReturnResponder: NSResponder?
    /// Back/forward history (M2-08).
    let navHistory = NavigationHistory()
    /// Set while back/forward shows a location, so that move is not
    /// itself recorded as a jump.
    var isNavigating = false
    var definitionRequest: (document: DocumentHandle, position: TextPos, deadline: Date)?
    var referencesRequest: (
        document: DocumentHandle,
        position: TextPos,
        answered: Bool,
        inFlight: Bool,
        deadline: Date,
        lastAttempt: Date
    )?
    var workspaceIndexState: (root: String, completed: UInt64, total: UInt64)?
    var referencePreviewGeneration = 0
    var referenceSnippetGeneration = 0
    var renameRequest: DocumentHandle?
    var codeActionRequest: DocumentHandle?
    var outlineRequest: DocumentHandle?
    var keyMonitor: Any?
    var overlayClickMonitor: Any?
    /// Tabs in the order they were last shown, most recent first (⌃Tab).
    var tabOrderByUse: [ObjectIdentifier] = []
    let editorView = EditorView(frame: .zero)
    let secondaryEditorView = EditorView(frame: .zero)
    var isEditorSplit: Bool { groups.count > 1 }
    /// The left pane shows the committed file of a diff tab.
    var isDiffPaneShown = false
    var diffMap = DiffLineMap()
    /// Scroll to the first change once the diff has been computed.
    var diffRevealPending = false
    var isSyncingDiffScroll = false
    /// Set while the preview drives the editor, so that does not echo back.
    var isSyncingPreviewScroll = false

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
    var isResolvingDiskConflict = false

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

    /// Panel terminals, for tests.
    var panelTerminals: [PanelTerminal] { terminals }
    var terminalPanelForTesting: TerminalPanelView { terminalPanel }
    var stickyScopeViewForTesting: StickyScopeView { stickyScopeView }
    var editorScrollViewForTesting: NSScrollView { scrollView }

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
        // The window takes files dragged in from outside the app.
        let dropView = FileDropView(frame: NSRect(x: 0, y: 0, width: 960, height: 640))
        dropView.autoresizingMask = [.width, .height]
        dropView.onDrop = { [weak self] urls in self?.open(urls: urls) }
        window.contentView = dropView
        buildContent()
        installOverlayClickMonitor()
        installMemoryPressureHandler()
        editorView.onChange = { [weak self] in
            if let document = self?.activeTab?.doc,
               var session = self?.snippetSession {
                session.noteDocumentChange(length: document.lenUtf16())
                self?.snippetSession = session
            }
            self?.updateEditedState()
            if let document = self?.activeTab?.doc { self?.refreshPanes(showing: document) }
            self?.scheduleAutoSave()
            self?.refreshFindStatus()
            self?.bridge.pollAnalysis()
            self?.requestCompletionIfNeeded()
            self?.refreshGitDiff()
            if self?.isMarkdownPreviewShown == true { self?.updateMarkdownPreview() }
            self?.updateBlameAnnotation()
            self?.scheduleBlameRefresh()
            self?.scheduleParameterHints()
            self?.scheduleTestMarkers()
        }
        editorView.onGitChangeClick = { [weak self] line in
            self?.toggleInlineGitHunk(at: line)
        }
        editorView.onReturnTypeHintDoubleClick = { [weak self] line, column in
            self?.insertReturnType(line: line, column: column)
        }
        editorView.onInlineHunkAction = { [weak self] action in
            self?.handleInlineHunkAction(action)
        }
        editorView.onSnippetInvalidated = { [weak self] in
            self?.snippetSession = nil
            self?.snippetDocumentKey = nil
        }
        editorView.onClickMove = { [weak self] before, after in
            guard let self, let tab = self.activeTab else { return }
            self.signaturePopup.isHidden = true
            self.lastSignatureRequest = nil
            self.signatureHelpDocument = nil
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
        editorView.onContextMenu = { [weak self] in self?.editorContextMenu() }
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
            self?.pathBar.show(symbols: self?.documentSymbols ?? [], atLine: primary.line)
            self?.updateBlameAnnotation()
        }
        breadcrumbBar.onSelect = { [weak self] symbol in
            guard let self, let tab = self.activeTab else { return }
            self.move(tab, toLine: symbol.line, col: symbol.column)
        }
        pathBar.onSelectSymbol = { [weak self] symbol in
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
            switch event {
            case let .pong(message):
                // Don't overwrite the status of an open document.
                if self?.editorView.document == nil {
                    self?.statusLabel.stringValue = "Core \(bridge.version) connected (\(message))"
                }
            case .goModulesStarted:
                self?.statusLabel.stringValue = "Downloading Go modules..."
            case .goModulesFinished:
                self?.statusLabel.stringValue = "Go modules downloaded; reindexing..."
            case let .goModulesFailed(_, message):
                self?.statusLabel.stringValue = "Go module download failed: \(message)"
            case let .workspaceIndexStarted(root, total):
                self?.workspaceIndexState = (root, 0, total)
                self?.statusBar.workspaceIndexStarted(root: root, total: total)
            case let .workspaceIndexProgress(root, completed, total):
                self?.workspaceIndexState = (root, completed, total)
                self?.statusBar.workspaceIndexProgress(root: root, completed: completed, total: total)
            case let .workspaceIndexFinished(root, total, changed):
                if self?.workspaceIndexState?.root == root { self?.workspaceIndexState = nil }
                self?.statusBar.workspaceIndexFinished(root: root, total: total, changed: changed)
            case let .phpIndexFinished(root, files):
                self?.statusBar.phpIndexFinished(root: root, files: files)
            }
        }
        statusBar.onGitClick = { [weak self] in self?.showGitBranchPicker() }
        statusBar.onGitGraphClick = { [weak self] in self?.toggleGitGraph(nil) }
        statusBar.onLanguageClick = { [weak self] in self?.showPHPAnalyzerTargetMenu() }
        statusBar.onIndentClick = { [weak self] in self?.showIndentationPicker() }
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
        for group in groups { group.pane?.editorView.releaseNonEssentialCaches() }
        loadedQuickOpen?.finder = nil
        fileFinder = nil
        finderGeneration += 1
        if event.contains(.critical) {
            evictInactiveSyntax(now: .now, includeRecentlyInactive: true)
        } else {
            evictInactiveSyntax(now: .now)
        }
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
        searchSidebar.onSearch = { [weak self] query, options in self?.searchSidebarWorkspace(query, options: options) }
        searchSidebar.onOpen = { [weak self] match in self?.openSearchSidebarMatch(match) }
        searchSidebar.onReplaceAll = { [weak self] query, replacement, options in
            self?.searchSidebarReplaceAll(query: query, replacement: replacement, options: options)
        }

        scrollView.hasVerticalScroller = true
        // Long lines scroll sideways; the line numbers stay at the left edge.
        scrollView.hasHorizontalScroller = true
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
        updateMinimap()
        secondaryScrollView.hasVerticalScroller = true
        secondaryScrollView.hasHorizontalScroller = true
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
        editorSplitView.isVertical = true
        editorSplitView.dividerStyle = .thin
        editorSplitView.translatesAutoresizingMaskIntoConstraints = false
        editorSplitView.addSubview(scrollView)

        tabBar.translatesAutoresizingMaskIntoConstraints = false
        tabBar.setContentHuggingPriority(.required, for: .vertical)
        tabBar.setContentCompressionResistancePriority(.required, for: .vertical)
        tabBar.onSelect = { [weak self] i in self?.selectTab(i, fromTabBar: true) }
        tabBar.onClose = { [weak self] i in self?.closeTab(at: i) }
        tabBar.onKeep = { [weak self] i in
            guard let self, self.tabs.indices.contains(i) else { return }
            self.keepOpen(self.tabs[i])
        }
        tabBar.onUnpin = { [weak self] i in
            guard let self, self.tabs.indices.contains(i) else { return }
            self.setPinned(false, tab: self.tabs[i])
        }
        tabBar.onMove = { [weak self] from, to in self?.moveTab(from: from, to: to) }
        tabBar.onDropOutside = { [weak self] i, point in
            guard let self else { return false }
            return self.dropTab(from: self.activeGroup, at: i, windowPoint: point)
        }
        tabBar.onTogglePreview = { [weak self] in self?.toggleMarkdownPreview(nil) }
        syntaxEvictionTimer = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
            self?.evictInactiveSyntax(now: .now)
        }
        lspTimer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.bridge.pollAnalysis()
                self?.refreshHover()
            self?.refreshCompletion()
                self?.refreshSignatureHelp()
                self?.refreshDefinition()
                self?.refreshReferences()
                self?.refreshRename()
                self?.refreshCodeActions()
                self?.refreshDocumentSymbols()
                self?.reloadSettings()
                guard let self, Date().timeIntervalSince(self.lastProblemsRefresh) >= 1 else { return }
                self.lastProblemsRefresh = .now
                let allProblems = self.bridge.core.problems()
                // Analysis workers can finish after the user switches folders.
                // Never let a late result from the previous workspace leak
                // into the new workspace's Problems panel or editor gutter.
                let problems: [ProblemInfo]
                if let root = self.workspace?.root() {
                    let rootURI = "file://\(URL(fileURLWithPath: root).standardizedFileURL.path)/"
                    problems = allProblems.filter {
                        $0.uri.hasPrefix(rootURI) && !$0.uri.contains("/node_modules/")
                    }
                } else {
                    problems = allProblems
                }
                self.problemsPanel.update(problems, workspaceRoot: self.workspace?.root())
                self.problemsPanel.isHidden = self.bottomPanelTab != .problems
                self.bottomPanelHeader.setProblemCount(problems.count)
                if let document = self.activeTab?.doc {
                    let markers = problems.filter { $0.uri == "file://\(document.path())" }
                        .map { ($0.line, $0.severity) }
                    self.scrollView.ruler.update(markers: markers, lineCount: document.lineCount())
                    // Redraw only when diagnostics change, not every 100 ms.
                    let revision = document.diagnosticsRevision()
                    if revision != self.lastDiagnosticsRevision {
                        self.lastDiagnosticsRevision = revision
                        self.editorView.needsDisplay = true
                    }
                }
            }
        }
        tabBar.titleBar = titleBar
        pathBar.translatesAutoresizingMaskIntoConstraints = false
        pathBar.show([])
        pathBar.isEnabled = UserDefaults.standard.object(forKey: "ShowBreadcrumbs") as? Bool ?? true
        breadcrumbBar.translatesAutoresizingMaskIntoConstraints = false
        breadcrumbBar.isEnabled = UserDefaults.standard.object(forKey: "ShowBreadcrumbs") as? Bool ?? true
        stickyScopeView.isEnabled = UserDefaults.standard.object(forKey: "StickyScroll") as? Bool ?? true
        updateWrapState()
        updateMinimap()
        breadcrumbBar.clear()
        findBarSlot.translatesAutoresizingMaskIntoConstraints = false
        stickyScopeView.translatesAutoresizingMaskIntoConstraints = false
        stickyScopeView.editor = editorView
        stickyScopeView.onSelect = { [weak self] line, pinned in self?.jumpToStickyLine(line, above: pinned) }
        // `BottomPanelContentView.layout()` sizes these (never below its
        // minimum). No autoresizing mask: one tied to the collapsed,
        // zero-height container conflicts with the panels' own constraints.
        for panel in [terminalPanel, outputPanel, portsPanel, problemsPanel, debugConsole, testResultsPanel] as [NSView] {
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
        // Positioned by frame in `hoverFrame`; without constraints Auto Layout
        // would collapse it to zero size.
        hoverPopover.translatesAutoresizingMaskIntoConstraints = true
        completionPopup.translatesAutoresizingMaskIntoConstraints = false
        signaturePopup.translatesAutoresizingMaskIntoConstraints = false
        findBarSlot.isHidden = true
        findBarHeight = findBarSlot.heightAnchor.constraint(equalToConstant: 0)
        editorCard.translatesAutoresizingMaskIntoConstraints = false
        bottomPanelCard.translatesAutoresizingMaskIntoConstraints = false
        bottomPanelCard.isHidden = true
        detailView.addSubview(editorCard)
        detailView.addSubview(bottomPanelCard)
        groupsSplitView.translatesAutoresizingMaskIntoConstraints = false
        groupsSplitView.addSubview(activeGroup.slot)
        editorCard.addSubview(groupsSplitView)
        groupChrome.translatesAutoresizingMaskIntoConstraints = false
        groupChrome.wantsLayer = true
        editorCard.addSubview(groupChrome)
        groupChrome.addSubview(tabBar)
        groupChrome.addSubview(pathBar)
        groupChrome.addSubview(breadcrumbBar)
        groupChrome.addSubview(findBarSlot)
        groupChrome.addSubview(editorSplitView)
        groupChrome.addSubview(stickyScopeView)
        editorTerminalView.translatesAutoresizingMaskIntoConstraints = false
        editorTerminalView.isHidden = true
        groupChrome.addSubview(editorTerminalView)
        gitGraphPanel.translatesAutoresizingMaskIntoConstraints = false
        groupChrome.addSubview(gitGraphPanel)
        mediaViewer.translatesAutoresizingMaskIntoConstraints = false
        mediaViewer.isHidden = true
        groupChrome.addSubview(mediaViewer)
        emptyEditor.translatesAutoresizingMaskIntoConstraints = false
        groupChrome.addSubview(emptyEditor)
        // `TerminalView` is layer-backed. Without a layer here, it was
        // composited above every non-layer sibling and blanked the tab bar
        // and path bar while a terminal tab was active.
        detailView.wantsLayer = true
        bottomPanelCard.addSubview(bottomPanelHeader)
        bottomPanelCard.addSubview(bottomPanelContent)
        // Every panel is added once and switched with `isHidden`; see
        // `setBottomPanelOpen`.
        debugConsole.isHidden = true
        for panel in [terminalPanel, problemsPanel, outputPanel, portsPanel, debugConsole, testResultsPanel] as [NSView] {
            bottomPanelContent.addSubview(panel)
        }
        bottomPanelContent.isHidden = true
        detailView.addSubview(hoverPopover)
        detailView.addSubview(blamePopover)
        detailView.addSubview(completionPopup)
        detailView.addSubview(signaturePopup)
        stickyTrailing = stickyScopeView.trailingAnchor.constraint(equalTo: editorSplitView.trailingAnchor)
        // `updateMinimap` ran before this constraint existed.
        stickyTrailing.constant = -editorRightInset
        bottomPanelGap = editorCard.bottomAnchor.constraint(equalTo: bottomPanelCard.topAnchor)
        NSLayoutConstraint.activate([
            editorCard.topAnchor.constraint(equalTo: detailView.topAnchor),
            editorCard.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
            editorCard.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
            bottomPanelGap,
            bottomPanelCard.leadingAnchor.constraint(equalTo: detailView.leadingAnchor),
            bottomPanelCard.trailingAnchor.constraint(equalTo: detailView.trailingAnchor),
            bottomPanelCard.bottomAnchor.constraint(equalTo: detailView.bottomAnchor),
            groupsSplitView.topAnchor.constraint(equalTo: editorCard.topAnchor),
            groupsSplitView.leadingAnchor.constraint(equalTo: editorCard.leadingAnchor),
            groupsSplitView.trailingAnchor.constraint(equalTo: editorCard.trailingAnchor),
            groupsSplitView.bottomAnchor.constraint(equalTo: editorCard.bottomAnchor),
            tabBar.topAnchor.constraint(equalTo: groupChrome.topAnchor),
            tabBar.leadingAnchor.constraint(equalTo: groupChrome.leadingAnchor),
            tabBar.trailingAnchor.constraint(equalTo: groupChrome.trailingAnchor),
            tabBar.heightAnchor.constraint(equalToConstant: Theme.tabHeight),
            pathBar.topAnchor.constraint(equalTo: tabBar.bottomAnchor),
            pathBar.leadingAnchor.constraint(equalTo: groupChrome.leadingAnchor),
            pathBar.trailingAnchor.constraint(equalTo: groupChrome.trailingAnchor),
            breadcrumbBar.topAnchor.constraint(equalTo: pathBar.bottomAnchor),
            breadcrumbBar.leadingAnchor.constraint(equalTo: groupChrome.leadingAnchor),
            breadcrumbBar.trailingAnchor.constraint(equalTo: groupChrome.trailingAnchor),
            findBarSlot.topAnchor.constraint(equalTo: breadcrumbBar.bottomAnchor),
            findBarSlot.leadingAnchor.constraint(equalTo: groupChrome.leadingAnchor),
            findBarSlot.trailingAnchor.constraint(equalTo: groupChrome.trailingAnchor),
            findBarHeight,
            editorSplitView.topAnchor.constraint(equalTo: findBarSlot.bottomAnchor),
            editorSplitView.leadingAnchor.constraint(equalTo: groupChrome.leadingAnchor),
            editorSplitView.trailingAnchor.constraint(equalTo: groupChrome.trailingAnchor),
            editorSplitView.bottomAnchor.constraint(equalTo: groupChrome.bottomAnchor),
            stickyScopeView.topAnchor.constraint(equalTo: editorSplitView.topAnchor),
            stickyScopeView.leadingAnchor.constraint(equalTo: editorSplitView.leadingAnchor),
            stickyTrailing,
            // A terminal tab covers the path bar, breadcrumbs and editor.
            editorTerminalView.topAnchor.constraint(equalTo: tabBar.bottomAnchor),
            editorTerminalView.leadingAnchor.constraint(equalTo: groupChrome.leadingAnchor),
            editorTerminalView.trailingAnchor.constraint(equalTo: groupChrome.trailingAnchor),
            editorTerminalView.bottomAnchor.constraint(equalTo: groupChrome.bottomAnchor),
            // The Git Graph tab covers the same area.
            gitGraphPanel.topAnchor.constraint(equalTo: tabBar.bottomAnchor),
            gitGraphPanel.leadingAnchor.constraint(equalTo: groupChrome.leadingAnchor),
            gitGraphPanel.trailingAnchor.constraint(equalTo: groupChrome.trailingAnchor),
            gitGraphPanel.bottomAnchor.constraint(equalTo: groupChrome.bottomAnchor),
            // And a picture or PDF.
            mediaViewer.topAnchor.constraint(equalTo: tabBar.bottomAnchor),
            mediaViewer.leadingAnchor.constraint(equalTo: groupChrome.leadingAnchor),
            mediaViewer.trailingAnchor.constraint(equalTo: groupChrome.trailingAnchor),
            mediaViewer.bottomAnchor.constraint(equalTo: groupChrome.bottomAnchor),
            // So does the empty state, under the tab bar.
            emptyEditor.topAnchor.constraint(equalTo: tabBar.bottomAnchor),
            emptyEditor.leadingAnchor.constraint(equalTo: groupChrome.leadingAnchor),
            emptyEditor.trailingAnchor.constraint(equalTo: groupChrome.trailingAnchor),
            emptyEditor.bottomAnchor.constraint(equalTo: groupChrome.bottomAnchor),
            bottomPanelHeader.topAnchor.constraint(equalTo: bottomPanelCard.topAnchor),
            bottomPanelHeader.leadingAnchor.constraint(equalTo: bottomPanelCard.leadingAnchor),
            bottomPanelHeader.trailingAnchor.constraint(equalTo: bottomPanelCard.trailingAnchor),
            bottomPanelHeaderHeight,
            bottomPanelContent.topAnchor.constraint(equalTo: bottomPanelHeader.bottomAnchor),
            bottomPanelContent.leadingAnchor.constraint(equalTo: bottomPanelCard.leadingAnchor),
            bottomPanelContent.trailingAnchor.constraint(equalTo: bottomPanelCard.trailingAnchor),
            bottomPanelContent.bottomAnchor.constraint(equalTo: bottomPanelCard.bottomAnchor),
            bottomPanelHeight,
        ])
        placeChrome(over: activeGroup)
        problemsPanel.onSelect = { [weak self] problem in self?.showProblem(problem) }
        debugger.onOutput = { [weak self] category, text in self?.debugConsole.append(text, category: category) }
        debugger.onChange = { [weak self] in self?.debugStateChanged() }
        debugger.onStop = { [weak self] path, line in
            _ = self?.show(NavLocation(key: path, line: UInt64(max(line, 1) - 1), col: 0))
        }
        debugConsole.onEvaluate = { [weak self] expression in self?.debugger.evaluate(expression) }
        editorView.onToggleBreakpoint = { [weak self] line in self?.toggleBreakpoint(line: line) }
        editorView.onRunTest = { [weak self] line in self?.runTest(atLine: line) }
        wireTestResults()
        testResultsPanel.isHidden = true
        problemsPanel.isHidden = true
        outputPanel.isHidden = true
        portsPanel.isHidden = true
        gitGraphPanel.isHidden = true
        terminalPanel.isHidden = true
        problemsPanel.isHidden = true
        bottomPanelHeader.onSelect = { [weak self] tab in
            guard let self else { return }
            if !self.isBottomPanelVisible { self.setBottomPanelOpen(true) }
            self.showBottomPanel(tab: tab)
        }
        bottomPanelHeader.onNewTerminal = { [weak self] in self?.newTerminal() }
        bottomPanelHeader.onKillTerminal = { [weak self] in self?.killPanelTerminal() }
        bottomPanelHeader.onSplitTerminal = { [weak self] in self?.splitTerminal(nil) }
        bottomPanelHeader.chipMenuProvider = { [weak self] in
            self?.activeTerminal.flatMap { self?.terminalMenu(for: $0) }
        }
        bottomPanelHeader.onToggleMaximize = { [weak self] in self?.toggleBottomPanelMaximized() }
        bottomPanelHeader.onResize = { [weak self] began, delta in self?.resizeBottomPanel(began: began, deltaY: delta) }
        if let saved = UserDefaults.standard.object(forKey: "BottomPanelHeight") as? Double, saved >= Double(Self.minBottomPanelHeight) {
            bottomPanelOpenHeight = CGFloat(saved)
        }
        bottomPanelHeader.onClose = { [weak self] in self?.setBottomPanelOpen(false) }
        terminalPanel.list.onSelect = { [weak self] index in
            guard let self, self.terminals.indices.contains(index) else { return }
            self.activeTerminalIndex = index
            self.ensureTerminal()
            self.window?.makeFirstResponder(self.terminalPanel.activePane)
        }
        terminalPanel.list.onSplit = { [weak self] index in
            guard let self, self.terminals.indices.contains(index) else { return }
            self.splitTerminal(self.terminalItem(self.terminals[index]))
        }
        terminalPanel.list.onKill = { [weak self] index in
            guard let self, self.terminals.indices.contains(index) else { return }
            self.killPanelTerminal(self.terminals[index])
        }
        terminalPanel.list.menuProvider = { [weak self] index in
            guard let self, self.terminals.indices.contains(index) else { return nil }
            return self.terminalMenu(for: self.terminals[index])
        }
        terminalPanel.menuProvider = { [weak self] terminal in self?.terminalMenu(for: terminal) }
        terminalPanel.onFocusTerminal = { [weak self] terminal in
            guard let self, let index = self.terminals.firstIndex(where: { $0 === terminal }),
                  index != self.activeTerminalIndex else { return }
            self.activeTerminalIndex = index
            self.refreshTerminalPicker()
        }
        stickyScopeView.isHidden = true
        terminalPanel.onOpenLink = { [weak self] path, line, col in
            self?.openTerminalLink(path: path, line: line, col: col)
        }
        terminalPanel.onExit = { [weak self] in self?.terminalExited() }
        editorTerminalView.onOpenLink = { [weak self] path, line, col in
            self?.openTerminalLink(path: path, line: line, col: col)
        }
        editorTerminalView.onExit = { [weak self] in self?.editorTerminalExited() }
        tabBar.menuForTab = { [weak self] index in self?.menu(forTab: index) }
        tabBar.activeTabMenuProvider = { [weak self] in
            guard let self, let index = self.activeIndex else { return nil }
            return self.menu(forTab: index)
        }
        hoverPopover.isHidden = true
        blamePopover.isHidden = true
        blamePopover.translatesAutoresizingMaskIntoConstraints = true
        blamePopover.onPointerEntered = { [weak self] in self?.cancelBlameHide() }
        blamePopover.onPointerExited = { [weak self] in self?.scheduleBlameHide() }
        editorView.onMouseExit = { [weak self] in
            self?.cancelBlameShow()
            self?.scheduleBlameHide()
        }
        completionPopup.isHidden = true
        signaturePopup.isHidden = true
        completionPopup.onClose = { [weak self] in self?.closeCompletion() }
        completionPopup.onSelect = { [weak self] item in self?.acceptCompletion(item) }
        editorView.onCompletionKey = { [weak self] event in self?.handleCompletionKey(event) ?? false }
        editorView.onTextInput = { [weak self] text in
            self?.completionDismissed = false
            self?.signatureHelpDocument = self?.activeTab.map { ObjectIdentifier($0.doc) }
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
        guard let url = URL(string: problem.uri), url.isFileURL else { return }
        do {
            try openFile(at: url)
            activeTab?.doc.goToLine(line: problem.line, col: problem.column)
            editorView.revealLine(Int(problem.line))
            editorView.window?.makeFirstResponder(editorView)
        } catch {
            statusLabel.stringValue = "Cannot open problem location: \(url.path)"
        }
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
            self?.openWorkspaceFile(workspace, entry, preview: false)
        }
        explorer.onPinFile = { [weak self] workspace, entry in
            self?.openWorkspaceFile(workspace, entry, preview: false)
        }
        explorer.onRename = { [weak self] entry, name in self?.renameEntry(entry, to: name) }
        explorer.onCreateNewItem = { [weak self] parent, folder, name in
            self?.createExplorerItem(parent: parent, folder: folder, name: name)
        }
        explorer.contextMenu = { [weak self] in self?.explorerContextMenu() }
        explorer.outlineView.onKey = { [weak self] event in self?.explorerKey(event) ?? false }
        pathBar.menuForComponent = { [weak self] index in self?.pathBarMenu(for: index) }

        sidebar.addSubview(sectionHeader)
        sidebar.addSubview(explorer.scrollView)
        sidebar.addSubview(sourceControl.view)
        sidebar.addSubview(searchSidebar.view)
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
            emptyState.centerXAnchor.constraint(equalTo: sidebar.centerXAnchor).withPriority(.defaultHigh),
            emptyState.centerYAnchor.constraint(equalTo: sidebar.centerYAnchor, constant: -20),
            emptyState.leadingAnchor.constraint(greaterThanOrEqualTo: sidebar.leadingAnchor, constant: 16).withPriority(.defaultLow),
            emptyState.trailingAnchor.constraint(lessThanOrEqualTo: sidebar.trailingAnchor, constant: -16).withPriority(.defaultLow),
        ])
        sourceControl.view.isHidden = true
        searchSidebar.view.isHidden = true
    }

    // MARK: Stored state of the feature extensions

    // Swift extensions cannot hold stored properties, so the state used by the
    // `MainWindowController+*.swift` files lives here.

    var resizeStartHeight: CGFloat = 0

    let minimap = MinimapView(frame: .zero)
    var minimapEnabled = UserDefaults.standard.object(forKey: "Minimap") as? Bool ?? true

    var wordWrapEnabled = UserDefaults.standard.bool(forKey: "WordWrap")

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

    /// Where `keybindings.json` lives. Tests point it elsewhere.
    var keybindingsURL = Keybindings.defaultURL
    var keybindings = Keybindings.defaults
    var keybindingsModified: Date?

    /// Background match count in flight, if any. Only the newest one may
    /// update the bar; older ones are cancelled.
    var countJob: FindCountJob?
    /// Caret when the find bar opened. Typing in the field moves the caret
    /// to the nearest match, so this is the place Back should return to.
    var findOrigin: NavLocation?

    /// Called after a background count updated the bar (tests wait on it).
    var onFindCountFinished: (() -> Void)?

    /// Where the session is saved. Tests point it elsewhere.
    var sessionURL = Session.defaultURL
    var didCloseWindow = false
    var isTerminatingApplication = false
}

private extension NSLayoutConstraint {
    func withPriority(_ priority: NSLayoutConstraint.Priority) -> NSLayoutConstraint {
        self.priority = priority
        return self
    }
}
