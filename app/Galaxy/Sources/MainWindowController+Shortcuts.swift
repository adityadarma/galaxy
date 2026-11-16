import AppKit
import os

extension MainWindowController {
    // MARK: Keyboard shortcuts (M2-10)

    /// Runs the command bound to key `id`, from the focused view, if its
    /// menu item would be enabled. `false` lets the key go on as usual
    /// (typing, a text field's own ⌘A, a disabled command).
    @discardableResult
    func performKeyBinding(_ id: String) -> Bool {
        // Bare and shifted function keys belong to the shell's programs
        // (htop, vim) while a terminal has focus.
        if window?.firstResponder is TerminalView, !id.contains("cmd+"),
           id.range(of: #"f\d+$"#, options: .regularExpression) != nil {
            return false
        }
        guard let command = keybindings.commandByKey[id],
              let action = CommandPaletteView.actions[command],
              let target = target(for: action) else { return false }
        let item = NSMenuItem(title: "", action: action, keyEquivalent: "")
        if let validator = target as? NSMenuItemValidation, !validator.validateMenuItem(item) { return false }
        _ = target.perform(action, with: self)
        return true
    }

    /// Reads `keybindings.json` again if it changed, and updates the menu.
    /// Problems are logged and shown in the status line.
    func reloadKeybindings(force: Bool = false) {
        let modified = (try? keybindingsURL.resourceValues(forKeys: [.contentModificationDateKey]))?
            .contentModificationDate
        guard force || modified != keybindingsModified else { return }
        keybindingsModified = modified
        keybindings = Keybindings.load(from: keybindingsURL)
        if let menu = NSApp.mainMenu { keybindings.apply(to: menu) }
        for problem in keybindings.problems {
            Log.ui.error("keybindings: \(problem, privacy: .public)")
        }
        if let first = keybindings.problems.first {
            let more = keybindings.problems.count > 1 ? " (+\(keybindings.problems.count - 1) more)" : ""
            statusLabel.stringValue = "Keyboard shortcuts: \(first)\(more)"
        }
    }

    /// Opens `keybindings.json`, created with every default listed in
    /// comments if it does not exist. Saving it applies it.
    @objc func openKeybindingsFile(_ sender: Any?) {
        let url = keybindingsURL
        do {
            if !FileManager.default.fileExists(atPath: url.path) {
                try FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                        withIntermediateDirectories: true)
                try keybindingsTemplate().write(to: url, atomically: true, encoding: .utf8)
            }
            try openFile(at: url)
        } catch {
            report(error, while: "open keybindings.json")
        }
    }

    private func openJSONSettings(at url: URL, action: String) {
        do {
            if !FileManager.default.fileExists(atPath: url.path) {
                try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
                try "{\n  \"editor\": {\n    \"insertSpaces\": true,\n    \"tabSize\": 4\n  }\n}\n".write(to: url, atomically: true, encoding: .utf8)
            }
            try openFile(at: url)
        } catch {
            report(error, while: action)
        }
    }

    @objc func openUserSettingsJson(_ sender: Any?) {
        openJSONSettings(at: globalSettingsURL, action: "open user settings")
    }

    @objc func openWorkspaceSettingsJson(_ sender: Any?) {
        guard let workspace else {
            statusLabel.stringValue = "Open a workspace to use workspace settings."
            return NSSound.beep()
        }
        openJSONSettings(at: GalaxySettings.workspaceURL(root: workspace.root()), action: "open workspace settings")
    }

    /// Every keyboard shortcut goes through here (M2-10), before menus
    /// and the focused view see the key. That covers keys a menu key
    /// equivalent cannot match (⌃⇧- arrives as `_`), and extra keys from
    /// `keybindings.json`.
    func installKeyMonitor() {
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .flagsChanged]) { [weak self] event in
            if event.type == .flagsChanged {
                let commandHeld = event.modifierFlags.contains(.command)
                DispatchQueue.main.async { [weak self] in
                    guard let self, self.window?.isKeyWindow == true else { return }
                    self.editorView.updateDefinitionLinkAtWindowMouse(
                        commandHeld: commandHeld
                    )
                    self.updateHover(commandHeld: commandHeld)
                }
                return event
            }
            // Only plain values cross into the main-actor closure.
            guard let id = Keybindings.keyID(for: event) else { return event }
            let windowNumber = event.windowNumber
            let handled = onAppKitMainThread { () -> Bool in
                guard let self, windowNumber == self.window?.windowNumber else { return false }
                if event.keyCode == 0x6F, event.modifierFlags.intersection(.deviceIndependentFlagsMask).isEmpty {
                    self.goToDefinition(self)
                    return true
                }
                return self.performKeyBinding(id)
            }
            return handled ? nil : event
        }
    }
}
