import AppKit
import os

extension MainWindowController {
    /// Right click on a tab, VSCode's editor tab menu. Items carry the tab,
    /// so they act on it rather than on the active one.
    func menu(forTab index: Int) -> NSMenu? { menu(forTab: index, in: activeGroup) }

    func menu(forTab index: Int, in group: EditorGroup) -> NSMenu? {
        let tabs = group.tabs
        guard tabs.indices.contains(index) else { return nil }
        let tab = tabs[index]
        let menu = NSMenu()
        menu.autoenablesItems = false
        func add(_ title: String, _ action: Selector, enabled: Bool = true) {
            let item = menu.addItem(withTitle: title, action: action, keyEquivalent: "")
            item.target = self
            item.representedObject = tab
            item.isEnabled = enabled
        }
        let others = tabs.filter { $0 !== tab && !$0.isPinned }
        let right = tabs[(index + 1)...].filter { !$0.isPinned }
        add("Close", #selector(closeTab(_:)))
        add("Close Others", #selector(closeOtherTabs(_:)), enabled: !others.isEmpty)
        add("Close to the Right", #selector(closeTabsToTheRight(_:)), enabled: !right.isEmpty)
        add("Close Saved", #selector(closeSavedTabs(_:)))
        add("Close All", #selector(closeAllTabs(_:)), enabled: tabs.contains { !$0.isPinned })
        menu.addItem(.separator())
        add(tab.isPinned ? "Unpin" : "Pin", #selector(togglePinTab(_:)))
        if tab.terminal != nil {
            menu.addItem(.separator())
            add("Move Terminal into Panel", #selector(moveTerminalToPanel(_:)))
        }
        if tab.isDocument {
            let onDisk = tab.fileKey != nil
            menu.addItem(.separator())
            add("Copy Path", #selector(copyTabPath(_:)), enabled: onDisk)
            add("Copy Relative Path", #selector(copyTabRelativePath(_:)), enabled: tab.relPath != nil)
            menu.addItem(.separator())
            add("Reveal in Finder", #selector(revealTabInFinder(_:)), enabled: onDisk && !tab.isDeletedOnDisk)
            add("Reveal in Explorer View", #selector(revealTabInExplorer(_:)),
                enabled: tab.relPath != nil && !tab.isDeletedOnDisk)
            menu.addItem(.separator())
            add("Split Right", #selector(splitEditorRight(_:)))
            add("Split Down", #selector(splitEditorDown(_:)))
        }
        keybindings.apply(to: menu)
        return menu
    }

    /// Right click in the editor text, VSCode's editor context menu. Items
    /// the language engine or the buffer cannot serve right now are disabled.
    func editorContextMenu() -> NSMenu? {
        guard activeTab?.isDocument == true, editorView.document != nil else { return nil }
        let readOnly = editorView.isReadOnly
        let menu = NSMenu()
        menu.autoenablesItems = false
        func add(_ title: String, _ action: Selector, target: AnyObject? = nil, enabled: Bool = true) {
            let item = menu.addItem(withTitle: title, action: action, keyEquivalent: "")
            item.target = target ?? self
            item.isEnabled = enabled
        }
        add("Go to Definition", #selector(goToDefinition(_:)))
        add("Go to References", #selector(findReferences(_:)))
        menu.addItem(.separator())
        add("Find All References", #selector(findReferences(_:)))
        menu.addItem(.separator())
        add("Rename Symbol", #selector(renameSymbol(_:)), enabled: !readOnly)
        add("Change All Occurrences", #selector(EditorView.selectAllOccurrences(_:)), target: editorView)
        add("Refactor…", #selector(showCodeActions(_:)), enabled: !readOnly)
        add("Source Action…", #selector(showCodeActions(_:)), enabled: !readOnly)
        menu.addItem(.separator())
        add("Cut", #selector(EditorView.cut(_:)), target: editorView, enabled: !readOnly)
        add("Copy", #selector(EditorView.copy(_:)), target: editorView)
        add("Paste", #selector(EditorView.paste(_:)), target: editorView, enabled: !readOnly)
        menu.addItem(.separator())
        add("Command Palette…", #selector(showCommandPalette(_:)))
        keybindings.apply(to: menu)
        return menu
    }

    /// The tab a tab-menu item was made for, if the sender is one.
    func menuTab(_ sender: Any?) -> EditorTab? {
        guard let tab = (sender as? NSMenuItem)?.representedObject as? EditorTab else { return nil }
        // The menu may belong to a tab of another group: that group takes focus.
        if let group = groups.first(where: { $0.tabs.contains { $0 === tab } }), group !== activeGroup {
            activateGroup(group)
        }
        return tab
    }

    /// A double click on a tab keeps it: the title stops being italic and the
    /// next single click in the explorer no longer replaces it.
    func keepOpen(_ tab: EditorTab) {
        guard tab.isPreview else { return }
        tab.isPreview = false
        refreshTabBar()
    }

    @objc func togglePinTab(_ sender: Any?) {
        guard let tab = menuTab(sender) ?? activeTab else { return }
        setPinned(!tab.isPinned, tab: tab)
    }

    /// Pinning moves the tab to the end of the pinned ones, unpinning to
    /// just after them, as in VSCode. A pinned tab is no longer a preview.
    func setPinned(_ pinned: Bool, tab: EditorTab) {
        guard tab.isPinned != pinned,
              let group = groups.first(where: { $0.tabs.contains { $0 === tab } }),
              let from = group.tabs.firstIndex(where: { $0 === tab }) else { return }
        let active = group.activeTab
        group.tabs.remove(at: from)
        tab.isPinned = pinned
        if pinned { tab.isPreview = false }
        group.tabs.insert(tab, at: group.tabs.prefix { $0.isPinned }.count)
        group.activeIndex = active.flatMap { a in group.tabs.firstIndex { $0 === a } }
        refreshTabBar()
    }

    /// Closes `doomed` one by one, stopping if a save prompt is cancelled.
    /// Pinned tabs are left alone: only their own Close closes them.
    private func closeTabs(_ doomed: [EditorTab]) {
        for tab in doomed where !tab.isPinned {
            guard let i = tabs.firstIndex(where: { $0 === tab }) else { continue }
            guard closeTab(at: i) else { return }
        }
    }

    @objc func closeOtherTabs(_ sender: Any?) {
        guard let keep = menuTab(sender) ?? activeTab else { return }
        closeTabs(tabs.filter { $0 !== keep })
    }

    @objc func closeTabsToTheRight(_ sender: Any?) {
        guard let pivot = menuTab(sender) ?? activeTab,
              let i = tabs.firstIndex(where: { $0 === pivot }) else { return }
        closeTabs(Array(tabs[(i + 1)...]))
    }

    /// Tabs without unsaved changes. Terminal tabs stay: closing one kills
    /// a running shell, which is not what "saved" suggests.
    @objc func closeSavedTabs(_ sender: Any?) {
        editorView.flushPendingUpdates()
        closeTabs(tabs.filter { $0.terminal == nil && !$0.doc.isModified() })
    }

    @objc func closeAllTabs(_ sender: Any?) {
        closeTabs(tabs)
    }

    @objc func copyTabPath(_ sender: Any?) {
        guard let tab = menuTab(sender) ?? activeTab, tab.fileKey != nil else { return NSSound.beep() }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(tab.doc.path(), forType: .string)
    }

    @objc func copyTabRelativePath(_ sender: Any?) {
        guard let rel = (menuTab(sender) ?? activeTab)?.relPath else { return NSSound.beep() }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(rel, forType: .string)
    }

    @objc func revealTabInFinder(_ sender: Any?) {
        guard let tab = menuTab(sender) ?? activeTab, tab.fileKey != nil else { return NSSound.beep() }
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: tab.doc.path())])
    }

    @objc func revealTabInExplorer(_ sender: Any?) {
        guard let rel = (menuTab(sender) ?? activeTab)?.relPath else { return NSSound.beep() }
        if !(isSidebarVisible && activeSidebarTab == .explorer) { showExplorerTab() }
        explorer.reveal(relPath: rel)
        window?.makeFirstResponder(explorer.outlineView)
    }
}
