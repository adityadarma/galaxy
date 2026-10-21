import AppKit
import os

extension MainWindowController {
    // MARK: Go to File (M2-07)

    var isWorkspaceSearchVisible: Bool { loadedWorkspaceSearch.map { !$0.isHidden } ?? false }

    private var workspaceSearch: WorkspaceSearch {
        if let panel = loadedWorkspaceSearch { return panel }
        let panel = WorkspaceSearch()
        panel.isHidden = true
        panel.translatesAutoresizingMaskIntoConstraints = false
        panel.onClose = { [weak self] in self?.closeWorkspaceSearch() }
        panel.onSearch = { [weak self] query, options in self?.searchWorkspace(query, options: options) }
        panel.onOpen = { [weak self] match in self?.openWorkspaceSearchMatch(match) }
        window?.contentView?.addSubview(panel)
        NSLayoutConstraint.activate([
            panel.topAnchor.constraint(equalTo: detailView.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: detailView.centerXAnchor),
            panel.widthAnchor.constraint(equalToConstant: WorkspaceSearch.width),
            panel.widthAnchor.constraint(lessThanOrEqualTo: detailView.widthAnchor, constant: -24),
        ])
        loadedWorkspaceSearch = panel
        return panel
    }

    @objc func showWorkspaceSearch(_ sender: Any?) {
        guard workspace != nil else { NSSound.beep(); statusLabel.stringValue = "Open a folder to search the workspace."; return }
        closeOverlays()
        let panel = workspaceSearch
        panel.isHidden = false
        panel.reset()
        panel.focus()
    }

    private func searchWorkspace(_ query: String, options: WorkspaceSearchOptions) {
        guard let workspace else { return }
        searchGeneration += 1
        let generation = searchGeneration
        guard !query.isEmpty else { workspaceSearch.show([], message: "Type to search the workspace"); return }
        workspaceSearch.show([], message: "Searching…")
        Task.detached(priority: .userInitiated) { [weak self, workspace] in
            let results = (try? workspace.search(query: query, options: options, limit: 10_000)) ?? []
            await MainActor.run {
                guard let self, self.searchGeneration == generation, self.isWorkspaceSearchVisible else { return }
                self.workspaceSearch.show(results, message: results.isEmpty ? "No results" : nil)
            }
        }
    }

    private func openWorkspaceSearchMatch(_ match: WorkspaceSearchMatch) {
        closeWorkspaceSearch()
        guard let workspace else { return }
        openWorkspaceFile(workspace, FileEntry(name: (match.relPath as NSString).lastPathComponent,
                                               relPath: match.relPath, isDir: false), preview: false)
        activeTab?.doc.goToLine(line: match.line, col: match.column)
    }

    func closeWorkspaceSearch() {
        guard isWorkspaceSearchVisible else { return }
        workspaceSearch.isHidden = true
        window?.makeFirstResponder(activeTab != nil ? editorView : explorer.outlineView)
    }

    var isQuickOpenVisible: Bool { loadedQuickOpen.map { !$0.isHidden } ?? false }

    /// Built on first ⌘P, like the find bar, to keep it out of startup.
    var quickOpen: QuickOpen {
        if let panel = loadedQuickOpen { return panel }
        let panel = QuickOpen()
        panel.isHidden = true
        panel.translatesAutoresizingMaskIntoConstraints = false
        panel.onOpen = { [weak self] match in self?.openQuickOpenMatch(match) }
        panel.onClose = { [weak self] in self?.closeGoToFile() }
        panel.onMode = { [weak self] text in self?.switchQuickAccess(to: text) }
        window?.contentView?.addSubview(panel)
        let preferred = panel.widthAnchor.constraint(equalToConstant: QuickOpen.width)
        preferred.priority = .defaultHigh
        NSLayoutConstraint.activate([
            panel.topAnchor.constraint(equalTo: titleBar.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: titleBar.centerXAnchor),
            panel.widthAnchor.constraint(lessThanOrEqualTo: titleBar.widthAnchor, constant: -24),
            preferred,
        ])
        loadedQuickOpen = panel
        return panel
    }

    /// ⌘P. Pressed again while open, it moves down the list, as in VSCode.
    @objc func showGoToFile(_ sender: Any?) {
        guard workspace != nil else {
            statusLabel.stringValue = "Open a folder to use Go to File (⇧⌘O)."
            NSSound.beep()
            return
        }
        if isQuickOpenVisible { return quickOpen.move(by: 1) }
        closeOverlays()
        let panel = quickOpen
        fileFinder?.setContext(current: activeTab.flatMap { relativePath($0.doc.path()) }, recent: recentFiles)
        panel.finder = fileFinder
        panel.hintShortcuts = keybindings.shortcutTexts
        panel.isHidden = false
        panel.reset()
        panel.focus()
    }

    /// The title bar's search box: Go to File with the mode list, or the
    /// command palette when no folder is open.
    @objc func showQuickAccess(_ sender: Any?) {
        guard workspace != nil else { return showCommandPalette(nil) }
        if isQuickOpenVisible { return closeGoToFile() }
        showGoToFile(nil)
    }

    /// Hands the quick-access field over to the panel for its prefix:
    /// `>` commands, `@` symbols, `:` line, `%` text. The rest of the text
    /// is carried over.
    private func switchQuickAccess(to text: String) {
        guard let prefix = text.first else { return }
        let rest = String(text.dropFirst())
        closeGoToFile()
        switch prefix {
        case ">":
            showCommandPalette(nil)
            commandPalette.setQuery(rest)
            Self.caretToEnd(commandPalette.field)
        case "@":
            showGoToSymbol(nil)
            guard isOutlinePickerVisible else { return }
            outlinePicker.setQuery(rest)
            Self.caretToEnd(outlinePicker.field)
        case ":":
            showGoToLine(nil)
            guard isGoToLineVisible else { return }
            goToLine.setInput(rest)
            Self.caretToEnd(goToLine.field)
        case "%":
            showWorkspaceSearch(nil)
            guard isWorkspaceSearchVisible, !rest.isEmpty else { return }
            workspaceSearch.field.stringValue = rest
            workspaceSearch.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
            Self.caretToEnd(workspaceSearch.field)
        default: break
        }
    }

    private static func caretToEnd(_ field: NSTextField) {
        field.currentEditor()?.selectedRange = NSRange(location: field.stringValue.utf16.count, length: 0)
    }

    func closeGoToFile() {
        guard isQuickOpenVisible else { return }
        quickOpen.isHidden = true
        window?.makeFirstResponder(activeTab != nil ? editorView : explorer.outlineView)
    }

    private func openQuickOpenMatch(_ match: FileMatch) {
        closeGoToFile()
        guard let workspace else { return }
        let name = (match.relPath as NSString).lastPathComponent
        openWorkspaceFile(workspace, FileEntry(name: name, relPath: match.relPath, isDir: false), preview: false)
    }
}
