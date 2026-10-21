import AppKit
import os

extension MainWindowController {
    @objc func splitEditorRight(_ sender: Any?) {
        // From a tab's menu: split that tab, not the one showing.
        guard let tab = menuTab(sender) ?? activeTab else { return NSSound.beep() }
        splitEditor(tab, vertical: true)
    }

    @objc func splitEditorDown(_ sender: Any?) {
        guard let tab = menuTab(sender) ?? activeTab else { return NSSound.beep() }
        splitEditor(tab, vertical: false)
    }

    /// Joins every group into the active one (VS Code "Join All Editor Groups").
    @objc func closeEditorSplit(_ sender: Any?) { joinAllGroups() }
}

// MARK: - Editor groups

extension MainWindowController {
    /// Another tab, in any group, shows the same document.
    func isDocumentShared(_ tab: EditorTab) -> Bool {
        guard tab.isDocument else { return false }
        return allTabs.contains { $0 !== tab && $0.isDocument && $0.doc === tab.doc }
    }

    func group(containing tab: EditorTab) -> EditorGroup? {
        groups.first { $0.tabs.contains { $0 === tab } }
    }

    /// An open tab for the same file, in any group, so a second tab shares
    /// its buffer instead of reading the file again (two buffers of one file
    /// would overwrite each other's edits).
    func sharedDocument(forFileKey key: String) -> DocumentHandle? {
        allTabs.first { $0.isDocument && $0.fileKey == key }?.doc
    }

    /// Pins the live editor stack over `group`'s slot.
    func placeChrome(over group: EditorGroup) {
        NSLayoutConstraint.deactivate(chromeSlotConstraints)
        chromeSlotConstraints = [
            groupChrome.topAnchor.constraint(equalTo: group.slot.topAnchor),
            groupChrome.leadingAnchor.constraint(equalTo: group.slot.leadingAnchor),
            groupChrome.trailingAnchor.constraint(equalTo: group.slot.trailingAnchor),
            groupChrome.bottomAnchor.constraint(equalTo: group.slot.bottomAnchor),
        ]
        NSLayoutConstraint.activate(chromeSlotConstraints)
    }

    /// The pane shown in `group`'s slot while the group is not the active one.
    func pane(for group: EditorGroup) -> EditorGroupPane {
        if let pane = group.pane { return pane }
        let pane = EditorGroupPane()
        pane.translatesAutoresizingMaskIntoConstraints = false
        group.slot.addSubview(pane)
        NSLayoutConstraint.activate([
            pane.topAnchor.constraint(equalTo: group.slot.topAnchor),
            pane.leadingAnchor.constraint(equalTo: group.slot.leadingAnchor),
            pane.trailingAnchor.constraint(equalTo: group.slot.trailingAnchor),
            pane.bottomAnchor.constraint(equalTo: group.slot.bottomAnchor),
        ])
        pane.editorView.setFont(Theme.editorFont)
        pane.editorView.renderWhitespace = secondaryEditorView.renderWhitespace
        pane.tabBar.titleBar = titleBar
        pane.tabBar.onSelect = { [weak self, weak group] i in
            guard let self, let group else { return }
            self.activateGroup(group)
            self.selectTab(i, fromTabBar: true)
        }
        pane.tabBar.onClose = { [weak self, weak group] i in
            guard let self, let group else { return }
            self.activateGroup(group)
            self.closeTab(at: i)
        }
        pane.tabBar.onKeep = { [weak self, weak group] i in
            guard let self, let group, group.tabs.indices.contains(i) else { return }
            self.keepOpen(group.tabs[i])
        }
        pane.tabBar.onUnpin = { [weak self, weak group] i in
            guard let self, let group, group.tabs.indices.contains(i) else { return }
            self.setPinned(false, tab: group.tabs[i])
        }
        pane.tabBar.onMove = { [weak self, weak group] from, to in
            guard let self, let group else { return }
            self.activateGroup(group)
            self.moveTab(from: from, to: to)
        }
        pane.tabBar.onDropOutside = { [weak self, weak group] i, point in
            guard let self, let group else { return false }
            return self.dropTab(from: group, at: i, windowPoint: point)
        }
        pane.tabBar.menuForTab = { [weak self, weak group] i in
            guard let self, let group else { return nil }
            return self.menu(forTab: i, in: group)
        }
        pane.onActivate = { [weak self, weak group] event in
            guard let self, let group else { return }
            self.paneClicked(group, event: event)
        }
        group.pane = pane
        return pane
    }

    /// Brings `group`'s pane up to date (or hides it for the active group).
    func refreshPane(of group: EditorGroup) {
        if group === activeGroup {
            group.pane?.isHidden = true
            group.pane?.clear()
            return
        }
        let pane = pane(for: group)
        pane.isHidden = false
        pane.show(items: tabItems(for: group.tabs), selected: group.activeIndex, tab: group.activeTab,
                  breadcrumbHeight: breadcrumbBar.isHidden ? 0 : 24)
    }

    func refreshGroupPanes() {
        for group in groups where group !== activeGroup { refreshPane(of: group) }
    }

    /// Edits made in the live editor show up in other groups' panes.
    func refreshPanes(showing document: DocumentHandle) {
        for group in groups where group !== activeGroup {
            if let pane = group.pane, pane.shownTab?.doc === document {
                pane.editorView.requestDocumentRefresh(scrollCaret: false)
            }
        }
    }

    // MARK: Switching

    /// Saves what the live editor knows about the tab it shows and clears
    /// what only makes sense for that tab (find, popups, terminal).
    func leaveActiveGroup() {
        if isFindVisible { closeFind(nil) }
        snippetSession = nil
        snippetDocumentKey = nil
        hoverPopover.isHidden = true
        hideBlamePopover()
        completionPopup.isHidden = true
        signaturePopup.isHidden = true
        editorView.flushPendingUpdates()
        if let tab = shownTab, tab === activeTab || group(containing: tab) === activeGroup {
            noteLeaving(tab)
            if tab.isDocument { tab.scrollY = scrollView.contentView.bounds.origin.y }
            tab.status = statusLabel.stringValue
            if autoSaveMode == .onFocusChange { autoSave(tab: tab) }
        }
        showEditorTerminal(for: nil)
        showEditorGitGraph(for: nil)
        shownTab = nil
    }

    /// Makes `group` the one the live editor works on.
    func activateGroup(_ group: EditorGroup) {
        guard group !== activeGroup, let newIndex = groups.firstIndex(where: { $0 === group }) else { return }
        // What the user scrolled in the pane counts as the tab's position.
        if let pane = group.pane, let tab = pane.shownTab, tab === group.activeTab { tab.scrollY = pane.scrollY }
        leaveActiveGroup()
        let old = activeGroup
        activeGroupIndex = newIndex
        placeChrome(over: group)
        if !old.tabs.isEmpty { refreshPane(of: old) }
        refreshPane(of: group)
        groupsSplitView.layoutSubtreeIfNeeded()
        detailView.layoutSubtreeIfNeeded()
        if let index = group.activeIndex ?? (group.tabs.isEmpty ? nil : 0) {
            selectTab(index, fromTabBar: true)
        } else {
            showNoDocument()
        }
        updateWrapState()
        refreshTabBar()
    }

    func focusGroup(at index: Int) {
        guard groups.indices.contains(index) else { return NSSound.beep() }
        activateGroup(groups[index])
    }

    @objc func focusFirstEditorGroup(_ sender: Any?) { focusGroup(at: 0) }
    @objc func focusSecondEditorGroup(_ sender: Any?) { focusGroup(at: 1) }
    @objc func focusThirdEditorGroup(_ sender: Any?) { focusGroup(at: 2) }

    /// A click in a pane below its tab bar: that group takes over, and the
    /// click lands where it was aimed.
    func paneClicked(_ group: EditorGroup, event: NSEvent) {
        activateGroup(group)
        guard event.type == .leftMouseDown, activeTab?.isDocument == true else { return }
        detailView.layoutSubtreeIfNeeded()
        if scrollView.bounds.contains(scrollView.convert(event.locationInWindow, from: nil)) {
            editorView.mouseDown(with: event)
        }
    }

    // MARK: Creating and removing

    /// Adds an empty group at `index`; the caller fills it and activates it.
    @discardableResult
    func makeGroup(at index: Int) -> EditorGroup {
        let active = activeGroup
        let group = EditorGroup()
        if groups.isEmpty || index >= groups.count {
            groupsSplitView.addSubview(group.slot, positioned: .above, relativeTo: groups.last?.slot)
            groups.append(group)
        } else if index <= 0 {
            groupsSplitView.addSubview(group.slot, positioned: .below, relativeTo: groups[0].slot)
            groups.insert(group, at: 0)
        } else {
            groupsSplitView.addSubview(group.slot, positioned: .above, relativeTo: groups[index - 1].slot)
            groups.insert(group, at: index)
        }
        activeGroupIndex = groups.firstIndex { $0 === active } ?? 0
        return group
    }

    func removeGroup(_ group: EditorGroup) {
        guard groups.count > 1, let index = groups.firstIndex(where: { $0 === group }) else { return }
        if group === activeGroup {
            activateGroup(groups[index > 0 ? index - 1 : index + 1])
        }
        let active = activeGroup
        group.pane?.clear()
        group.slot.removeFromSuperview()
        groups.remove(at: index)
        activeGroupIndex = groups.firstIndex { $0 === active } ?? 0
        if groups.count == 1 { groupsSplitView.isVertical = true }
        groupsSplitView.adjustSubviews()
        updateWrapState()
        refreshTabBar()
        updateEditedState()
    }

    /// Moves `tab` into a new group beside its own (VS Code "Split Right/Down").
    /// A group's only tab stays put and is opened a second time instead:
    /// moving it would leave nothing to split.
    func splitEditor(_ tab: EditorTab, vertical: Bool) {
        guard tab.isDocument, let source = group(containing: tab),
              let index = source.tabs.firstIndex(where: { $0 === tab }) else { return NSSound.beep() }
        if isMarkdownPreviewShown {
            isMarkdownPreviewOn = false
            updateMarkdownPreview()
        }
        if source !== activeGroup { activateGroup(source) }
        editorView.flushPendingUpdates()
        if groups.count == 1 { groupsSplitView.isVertical = vertical }
        if tab === shownTab { tab.scrollY = scrollView.contentView.bounds.origin.y }
        let group = makeGroup(at: activeGroupIndex + 1)
        if source.tabs.count > 1 {
            moveTab(from: source, at: index, to: group, at: nil)
        } else {
            let copy = EditorTab(doc: tab.doc, title: tab.title, pathComponents: tab.pathComponents,
                                 windowTitle: tab.windowTitle, isPreview: false, status: tab.status)
            copy.fileKey = tab.fileKey
            copy.relPath = tab.relPath
            copy.isEdited = tab.isEdited
            copy.isDeletedOnDisk = tab.isDeletedOnDisk
            copy.scrollY = tab.scrollY
            group.tabs = [copy]
            group.activeIndex = 0
            activateGroup(group)
        }
        groupsSplitView.layoutSubtreeIfNeeded()
        groupsSplitView.equalize()
    }

    /// Puts every group's tabs in the active group, in group order.
    func joinAllGroups() {
        guard groups.count > 1 else { return }
        let target = activeGroup
        let active = target.activeTab
        var joined: [EditorTab] = []
        for group in groups {
            for tab in group.tabs {
                // The same document is already open here: one tab is enough.
                if tab.isDocument, joined.contains(where: { $0.isDocument && $0.doc === tab.doc }) { continue }
                joined.append(tab)
            }
            if group !== target {
                group.pane?.clear()
                group.slot.removeFromSuperview()
            }
        }
        // Pinned tabs stay first (the sort is stable).
        target.tabs = joined.filter(\.isPinned) + joined.filter { !$0.isPinned }
        groups = [target]
        activeGroupIndex = 0
        target.activeIndex = active.flatMap { a in
            target.tabs.firstIndex { $0 === a || ($0.isDocument && a.isDocument && $0.doc === a.doc) }
        }
        shownTab = target.activeTab
        groupsSplitView.isVertical = true
        groupsSplitView.adjustSubviews()
        updateWrapState()
        refreshTabBar()
        updateEditedState()
    }

    // MARK: Moving tabs between groups

    /// Insertion index in `bar` for a drop at `windowPoint`.
    private func insertionIndex(in bar: TabBar, windowPoint: NSPoint) -> Int {
        let x = bar.convert(windowPoint, from: nil).x
        for (i, view) in bar.tabViews.enumerated() where x < bar.convert(view.bounds, from: view).midX { return i }
        return bar.tabViews.count
    }

    /// A tab of `source` was dropped on the window: takes it if another
    /// group's area is under the point.
    func dropTab(from source: EditorGroup, at index: Int, windowPoint: NSPoint) -> Bool {
        guard source.tabs.indices.contains(index) else { return false }
        let point = groupsSplitView.convert(windowPoint, from: nil)
        guard let target = groups.first(where: { $0 !== source && $0.slot.frame.contains(point) }) else { return false }
        let bar = target === activeGroup ? tabBar : pane(for: target).tabBar
        moveTab(from: source, at: index, to: target, at: insertionIndex(in: bar, windowPoint: windowPoint))
        return true
    }

    /// Moves a tab to another group and shows it there. The source group
    /// closes if that was its last tab.
    func moveTab(from source: EditorGroup, at index: Int, to target: EditorGroup, at position: Int?) {
        guard source !== target, source.tabs.indices.contains(index) else { return }
        let tab = source.tabs[index]
        if source === activeGroup { leaveActiveGroup() }
        source.tabs.remove(at: index)
        if let a = source.activeIndex {
            source.activeIndex = a > index ? a - 1 : (a == index ? nil : a)
        }
        if source.activeIndex == nil, !source.tabs.isEmpty { source.activeIndex = min(index, source.tabs.count - 1) }

        let targetIndex: Int
        if tab.isDocument, let existing = target.tabs.firstIndex(where: { $0.isDocument && $0.doc === tab.doc }) {
            // Same document already open there: show that tab, drop this one.
            targetIndex = existing
        } else {
            let pinned = target.tabs.prefix { $0.isPinned }.count
            let wanted = position ?? target.tabs.count
            let at = tab.isPinned ? min(wanted, pinned) : max(wanted, pinned)
            target.tabs.insert(tab, at: at)
            if let a = target.activeIndex, a >= at { target.activeIndex = a + 1 }
            targetIndex = at
        }
        target.activeIndex = targetIndex
        if target === activeGroup {
            selectTab(targetIndex, fromTabBar: true)
        } else {
            activateGroup(target)
        }
        if source.tabs.isEmpty { removeGroup(source) } else { refreshPane(of: source) }
    }

    /// Moves the active tab one group to the left or right, opening a new
    /// group when there is none on that side.
    func moveActiveTab(by delta: Int) {
        guard let index = activeIndex else { return NSSound.beep() }
        let next = activeGroupIndex + delta
        if groups.indices.contains(next) {
            moveTab(from: activeGroup, at: index, to: groups[next], at: nil)
        } else if tabs.count > 1 {
            if groups.count == 1 { groupsSplitView.isVertical = true }
            let group = makeGroup(at: delta > 0 ? groups.count : 0)
            let source = activeGroup
            // Placed first so the group has a tab when it becomes active.
            moveTab(from: source, at: index, to: group, at: nil)
            groupsSplitView.equalize()
        } else {
            NSSound.beep()
        }
    }

    @objc func moveEditorToNextGroup(_ sender: Any?) { moveActiveTab(by: 1) }
    @objc func moveEditorToPreviousGroup(_ sender: Any?) { moveActiveTab(by: -1) }
}

extension MainWindowController {
    /// Shows `tab`, in whichever group holds it.
    func focusTab(_ tab: EditorTab) {
        guard let group = group(containing: tab), let index = group.tabs.firstIndex(where: { $0 === tab }) else { return }
        if group !== activeGroup { activateGroup(group) }
        if activeIndex != index { selectTab(index) }
    }

    /// Closes a clean tab wherever it is, without asking.
    func closeTabAnywhere(_ tab: EditorTab, keepTerminal: Bool = false) {
        guard let group = group(containing: tab) else { return }
        if group === activeGroup {
            if let i = tabs.firstIndex(where: { $0 === tab }) { closeTab(at: i, keepTerminal: keepTerminal) }
            return
        }
        guard let i = group.tabs.firstIndex(where: { $0 === tab }) else { return }
        if tab.terminal != nil, !keepTerminal { tab.terminal?.kill() }
        group.tabs.remove(at: i)
        if let a = group.activeIndex {
            group.activeIndex = a > i ? a - 1 : (a == i ? nil : a)
        }
        if group.tabs.isEmpty {
            removeGroup(group)
        } else {
            if group.activeIndex == nil { group.activeIndex = min(i, group.tabs.count - 1) }
            refreshPane(of: group)
        }
        refreshTabBar()
        updateEditedState()
    }

    /// Prompts for every group's unsaved documents, one document at a time.
    func confirmDiscardAllGroups() -> Bool {
        editorView.flushPendingUpdates()
        var asked: [DocumentHandle] = []
        for group in groups {
            for tab in group.tabs where tab.isDocument && tab.doc.isModified() {
                if asked.contains(where: { $0 === tab.doc }) { continue }
                asked.append(tab.doc)
                focusTab(tab)
                guard confirmDiscardActiveTab() else { return false }
            }
        }
        return true
    }

    /// Back to one empty group (new folder). Kills terminal tabs.
    func resetGroups() {
        for tab in allTabs { tab.terminal?.kill() }
        let keep = activeGroup
        for group in groups where group !== keep {
            group.pane?.clear()
            group.slot.removeFromSuperview()
        }
        groups = [keep]
        activeGroupIndex = 0
        keep.tabs = []
        keep.activeIndex = nil
        groupsSplitView.isVertical = true
        groupsSplitView.adjustSubviews()
    }
}

extension MainWindowController {
    /// Left edge of the live editor in `detailView`, for popups placed there.
    var editorOriginInDetail: CGFloat {
        detailView.convert(NSPoint.zero, from: editorView).x
    }
}
