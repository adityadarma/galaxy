import AppKit
import os

extension MainWindowController {
    // MARK: Saving

    /// Window title, and the unsaved dots on the tab, the path bar and the
    /// window's close button. Runs at most once per frame while typing.
    /// The first edit keeps a preview tab open.
    func updateEditedState() {
        let title = activeTab?.windowTitle ?? workspace?.name() ?? "Galaxy"
        if window?.title != title { window?.title = title }
        if let i = activeIndex {
            let tab = tabs[i]
            let edited = tab.doc.isModified()
            if edited != tab.isEdited || (edited && tab.isPreview) {
                tab.isEdited = edited
                if edited { tab.isPreview = false }
                refreshTabBar()
            }
            pathBar.setEdited(edited)
        } else {
            pathBar.setEdited(false)
        }
        // A document shown in several groups is edited in all of them.
        var changed = false
        if let i = activeIndex {
            let tab = tabs[i]
            for other in allTabs where other !== tab && other.isDocument && other.doc === tab.doc && other.isEdited != tab.isEdited {
                other.isEdited = tab.isEdited
                changed = true
            }
        }
        if changed { refreshTabBar() }
        window?.isDocumentEdited = allTabs.contains { $0.isEdited }
        explorer.dirtyPaths = Set(allTabs.compactMap { $0.isEdited ? $0.relPath : nil })
    }

    private var documentName: String {
        guard let doc = editorView.document else { return "Untitled" }
        let path = doc.path()
        return path.isEmpty ? "Untitled" : URL(fileURLWithPath: path).lastPathComponent
    }

    @objc func saveDocument(_ sender: Any?) {
        _ = save()
    }

    @objc func saveDocumentAs(_ sender: Any?) {
        _ = saveAs()
    }

    /// Saves to the current path, or asks for one. `false` if it failed or
    /// was cancelled.
    @discardableResult
    func save() -> Bool {
        guard activeTab?.isDocument != false, let doc = editorView.document else { return true }
        if doc.path().isEmpty { return saveAs() }
        if doc.checkDisk() == .changed, let tab = activeTab {
            tab.hasDiskConflict = false
            if askDiskConflict(documentName, true) == .reload {
                reloadFromDisk(tab)
                updateEditedState()
                return false
            }
        }
        do {
            try doc.save()
        } catch {
            report(error, while: "save \(documentName)")
            return false
        }
        if let tab = activeTab, tab.isDeletedOnDisk {
            tab.isDeletedOnDisk = false
            refreshTabBar()
        }
        savedStatus(doc)
        return true
    }

    @discardableResult
    func saveAs() -> Bool {
        guard activeTab?.isDocument != false, let doc = editorView.document, let url = askSaveLocation(documentName) else { return false }
        do {
            try doc.saveAs(path: url.path)
        } catch {
            report(error, while: "save \(url.lastPathComponent)")
            return false
        }
        if let tab = activeTab {
            tab.title = url.lastPathComponent
            tab.windowTitle = url.lastPathComponent
            tab.pathComponents = [url.lastPathComponent]
            tab.fileKey = Self.fileKey(doc.path())
            tab.relPath = relativePath(doc.path())
            pathBar.show(tab.pathComponents)
            refreshTabBar()
        }
        savedStatus(doc)
        return true
    }

    private func savedStatus(_ doc: DocumentHandle) {
        statusLabel.stringValue = "\(documentName): saved, \(doc.lineCount()) lines, \(doc.lenBytes()) bytes"
        updateEditedState()
        // Saving keybindings.json applies it right away.
        if Self.fileKey(doc.path()) == Self.fileKey(keybindingsURL.path) {
            reloadKeybindings(force: true)
            if keybindings.problems.isEmpty { statusLabel.stringValue = "Keyboard shortcuts updated." }
        }
        if isSettingsDocument(doc) {
            reloadSettings(force: true)
            if settingsProblems.isEmpty { statusLabel.stringValue = "Settings updated." }
        }
    }

    /// `true` when it is safe to close the window or quit: every tab with
    /// unsaved changes was saved or discarded. Stops at the first Cancel,
    /// with that tab shown.
    func confirmDiscardChanges() -> Bool {
        confirmDiscardAllGroups()
    }

    /// Asks about the active tab only.
    func confirmDiscardActiveTab() -> Bool { confirmDiscardActive() }

    func confirmDiscardActive() -> Bool {
        guard let doc = editorView.document, doc.isModified() else { return true }
        switch askSaveChoice(documentName) {
        case .save: return save()
        case .discard: return true
        case .cancel: return false
        }
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        confirmDiscardChanges()
    }

    func scheduleAutoSave() {
        guard autoSaveMode == .afterDelay else { return }
        autoSaveWorkItem?.cancel()
        let item = DispatchWorkItem { [weak self] in
            Task { @MainActor [weak self] in self?.autoSaveModifiedDocuments() }
        }
        autoSaveWorkItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + autoSaveDelay, execute: item)
    }

    func autoSaveModifiedDocuments() {
        autoSaveWorkItem = nil
        editorView.flushPendingUpdates()
        for tab in allTabs where tab.isDocument && tab.doc.isModified() && !tab.doc.path().isEmpty {
            autoSave(tab: tab)
        }
        updateEditedState()
    }

    func autoSave(tab: EditorTab) {
        guard tab.doc.isModified(), !tab.doc.path().isEmpty else { return }
        do {
            try tab.doc.save()
            tab.isEdited = false
        } catch {
            Log.ui.error("auto-save failed for \(tab.doc.path(), privacy: .public): \(String(describing: error), privacy: .public)")
        }
    }
}
