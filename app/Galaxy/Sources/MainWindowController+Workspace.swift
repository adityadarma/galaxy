import AppKit
import os

extension MainWindowController {
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

    func openWorkspace(at url: URL, restoresTabsIncrementally: Bool = false,
                       rememberRecent: Bool = true) throws {
        if openExistingWorkspace?(url) == true { return }
        // A new folder is a new project session. Do not carry files from the
        // previous workspace into the new tab bar.
        guard confirmDiscardAllGroups() else { return }
        saveWorkspaceSession()
        // Shells belong to the old folder; VSCode reloads the window here.
        resetGroups()
        snippetSession = nil
        snippetDocumentKey = nil
        showNoDocument()
        // Set the root before Core starts detached scan workers. A fast worker
        // may emit its start event before `openWorkspace` returns.
        statusBar.resetWorkspaceScan(root: url.path)
        workspaceIndexState = (url.path, 0, 0)
        let ws: Workspace
        do {
            ws = try bridge.core.openWorkspace(path: url.path)
        } catch {
            workspaceIndexState = nil
            throw error
        }
        workspace = ws
        gitStatusSnapshot = nil
        titleBar.setProjectTitle(ws.name())
        if rememberRecent { Self.rememberProject(url.path) }
        workspaceSettingsModified = nil
        fileFinder = nil
        recentFiles = []
        if isQuickOpenVisible { closeGoToFile() }
        loadedQuickOpen?.finder = nil
        // Apply files.exclude before the explorer lists the new root.
        reloadSettings(force: true)
        explorer.setWorkspace(ws)
        updateSidebarContent()
        setSidebarVisible(true)
        if tabs.isEmpty { pathBar.show([]) }
        updateEditedState()
        statusLabel.stringValue = ""
        resetBottomPanelForNewWorkspace()
        if let session = Session.load(from: Session.workspaceURL(for: ws.root())).map(Self.deduplicatedSession) {
            if restoresTabsIncrementally, session.extraGroups?.isEmpty ?? true {
                restoreTabsIncrementally(session.tabs, active: session.active, workspace: ws)
            } else {
                restoreTabs(from: session)
            }
        }
        // Git, filesystem watching, and the Go to File index are not needed
        // for the restored editor frame. Start them after AppKit presents it.
        DispatchQueue.main.async { [weak self, weak ws] in
            guard let self, let ws, self.workspace === ws else { return }
            self.rebuildFileFinder()
            self.startWatching(ws)
            self.refreshGitStatus()
        }
    }

    /// The panel keeps its place (open, tab, height), but what it shows
    /// starts over in the new folder: fresh shells there, no old debug run.
    private func resetBottomPanelForNewWorkspace() {
        let hadFocus = terminalPanel.owns(window?.firstResponder)
        for terminal in terminals { terminal.handle.kill() }
        terminals.removeAll()
        activeTerminalIndex = 0
        nextTerminalGroup = 0
        terminalPanel.show(terminals: [], activeIndex: 0)
        refreshTerminalPicker()
        if debugger.state != .idle { debugger.stop() }
        debugConsole.clear()
        guard isBottomPanelVisible else { return }
        showBottomPanel(tab: bottomPanelTab)
        if !hadFocus, bottomPanelTab == .terminal { window?.makeFirstResponder(editorView) }
    }

    @objc func openRecentProject(_ sender: Any?) {
        guard let path = (sender as? NSMenuItem)?.representedObject as? String else { return }
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory), isDirectory.boolValue else {
            notify("Recent project no longer exists: \(path)", severity: .warning)
            return
        }
        open(url: URL(fileURLWithPath: path))
    }

    @objc func openRecentFile(_ sender: Any?) {
        guard let path = (sender as? NSMenuItem)?.representedObject as? String else { return }
        openRecentPath(path)
    }

    func openRecentPath(_ path: String) {
        guard FileManager.default.fileExists(atPath: path) else {
            notify("Recently opened item no longer exists: \(path)", severity: .warning)
            return
        }
        open(url: URL(fileURLWithPath: path))
    }

    static func recentProjectPaths() -> [String] {
        UserDefaults.standard.stringArray(forKey: "RecentProjectPaths") ?? []
    }

    static func recentFilePaths() -> [String] {
        UserDefaults.standard.stringArray(forKey: "RecentFilePaths") ?? []
    }

    static func forgetRecentPath(_ path: String, isFolder: Bool) {
        let key = isFolder ? "RecentProjectPaths" : "RecentFilePaths"
        let identity = fileKey(path) ?? path
        UserDefaults.standard.set((UserDefaults.standard.stringArray(forKey: key) ?? [])
            .filter { fileKey($0) != identity }, forKey: key)
        MainMenu.refreshRecentProjects()
    }

    static func clearRecentlyOpened() {
        UserDefaults.standard.removeObject(forKey: "RecentProjectPaths")
        UserDefaults.standard.removeObject(forKey: "RecentFilePaths")
        MainMenu.refreshRecentProjects()
    }

    @objc func clearRecentlyOpened(_ sender: Any?) { Self.clearRecentlyOpened() }

    private static func rememberProject(_ path: String) {
        let key = fileKey(path) ?? path
        var paths = recentProjectPaths().filter { fileKey($0) != key }
        paths.insert(path, at: 0)
        UserDefaults.standard.set(Array(paths.prefix(100)), forKey: "RecentProjectPaths")
        MainMenu.refreshRecentProjects()
    }

    static func rememberFile(_ path: String) {
        let key = fileKey(path) ?? path
        var paths = recentFilePaths().filter { fileKey($0) != key }
        paths.insert(path, at: 0)
        UserDefaults.standard.set(Array(paths.prefix(100)), forKey: "RecentFilePaths")
        MainMenu.refreshRecentProjects()
    }

    /// Opens a file in its own tab (or shows its tab if already open).
    func openFile(at url: URL, rememberRecent: Bool = true) throws {
        if MediaKind.kind(for: url) != nil {
            openMediaFile(at: url)
            if rememberRecent { Self.rememberFile(url.path) }
            return
        }
        if let i = tabIndex(forPath: url.path) {
            tabs[i].isPreview = false
            selectTab(i)
            if rememberRecent { Self.rememberFile(url.path) }
            return
        }
        let doc = try sharedDocument(forFileKey: Self.fileKey(url.path) ?? "") ?? bridge.openDocument(path: url.path)
        let name = url.lastPathComponent
        let branch = gitStatusSnapshot?.branch
        let tab = EditorTab(doc: doc, title: name, pathComponents: [name], windowTitle: name,
                            isPreview: false, status: describe(branch: branch))
        tab.fileKey = Self.fileKey(doc.path())
        tab.relPath = relativePath(doc.path())
        add(tab, replacing: slotForOpening(preview: false))
        if rememberRecent { Self.rememberFile(url.path) }
    }

    /// Single click in the explorer opens a preview tab, which the next
    /// single click replaces; a double click or an edit keeps it.
    func openWorkspaceFile(_ workspace: Workspace, _ entry: FileEntry, preview: Bool,
                           rememberRecent: Bool = true) {
        let path = URL(fileURLWithPath: workspace.root()).appendingPathComponent(entry.relPath).path
        let fileURL = URL(fileURLWithPath: path)
        if MediaKind.kind(for: fileURL) != nil {
            openMediaFile(at: fileURL, preview: preview)
            if !preview && rememberRecent { Self.rememberFile(path) }
            return
        }
        if let i = tabIndex(forPath: path) {
            if !preview { tabs[i].isPreview = false }
            selectTab(i)
            if !preview && rememberRecent { Self.rememberFile(path) }
            window?.makeFirstResponder(editorView)
            return
        }
        do {
            let doc = try sharedDocument(forFileKey: Self.fileKey(path) ?? "") ?? workspace.openFile(relPath: entry.relPath)
            let branch = gitStatusSnapshot?.branch
            let tab = EditorTab(
                doc: doc, title: entry.name,
                pathComponents: entry.relPath.split(separator: "/").map(String.init),
                windowTitle: "\(entry.name) — \(workspace.name())", isPreview: preview,
                status: describe(branch: branch))
            tab.fileKey = Self.fileKey(doc.path())
            tab.relPath = entry.relPath
            add(tab, replacing: slotForOpening(preview: preview))
            if !preview && rememberRecent { Self.rememberFile(path) }
            window?.makeFirstResponder(editorView)
        } catch {
            report(error, while: "open \(entry.relPath)")
        }
    }

    /// Lists the folder represented by a path-bar segment. Folder entries
    /// stay lazy as submenus; choosing a file opens it in a regular tab.
    func pathBarMenu(for component: Int) -> NSMenu? {
        guard let workspace, component >= 0, component < pathBar.components.count - 1 else {
            return nil
        }
        let folder = pathBar.components.prefix(component + 1).joined(separator: "/")
        return pathBarMenu(workspace: workspace, folder: folder)
    }

    private func pathBarMenu(workspace: Workspace, folder: String) -> NSMenu? {
        let entries: [FileEntry]
        do {
            entries = try workspace.listDir(relPath: folder)
        } catch {
            Log.ui.error("list path bar folder \(folder, privacy: .public) failed: \(String(describing: error), privacy: .public)")
            return nil
        }
        let menu = NSMenu(title: folder)
        for entry in entries {
            let item = NSMenuItem(title: entry.name, action: nil, keyEquivalent: "")
            item.image = FileGlyph.for(name: entry.name).image
            let relPath = entry.relPath
            if entry.isDir {
                item.submenu = pathBarMenu(workspace: workspace, folder: relPath)
            } else {
                item.target = self
                item.action = #selector(openPathBarFile(_:))
                item.representedObject = entry
            }
            menu.addItem(item)
        }
        return menu
    }

    @objc private func openPathBarFile(_ sender: NSMenuItem) {
        guard let workspace, let entry = sender.representedObject as? FileEntry else { return }
        openWorkspaceFile(workspace, entry, preview: false)
    }

    private func describe(branch: String?) -> String {
        // The branch is shown once in the left status-bar Git label.
        return branch == nil ? "No Git branch" : ""
    }

    func report(_ error: Error, while action: String) {
        Log.ui.error("\(action, privacy: .public) failed: \(String(describing: error), privacy: .public)")
        notify("Could not \(action). \(error.localizedDescription)", severity: .error)
    }
}
