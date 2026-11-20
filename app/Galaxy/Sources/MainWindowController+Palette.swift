import AppKit
import os

extension MainWindowController {
    // MARK: Command palette (M2-09)

    var isCommandPaletteVisible: Bool { loadedPalette.map { !$0.isHidden } ?? false }

    var commandPalette: CommandPaletteView {
        if let panel = loadedPalette { return panel }
        let panel = CommandPaletteView()
        panel.palette = commands
        panel.isHidden = true
        panel.translatesAutoresizingMaskIntoConstraints = false
        panel.onRun = { [weak self] match in self?.runCommand(match.id) }
        panel.onClose = { [weak self] in self?.closeCommandPalette() }
        window?.contentView?.addSubview(panel)
        let preferred = panel.widthAnchor.constraint(equalToConstant: CommandPaletteView.width)
        preferred.priority = .defaultHigh
        NSLayoutConstraint.activate([
            panel.topAnchor.constraint(equalTo: titleBar.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: titleBar.centerXAnchor),
            panel.widthAnchor.constraint(lessThanOrEqualTo: titleBar.widthAnchor, constant: -24),
            preferred,
        ])
        loadedPalette = panel
        return panel
    }

    /// ⇧⌘P.
    @objc func showCommandPalette(_ sender: Any?) {
        if isCommandPaletteVisible { return commandPalette.move(by: 1) }
        closeOverlays()
        paletteReturnResponder = window?.firstResponder
        // Decided before the field takes focus, from what had it.
        let available = availableCommandIDs()
        let shortcuts = keybindings.shortcutTexts
        let panel = commandPalette
        panel.isHidden = false
        panel.reset(available: available, shortcuts: shortcuts)
        panel.focus()
    }

    func closeCommandPalette() {
        guard isCommandPaletteVisible else { return }
        commandPalette.isHidden = true
        restoreFocusAfterPalette()
    }

    private func restoreFocusAfterPalette() {
        let target = paletteReturnResponder ?? (activeTab != nil ? editorView : nil)
        window?.makeFirstResponder(target)
    }

    /// Ids whose action has a target that accepts it right now, asked the
    /// way AppKit asks before enabling a menu item. Go to File and Go to
    /// Line stay enabled in the menu (they explain what they need) but are
    /// not offered here when they could only explain.
    func availableCommandIDs() -> [String] {
        commands.commands().map(\.id).filter { id in
            guard let action = CommandPaletteView.actions[id], let target = target(for: action) else { return false }
            switch action {
            case #selector(showGoToFile(_:)) where workspace == nil: return false
            case #selector(showGoToLine(_:)) where editorView.document == nil: return false
            case #selector(refreshRustGeneratedSources(_:)) where workspace == nil: return false
            default: break
            }
            let item = NSMenuItem(title: "", action: action, keyEquivalent: "")
            return (target as? NSMenuItemValidation)?.validateMenuItem(item) ?? true
        }
    }

    /// First object that handles `action`, searched like AppKit does for a
    /// menu item: from the window's first responder up through the window
    /// and this controller, then the app and its delegate. Walked by hand
    /// so it also works while the window is not key (tests).
    func target(for action: Selector) -> NSObject? {
        var responder: NSResponder? = window?.firstResponder ?? window
        while let r = responder {
            if r.responds(to: action) { return r }
            responder = r.nextResponder
        }
        for candidate in [window as NSObject?, self, NSApp, NSApp.delegate as? NSObject] {
            if let c = candidate, c.responds(to: action) { return c }
        }
        return nil
    }

    /// Runs command `id` from where focus was, as its menu item would.
    func runCommand(_ id: String) {
        commandPalette.isHidden = true
        restoreFocusAfterPalette()
        guard let action = CommandPaletteView.actions[id], let target = target(for: action) else {
            return NSSound.beep()
        }
        commands.noteUsed(id: id)
        _ = target.perform(action, with: self)
    }

    /// Closes transient navigation overlays, whichever is open.
    func closeOverlays() {
        if isRecentItemsVisible { closeRecentItems() }
        closeReferencesPeek()
        if isWorkspaceSearchVisible { closeWorkspaceSearch() }
        if isQuickOpenVisible { closeGoToFile() }
        if isOutlinePickerVisible { closeGoToSymbol() }
        if isGoToLineVisible { closeGoToLine() }
        if isPHPVersionPickerVisible { closePHPVersionPicker() }
        if isIndentationPickerVisible { closeIndentationPicker() }
        if loadedGitPicker?.isHidden == false { closeGitBranchPicker() }
        if isCommandPaletteVisible {
            commandPalette.isHidden = true
        }
    }

    func installOverlayClickMonitor() {
        guard overlayClickMonitor == nil else { return }
        overlayClickMonitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown]) {
            [weak self] event in
            let windowNumber = event.windowNumber
            let location = event.locationInWindow
            var consume = false
            onAppKitMainThread {
                guard let self, windowNumber == self.window?.windowNumber else { return }
                let branchPickerOpen = self.loadedGitPicker?.isHidden == false
                let overlays: [NSView] = [
                    self.loadedPalette,
                    self.loadedPHPVersionPicker,
                    self.loadedIndentationPicker,
                    self.loadedGitPicker,
                    self.loadedRecentItemsPicker,
                ].compactMap { $0 }.filter { !$0.isHidden }
                guard !overlays.isEmpty else { return }

                // Let the status items receive their click so a second click toggles
                // their picker closed instead of closing and immediately reopening it.
                let triggers = [self.statusBar.language, self.statusBar.indent, self.statusBar.git]
                let isInside = (overlays + triggers).contains { view in
                    view.bounds.contains(view.convert(location, from: nil))
                }
                if !isInside {
                    self.closeOverlays()
                    consume = branchPickerOpen
                }
            }
            return consume ? nil : event
        }
    }
}
