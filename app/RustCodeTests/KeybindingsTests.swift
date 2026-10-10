import XCTest
@testable import RustCode

/// M2-10: default shortcuts and keybindings.json.
@MainActor
final class KeybindingsTests: XCTestCase {
    // Set in setUp, which XCTest calls outside the main actor.
    nonisolated(unsafe) private var dir: URL!

    override func setUpWithError() throws {
        dir = FileManager.default.temporaryDirectory.appendingPathComponent("rcode-keys-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: dir)
    }

    private func key(_ chars: String, code: UInt16, _ mods: NSEvent.ModifierFlags, window: NSWindow? = nil) -> NSEvent {
        NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: mods, timestamp: 0,
                         windowNumber: window?.windowNumber ?? 0, context: nil, characters: chars,
                         charactersIgnoringModifiers: chars, isARepeat: false, keyCode: code)!
    }

    func testKeyEventsBecomeTheCoresKeyIDs() {
        XCTAssertEqual(Keybindings.keyID(for: key("P", code: 35, [.command, .shift])), "shift+cmd+p")
        // Shift turns - into _ on a US layout; the id still names the key.
        XCTAssertEqual(Keybindings.keyID(for: key("_", code: 27, [.control, .shift])), "ctrl+shift+-")
        XCTAssertEqual(Keybindings.keyID(for: key("\t", code: 48, [.control])), "ctrl+tab")
        XCTAssertEqual(Keybindings.keyID(for: key("\u{F700}", code: 126, [.command, .option, .function, .numericPad])),
                       "alt+cmd+up", "arrow keys carry fn and numeric-pad flags; they are ignored")
        XCTAssertEqual(Keybindings.keyID(for: key("\u{F708}", code: 96, [])), "f5")
    }

    func testEveryDefaultIsOnItsMenuItemAndMatchesTheOldShortcuts() throws {
        let menu = MainMenu.build()
        Keybindings.defaults.apply(to: menu)
        func item(_ action: Selector) throws -> NSMenuItem {
            var found: NSMenuItem?
            func walk(_ m: NSMenu) {
                for i in m.items {
                    if i.action == action { found = i }
                    if let sub = i.submenu { walk(sub) }
                }
            }
            walk(menu)
            return try XCTUnwrap(found, NSStringFromSelector(action))
        }
        let palette = try item(#selector(MainWindowController.showCommandPalette(_:)))
        XCTAssertEqual(palette.keyEquivalent, "p")
        XCTAssertEqual(palette.keyEquivalentModifierMask, [.command, .shift])
        let above = try item(#selector(EditorView.addCursorAbove(_:)))
        XCTAssertEqual(above.keyEquivalent, "\u{F700}")
        XCTAssertEqual(above.keyEquivalentModifierMask, [.command, .option])
        let last = try item(#selector(MainWindowController.selectLastUsedTab(_:)))
        XCTAssertEqual(last.keyEquivalent, "\t")
        XCTAssertEqual(last.keyEquivalentModifierMask, .control)
        // Not commands: left alone.
        XCTAssertEqual(try item(#selector(NSApplication.hide(_:))).keyEquivalent, "h")
        XCTAssertEqual(Keybindings.defaults.problems, [])
    }

    func testTheUserFileAddsRemovesAndReportsProblems() throws {
        let url = dir.appendingPathComponent("keybindings.json")
        try """
        [
          // Second key for Go to File.
          { "key": "cmd+t", "command": "workbench.action.quickOpen" },
          { "key": "cmd+b", "command": "-workbench.action.toggleSidebarVisibility" },
          { "key": "cmd+k", "command": "nope" },
        ]
        """.write(to: url, atomically: true, encoding: .utf8)
        let map = Keybindings.load(from: url)
        XCTAssertEqual(map.commandByKey["cmd+t"], "workbench.action.quickOpen")
        XCTAssertEqual(map.commandByKey["cmd+p"], "workbench.action.quickOpen", "the default stays")
        XCTAssertEqual(map.primary["workbench.action.quickOpen"]?.keyId, "cmd+t", "the user's key is shown")
        XCTAssertNil(map.commandByKey["cmd+b"])
        XCTAssertEqual(map.problems.count, 1)
        XCTAssertTrue(map.problems[0].contains("unknown command \"nope\""), map.problems[0])

        let missing = Keybindings.load(from: dir.appendingPathComponent("none.json"))
        XCTAssertEqual(missing.bindings, Keybindings.defaults.bindings)
        XCTAssertEqual(missing.problems, [])
    }

    func testKeysRunTheirCommandOnlyWhenItIsAvailable() throws {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.newDocument(nil)
        XCTAssertTrue(c.performKeyBinding("shift+cmd+p"))
        XCTAssertTrue(c.isCommandPaletteVisible)
        c.closeCommandPalette()
        // Back has nowhere to go: the key is not taken.
        XCTAssertFalse(c.performKeyBinding("ctrl+-"))
        XCTAssertFalse(c.performKeyBinding("cmd+k"), "unbound")
    }

    func testOpeningTheFileCreatesItAndSavingAppliesIt() throws {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.askSaveChoice = { _ in .discard }
        c.keybindingsURL = dir.appendingPathComponent("User/keybindings.json")
        c.openKeybindingsFile(nil)
        let text = try String(contentsOf: c.keybindingsURL, encoding: .utf8)
        XCTAssertTrue(text.contains("\"key\": \"cmd+p\", \"command\": \"workbench.action.quickOpen\""))
        XCTAssertEqual(c.activeTab?.title, "keybindings.json")

        // Replace the contents as the user would, then save.
        let doc = try XCTUnwrap(c.editorView.document)
        doc.selectAll()
        doc.edit(command: .insertText(text: "[ { \"key\": \"cmd+t\", \"command\": \"workbench.action.showCommands\" } ]"))
        c.editorView.didChangeDocument()
        XCTAssertTrue(c.save())
        XCTAssertEqual(c.keybindings.commandByKey["cmd+t"], "workbench.action.showCommands")
        XCTAssertEqual(c.statusLabel.stringValue, "Keyboard shortcuts updated.")
        XCTAssertTrue(c.performKeyBinding("cmd+t"))
        XCTAssertTrue(c.isCommandPaletteVisible)
        c.closeCommandPalette()

        // A broken file: defaults, and the status line says why.
        doc.selectAll()
        doc.edit(command: .insertText(text: "[ { "))
        c.editorView.didChangeDocument()
        XCTAssertTrue(c.save())
        XCTAssertNil(c.keybindings.commandByKey["cmd+t"])
        XCTAssertTrue(c.statusLabel.stringValue.hasPrefix("Keyboard shortcuts: keybindings.json line 1"),
                      c.statusLabel.stringValue)
    }
}
