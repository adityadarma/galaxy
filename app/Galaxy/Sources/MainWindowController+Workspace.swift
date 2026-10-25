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

    func openWorkspace(at url: URL) throws {
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
        // Apply files.exclude before the explorer lists the new root.
        reloadSettings(force: true)
        explorer.setWorkspace(ws)
        updateSidebarContent()
        setSidebarVisible(true)
        if tabs.isEmpty { pathBar.show([]) }
        updateEditedState()
        statusLabel.stringValue = ""
        resetBottomPanelForNewWorkspace()
        if let session = Session.load(from: Session.workspaceURL(for: ws.root())) {
            restoreTabs(from: session)
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
        let doc = try sharedDocument(forFileKey: Self.fileKey(url.path) ?? "") ?? bridge.openDocument(path: url.path)
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
            let doc = try sharedDocument(forFileKey: Self.fileKey(path) ?? "") ?? workspace.openFile(relPath: entry.relPath)
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
        if let window { NSAlert(error: error).beginSheetModal(for: window) }
    }
}
