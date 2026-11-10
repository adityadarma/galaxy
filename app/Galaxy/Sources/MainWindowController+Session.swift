import AppKit
import os

extension MainWindowController {
    // MARK: Session (M2-11)

    var isClosingForApplicationTermination: Bool { isTerminatingApplication }

    /// The folder, the file tabs with caret and scroll, and the active tab.
    func currentSession() -> Session {
        var session = Session()
        session.workspace = workspace?.root()
        session.sidebarVisible = isSidebarVisible
        var saved: [Session.Group] = []
        for group in groups {
            var out = Session.Group()
            for tab in group.tabs where tab.isDocument {
                let path = tab.doc.path()
                guard !path.isEmpty, !tab.isDeletedOnDisk else { continue }
                let loc = location(of: tab)
                let live = group === activeGroup && tab === shownTab
                let y = live ? scrollView.contentView.bounds.origin.y : tab.scrollY
                if tab === group.activeTab { out.active = out.tabs.count }
                out.tabs.append(Session.Tab(path: path, isPreview: tab.isPreview, line: loc.line, col: loc.col,
                                            scrollY: y.map(Double.init), isPinned: tab.isPinned ? true : nil,
                                            isDiff: tab.diffBase != nil ? true : nil))
            }
            saved.append(out)
        }
        session.tabs = saved[0].tabs
        session.active = saved[0].active
        if saved.count > 1 {
            session.extraGroups = Array(saved.dropFirst())
            session.activeGroup = activeGroupIndex
            session.groupsVertical = groupsSplitView.isVertical
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

    func saveWorkspaceSession() {
        guard let workspace else { return }
        do {
            try currentSession().save(to: Session.workspaceURL(for: workspace.root()))
        } catch {
            Log.app.error("saving workspace session failed: \(String(describing: error), privacy: .public)")
        }
    }

    func saveWorkspaceSessionForQuit() { saveWorkspaceSession() }

    /// `tabs` without a second entry for the same file (and the same kind of
    /// tab: plain or diff), with `active` following the entry that was kept.
    /// Older sessions saved a diff tab as a plain one, so they hold both.
    static func deduplicatedTabs(_ tabs: [Session.Tab], active: Int?) -> (tabs: [Session.Tab], active: Int?) {
        var kept: [String: Int] = [:]
        var out: [Session.Tab] = []
        var newActive: Int?
        for (i, tab) in tabs.enumerated() {
            let key = (fileKey(tab.path) ?? tab.path) + (tab.isDiff == true ? "#diff" : "")
            if let existing = kept[key] {
                if i == active { newActive = existing }
                continue
            }
            kept[key] = out.count
            if i == active { newActive = out.count }
            out.append(tab)
        }
        return (out, newActive)
    }

    /// `session` with each group's duplicate tabs removed.
    static func deduplicatedSession(_ session: Session) -> Session {
        var session = session
        (session.tabs, session.active) = deduplicatedTabs(session.tabs, active: session.active)
        session.extraGroups = session.extraGroups?.map { group in
            var group = group
            (group.tabs, group.active) = deduplicatedTabs(group.tabs, active: group.active)
            return group
        }
        return session
    }

    func restoreTabs(from session: Session) {
        let session = Self.deduplicatedSession(session)
        restoreTabs(session.tabs, active: session.active)
        guard let extra = session.extraGroups, !extra.isEmpty else { return }
        let first = activeGroup
        if let vertical = session.groupsVertical { groupsSplitView.isVertical = vertical }
        for saved in extra where !saved.tabs.isEmpty {
            let group = makeGroup(at: groups.count)
            // The new group needs a tab before it can take focus.
            guard let tab = openRestoredTab(saved.tabs[0]) else { groupsSplitView.equalize(); removeEmptyGroup(group); continue }
            group.tabs = [tab]
            group.activeIndex = 0
            groupsSplitView.equalize()
            activateGroup(group)
            restoreTabs(Array(saved.tabs.dropFirst()), active: saved.active.map { max(0, $0 - 1) }, firstRestored: tab, firstSaved: saved.tabs[0], activeIsFirst: saved.active == 0)
        }
        let wanted = session.activeGroup ?? 0
        if groups.indices.contains(wanted) { activateGroup(groups[wanted]) } else { activateGroup(first) }
    }

    private func removeEmptyGroup(_ group: EditorGroup) {
        guard let i = groups.firstIndex(where: { $0 === group }), groups.count > 1, group !== activeGroup else { return }
        group.slot.removeFromSuperview()
        groups.remove(at: i)
        if i <= activeGroupIndex { activeGroupIndex = max(0, activeGroupIndex - 1) }
    }

    /// A tab for a saved file, not yet in any group. It shares the buffer
    /// of the same file in another group.
    private func openRestoredTab(_ saved: Session.Tab) -> EditorTab? {
        guard FileManager.default.fileExists(atPath: saved.path) else { return nil }
        let key = Self.fileKey(saved.path)
        let name = (saved.path as NSString).lastPathComponent
        do {
            let doc: DocumentHandle
            var components = [name]
            var relPath: String?
            if let key, let existing = sharedDocument(forFileKey: key) {
                doc = existing
            } else if let workspace, let rel = relativePath(saved.path) {
                doc = try workspace.openFile(relPath: rel)
            } else {
                doc = try bridge.openDocument(path: saved.path)
            }
            if workspace != nil, let rel = relativePath(saved.path) {
                relPath = rel
                components = rel.split(separator: "/").map(String.init)
            }
            let tab = EditorTab(doc: doc, title: name, pathComponents: components, windowTitle: name,
                                isPreview: false, status: "")
            tab.fileKey = key
            tab.relPath = relPath
            tab.isPinned = saved.isPinned == true
            if saved.isDiff == true, let workspace, let relPath,
               let base = (try? workspace.gitBaseDocument(relPath: relPath)) ?? nil {
                tab.diffBase = base
            }
            tab.scrollY = saved.scrollY.map { CGFloat($0) }
            tab.isEdited = doc.isModified()
            applyTagEditing(to: tab)
            doc.goToLine(line: saved.line, col: saved.col)
            return tab
        } catch {
            return nil
        }
    }

    private func restoreTabs(_ savedTabs: [Session.Tab], active: Int?,
                             firstRestored: EditorTab? = nil, firstSaved: Session.Tab? = nil,
                             activeIsFirst: Bool = false) {
        var restored: [(EditorTab, CGFloat?)] = []
        var target: EditorTab? = activeIsFirst ? firstRestored : nil
        for (i, saved) in savedTabs.enumerated() {
            guard FileManager.default.fileExists(atPath: saved.path) else { continue }
            if let workspace, let rel = relativePath(saved.path) {
                if saved.isDiff == true {
                    openWorkspaceDiff(workspace, relPath: rel)
                } else {
                    let entry = FileEntry(name: (rel as NSString).lastPathComponent, relPath: rel, isDir: false)
                    openWorkspaceFile(workspace, entry, preview: saved.isPreview)
                }
            } else {
                try? openFile(at: URL(fileURLWithPath: saved.path))
            }
            // A diff with no committed version opens as a plain tab.
            guard let index = tabIndex(forPath: saved.path, diff: saved.isDiff == true)
                ?? tabIndex(forPath: saved.path) else { continue }
            let tab = tabs[index]
            tab.doc.goToLine(line: saved.line, col: saved.col)
            if saved.isPinned == true { setPinned(true, tab: tab) }
            restored.append((tab, saved.scrollY.map { CGFloat($0) }))
            if i == active { target = tab }
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

    /// A large saved tab set must yield to AppKit between small batches, or the
    /// just-shown launch window appears frozen while every editor is created.
    func restoreTabsIncrementally(
        _ savedTabs: [Session.Tab],
        active: Int?,
        workspace: Workspace
    ) {
        let activeIndex = active.flatMap { savedTabs.indices.contains($0) ? $0 : nil }
        let candidates = (activeIndex.map { [$0] } ?? []) + savedTabs.indices.filter { $0 != activeIndex }
        guard let (firstIndex, firstTab) = candidates.lazy.compactMap({ index in
            self.openRestoredTab(savedTabs[index]).map { (index, $0) }
        }).first else { return }

        tabs.append(firstTab)
        selectTab(0)

        let pending = savedTabs.enumerated().filter { $0.offset != firstIndex }
        let batchSize = 2
        var next = 0

        func restoreNextBatch() {
            guard self.workspace === workspace, next < pending.count else { return }
            let end = min(next + batchSize, pending.count)
            for (savedIndex, saved) in pending[next..<end] {
                guard let tab = self.openRestoredTab(saved) else { continue }
                // The user may have opened this file while the batches ran. A
                // diff without a committed version came back as a plain tab.
                if self.tabIndex(forPath: saved.path, diff: tab.diffBase != nil) != nil { continue }
                let insertionIndex = min(savedIndex, self.tabs.count)
                self.tabs.insert(tab, at: insertionIndex)
                if let activeIndex = self.activeIndex, activeIndex >= insertionIndex {
                    self.activeIndex = activeIndex + 1
                }
            }
            self.refreshTabBar()
            next = end
            if next < pending.count {
                DispatchQueue.main.async(execute: restoreNextBatch)
            }
        }

        DispatchQueue.main.async(execute: restoreNextBatch)
    }

    func saveSessionForQuit(otherWindows: [Session] = []) {
        if !didCloseWindow {
            var session = currentSession()
            session.otherWindows = otherWindows.isEmpty ? nil : otherWindows
            session.fullScreen = window?.styleMask.contains(.fullScreen) == true
            do {
                try session.save(to: sessionURL)
            } catch {
                Log.app.error("saving session failed: \(String(describing: error), privacy: .public)")
            }
            if let window { UserDefaults.standard.set(window.styleMask.contains(.fullScreen), forKey: Self.fullScreenKey) }
        }
    }

    func prepareForApplicationQuit() { isTerminatingApplication = true }

    /// Reopens what `session` had: the folder if it still exists, and the
    /// tabs whose files still exist, with caret and scroll. Returns
    /// `false` if nothing could be restored.
    @discardableResult
    func restore(_ session: Session, incrementally: Bool = false) -> Bool {
        isNavigating = true
        defer { isNavigating = false }
        var restoredWorkspace = false
        var restoresWorkspaceTabs = false
        if let root = session.workspace {
            var isDir: ObjCBool = false
            if FileManager.default.fileExists(atPath: root, isDirectory: &isDir), isDir.boolValue {
                do {
                    if let workspaceSession = Session.load(from: Session.workspaceURL(for: root)) {
                        restoresWorkspaceTabs = !workspaceSession.tabs.isEmpty
                            || !(workspaceSession.extraGroups?.isEmpty ?? true)
                    }
                    try openWorkspace(at: URL(fileURLWithPath: root), restoresTabsIncrementally: incrementally && restoresWorkspaceTabs)
                    restoredWorkspace = true
                } catch {
                    Log.app.error("restoring workspace failed: \(String(describing: error), privacy: .public)")
                }
            }
        }
        // Opening the folder already restored its own saved session. Doing
        // it twice would add the extra groups a second time.
        if allTabs.isEmpty, !restoresWorkspaceTabs { restoreTabs(from: session) }
        if restoredWorkspace { setSidebarVisible(session.sidebarVisible) }
        Log.app.info("restored session: workspace \(restoredWorkspace), \(self.allTabs.count) tabs")
        return restoredWorkspace || !allTabs.isEmpty
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
        debugger.stop()
        testRunner.stopAll()
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
        keyMonitor = nil
        workspaceWatch?.stop()
        workspaceWatch = nil
        onWindowClosed?()
    }
}
