import AppKit
import XCTest
@testable import Galaxy

@MainActor
final class TerminalPanelTests: XCTestCase {
    private func key(_ keyCode: UInt16, _ characters: String, _ flags: NSEvent.ModifierFlags = []) -> NSEvent {
        NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0,
                         windowNumber: 0, context: nil, characters: characters,
                         charactersIgnoringModifiers: characters.lowercased(), isARepeat: false,
                         keyCode: keyCode)!
    }

    func testKeysSendXtermSequences() {
        XCTAssertEqual(TerminalView.bytes(for: key(36, "\r"), appCursor: false), "\r")
        XCTAssertEqual(TerminalView.bytes(for: key(51, "\u{7f}"), appCursor: false), "\u{7f}")
        XCTAssertEqual(TerminalView.bytes(for: key(126, "\u{F700}"), appCursor: false), "\u{1b}[A")
        XCTAssertEqual(TerminalView.bytes(for: key(126, "\u{F700}"), appCursor: true), "\u{1b}OA",
                       "vim and less ask for application cursor keys")
        XCTAssertEqual(TerminalView.bytes(for: key(8, "c", .control), appCursor: false), "\u{03}", "⌃C")
        XCTAssertEqual(TerminalView.bytes(for: key(2, "d", .control), appCursor: false), "\u{04}", "⌃D")
        XCTAssertEqual(TerminalView.bytes(for: key(123, "\u{F702}", .option), appCursor: false), "\u{1b}b",
                       "⌥← moves back a word")
        XCTAssertEqual(TerminalView.bytes(for: key(124, "\u{F703}", .shift), appCursor: false), "\u{1b}[1;2C")
        XCTAssertEqual(TerminalView.bytes(for: key(48, "\t", .shift), appCursor: false), "\u{1b}[Z")
    }

    func testFileLinksAreFoundInOutput() {
        let link = TerminalView.fileLink(in: "error: src/main.rs:12:5: expected `;`")
        XCTAssertEqual(link?.path, "src/main.rs")
        XCTAssertEqual(link?.line, 12)
        XCTAssertEqual(link?.column, 5)
        XCTAssertEqual(TerminalView.fileLink(in: "  at ./app/index.ts:7")?.line, 7)
        XCTAssertNil(TerminalView.fileLink(in: "no link here"))
    }

    func testWideCharactersTakeTwoCells() {
        XCTAssertEqual(TerminalView.cellCount("a"), 1)
        XCTAssertEqual(TerminalView.cellCount("漢"), 2)
        XCTAssertEqual(TerminalView.cellCount("😀"), 2)
    }

    func testPaletteFollowsAnsiAndInverse() {
        let palette = TerminalPalette(appearance: NSAppearance(named: .darkAqua)!)
        let run = TerminalRunInfo(column: 0, text: "x", foreground: .indexed(index: 1), background: .default,
                                  bold: false, italic: false, dim: false, underline: false, strikeout: false,
                                  inverse: false)
        XCTAssertEqual(palette.colors(for: run).foreground, palette.ansi[1])
        XCTAssertNil(palette.colors(for: run).background, "default background is not painted")
        var inverse = run
        inverse.inverse = true
        XCTAssertEqual(palette.colors(for: inverse).background, palette.ansi[1])
        var bold = run
        bold.bold = true
        XCTAssertEqual(palette.colors(for: bold).foreground, palette.ansi[9], "bold uses the bright color")
    }

    func testHeaderShowsTerminalActionsOnlyOnTheTerminalTab() {
        let header = BottomPanelHeader(frame: NSRect(x: 0, y: 0, width: 900, height: BottomPanelHeader.height))
        header.setTerminals(names: ["zsh"], selected: 0)
        XCTAssertFalse(header.terminalPicker.isHidden)
        XCTAssertEqual(header.terminalPicker.titleOfSelectedItem, "zsh")
        header.setTerminals(names: ["zsh", "zsh"], selected: 1)
        XCTAssertEqual(header.terminalPicker.titleOfSelectedItem, "2: zsh")
        header.select(.problems)
        XCTAssertTrue(header.terminalPicker.isHidden)
        XCTAssertEqual(header.selectedTab, .problems)
    }

    func testPanelOpensClosesAndMaximizesWithoutRemovingViews() {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        XCTAssertFalse(c.isBottomPanelVisible)
        c.toggleTerminal(nil)
        XCTAssertTrue(c.isBottomPanelVisible)
        c.toggleTerminal(nil)
        XCTAssertFalse(c.isBottomPanelVisible)
        XCTAssertFalse(c.isBottomPanelMaximized, "closing restores the size")
    }

    func testTitleBarButtonTogglesPanelAndEditorStaysVisible() {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.window?.setContentSize(NSSize(width: 1200, height: 800))
        c.newDocument(nil)
        c.titleBar.panelButton.performClick(nil)
        XCTAssertTrue(c.isBottomPanelVisible)
        XCTAssertEqual(c.titleBar.panelButton.state, .on)
        c.window?.layoutIfNeeded()
        let editor = c.editorView.enclosingScrollView!.convert(c.editorView.enclosingScrollView!.bounds, to: nil)
        XCTAssertGreaterThan(editor.height, 200, "the editor keeps its space above the panel")
        c.titleBar.panelButton.performClick(nil)
        XCTAssertFalse(c.isBottomPanelVisible)
        XCTAssertEqual(c.titleBar.panelButton.state, .off)
    }

    func testProblemBadgeIsARoundPillThatGrowsWithTheCount() {
        let badge = CountBadge()
        badge.count = 7
        XCTAssertEqual(badge.intrinsicContentSize, NSSize(width: CountBadge.height, height: CountBadge.height), "one digit: a circle")
        badge.count = 128
        XCTAssertGreaterThan(badge.intrinsicContentSize.width, CountBadge.height, "more digits: a wider pill")
        badge.count = 5000
        XCTAssertEqual(badge.text, "999+")
    }

    func testBlockAndBoxCharactersFillTheirCell() throws {
        let cell = NSRect(x: 10, y: 20, width: 8, height: 16)
        func union(_ scalar: Unicode.Scalar) throws -> NSRect {
            try XCTUnwrap(TerminalView.customGlyph(scalar, cell: cell)).map(\.rect).reduce(NSRect.null) { $0.union($1) }
        }
        XCTAssertEqual(try union("\u{2588}"), cell, "full block")
        XCTAssertEqual(try union("\u{2580}"), NSRect(x: 10, y: 20, width: 8, height: 8), "upper half")
        XCTAssertEqual(try union("\u{2584}"), NSRect(x: 10, y: 28, width: 8, height: 8), "lower half")
        XCTAssertEqual(try union("\u{258C}"), NSRect(x: 10, y: 20, width: 4, height: 16), "left half")
        XCTAssertEqual(try union("\u{2590}"), NSRect(x: 14, y: 20, width: 4, height: 16), "right half")
        XCTAssertEqual(try union("\u{259B}"), cell, "three quadrants span the cell")
        // Vertical lines run top to bottom, so stacked rows join up.
        let bar = try union("\u{2502}")
        XCTAssertEqual(bar.minY, cell.minY)
        XCTAssertEqual(bar.maxY, cell.maxY)
        let dash = try union("\u{2500}")
        XCTAssertEqual(dash.minX, cell.minX)
        XCTAssertEqual(dash.maxX, cell.maxX)
        XCTAssertNil(TerminalView.customGlyph("a", cell: cell))
    }

    func testPanelTopEdgeDragsTheHeight() throws {
        let c = MainWindowController(bridge: CoreBridge())
        c.window?.setContentSize(NSSize(width: 1000, height: 800))
        c.showWindow(nil)
        defer { c.window?.orderOut(nil); UserDefaults.standard.removeObject(forKey: "BottomPanelHeight") }
        c.toggleTerminal(nil)
        c.window?.layoutIfNeeded()
        let header = c.bottomPanelHeaderForTesting
        let before = c.bottomPanelContentHeight
        // The top strip of the header is the drag handle.
        let sash = header.superview!.convert(NSPoint(x: header.bounds.midX, y: 2), from: header)
        XCTAssertTrue(header.hitTest(sash) === header)
        header.onResize?(true, 0)
        header.onResize?(false, 100)
        XCTAssertEqual(c.bottomPanelContentHeight, before + 100, accuracy: 0.5)
        header.onResize?(true, 0)
        header.onResize?(false, -10_000)
        XCTAssertEqual(c.bottomPanelContentHeight, MainWindowController.minBottomPanelHeight, "never below the minimum")
        header.onResize?(true, 0)
        header.onResize?(false, 10_000)
        c.window?.layoutIfNeeded()
        let editorTop = c.tabBar.convert(c.tabBar.bounds, to: nil).minY
        let headerTop = header.convert(header.bounds, to: nil).maxY
        XCTAssertGreaterThanOrEqual(editorTop - headerTop, 39, "the editor keeps some room")
        XCTAssertEqual(UserDefaults.standard.double(forKey: "BottomPanelHeight"), Double(c.bottomPanelContentHeight), accuracy: 0.5)
    }

    func testTerminalMovesIntoEditorAreaAndBack() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-term-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try "hello".write(to: root.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: root) }
        let c = MainWindowController(bridge: CoreBridge())
        c.askSaveChoice = { _ in XCTFail("terminal tabs never ask to save"); return .cancel }
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        try c.openWorkspace(at: root)
        try c.openFile(at: root.appendingPathComponent("a.txt"))

        // Panel terminal → editor tab, same shell.
        c.toggleTerminal(nil)
        XCTAssertTrue(c.isBottomPanelVisible)
        c.moveTerminalToEditor(nil)
        let tab = try XCTUnwrap(c.activeTab)
        let handle = try XCTUnwrap(tab.terminal)
        XCTAssertTrue(handle.isRunning())
        XCTAssertEqual(c.terminalTabs.count, 1)
        XCTAssertTrue(c.tabBar.items.last?.isTerminal == true)
        XCTAssertEqual(c.tabBar.items.last?.title, handle.shellName())
        XCTAssertFalse(c.editorTerminalView.isHidden)
        XCTAssertTrue(c.window?.firstResponder === c.editorTerminalView)
        c.window?.layoutIfNeeded()
        let terminalFrame = c.editorTerminalView.convert(c.editorTerminalView.bounds, to: nil)
        let tabFrame = c.tabBar.convert(c.tabBar.bounds, to: nil)
        // The layer-backed terminal must not be composited over the tab bar:
        // their parent needs a layer too.
        XCTAssertTrue(c.tabBar.superview?.wantsLayer == true)
        XCTAssertFalse(c.tabBar.isHidden)
        XCTAssertEqual(c.tabBar.items.count, 2, "the file tab stays")
        XCTAssertLessThanOrEqual(terminalFrame.maxY, tabFrame.minY + 0.5, "the terminal sits below the tab bar")
        XCTAssertFalse(c.isBottomPanelVisible, "the panel closes with its last terminal")

        // Switching to the file hides the terminal; back shows it again.
        c.selectTab(0)
        XCTAssertTrue(c.editorTerminalView.isHidden)
        XCTAssertTrue(c.window?.firstResponder === c.editorView)
        c.selectTab(1)
        XCTAssertFalse(c.editorTerminalView.isHidden)

        // Session and unsaved prompts skip the terminal tab.
        XCTAssertEqual(c.currentSession().tabs.count, 1)
        XCTAssertTrue(c.confirmDiscardChanges())

        // Back into the panel, still the same running shell.
        c.moveTerminalToPanel(nil)
        XCTAssertTrue(c.terminalTabs.isEmpty)
        XCTAssertTrue(c.isBottomPanelVisible)
        XCTAssertTrue(handle.isRunning())
        XCTAssertTrue(c.editorTerminalView.isHidden)

        // A new editor terminal is killed when its tab closes.
        c.createTerminalEditor(nil)
        let other = try XCTUnwrap(c.activeTab?.terminal)
        XCTAssertTrue(c.closeTab(at: try XCTUnwrap(c.activeIndex)))
        let deadline = Date().addingTimeInterval(3)
        while other.isRunning() && Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.05)) }
        XCTAssertFalse(other.isRunning())
        XCTAssertTrue(c.terminalTabs.isEmpty)
        handle.kill()
    }

    func testTerminalMenuHasItsOwnCommands() throws {
        let menu = try XCTUnwrap(MainMenu.build().items.compactMap(\.submenu).first { $0.title == "Terminal" })
        for title in ["New Terminal", "Run Selected Text", "Run Active File", "Clear Terminal", "Kill Terminal"] {
            XCTAssertNotNil(menu.item(withTitle: title), title)
        }
        let view = try XCTUnwrap(MainMenu.build().items.compactMap(\.submenu).first { $0.title == "View" })
        XCTAssertNil(view.item(withTitle: "Move Terminal into Panel"), "terminal items moved out of View")
        Keybindings.defaults.apply(to: menu)
        let new = try XCTUnwrap(menu.item(withTitle: "New Terminal"))
        XCTAssertEqual(new.keyEquivalent, "`")
        XCTAssertEqual(new.keyEquivalentModifierMask, [.control, .shift])
    }

    func testRunSelectedTextAndKillTerminal() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-term-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try "echo ok".write(to: root.appendingPathComponent("a.txt"), atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: root) }
        let c = MainWindowController(bridge: CoreBridge())
        c.askSaveChoice = { _ in .discard }
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        try c.openWorkspace(at: root)
        try c.openFile(at: root.appendingPathComponent("a.txt"))

        let kill = NSMenuItem(title: "Kill Terminal", action: #selector(MainWindowController.killTerminal(_:)), keyEquivalent: "")
        XCTAssertFalse(c.validateMenuItem(kill), "nothing to kill yet")

        // No terminal yet: running text opens one in the panel.
        c.editorView.document?.selectAll()
        c.runSelectedTextInTerminal(nil)
        XCTAssertTrue(c.isBottomPanelVisible)
        XCTAssertTrue(c.validateMenuItem(kill))

        c.killTerminal(nil)
        XCTAssertFalse(c.isBottomPanelVisible, "the panel closes with its last terminal")
        XCTAssertFalse(c.validateMenuItem(kill))
    }
}
