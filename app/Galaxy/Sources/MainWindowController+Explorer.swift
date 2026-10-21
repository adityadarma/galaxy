import AppKit
import os

extension MainWindowController {
    @objc func refreshExplorer(_ sender: Any?) {
        guard workspace != nil else { return NSSound.beep() }
        explorer.refresh()
    }

    @objc func collapseExplorerFolders(_ sender: Any?) {
        guard workspace != nil else { return NSSound.beep() }
        explorer.collapseAll()
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
        for tab in allTabs.reversed() where under(tab) {
            if tab.doc.isModified() {
                tab.isDeletedOnDisk = true
                _ = tab.doc.checkDisk()
            } else {
                closeTabAnywhere(tab)
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
        for tab in allTabs {
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
    func explorerKey(_ event: NSEvent) -> Bool {
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
}
