import AppKit
import os

extension MainWindowController {
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
        if activeIndex != index {
            snippetSession = nil
            snippetDocumentKey = nil
        }
        if let old = activeTab, old === shownTab, old !== tabs[index] {
            recordFindOrigin()
            noteLeaving(old)
            if autoSaveMode == .onFocusChange { autoSave(tab: old) }
        }
        if let old = activeTab, old === shownTab {
            old.scrollY = scrollView.contentView.bounds.origin.y
            old.status = statusLabel.stringValue
        }
        activeIndex = index
        let tab = tabs[index]
        shownTab = tab
        // Diagnostic revisions belong to a document. Two tabs can have the
        // same revision number, so force the viewport to discard decorations
        // drawn for the previously shown document.
        lastDiagnosticsRevision = nil
        refreshGitDiff()
        refreshBlame()
        tab.lastActiveAt = .now
        _ = tab.doc.resumeSyntax()
        let id = ObjectIdentifier(tab)
        tabOrderByUse.removeAll { $0 == id }
        tabOrderByUse.insert(id, at: 0)
        pathBar.show(tab.pathComponents)
        editorView.document = tab.doc
        parameterHintsRequest = nil
        scheduleParameterHints(after: 0)
        refreshDebugMarkers()
        applyDiffPane(for: tab)
        updateMarkdownPreview()
        let root = workspace?.root() ?? (tab.doc.path() as NSString).deletingLastPathComponent
        if tab.isDocument {
            let language = tab.doc.syntaxLanguage() ?? StatusBar.language(for: tab.doc.path())
            bridge.registerDocument(document: tab.doc, language: language, root: root)
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
        showEmptyEditor(false)
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

    func evictInactiveSyntax(now: Date, includeRecentlyInactive: Bool) {
        let visible = groups.compactMap(\.activeTab).map(\.doc)
        for tab in allTabs where !visible.contains(where: { $0 === tab.doc }) && (includeRecentlyInactive || now.timeIntervalSince(tab.lastActiveAt) >= Self.syntaxEvictionInterval) {
            _ = tab.doc.suspendSyntax()
        }
    }

    /// ⌃Tab: the tab shown before this one. Pressed again, it goes back.
    @objc func selectLastUsedTab(_ sender: Any?) {
        let live = Set(allTabs.map(ObjectIdentifier.init))
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
        if let tab = menuTab(sender) {
            if let i = tabs.firstIndex(where: { $0 === tab }) { closeTab(at: i) }
        } else if let i = activeIndex {
            closeTab(at: i)
        } else {
            window?.performClose(nil)
        }
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
        // A document that another tab still shows is not closed: no prompt.
        let shared = isDocumentShared(tabs[index])
        if tabs[index].doc.isModified(), !shared {
            if activeIndex != index { selectTab(index) }
            guard confirmDiscardActive() else { return false }
        }
        let wasActive = activeIndex == index
        // A closed file can be reopened by Back; an Untitled buffer cannot.
        if tabs[index].fileKey == nil {
            if !shared { navHistory.forget(key: tabs[index].navKey) }
        } else if wasActive {
            noteLeaving(tabs[index])
        }
        let closedID = ObjectIdentifier(tabs[index])
        tabOrderByUse.removeAll { $0 == closedID }
        tabs.remove(at: index)
        if wasActive { shownTab = nil }
        if let a = activeIndex {
            activeIndex = a > index ? a - 1 : (a == index ? nil : a)
        }
        if tabs.isEmpty {
            // An empty group closes, unless it is the last one.
            if groups.count > 1 { removeGroup(activeGroup) } else { showNoDocument() }
        } else if wasActive {
            selectTab(tabToShowAfterClosing(at: index), fromTabBar: true)
        } else {
            refreshTabBar()
            updateEditedState()
        }
        return true
    }

    /// The tab to show once the active tab at `index` is gone: the one used
    /// last in this group (the place a jump started from), else the
    /// neighbour that took its slot.
    private func tabToShowAfterClosing(at index: Int) -> Int {
        for id in tabOrderByUse {
            if let i = tabs.firstIndex(where: { ObjectIdentifier($0) == id }) { return i }
        }
        return min(index, tabs.count - 1)
    }

    /// Pinned tabs are always the first ones.
    private var pinnedCount: Int { tabs.prefix { $0.isPinned }.count }

    func moveTab(from: Int, to: Int) {
        guard tabs.indices.contains(from), tabs.indices.contains(to) else { return }
        // A tab stays on its side of the pinned boundary.
        let pinned = pinnedCount
        let to = tabs[from].isPinned ? min(to, pinned - 1) : max(to, pinned)
        guard from != to else { return }
        let active = activeTab
        tabs.insert(tabs.remove(at: from), at: to)
        activeIndex = active.flatMap { a in tabs.firstIndex { $0 === a } }
        refreshTabBar()
    }

    func tabItems(for list: [EditorTab]) -> [TabBar.Item] {
        let statusMap = explorer.gitStatusMap
        let gitPaths = explorer.gitChangedPaths
        return list.map { tab in
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
                isGitGraph: tab.isGitGraph,
                isPinned: tab.isPinned
            )
        }
    }

    func refreshTabBar() {
        tabBar.setPreviewButton(visible: canPreviewMarkdown, active: isMarkdownPreviewShown)
        tabBar.set(items: tabItems(for: tabs), selected: activeIndex)
        statusBar.setGitGraphVisible(isGitGraphTabShown)
        refreshGroupPanes()
    }

    /// Adds `tab` after the active one, or puts it in place of tab
    /// `replacing`, and shows it.
    func applyTagEditing(to tab: EditorTab) {
        bridge.setTagEditing(document: tab.doc, autoClose: autoClosingTags, autoRename: linkedEditing)
    }

    func add(_ tab: EditorTab, replacing: Int?) {
        applyTagEditing(to: tab)
        if let r = replacing {
            if activeIndex == r {
                noteLeaving(tabs[r])
                activeIndex = nil
            }
            tabs[r] = tab
            selectTab(r)
        } else {
            // After the active tab, but never among the pinned ones.
            let at = max(activeIndex.map { $0 + 1 } ?? tabs.count, pinnedCount)
            tabs.insert(tab, at: at)
            if let a = activeIndex, a >= at { activeIndex = a + 1 }
            selectTab(at)
        }
    }

    /// Where a newly opened file goes: over the current preview tab, or
    /// over an untouched empty "Untitled" tab, or (nil) a new tab.
    func slotForOpening(preview: Bool) -> Int? {
        if preview, let i = tabs.firstIndex(where: { $0.isPreview && !$0.isEdited }) { return i }
        guard let i = activeIndex, tabs[i].isDocument else { return nil }
        let doc = tabs[i].doc
        return doc.path().isEmpty && !doc.isModified() && doc.lenBytes() == 0 ? i : nil
    }

    /// Same file, however it was reached (symlinks, /private/var).
    static func fileKey(_ path: String) -> String? {
        path.isEmpty ? nil : URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL.path
    }

    /// The plain tab of a file, or with `diff` its working-tree diff tab.
    func tabIndex(forPath path: String, diff: Bool = false) -> Int? {
        guard let key = Self.fileKey(path) else { return nil }
        return tabs.firstIndex { $0.fileKey == key && ($0.diffBase != nil) == diff }
    }

    /// Shows the shortcut hints over the editor, or hides them.
    private func showEmptyEditor(_ visible: Bool) {
        if visible { emptyEditor.reload() }
        emptyEditor.isHidden = !visible
    }

    /// Editor with no document: after the last tab is closed.
    func showNoDocument() {
        activeIndex = nil
        shownTab = nil
        if isFindVisible { closeFind(nil) }
        showEditorTerminal(for: nil)
        showEditorGitGraph(for: nil)
        showEmptyEditor(true)
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
}
