import XCTest
@testable import Galaxy

/// M2-09: command registry and palette (⇧⌘P).
@MainActor
final class CommandPaletteTests: XCTestCase {
    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-cmd-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let long = (1...200).map { "line \($0)" }.joined(separator: "\n")
        try long.write(to: root.appendingPathComponent("long.txt"), atomically: true, encoding: .utf8)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    private func makeController() -> MainWindowController {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        c.askSaveChoice = { _ in .discard }
        return c
    }

    private func command(_ panel: CommandPaletteView, _ selector: Selector) throws {
        let editor = try XCTUnwrap(panel.field.currentEditor() as? NSTextView)
        _ = panel.control(panel.field, textView: editor, doCommandBy: selector)
    }

    func testEveryCommandHasAnActionAndEveryActionACommand() {
        let ids = Set(CommandPalette().commands().map(\.id))
        XCTAssertEqual(ids, Set(CommandPaletteView.actions.keys))
    }

    func testRustGeneratedSourceRefreshCommandIsRegistered() {
        XCTAssertNotNil(CommandPaletteView.actions["galaxy.rust.refreshGeneratedSources"])
        XCTAssertTrue(CommandPalette().commands().contains { $0.id == "galaxy.rust.refreshGeneratedSources" })
    }

    /// NSWindow implements `toggleSidebar:`, `selectNextTab:` and others
    /// for its own features, and it comes before the controller in the
    /// responder chain, so a same-named action never reached us.
    func testNoActionIsShadowedByAppKit() {
        let window = NSWindow()
        let view = NSView()
        for (id, action) in CommandPaletteView.actions {
            let ours = MainWindowController.instancesRespond(to: action) || EditorView.instancesRespond(to: action)
            guard ours else { continue }
            if EditorView.instancesRespond(to: action), !MainWindowController.instancesRespond(to: action) {
                continue  // the editor is first in the chain
            }
            XCTAssertFalse(window.responds(to: action), "\(id): NSWindow takes \(action)")
            XCTAssertFalse(view.responds(to: action), "\(id): every view takes \(action)")
        }
    }

    func testEveryMenuItemIsACommand() {
        // Menu items without a command would be missing from the palette.
        let actions = Set(CommandPaletteView.actions.values)
        var missing: [String] = []
        func walk(_ menu: NSMenu) {
            for item in menu.items {
                if let sub = item.submenu { walk(sub) }
                if item.submenu == nil, let action = item.action, !actions.contains(action),
                   action != #selector(NSApplication.hide(_:)),
                   action != #selector(NSApplication.hideOtherApplications(_:)),
                   action != #selector(NSWindow.performMiniaturize(_:)),  // system window items
                   action != #selector(NSWindow.performZoom(_:)),
                   action != #selector(NSApplication.arrangeInFront(_:)),
                   action != #selector(NSWindow.makeKeyAndOrderFront(_:)),  // AppKit's open-window list
                   action != #selector(MainWindowController.showCommandPalette(_:)),
                   action != #selector(MainWindowController.openRecentProject(_:)) {  // a list, not one command
                    missing.append(NSStringFromSelector(action))
                }
            }
        }
        walk(MainMenu.build())
        XCTAssertEqual(missing, [])
    }

    func testTypingFiltersAndReturnRunsTheCommand() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: root.appendingPathComponent("long.txt"))
        c.showCommandPalette(nil)
        let panel = c.commandPalette
        XCTAssertTrue(c.isCommandPaletteVisible)
        XCTAssertTrue(c.window?.firstResponder === panel.field.currentEditor(), "the field has focus")

        panel.setQuery("go line")
        XCTAssertEqual(panel.results.first?.id, "workbench.action.gotoLine")
        XCTAssertEqual(panel.results.first?.label, "Go: Go to Line…")
        try command(panel, #selector(NSResponder.insertNewline(_:)))
        XCTAssertFalse(c.isCommandPaletteVisible)
        XCTAssertTrue(c.isGoToLineVisible, "the command ran")
        XCTAssertEqual(c.commands.recent(), ["workbench.action.gotoLine"])

        c.closeGoToLine()
        c.showCommandPalette(nil)
        XCTAssertEqual(c.commandPalette.results.first?.id, "workbench.action.gotoLine", "recent first")
        XCTAssertTrue(c.commandPalette.results.first?.isRecent == true)
    }

    func testEditorCommandsRunOnTheEditor() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: root.appendingPathComponent("long.txt"))
        c.window?.makeFirstResponder(c.editorView)
        c.showCommandPalette(nil)
        c.commandPalette.setQuery("select all")
        XCTAssertEqual(c.commandPalette.results.first?.id, "editor.action.selectAll")
        try command(c.commandPalette, #selector(NSResponder.insertNewline(_:)))
        XCTAssertTrue(c.window?.firstResponder === c.editorView, "focus goes back to the editor")
        let range = c.editorView.document!.selectedRange()
        XCTAssertEqual(range.length, c.editorView.document!.lenUtf16())
    }

    func testOnlyCommandsThatCanRunAreListed() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        let empty = Set(c.availableCommandIDs())
        XCTAssertTrue(empty.contains("workbench.action.files.openFolder"))
        XCTAssertTrue(empty.contains("workbench.action.toggleSidebarVisibility"))
        for id in ["workbench.action.files.save", "workbench.action.gotoLine", "workbench.action.quickOpen",
                   "undo", "actions.find", "workbench.action.navigateBack"] {
            XCTAssertFalse(empty.contains(id), id)
        }

        try c.openWorkspace(at: root)
        try c.openFile(at: root.appendingPathComponent("long.txt"))
        let open = Set(c.availableCommandIDs())
        for id in ["workbench.action.files.save", "workbench.action.gotoLine", "workbench.action.quickOpen",
                   "actions.find", "workbench.files.action.refreshFilesExplorer"] {
            XCTAssertTrue(open.contains(id), id)
        }
        XCTAssertTrue(open.contains("workbench.action.openUserSettingsJson"))
        XCTAssertTrue(open.contains("workbench.action.openWorkspaceSettingsJson"))
        XCTAssertFalse(open.contains("undo"), "nothing to undo yet")
    }

    func testShortcutsAreShownAndEscapeCloses() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: root.appendingPathComponent("long.txt"))
        c.showCommandPalette(nil)
        let panel = c.commandPalette
        XCTAssertEqual(panel.shortcuts["workbench.action.files.saveAs"], "⇧⌘S")
        XCTAssertEqual(panel.shortcuts["workbench.action.gotoLine"], "⌃G")
        XCTAssertEqual(panel.shortcuts["workbench.action.navigateBack"], "⌃-")
        XCTAssertEqual(CommandPaletteView.describe(key: "\u{F700}", modifiers: [.command, .option]), "⌥⌘↑")

        panel.setQuery("qqqzzz")
        XCTAssertTrue(panel.results.isEmpty)
        XCTAssertFalse(panel.message.isHidden)
        try command(panel, #selector(NSResponder.cancelOperation(_:)))
        XCTAssertFalse(c.isCommandPaletteVisible)
        XCTAssertTrue(c.commands.recent().isEmpty, "nothing ran")
    }

    func testOpeningOnePanelClosesTheOthers() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openWorkspace(at: root)
        try c.openFile(at: root.appendingPathComponent("long.txt"))
        c.showGoToLine(nil)
        c.showCommandPalette(nil)
        XCTAssertFalse(c.isGoToLineVisible)
        XCTAssertTrue(c.isCommandPaletteVisible)
        c.showGoToFile(nil)
        XCTAssertFalse(c.isCommandPaletteVisible)
        XCTAssertTrue(c.isQuickOpenVisible)
    }

    func testMenuShortcut() throws {
        let menu = try XCTUnwrap(MainMenu.build().items.compactMap(\.submenu).first { $0.title == "View" })
        let item = try XCTUnwrap(menu.item(withTitle: "Command Palette…"))
        XCTAssertEqual(item.keyEquivalent, "p")
        XCTAssertEqual(item.keyEquivalentModifierMask, [.command, .shift])
    }
}
