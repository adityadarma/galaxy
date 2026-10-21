import AppKit
import os

extension MainWindowController {
    // MARK: Changes on disk (M2-03)

    /// Compares open files with the disk: those under `changed` (paths
    /// relative to the workspace, from the watcher), or all of them when
    /// `nil`. Unedited tabs reload; edited ones are asked about when shown;
    /// deleted ones keep their text and are marked "(deleted)".
    func checkOpenFiles(changed: [String]? = nil) {
        let touched: (EditorTab) -> Bool = { tab in
            guard let changed else { return true }
            guard let rel = tab.relPath else { return false }
            return changed.contains { rel == $0 || rel.hasPrefix($0 + "/") }
        }
        var tabBarChanged = false
        var checked: [DocumentHandle] = []
        for tab in allTabs where tab.fileKey != nil && touched(tab) {
            // Tabs sharing a document share its state on disk: look once.
            if checked.contains(where: { $0 === tab.doc }) { continue }
            checked.append(tab.doc)
            defer {
                for other in allTabs where other !== tab && other.doc === tab.doc {
                    other.isDeletedOnDisk = tab.isDeletedOnDisk
                    other.hasDiskConflict = tab.hasDiskConflict
                    other.isEdited = tab.doc.isModified()
                }
            }
            let wasDeleted = tab.isDeletedOnDisk
            switch tab.doc.checkDisk() {
            case .unchanged:
                tab.isDeletedOnDisk = false
            case .deleted:
                tab.isDeletedOnDisk = true
                tab.hasDiskConflict = false
            case .changed:
                tab.isDeletedOnDisk = false
                if tab.doc === activeTab?.doc { editorView.flushPendingUpdates() }
                if tab.doc.isModified() {
                    tab.hasDiskConflict = true
                } else {
                    reloadFromDisk(tab)
                }
            }
            let edited = tab.doc.isModified()
            if wasDeleted != tab.isDeletedOnDisk || edited != tab.isEdited {
                tab.isEdited = edited
                tabBarChanged = true
            }
        }
        if tabBarChanged { refreshTabBar() }
        updateEditedState()
        resolveDiskConflictIfNeeded()
    }

    /// Asks about the active tab's conflict, once the window is key.
    func resolveDiskConflictIfNeeded() {
        guard let tab = activeTab, tab.hasDiskConflict, !isResolvingDiskConflict,
              !promptsOnlyWhenKey || window?.isKeyWindow == true else { return }
        isResolvingDiskConflict = true
        defer { isResolvingDiskConflict = false }
        tab.hasDiskConflict = false
        switch askDiskConflict(tab.title, false) {
        case .reload: reloadFromDisk(tab)
        case .keepMine: tab.doc.keepBufferOverDisk()
        }
        updateEditedState()
    }

    /// Reloads `tab` from disk as one undoable step.
    func reloadFromDisk(_ tab: EditorTab) {
        do {
            _ = try tab.doc.reloadFromDisk()
        } catch {
            return report(error, while: "reload \(tab.title)")
        }
        tab.hasDiskConflict = false
        tab.isDeletedOnDisk = false
        tab.isEdited = tab.doc.isModified()
        let message = "\(tab.title): reloaded from disk. ⌘Z restores the previous text."
        refreshPanes(showing: tab.doc)
        if tab === activeTab {
            editorView.requestDocumentRefresh(scrollCaret: false)
            editorView.scheduleChangeNotification()
            statusBar.show(document: tab.doc, path: tab.doc.path())
            statusLabel.stringValue = message
        } else {
            tab.status = message
        }
        refreshTabBar()
    }
}
