import AppKit
import os

extension MainWindowController {
    /// Opens the Git Graph as an editor tab, or closes it when it is the
    /// tab showing. One graph tab at a time.
    @objc func toggleGitGraph(_ sender: Any?) {
        if let existing = allTabs.first(where: \.isGitGraph) {
            if existing === activeTab, let i = activeIndex { closeTab(at: i) } else { focusTab(existing) }
            return
        }
        let tab = EditorTab(doc: bridge.newDocument(), title: "Git Graph", pathComponents: ["Git Graph"],
                            windowTitle: "Git Graph", isPreview: false, status: "", isGitGraph: true)
        add(tab, replacing: nil)
    }

    var isGitGraphTabShown: Bool { activeTab?.isGitGraph == true }

    /// Shows or hides the Git Graph for the active tab, and keeps it
    /// refreshing only while it is showing.
    func showEditorGitGraph(for tab: EditorTab?) {
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

    // MARK: Git blame

    @objc func toggleGitBlame(_ sender: Any?) {
        setGitBlame(!gitBlameEnabled)
    }

    func setGitBlame(_ on: Bool) {
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
    func refreshBlame() {
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

    func scheduleBlameRefresh() {
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
    /// Returns `true` when it is, so the code hover stays out of the way.
    func handleBlameHover(point: NSPoint) -> Bool {
        guard let commit = blameCommit, let rect = editorView.blameAnnotationRect(), rect.contains(point) else {
            cancelBlameShow()
            if !blamePopover.isHidden { scheduleBlameHide() }
            return false
        }
        cancelBlameHide()
        hoverPopover.isHidden = true
        if blamePopover.isHidden { scheduleBlameShow(commit) }
        return true
    }

    /// Avoid opening the card while the pointer merely crosses an annotation.
    func scheduleBlameShow(_ commit: BlameCommitInfo) {
        guard blameShowItem == nil else { return }
        let item = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.blameShowItem = nil
            guard
                  self.blameCommit?.sha == commit.sha,
                  let point = self.editorView.lastMousePoint,
                  let currentRect = self.editorView.blameAnnotationRect(),
                  currentRect.contains(point) else { return }
            self.blamePopover.show(commit)
            let anchor = self.detailView.convert(currentRect, from: self.editorView)
            let visible = self.detailView.convert(self.editorView.visibleRect, from: self.editorView).insetBy(dx: 8, dy: 4)
            self.blamePopover.frame = HoverPopover.placement(size: self.blamePopover.fittingSize(), anchor: anchor, in: visible, gap: 2)
            self.blamePopover.isHidden = false
            self.blamePopover.needsLayout = true
        }
        blameShowItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 1, execute: item)
    }

    func cancelBlameShow() {
        blameShowItem?.cancel()
        blameShowItem = nil
    }

    /// Closes the card after a short grace period, so the pointer can cross
    /// the gap between the annotation and the card to reach `Copy SHA`.
    func scheduleBlameHide() {
        guard !blamePopover.isHidden else { return }
        blameHideItem?.cancel()
        let item = DispatchWorkItem { [weak self] in self?.hideBlamePopover() }
        blameHideItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25, execute: item)
    }

    func cancelBlameHide() {
        blameHideItem?.cancel()
        blameHideItem = nil
    }

    func hideBlamePopover() {
        cancelBlameShow()
        cancelBlameHide()
        blamePopover.isHidden = true
    }

    func refreshGitStatus() {
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

    func showGitBranchPicker() {
        guard let workspace else { return }
        let picker = loadedGitPicker ?? GitBranchPicker()
        if loadedGitPicker == nil {
            picker.onClose = { [weak self] in self?.closeGitBranchPicker() }
            picker.onCheckout = { [weak self] name in self?.checkoutGitBranch(name) }
            picker.onCreate = { [weak self] in self?.createGitBranch() }
            window?.contentView?.addSubview(picker)
            NSLayoutConstraint.activate([
                picker.topAnchor.constraint(equalTo: detailView.topAnchor, constant: Theme.tabHeight + 8),
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

    func refreshGitDiff() {
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
    /// as a normal tab. The diff gets its own tab, shared document with the
    /// file's plain tab, which stays as it is.
    func openWorkspaceDiff(_ workspace: Workspace, relPath: String) {
        let name = (relPath as NSString).lastPathComponent
        guard let base = (try? workspace.gitBaseDocument(relPath: relPath)) ?? nil else {
            return openWorkspaceFile(workspace, FileEntry(name: name, relPath: relPath, isDir: false), preview: false)
        }
        diffRevealPending = true
        let path = URL(fileURLWithPath: workspace.root()).appendingPathComponent(relPath).path
        if let i = tabIndex(forPath: path, diff: true) {
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
            let doc = try sharedDocument(forFileKey: Self.fileKey(path) ?? "") ?? workspace.openFile(relPath: relPath)
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
    func applyDiffPane(for tab: EditorTab) {
        editorView.lineTints = [:]
        secondaryEditorView.lineTints = [:]
        guard let base = tab.diffBase else {
            if isDiffPaneShown { leaveDiffPane() }
            secondaryEditorView.document = nil
            return
        }
        if !isDiffPaneShown {
            isDiffPaneShown = true
            updateWrapState()
            updateMinimap()
            removeMarkdownPreviewIfLoaded()
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

    func leaveDiffPane() {
        isDiffPaneShown = false
        updateWrapState()
        updateMinimap()
        diffMap = DiffLineMap()
        secondaryEditorView.isReadOnly = false
        secondaryEditorView.lineTints = [:]
        editorView.lineTints = [:]
        secondaryScrollView.removeFromSuperview()
        editorSplitView.isVertical = true
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
    func installDiffScrollSync() {
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
}
