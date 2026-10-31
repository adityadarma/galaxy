import AppKit
import os

extension MainWindowController {
    // MARK: Minimap

    @objc func toggleMinimap(_ sender: Any?) {
        minimapEnabled.toggle()
        UserDefaults.standard.set(minimapEnabled, forKey: "Minimap")
        updateMinimap()
    }

    /// The minimap sits over the right edge of the primary editor, under
    /// the scroller; the diff pane has its own layout and goes without.
    func updateMinimap() {
        let show = minimapEnabled && !isDiffPaneShown
        if minimap.superview == nil {
            minimap.editor = editorView
            minimap.scrollView = scrollView
            minimap.autoresizingMask = []
            // Below the overlay scroller, so it keeps drawing over the map.
            scrollView.addSubview(minimap, positioned: .below, relativeTo: scrollView.verticalScroller)
            editorView.onLayoutChange = { [weak self] in self?.minimap.needsDisplay = true }
            scrollView.contentView.postsBoundsChangedNotifications = true
            scrollView.postsFrameChangedNotifications = true
            let center = NotificationCenter.default
            center.addObserver(forName: NSView.boundsDidChangeNotification, object: scrollView.contentView, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.scheduleScrollDecorationUpdate()
                }
            }
            // The scroll view is created empty and sized by its split view
            // later, so the map follows every resize.
            center.addObserver(forName: NSView.frameDidChangeNotification, object: scrollView, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.layoutMinimap() }
            }
        }
        minimap.isHidden = !show
        editorView.rightInset = show ? MinimapView.width : 0
        editorView.applyRightInset()
        stickyTrailing?.constant = show ? -MinimapView.width : 0
        layoutMinimap()
    }

    private func layoutMinimap() {
        // Autoresizing alone loses the inset when the scroll view started
        // out narrower than the minimap; set the width from the scroll view.
        editorView.applyRightInset()
        let width = MinimapView.width
        minimap.frame = NSRect(x: scrollView.bounds.width - width, y: 0, width: width, height: scrollView.bounds.height)
        minimap.needsDisplay = true
    }

    /// Keeps scroll painting responsive when trackpads emit many bounds-change
    /// notifications. The editor itself scrolls natively; only decorations are
    /// rate-limited to 30 Hz.
    func scheduleScrollDecorationUpdate() {
        guard !scrollDecorationUpdatePending else { return }
        scrollDecorationUpdatePending = true
        let interval = 1.0 / 30.0
        let delay = max(0, interval - Date().timeIntervalSince(lastScrollDecorationUpdate))
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
            guard let self else { return }
            self.scrollDecorationUpdatePending = false
            self.lastScrollDecorationUpdate = Date()
            if !self.minimap.isHidden { self.minimap.needsDisplay = true }
            self.updateStickyScope()
            self.scheduleParameterHints()
        }
    }

    // MARK: Word wrap

    @objc func toggleWordWrap(_ sender: Any?) {
        wordWrapEnabled.toggle()
        UserDefaults.standard.set(wordWrapEnabled, forKey: "WordWrap")
        updateWrapState()
    }

    /// Wrap needs one row table per document, so the split and diff panes,
    /// which show a document twice, keep one row per line.
    func updateWrapState() {
        // Another group showing the same document would fight over its wrap width.
        let shownElsewhere = groups.contains { group in
            group !== activeGroup && group.activeTab.map { $0.isDocument && $0.doc === activeTab?.doc } == true
        }
        let suspended = isDiffPaneShown || shownElsewhere
        editorView.wrapEnabled = wordWrapEnabled
        editorView.wrapSuspended = suspended
        secondaryEditorView.wrapSuspended = true
        updateStickyScope()
        parameterHintsRequest = nil
        scheduleParameterHints(after: 0)
    }

    // MARK: View menu

    @objc func showProblemsPanel(_ sender: Any?) { revealBottomPanel(.problems) }
    @objc func showOutputPanel(_ sender: Any?) { revealBottomPanel(.output) }

    func revealBottomPanel(_ tab: BottomPanelHeader.Tab) {
        if !isBottomPanelVisible { setBottomPanelOpen(true) }
        showBottomPanel(tab: tab)
    }

    static let minEditorFontSize: CGFloat = 10
    static let maxEditorFontSize: CGFloat = 24

    /// Changes the editor font size by `delta` points, within the range
    /// Settings allows, and keeps it for the next launch.
    private func zoomEditor(by delta: CGFloat?) {
        let defaults = UserDefaults.standard
        let current = defaults.object(forKey: "EditorFontSize") as? CGFloat ?? 13
        let next = delta.map { current + $0 } ?? 13
        defaults.set(Double(min(max(next, Self.minEditorFontSize), Self.maxEditorFontSize)), forKey: "EditorFontSize")
        applyPreferences()
    }

    @objc func zoomEditorIn(_ sender: Any?) { zoomEditor(by: 1) }
    @objc func zoomEditorOut(_ sender: Any?) { zoomEditor(by: -1) }
    @objc func zoomEditorReset(_ sender: Any?) { zoomEditor(by: nil) }

    @objc func toggleBreadcrumbs(_ sender: Any?) {
        let on = !breadcrumbBar.isEnabled
        breadcrumbBar.isEnabled = on
        UserDefaults.standard.set(on, forKey: "ShowBreadcrumbs")
        breadcrumbBar.show(symbols: documentSymbols, atLine: editorView.document?.primaryCursor().line ?? 0)
    }

    @objc func toggleStickyScroll(_ sender: Any?) {
        let on = !stickyScopeView.isEnabled
        stickyScopeView.isEnabled = on
        UserDefaults.standard.set(on, forKey: "StickyScroll")
        updateStickyScope()
    }

    // MARK: Run menu

    @objc func runBuildTask(_ sender: Any?) { runTask("build") { $0.build } }
    @objc func runTestTask(_ sender: Any?) { runTask("test") { $0.test } }

    @objc func downloadGoModules(_ sender: Any?) {
        guard let document = activeTab?.doc, document.path().lowercased().hasSuffix(".go") else {
            statusLabel.stringValue = "Open a Go file inside the module to download."
            return NSSound.beep()
        }
        guard bridge.core.downloadGoModules(root: document.path()) else {
            statusLabel.stringValue = "No Go module found for this file."
            return NSSound.beep()
        }
        statusLabel.stringValue = "Downloading Go modules for the active Go module..."
    }

    /// Sends the project's build or test command to the panel terminal.
    private func runTask(_ name: String, pick: (RunTasks.Detected) -> String?) {
        guard let workspace else { return NSSound.beep() }
        let detected = RunTasks.detect(in: URL(fileURLWithPath: workspace.root()))
        guard let command = pick(detected) else {
            statusLabel.stringValue = "No \(name) task found for this folder."
            return NSSound.beep()
        }
        if terminals.isEmpty {
            newTerminal()
        } else {
            revealBottomPanel(.terminal)
        }
        guard let terminal = activeTerminal else { return NSSound.beep() }
        try? terminal.handle.write(input: command + "\r")
        statusLabel.stringValue = "Running: \(command)"
    }
}
