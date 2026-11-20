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

        var map: [String: GitFileStatus] = [:]
        for change in status.changes {
            map[change.path] = GitFileStatus.from(code: change.code)
        }
        explorer.gitStatusMap = map
        explorer.gitChangedPaths = Set(status.changedPaths)
        sourceControl.refresh(status: status)
        activityBar.setBadge(sourceControl.changeCount, for: .sourceControl)
        refreshTabBar()
        refreshGitDiff()
        refreshBlame()
    }

    func showGitBranchPicker() {
        guard let workspace else { return }
        closeOverlays()
        let picker = loadedGitPicker ?? GitBranchPicker()
        if loadedGitPicker == nil {
            guard let contentView = window?.contentView else { return }
            picker.onClose = { [weak self] in self?.closeGitBranchPicker() }
            picker.onCheckout = { [weak self] name in self?.checkoutGitBranch(name) }
            picker.onCreate = { [weak self] in self?.createGitBranch() }
            picker.onCreateFrom = { [weak self] in self?.createGitBranchFrom() }
            picker.onCheckoutDetached = { [weak self] in self?.checkoutDetached() }
            contentView.addSubview(picker)
            let preferred = picker.widthAnchor.constraint(equalToConstant: 620)
            preferred.priority = .defaultHigh
            NSLayoutConstraint.activate([
                picker.topAnchor.constraint(equalTo: titleBar.topAnchor, constant: 6),
                picker.centerXAnchor.constraint(equalTo: titleBar.centerXAnchor),
                picker.widthAnchor.constraint(lessThanOrEqualTo: titleBar.widthAnchor, constant: -24),
                preferred,
            ])
            loadedGitPicker = picker
        }
        picker.reset(branches: bridge.gitBranches(workspace: workspace),
                     commits: bridge.gitLogGraph(workspace: workspace))
        picker.isHidden = false
        picker.focus()
    }

    func closeGitBranchPicker() {
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

    private func createGitBranchFrom() {
        let alert = NSAlert()
        alert.messageText = "Create new branch from a commit"
        let name = NSTextField(string: "")
        name.placeholderString = "New branch name"
        let source = NSTextField(string: "")
        source.placeholderString = "Branch, tag, or commit"
        let fields = NSStackView(views: [name, source])
        fields.orientation = .vertical
        fields.spacing = 8
        fields.frame = NSRect(x: 0, y: 0, width: 320, height: 56)
        alert.accessoryView = fields
        alert.addButton(withTitle: "Create")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn, let workspace else { return }
        do {
            try workspace.gitCreateBranchAt(name: name.stringValue, hash: source.stringValue)
            closeGitBranchPicker()
            refreshGitStatus()
            statusLabel.stringValue = ""
        } catch {
            statusLabel.stringValue = "Could not create branch from \(source.stringValue)."
        }
    }

    private func checkoutDetached() {
        let alert = NSAlert()
        alert.messageText = "Checkout detached"
        alert.informativeText = "Enter a branch, tag, or commit hash."
        let source = NSTextField(string: "")
        source.placeholderString = "Branch, tag, or commit"
        source.frame = NSRect(x: 0, y: 0, width: 320, height: 24)
        alert.accessoryView = source
        alert.addButton(withTitle: "Checkout")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn, let workspace else { return }
        do {
            try workspace.gitCheckoutCommit(hash: source.stringValue)
            closeGitBranchPicker()
            refreshGitStatus()
            statusLabel.stringValue = ""
        } catch {
            statusLabel.stringValue = "Could not checkout \(source.stringValue)."
        }
    }

    func refreshGitDiff() {
        guard let workspace, let tab = activeTab, let path = tab.relPath else {
            editorView.gitLineChanges = [:]
            scrollView.ruler.updateGit(changes: [:], lineCount: 1)
            return
        }
        let text = tab.doc.text()
        let document = tab.doc
        let wantsHunks = tab.diffBase != nil
        let open = editorView.inlineGitHunk
        DispatchQueue.global(qos: .utility).async { [weak self, workspace, path, text, open, document] in
            let changes = (try? workspace.gitDiffLines(relPath: path, text: text)) ?? []
            let mapped = Self.gitLineMarkers(changes)
            let hunks = (open != nil || wantsHunks) ? ((try? workspace.gitDiffHunks(relPath: path, text: text)) ?? []) : []
            let map = wantsHunks ? DiffLineMap(hunks) : nil
            // An open inline hunk follows the edit: same HEAD hunk, new text.
            // It closes once the change is gone, e.g. after an undo.
            let refreshed = open.map { Self.refreshedInlineHunk($0, hunks: hunks, workspace: workspace, document: document, path: path, text: text) }
            Task { @MainActor [weak self] in
                guard let self, self.activeTab?.relPath == path else { return }
                self.editorView.gitLineChanges = mapped
                self.scrollView.ruler.updateGit(changes: mapped, lineCount: document.lineCount())
                if let refreshed, self.editorView.inlineGitHunk == open { self.editorView.inlineGitHunk = refreshed }
                if let map, self.isDiffPaneShown { self.applyDiff(map) }
            }
        }
    }

    /// Expands one working-tree change in a peek zone below its last line. The
    /// editor owns layout; this supplies the HEAD side and the context rows.
    func toggleInlineGitHunk(at line: UInt64) {
        if let open = editorView.inlineGitHunk, open.contains(line: line) {
            editorView.inlineGitHunk = nil
            return
        }
        showInlineGitHunk { hunks in
            hunks.firstIndex {
                if $0.newLen == 0 {
                    // A deletion has no working-tree row. Its red marker is
                    // anchored to the following row, which opens this hunk.
                    return line == $0.newStart
                }
                return line >= $0.newStart && line < $0.newStart + $0.newLen
            }
        }
    }

    /// The zone's header buttons.
    func handleInlineHunkAction(_ action: EditorView.InlineHunkAction) {
        guard let open = editorView.inlineGitHunk else { return }
        let current = open.newStart
        switch action {
        case .close:
            editorView.inlineGitHunk = nil
        case .next:
            showInlineGitHunk { hunks in hunks.firstIndex { $0.newStart > current } ?? (hunks.isEmpty ? nil : 0) }
        case .previous:
            showInlineGitHunk { hunks in hunks.lastIndex { $0.newStart < current } ?? hunks.indices.last }
        case .stage:
            stageInlineGitHunk(open)
        case .revert:
            revertInlineGitHunk(open)
        }
    }

    /// Stages only the open hunk. The zone stays: it compares with HEAD, and
    /// the Source Control list shows the file as staged.
    private func stageInlineGitHunk(_ hunk: EditorView.InlineGitHunk) {
        guard let workspace, let tab = activeTab, let path = tab.relPath else { return }
        let text = tab.doc.text()
        let oldStart = hunk.oldStart
        DispatchQueue.global(qos: .userInitiated).async { [weak self, workspace, path, text, oldStart] in
            let failure: String? = {
                do {
                    try workspace.gitStageHunk(relPath: path, text: text, oldStart: oldStart)
                    return nil
                } catch {
                    return "Could not stage the change."
                }
            }()
            Task { @MainActor [weak self] in
                guard let self else { return }
                if let failure {
                    self.statusLabel.stringValue = failure
                } else {
                    self.statusLabel.stringValue = "Staged the change."
                    // The peek is for a working-tree change. Closing it gives
                    // immediate feedback even though gutter markers compare
                    // the editor to HEAD and still include staged changes.
                    self.editorView.inlineGitHunk = nil
                    self.refreshGitStatus()
                }
            }
        }
    }

    private func showInlineGitHunk(choose: @escaping @Sendable ([GitDiffHunkInfo]) -> Int?) {
        guard let workspace, let tab = activeTab, let path = tab.relPath else { return }
        let text = tab.doc.text()
        let document = tab.doc
        DispatchQueue.global(qos: .userInitiated).async { [weak self, workspace, path, text, document] in
            let hunks = (try? workspace.gitDiffHunks(relPath: path, text: text)) ?? []
            guard let index = choose(hunks), hunks.indices.contains(index) else { return }
            let inline = Self.inlineHunk(hunks, index: index, workspace: workspace, document: document, path: path, text: text)
            Task { @MainActor [weak self] in
                guard let self, self.activeTab?.relPath == path else { return }
                self.editorView.inlineGitHunk = inline
                self.editorView.revealLine(Int(inline.newStart))
            }
        }
    }

    /// Puts the HEAD lines back in place of the hunk's working-tree lines.
    private func revertInlineGitHunk(_ hunk: EditorView.InlineGitHunk) {
        guard let document = activeTab?.doc else { return }
        let text = document.text()
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        let total = UInt64(text.utf16.count)
        let offset = { (line: Int) -> UInt64 in
            min(total, UInt64(lines.prefix(line).reduce(0) { $0 + $1.utf16.count + 1 }))
        }
        let first = Int(hunk.newStart)
        let end = first + Int(hunk.newLen)
        let start = offset(first)
        var body = hunk.oldLines.joined(separator: "\n")
        if !hunk.oldLines.isEmpty, end < lines.count { body += "\n" }
        // Restoring lines after the last line of a file with no final newline.
        if hunk.newLen == 0, !hunk.oldLines.isEmpty, end >= lines.count, !text.isEmpty, !text.hasSuffix("\n") {
            body = "\n" + body
        }
        document.commitText(text: body, replacement: Utf16Range(location: start, length: offset(end) - start))
        editorView.didChangeDocument()
    }

    nonisolated private static func inlineHunk(
        _ hunks: [GitDiffHunkInfo],
        index: Int,
        workspace: Workspace,
        document: DocumentHandle,
        path: String,
        text: String
    ) -> EditorView.InlineGitHunk {
        typealias Row = EditorView.InlineGitHunk.Row
        let hunk = hunks[index]
        let base = (try? workspace.gitBaseDocument(relPath: path)) ?? nil
        // The HEAD document is new, and its parser runs in the background.
        // Reading styles at once gives removed rows none on a slow machine
        // (seen on CI), so wait briefly for the first tree. This runs off
        // the main thread.
        if let base, base.syntaxActive() {
            let deadline = Date().addingTimeInterval(1)
            while !base.pollSyntax(), Date() < deadline { usleep(5_000) }
        }
        let oldAll = gitLines(base?.text() ?? "")
        let newAll = gitLines(text)
        let oldStart = Int(hunk.oldStart), oldLen = Int(hunk.oldLen)
        let newStart = Int(hunk.newStart), newLen = Int(hunk.newLen)
        let context = 3
        // Runs by 0-based line: the working tree for context and added rows,
        // HEAD for removed ones.
        let newFirst = newStart - min(context, oldStart, newStart)
        let newCount = min(newAll.count, newStart + newLen + context) - newFirst
        let newStyles = styleRuns(document, first: newFirst, count: newCount)
        let oldStyles = styleRuns(base, first: oldStart, count: min(oldLen, max(oldAll.count - oldStart, 0)))
        var rows: [Row] = []
        let before = min(context, oldStart, newStart)
        for k in stride(from: before, to: 0, by: -1) where newStart - k < newAll.count {
            rows.append(Row(kind: .context, oldNumber: oldStart - k + 1, newNumber: newStart - k + 1, text: newAll[newStart - k],
                            styles: newStyles[newStart - k] ?? []))
        }
        let oldChanged = Array(oldAll.dropFirst(oldStart).prefix(oldLen))
        let newChanged = Array(newAll.dropFirst(newStart).prefix(newLen))
        for i in 0..<oldChanged.count {
            rows.append(Row(kind: .removed, oldNumber: oldStart + i + 1, newNumber: nil, text: oldChanged[i],
                            styles: oldStyles[oldStart + i] ?? []))
        }
        for i in 0..<newChanged.count {
            rows.append(Row(kind: .added, oldNumber: nil, newNumber: newStart + i + 1, text: newChanged[i],
                            styles: newStyles[newStart + i] ?? []))
        }
        for j in 0..<context {
            let n = newStart + newLen + j, o = oldStart + oldLen + j
            guard n < newAll.count, o < oldAll.count else { break }
            rows.append(Row(kind: .context, oldNumber: o + 1, newNumber: n + 1, text: newAll[n], styles: newStyles[n] ?? []))
        }
        // The zone sits under the last changed line; a pure deletion has none,
        // so it sits under the line before the gap.
        let last = newLen == 0 ? max(newStart - 1, 0) : newStart + newLen - 1
        return EditorView.InlineGitHunk(
            anchorLine: UInt64(min(last, max(newAll.count - 1, 0))),
            rows: rows,
            oldStart: hunk.oldStart,
            newStart: hunk.newStart,
            newLen: hunk.newLen,
            fileName: (path as NSString).lastPathComponent,
            index: index,
            total: hunks.count
        )
    }

    /// `open` rebuilt from the current text, or nil when its change is gone.
    nonisolated private static func refreshedInlineHunk(
        _ open: EditorView.InlineGitHunk,
        hunks: [GitDiffHunkInfo],
        workspace: Workspace,
        document: DocumentHandle,
        path: String,
        text: String
    ) -> EditorView.InlineGitHunk? {
        let index = hunks.firstIndex { $0.oldStart == open.oldStart }
            ?? hunks.firstIndex { open.contains(line: $0.newStart) || $0.newStart == open.newStart }
        guard let index else { return nil }
        return inlineHunk(hunks, index: index, workspace: workspace, document: document, path: path, text: text)
    }

    /// Syntax runs of `count` lines from `first`, keyed by 0-based line.
    nonisolated private static func styleRuns(_ document: DocumentHandle?, first: Int, count: Int) -> [Int: [StyleRun]] {
        guard let document, count > 0 else { return [:] }
        let snapshot = document.viewport(firstLine: UInt64(first), count: UInt64(count))
        return Dictionary(grouping: snapshot.styles, by: { Int($0.line) })
    }

    /// Lines of `text`; a final newline ends the last line rather than adding one.
    nonisolated private static func gitLines(_ text: String) -> [String] {
        guard !text.isEmpty else { return [] }
        var lines = text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        if text.hasSuffix("\n") { lines.removeLast() }
        return lines
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
        editorView.diffSpacers = []
        secondaryEditorView.diffSpacers = []
        editorView.diffSide = nil
        secondaryEditorView.diffSide = nil
        guard let base = tab.diffBase else {
            if isDiffPaneShown { leaveDiffPane() }
            secondaryEditorView.document = nil
            return
        }
        if !isDiffPaneShown {
            isDiffPaneShown = true
            diffHunkActions.isHidden = false
            // The overview ruler occupies the right edge. Give the diff a
            // persistent, reserved scrollbar beside it rather than an
            // auto-hiding overlay that is obscured by the ruler.
            scrollView.scrollerStyle = .legacy
            scrollView.autohidesScrollers = false
            secondaryScrollView.scrollerStyle = .legacy
            secondaryScrollView.autohidesScrollers = false
            updateWrapState()
            updateMinimap()
            removeMarkdownPreviewIfLoaded()
            secondaryScrollView.removeFromSuperview()
            editorSplitView.isVertical = true
            editorSplitView.addSubview(secondaryScrollView, positioned: .below, relativeTo: scrollView)
            editorSplitView.showsDiffGutter = true
        }
        secondaryEditorView.isReadOnly = true
        secondaryEditorView.diffSide = .removed
        editorView.diffSide = .added
        secondaryEditorView.diffPartner = editorView
        editorView.diffPartner = secondaryEditorView
        secondaryEditorView.document = base
        editorSplitView.layoutSubtreeIfNeeded()
        // A pane added to a split view starts with no width; give each side half.
        if secondaryScrollView.frame.width < 1 {
            editorSplitView.setPosition(editorSplitView.bounds.width / 2, ofDividerAt: 0)
        }
        editorSplitView.adjustSubviews()
        secondaryEditorView.requestDocumentRefresh(scrollCaret: false)
        layoutDiffHunkActions()
    }

    func leaveDiffPane() {
        isDiffPaneShown = false
        diffHunkActions.isHidden = true
        diffHunkActions.subviews.forEach { $0.removeFromSuperview() }
        scrollView.scrollerStyle = .overlay
        scrollView.autohidesScrollers = true
        secondaryScrollView.scrollerStyle = .overlay
        secondaryScrollView.autohidesScrollers = true
        updateWrapState()
        updateMinimap()
        diffMap = DiffLineMap()
        secondaryEditorView.isReadOnly = false
        secondaryEditorView.diffSide = nil
        editorView.diffSide = nil
        secondaryEditorView.diffPartner = nil
        editorView.diffPartner = nil
        editorView.updateDocumentWidth()
        secondaryEditorView.diffSpacers = []
        editorView.diffSpacers = []
        secondaryEditorView.lineTints = [:]
        editorView.lineTints = [:]
        secondaryScrollView.removeFromSuperview()
        editorSplitView.showsDiffGutter = false
        editorSplitView.isVertical = true
        editorSplitView.adjustSubviews()
    }

    /// Tints the changed lines of both panes and, the first time, scrolls to
    /// the first change.
    private func applyDiff(_ map: DiffLineMap) {
        diffMap = map
        secondaryEditorView.diffSpacers = map.oldSpacers
        editorView.diffSpacers = map.newSpacers
        editorView.lineTints = Dictionary(map.newChangedLines.map { ($0, Theme.diffAdded) }, uniquingKeysWith: { a, _ in a })
        secondaryEditorView.lineTints = Dictionary(map.oldChangedLines.map { ($0, Theme.diffRemoved) }, uniquingKeysWith: { a, _ in a })
        diffHunkActions.show(map: map, lineHeight: editorView.metrics.lineHeight)
        layoutDiffHunkActions()
        guard diffRevealPending else { return }
        diffRevealPending = false
        let lineHeight = editorView.metrics.lineHeight
        let first = max(0, (map.hunks.first?.newStart ?? 0) - 3)
        let visual = map.visualRow(forNew: first)
        scrollView.contentView.scroll(to: NSPoint(x: 0, y: CGFloat(visual) * lineHeight - scrollView.contentInsets.top))
        scrollView.reflectScrolledClipView(scrollView.contentView)
        syncDiffScroll(fromPrimary: true)
    }

    /// Keeps both panes on the same lines while either one scrolls.
    func installDiffScrollSync() {
        NotificationCenter.default.addObserver(
            forName: NSSplitView.didResizeSubviewsNotification, object: editorSplitView, queue: .main
        ) { [weak self] _ in
            onAppKitMainThread { self?.diffHunkActions.updatePositions() }
        }
        for (view, isPrimary) in [(scrollView, true), (secondaryScrollView, false)] {
            view.contentView.postsBoundsChangedNotifications = true
            NotificationCenter.default.addObserver(
                forName: NSView.boundsDidChangeNotification, object: view.contentView, queue: .main
            ) { [weak self] _ in
                onAppKitMainThread {
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
        let x = max(0, source.contentView.bounds.origin.x)
        isSyncingDiffScroll = true
        defer { isSyncingDiffScroll = false }
        target.contentView.scroll(to: NSPoint(x: x, y: y - inset))
        target.reflectScrolledClipView(target.contentView)
        layoutDiffHunkActions()
    }

    func layoutDiffHunkActions() {
        guard isDiffPaneShown else { return }
        diffHunkActions.layoutSubtreeIfNeeded()
        diffHunkActions.updatePositions()
    }

    func revertDiffHunk(at index: Int) {
        guard diffMap.hunks.indices.contains(index), let workspace, let tab = activeTab,
              let path = tab.relPath, let base = tab.diffBase else { return }
        let oldText = base.text()
        let text = tab.doc.text()
        // Diff calculation is asynchronous; never apply stale offsets to a newer edit.
        guard let current = try? workspace.gitDiffHunks(relPath: path, text: text),
              DiffLineMap(current).hunks == diffMap.hunks,
              let edit = Self.revertedDiffHunk(diffMap.hunks[index], base: oldText, working: text) else {
            refreshGitDiff()
            return
        }
        tab.doc.commitText(text: edit.text, replacement: edit.range)
        editorView.didChangeDocument()
        refreshGitDiff()
    }

    /// Replace exactly the changed lines, preserving the surrounding newline
    /// (including when either side has no final newline).
    nonisolated static func revertedDiffHunk(_ hunk: DiffLineMap.Hunk, base: String, working: String)
        -> (range: Utf16Range, text: String)? {
        func offsets(_ text: String) -> [Int] {
            var result = [0]
            for (offset, unit) in text.utf16.enumerated() where unit == 10 { result.append(offset + 1) }
            if !text.hasSuffix("\n"), !text.isEmpty { result.append(text.utf16.count) }
            return result
        }
        let old = offsets(base)
        let new = offsets(working)
        guard hunk.oldStart >= 0, hunk.oldLen >= 0, hunk.newStart >= 0, hunk.newLen >= 0,
              hunk.oldStart + hunk.oldLen < old.count,
              hunk.newStart + hunk.newLen < new.count else { return nil }
        // `.lines()` in the Git diff excludes the trailing empty element;
        // `offsets` includes it when the text ends in a newline.
        let oldCount = old.count - 1
        let newCount = new.count - 1
        guard hunk.oldStart + hunk.oldLen <= oldCount,
              hunk.newStart + hunk.newLen <= newCount else { return nil }
        let oldStart = old[hunk.oldStart]
        let oldEnd = hunk.oldStart + hunk.oldLen < oldCount
            ? old[hunk.oldStart + hunk.oldLen] : base.utf16.count
        var newStart = new[hunk.newStart]
        let newEnd = hunk.newStart + hunk.newLen < newCount
            ? new[hunk.newStart + hunk.newLen] : working.utf16.count
        // When lines were appended to a file without a trailing newline, the
        // inserted separator belongs to the insertion too.
        if hunk.oldLen == 0, hunk.oldStart == oldCount, !base.isEmpty,
           !base.hasSuffix("\n"), newStart > 0 {
            newStart -= 1
        }
        guard oldStart <= oldEnd, newStart <= newEnd else { return nil }
        let units = Array(base.utf16)
        var body = String(decoding: units[oldStart..<oldEnd], as: UTF16.self)
        if newEnd < working.utf16.count, !body.isEmpty, !body.hasSuffix("\n") { body += "\n" }
        if hunk.newLen == 0, newStart == working.utf16.count, !working.isEmpty,
           !working.hasSuffix("\n"), !body.isEmpty { body = "\n" + body }
        return (Utf16Range(location: UInt64(newStart), length: UInt64(newEnd - newStart)), body)
    }

    func stageDiffHunk(at index: Int) {
        guard diffMap.hunks.indices.contains(index), let workspace, let tab = activeTab,
              let path = tab.relPath else { return }
        let text = tab.doc.text()
        guard let current = try? workspace.gitDiffHunks(relPath: path, text: text),
              DiffLineMap(current).hunks == diffMap.hunks else {
            refreshGitDiff()
            return
        }
        let oldStart = UInt64(diffMap.hunks[index].oldStart)
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let failure: String? = {
                do { try workspace.gitStageHunk(relPath: path, text: text, oldStart: oldStart); return nil }
                catch { return "Could not stage change." }
            }()
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.statusLabel.stringValue = failure ?? "Staged change."
                if failure == nil { self.refreshGitStatus() }
            }
        }
    }

    /// Pure helper called from the utility queue in `refreshGitDiff()`, so it
    /// must not inherit the controller's main-actor isolation.
    nonisolated static func gitLineMarkers(_ changes: [GitLineChangeInfo]) -> [UInt64: GitLineChangeKind] {
        // A deletion marks the line above it. If that line carries an edit
        // too, the edit shows: it is the change on the line itself.
        let priority: [GitLineChangeKind: Int] = [.deleted: 0, .added: 1, .modified: 2]
        return changes.reduce(into: [:]) { markers, change in
            if let current = markers[change.line],
               priority[current, default: 0] >= priority[change.kind, default: 0] { return }
            markers[change.line] = change.kind
        }
    }
}
