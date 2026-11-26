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

    /// Adds an in-memory item, then lets the user name it inline.
    private func createExplorerItem(folder: Bool) {
        let parent = explorer.targetFolder
        guard explorer.beginNewItem(parent: parent, folder: folder) else { return NSSound.beep() }
    }

    func createExplorerItem(parent: String, folder: Bool, name: String) {
        guard let workspace else { return }
        do {
            let rel = folder
                ? try workspace.createFolder(parentRelPath: parent, name: name)
                : try workspace.createFile(parentRelPath: parent, name: name)
            explorer.refresh()
            explorer.reveal(relPath: rel)
            updateFileFinder(changed: [rel])
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

    /// Copies the selected filesystem item as a file URL, so another Galaxy
    /// window can paste it into its own workspace.
    @objc func copyExplorerItem(_ sender: Any?) {
        guard let workspace, let entry = explorer.selectedEntry else { return NSSound.beep() }
        let url = URL(fileURLWithPath: workspace.root()).appendingPathComponent(entry.relPath)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.writeObjects([url as NSURL])
    }

    /// Copies filesystem URLs from any app window into the selected folder.
    @objc func pasteExplorerItem(_ sender: Any?) {
        guard workspace != nil else { return NSSound.beep() }
        let items = NSPasteboard.general.readObjects(
            forClasses: [NSURL.self],
            options: [.urlReadingFileURLsOnly: true]
        ) as? [URL] ?? []
        guard !items.isEmpty else { return NSSound.beep() }
        copyIntoExplorer(items, folder: explorer.targetFolder)
    }

    /// Copies files and folders (pasted, or dropped from Finder) into the
    /// workspace folder `folder` (`""` is the root).
    func copyIntoExplorer(_ items: [URL], folder: String) {
        guard let workspace else { return }
        let destination = URL(fileURLWithPath: workspace.root()).appendingPathComponent(folder)
        let manager = FileManager.default
        var pasted: [String] = []
        for source in items {
            let source = source.standardizedFileURL
            let target = Self.uniquePasteTarget(for: source, in: destination)
            do {
                try manager.copyItem(at: source, to: target)
                let rel = target.path.replacingOccurrences(of: workspace.root() + "/", with: "")
                pasted.append(rel)
            } catch {
                report(error, while: "paste \(source.lastPathComponent)")
            }
        }
        guard !pasted.isEmpty else { return }
        explorer.refresh()
        explorer.reveal(relPath: pasted[0])
        updateFileFinder(changed: pasted)
        refreshGitStatus()
    }

    /// `name.ext` becomes `name copy.ext`, then `name copy 2.ext`, when the
    /// name is taken in `folder`.
    static func uniquePasteTarget(for source: URL, in folder: URL) -> URL {
        let manager = FileManager.default
        let first = folder.appendingPathComponent(source.lastPathComponent).standardizedFileURL
        guard manager.fileExists(atPath: first.path) else { return first }
        let ext = source.pathExtension
        let base = source.deletingPathExtension().lastPathComponent
        var n = 1
        while true {
            let suffix = n == 1 ? " copy" : " copy \(n)"
            let name = ext.isEmpty ? base + suffix : base + suffix + "." + ext
            let candidate = folder.appendingPathComponent(name).standardizedFileURL
            if !manager.fileExists(atPath: candidate.path) { return candidate }
            n += 1
        }
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
        if event.keyCode == 9, mods == [.command] { // ⌘V pastes into the root when nothing is selected
            pasteExplorerItem(nil)
            return true
        }
        guard explorer.selectedEntry != nil else { return false }
        switch (event.keyCode, mods) {
        case (8, [.command]): // ⌘C
            copyExplorerItem(nil)
            return true
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
        add("Copy", #selector(copyExplorerItem(_:)), key: "c", mods: .command)
        add("Paste", #selector(pasteExplorerItem(_:)), key: "v", mods: .command)
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
