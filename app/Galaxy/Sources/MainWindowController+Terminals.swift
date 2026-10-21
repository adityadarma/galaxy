import AppKit
import os

extension MainWindowController {
    func ensureTerminal() {
        guard workspace != nil else {
            terminals.removeAll()
            refreshTerminalPicker()
            terminalPanel.show(terminals: [], activeIndex: 0)
            return
        }
        if terminals.isEmpty, let terminal = makePanelTerminal() {
            terminals = [terminal]
            activeTerminalIndex = 0
        }
        refreshTerminalPicker()
        guard terminals.indices.contains(activeTerminalIndex) else { return }
        terminalPanel.show(terminals: terminals, activeIndex: activeTerminalIndex)
    }

    /// The header chip and the terminal list follow `terminals`.
    func refreshTerminalPicker() {
        bottomPanelHeader.setTerminals(terminals.map(\.summary))
        terminalPanel.updateList(terminals, selected: activeTerminalIndex)
    }

    var activeTerminal: PanelTerminal? {
        terminals.indices.contains(activeTerminalIndex) ? terminals[activeTerminalIndex] : nil
    }

    /// A new shell in the workspace. Without `group` it gets a pane of its own.
    private func makePanelTerminal(group: Int? = nil) -> PanelTerminal? {
        guard let workspace, let handle = bridge.terminal(workspace: workspace, cols: 100, rows: 24) else { return nil }
        var target = group
        if target == nil {
            target = nextTerminalGroup
            nextTerminalGroup += 1
        }
        return PanelTerminal(handle: handle, group: target ?? 0)
    }

    private func revealNewTerminal() {
        if !isBottomPanelVisible { setBottomPanelOpen(true) }
        showBottomPanel(tab: .terminal)
    }

    func newTerminal() {
        guard let terminal = makePanelTerminal() else { return NSSound.beep() }
        terminals.append(terminal)
        activeTerminalIndex = terminals.count - 1
        revealNewTerminal()
    }

    /// Takes a terminal out of the panel; the others stay where they are.
    private func takeTerminal(_ terminal: PanelTerminal) -> Bool {
        guard let index = terminals.firstIndex(where: { $0 === terminal }) else { return false }
        terminals.remove(at: index)
        if index < activeTerminalIndex { activeTerminalIndex -= 1 }
        removedTerminal()
        return true
    }

    /// Kills a terminal (the active one by default); the panel closes with the last one.
    func killPanelTerminal(_ terminal: PanelTerminal? = nil) {
        guard let target = terminal ?? activeTerminal else { return }
        target.handle.kill()
        _ = takeTerminal(target)
    }

    /// A shell exited (`exit`, ⌃D).
    func terminalExited() {
        let exited = terminals.filter { !$0.handle.isRunning() }
        guard !exited.isEmpty else { return }
        for terminal in exited { _ = takeTerminal(terminal) }
    }

    private func removedTerminal() {
        let hadFocus = terminalPanel.owns(window?.firstResponder)
        terminalPanel.show(terminals: [], activeIndex: 0)
        activeTerminalIndex = max(0, min(activeTerminalIndex, terminals.count - 1))
        if terminals.isEmpty {
            refreshTerminalPicker()
            if bottomPanelTab == .terminal { setBottomPanelOpen(false) }
        } else {
            ensureTerminal()
            if hadFocus { window?.makeFirstResponder(terminalPanel.activePane) }
        }
    }

    // MARK: Terminals in the editor area

    /// Terminal tabs, in tab order.
    var terminalTabs: [EditorTab] { allTabs.filter { $0.terminal != nil } }

    /// Moves the panel's active terminal into a new editor tab, keeping the
    /// shell running. With no panel terminal, starts a new one there.
    @objc func moveTerminalToEditor(_ sender: Any?) {
        guard let workspace else { return NSSound.beep() }
        if let target = menuTerminal(sender) ?? activeTerminal {
            let name = target.customName
            _ = takeTerminal(target)
            openTerminalTab(target.handle, name: name)
        } else if let handle = bridge.terminal(workspace: workspace, cols: 100, rows: 24) {
            openTerminalTab(handle)
        } else {
            NSSound.beep()
        }
    }

    /// Opens a new terminal as an editor tab.
    @objc func createTerminalEditor(_ sender: Any?) {
        guard let workspace, let handle = bridge.terminal(workspace: workspace, cols: 100, rows: 24) else {
            return NSSound.beep()
        }
        openTerminalTab(handle)
    }

    // MARK: Terminal menu

    /// The terminal that input goes to: the active terminal tab, else the
    /// panel's active terminal.
    var activeTerminalHandle: TerminalHandle? {
        if let handle = activeTab?.terminal { return handle }
        return activeTerminal?.handle
    }

    /// Like `activeTerminalHandle`, but opens the panel terminal if there is none.
    private func terminalForInput() -> TerminalHandle? {
        if activeTerminalHandle == nil { newTerminal() }
        return activeTerminalHandle
    }

    @objc func createTerminal(_ sender: Any?) { newTerminal() }

    @objc func killTerminal(_ sender: Any?) {
        if let target = menuTerminal(sender) {
            killPanelTerminal(target)
        } else if let index = activeIndex, tabs[index].terminal != nil {
            tabs[index].terminal?.kill()
            closeTab(at: index, keepTerminal: true)
        } else {
            killPanelTerminal(menuTerminal(sender))
        }
    }

    @objc func clearActiveTerminal(_ sender: Any?) {
        try? activeTerminalHandle?.write(input: "\u{0c}")
    }

    /// Sends the selection (or the current line, when nothing is selected)
    /// to the active terminal and presses Return.
    @objc func runSelectedTextInTerminal(_ sender: Any?) {
        guard let document = activeTab.flatMap({ $0.isDocument ? $0.doc : nil }) else { return NSSound.beep() }
        var text = document.copy().text
        while text.hasSuffix("\n") { text.removeLast() }
        guard !text.isEmpty, let handle = terminalForInput() else { return NSSound.beep() }
        try? handle.write(input: text.replacingOccurrences(of: "\n", with: "\r") + "\r")
    }

    /// Sends the active file's path, quoted, to the active terminal.
    @objc func runActiveFileInTerminal(_ sender: Any?) {
        guard let path = activeTab?.fileKey, activeTab?.isDocument == true else { return NSSound.beep() }
        guard let handle = terminalForInput() else { return NSSound.beep() }
        let quoted = "'" + path.replacingOccurrences(of: "'", with: "'\\''") + "'"
        try? handle.write(input: quoted + "\r")
    }

    /// Moves a terminal tab (the clicked one from the tab menu, else the
    /// active one) back into the bottom panel.
    @objc func moveTerminalToPanel(_ sender: Any?) {
        let target = menuTab(sender) ?? activeTab
        guard let tab = target, let handle = tab.terminal,
              let index = tabs.firstIndex(where: { $0 === tab }) else { return NSSound.beep() }
        closeTab(at: index, keepTerminal: true)
        adoptTerminal(PanelTerminal(handle: handle, group: 0))
    }

    /// Takes a terminal over (moved from the editor area or another window).
    func adoptTerminal(_ terminal: PanelTerminal) {
        terminal.group = nextTerminalGroup
        nextTerminalGroup += 1
        terminals.append(terminal)
        activeTerminalIndex = terminals.count - 1
        revealNewTerminal()
    }

    private func openTerminalTab(_ handle: TerminalHandle, name customName: String? = nil) {
        let name = customName ?? handle.shellName()
        let tab = EditorTab(doc: bridge.newDocument(), title: name, pathComponents: [name], windowTitle: name,
                            isPreview: false, status: "Terminal: \(name)", terminal: handle)
        add(tab, replacing: nil)
    }

    /// Shows or hides the editor-area terminal for the active tab.
    func showEditorTerminal(for tab: EditorTab?) {
        if let handle = tab?.terminal {
            editorTerminalView.isHidden = false
            editorTerminalView.start(handle: handle)
        } else if !editorTerminalView.isHidden {
            if window?.firstResponder === editorTerminalView { window?.makeFirstResponder(editorView) }
            editorTerminalView.stop()
            editorTerminalView.isHidden = true
        }
    }

    /// The shell in the active terminal tab exited: close the tab.
    func editorTerminalExited() {
        guard let index = activeIndex, let handle = tabs[index].terminal, !handle.isRunning() else { return }
        closeTab(at: index, keepTerminal: true)
    }

    /// A stand-in menu item carrying `terminal`, for calling menu actions from code.
    func terminalItem(_ terminal: PanelTerminal) -> NSMenuItem {
        let item = NSMenuItem()
        item.representedObject = terminal
        return item
    }

    /// The terminal a context-menu item was made for, if the sender is one.
    private func menuTerminal(_ sender: Any?) -> PanelTerminal? {
        (sender as? NSMenuItem)?.representedObject as? PanelTerminal
    }

    /// The terminal menu (header chip, list rows and the terminal itself).
    func terminalMenu(for terminal: PanelTerminal) -> NSMenu {
        let menu = NSMenu()
        func add(_ title: String, _ action: Selector) {
            let item = menu.addItem(withTitle: title, action: action, keyEquivalent: "")
            item.target = self
            item.representedObject = terminal
        }
        add("Split Terminal", #selector(splitTerminal(_:)))
        add("Move Terminal into Editor Area", #selector(moveTerminalToEditor(_:)))
        add("Move Terminal into New Window", #selector(moveTerminalToNewWindow(_:)))
        menu.addItem(.separator())
        add("Change Color\u{2026}", #selector(changeTerminalColor(_:)))
        add("Change Icon\u{2026}", #selector(changeTerminalIcon(_:)))
        add("Rename\u{2026}", #selector(renameTerminal(_:)))
        add("Toggle Size to Content Width", #selector(toggleTerminalListSize(_:)))
        menu.addItem(.separator())
        add("Kill Terminal", #selector(killTerminal(_:)))
        keybindings.apply(to: menu)
        return menu
    }

    @objc func splitTerminal(_ sender: Any?) {
        guard let source = menuTerminal(sender) ?? activeTerminal else { return newTerminal() }
        guard let terminal = makePanelTerminal(group: source.group) else { return NSSound.beep() }
        // Next to the terminal it was split from, so a group stays together.
        let last = terminals.lastIndex { $0.group == source.group } ?? terminals.count - 1
        terminals.insert(terminal, at: last + 1)
        activeTerminalIndex = last + 1
        revealNewTerminal()
    }

    @objc func moveTerminalToNewWindow(_ sender: Any?) {
        guard let workspace, let open = openTerminalInNewWindow,
              let target = menuTerminal(sender) ?? activeTerminal else { return NSSound.beep() }
        let root = URL(fileURLWithPath: workspace.root())
        guard takeTerminal(target) else { return }
        open(target, root)
    }

    @objc func renameTerminal(_ sender: Any?) {
        guard let terminal = menuTerminal(sender) ?? activeTerminal else { return NSSound.beep() }
        let alert = NSAlert()
        alert.messageText = "Rename Terminal"
        alert.informativeText = "Leave empty to use the shell's name."
        let field = NSTextField(string: terminal.customName ?? "")
        field.placeholderString = terminal.handle.shellName()
        field.frame = NSRect(x: 0, y: 0, width: 280, height: 24)
        alert.accessoryView = field
        alert.addButton(withTitle: "Rename")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        renameTerminal(terminal, to: field.stringValue)
    }

    func renameTerminal(_ terminal: PanelTerminal, to name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        terminal.customName = trimmed.isEmpty ? nil : trimmed
        refreshTerminalPicker()
    }

    @objc func changeTerminalColor(_ sender: Any?) {
        guard let terminal = menuTerminal(sender) ?? activeTerminal else { return NSSound.beep() }
        let popup = NSPopUpButton(frame: NSRect(x: 0, y: 0, width: 220, height: 26), pullsDown: false)
        popup.addItem(withTitle: "Default")
        for choice in TerminalChoices.colors {
            popup.addItem(withTitle: choice.name)
            popup.lastItem?.image = TerminalChoices.swatch(choice.color)
        }
        if let current = terminal.colorName { popup.selectItem(withTitle: current) }
        let alert = NSAlert()
        alert.messageText = "Change Terminal Color"
        alert.accessoryView = popup
        alert.addButton(withTitle: "Change")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        setTerminalColor(terminal, name: popup.indexOfSelectedItem == 0 ? nil : popup.titleOfSelectedItem)
    }

    func setTerminalColor(_ terminal: PanelTerminal, name: String?) {
        terminal.colorName = name.flatMap { TerminalChoices.color(named: $0) == nil ? nil : $0 }
        refreshTerminalPicker()
    }

    @objc func changeTerminalIcon(_ sender: Any?) {
        guard let terminal = menuTerminal(sender) ?? activeTerminal else { return NSSound.beep() }
        let popup = NSPopUpButton(frame: NSRect(x: 0, y: 0, width: 220, height: 26), pullsDown: false)
        for choice in TerminalChoices.icons {
            popup.addItem(withTitle: choice.title)
            popup.lastItem?.image = TerminalChoices.symbol(choice.symbol)
        }
        if let index = TerminalChoices.icons.firstIndex(where: { $0.symbol == terminal.icon }) {
            popup.selectItem(at: index)
        }
        let alert = NSAlert()
        alert.messageText = "Change Terminal Icon"
        alert.accessoryView = popup
        alert.addButton(withTitle: "Change")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        setTerminalIcon(terminal, symbol: TerminalChoices.icons[popup.indexOfSelectedItem].symbol)
    }

    func setTerminalIcon(_ terminal: PanelTerminal, symbol: String) {
        terminal.icon = symbol
        refreshTerminalPicker()
    }

    /// Sizes the terminal list to its longest name, or back to the default width.
    @objc func toggleTerminalListSize(_ sender: Any?) {
        terminalPanel.listFitsContent.toggle()
    }
}
