import AppKit
import os

extension MainWindowController {
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
        for group in groups { group.pane?.editorView.setFont(Theme.editorFont) }
        applyViewToggles()
        reloadSettings(force: true)
    }

    /// Follows the View toggles saved in UserDefaults, which the Settings
    /// window writes as well as the View menu.
    private func applyViewToggles() {
        let defaults = UserDefaults.standard
        let minimap = defaults.object(forKey: "Minimap") as? Bool ?? true
        if minimap != minimapEnabled {
            minimapEnabled = minimap
            updateMinimap()
        }
        let wrap = defaults.bool(forKey: "WordWrap")
        if wrap != wordWrapEnabled {
            wordWrapEnabled = wrap
            updateWrapState()
        }
        let breadcrumbs = defaults.object(forKey: "ShowBreadcrumbs") as? Bool ?? true
        if breadcrumbs != breadcrumbBar.isEnabled {
            breadcrumbBar.isEnabled = breadcrumbs
            breadcrumbBar.show(symbols: documentSymbols, atLine: editorView.document?.primaryCursor().line ?? 0)
        }
        let sticky = defaults.object(forKey: "StickyScroll") as? Bool ?? true
        if sticky != stickyScopeView.isEnabled {
            stickyScopeView.isEnabled = sticky
            updateStickyScope()
        }
        // Redraw rows in place so folder icons follow the setting without
        // relisting or collapsing anything.
        let outline = explorer.outlineView
        if outline.numberOfRows > 0 {
            outline.reloadData(forRowIndexes: IndexSet(integersIn: 0..<outline.numberOfRows),
                               columnIndexes: IndexSet(integersIn: 0..<outline.numberOfColumns))
        }
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
            if let ws = self.workspace, ws.setExplorerExcludes(globs: settings.effectiveFilesExclude) {
                explorer.refresh()
            }
            autoSaveMode = settings.effectiveAutoSave
            autoSaveDelay = settings.effectiveAutoSaveDelay
            if autoSaveMode != .afterDelay { autoSaveWorkItem?.cancel(); autoSaveWorkItem = nil }
            for tab in allTabs {
                bridge.setIndent(document: tab.doc, useTabs: settings.useTabs, tabSize: settings.effectiveTabSize)
                applyTagEditing(to: tab)
            }
            editorView.renderWhitespace = settings.effectiveRenderWhitespace
            for group in groups { group.pane?.editorView.renderWhitespace = settings.effectiveRenderWhitespace }
            secondaryEditorView.renderWhitespace = settings.effectiveRenderWhitespace
            statusBar.show(document: activeTab?.doc, path: activeTab?.doc.path())
        } catch {
            settingsProblems = ["settings.json could not be read: \(error.localizedDescription)"]
            statusLabel.stringValue = settingsProblems[0]
            Log.ui.error("settings: \(String(describing: error), privacy: .public)")
        }
    }

    func updateSidebarContent() {
        switch activeSidebarTab {
        case .explorer:
            sourceControl.view.isHidden = true
            searchSidebar.view.isHidden = true
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
            sectionHeader.mode = .search
            sectionHeader.title = "SEARCH"
            activityBar.setActive(isSidebarVisible ? .search : nil)
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

    func showSearchTab() {
        activeSidebarTab = .search
        updateSidebarContent()
        setSidebarVisible(true)
        searchSidebar.reset()
        searchSidebar.focus()
    }

    func searchSidebarWorkspace(_ query: String, options: WorkspaceSearchOptions) {
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

    func openSearchSidebarMatch(_ match: WorkspaceSearchMatch) {
        guard let workspace else { return }
        openWorkspaceFile(workspace, FileEntry(name: (match.relPath as NSString).lastPathComponent, relPath: match.relPath, isDir: false), preview: false)
        activeTab?.doc.goToLine(line: match.line, col: match.column)
    }

    func searchSidebarReplaceAll(query: String, replacement: String, options: WorkspaceSearchOptions) {
        guard !query.isEmpty else { return }
        var totalReplaced = 0
        var seen: [DocumentHandle] = []
        for tab in allTabs where tab.isDocument {
            // A document shown in two groups is replaced once.
            if seen.contains(where: { $0 === tab.doc }) { continue }
            seen.append(tab.doc)
            _ = tab.doc.setFind(pattern: query, config: FindConfig(caseSensitive: options.caseSensitive, wholeWord: options.wholeWord, regex: options.regex))
            let count = tab.doc.replaceAll(replacement: replacement)
            if count > 0 {
                totalReplaced += Int(count)
                if tab === activeTab || tab.doc === activeTab?.doc {
                    editorView.didChangeDocument()
                }
                refreshPanes(showing: tab.doc)
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
