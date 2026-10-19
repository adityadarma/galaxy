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

    func testMouseReportsUseXtermEncoding() {
        XCTAssertEqual(TerminalView.mouseReport(button: 0, column: 5, row: 7, sgr: true), "\u{1b}[<0;5;7M")
        XCTAssertEqual(TerminalView.mouseReport(button: 0, column: 5, row: 7, release: true, sgr: true), "\u{1b}[<0;5;7m")
        XCTAssertEqual(TerminalView.mouseReport(button: 64, column: 1, row: 1, sgr: true), "\u{1b}[<64;1;1M", "wheel up")
        XCTAssertEqual(TerminalView.mouseReport(button: 65, column: 1, row: 1, sgr: true), "\u{1b}[<65;1;1M", "wheel down")
        XCTAssertEqual(TerminalView.mouseReport(button: 0, column: 2, row: 3, motion: true, sgr: true), "\u{1b}[<32;2;3M", "drag")
        XCTAssertEqual(TerminalView.mouseReport(button: 0, column: 2, row: 3, modifiers: .control, sgr: true), "\u{1b}[<16;2;3M")
        XCTAssertEqual(TerminalView.mouseReport(button: 0, column: 1, row: 1, sgr: false), "\u{1b}[M !!")
        XCTAssertEqual(TerminalView.mouseReport(button: 0, column: 1, row: 1, release: true, sgr: false), "\u{1b}[M#!!")
        XCTAssertNil(TerminalView.mouseReport(button: 0, column: 200, row: 1, sgr: false), "X10 can't encode wide grids")
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

    func testHeaderShowsTheChipForOneTerminalOnly() {
        let header = BottomPanelHeader(frame: NSRect(x: 0, y: 0, width: 900, height: BottomPanelHeader.height))
        let zsh = TerminalSummary(name: "zsh", icon: PanelTerminal.defaultIcon, tint: nil, group: 0)
        header.setTerminals([zsh])
        XCTAssertFalse(header.terminalChip.isHidden, "one terminal: the chip names it")
        header.setTerminals([zsh, zsh])
        XCTAssertTrue(header.terminalChip.isHidden, "two or more: the list takes over")
        header.setTerminals([zsh])
        header.select(.problems)
        XCTAssertTrue(header.terminalChip.isHidden)
        XCTAssertEqual(header.selectedTab, .problems)
    }

    func testChipOpensTheTerminalMenu() {
        let header = BottomPanelHeader(frame: NSRect(x: 0, y: 0, width: 900, height: BottomPanelHeader.height))
        var asked = 0
        header.chipMenuProvider = { asked += 1; return NSMenu() }
        XCTAssertNotNil(header.terminalChip.menu(for: NSEvent.mouseEvent(
            with: .rightMouseDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0,
            context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!))
        XCTAssertEqual(asked, 1)
    }

    func testListShowsFromTwoTerminalsAndFollowsContentWidth() {
        let panel = TerminalPanelView(frame: NSRect(x: 0, y: 0, width: 800, height: 300))
        let one = TerminalSummary(name: "zsh", icon: PanelTerminal.defaultIcon, tint: nil, group: 0)
        panel.setList([one], selected: 0)
        XCTAssertTrue(panel.list.isHidden, "a single terminal has no list")
        panel.setList([one, one], selected: 1)
        XCTAssertFalse(panel.list.isHidden)
        XCTAssertEqual(panel.list.frame.width, TerminalListView.defaultWidth)
        XCTAssertEqual(panel.list.frame.maxX, 800, accuracy: 0.5, "the list sits at the right edge")

        panel.listFitsContent = true
        defer { panel.listFitsContent = false }
        panel.layoutSubtreeIfNeeded()
        XCTAssertLessThan(panel.list.frame.width, TerminalListView.defaultWidth, "short names need less than the default")
        XCTAssertGreaterThanOrEqual(panel.list.frame.width, TerminalListView.minimumWidth)
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
        for title in ["New Terminal", "Run Selected Text", "Run Active File", "Clear Terminal", "Kill Terminal",
                      "Split Terminal", "Move Terminal into New Window", "Rename Terminal\u{2026}",
                      "Change Terminal Color\u{2026}", "Change Terminal Icon\u{2026}"] {
            XCTAssertNotNil(menu.item(withTitle: title), title)
        }
        let view = try XCTUnwrap(MainMenu.build().items.compactMap(\.submenu).first { $0.title == "View" })
        XCTAssertNil(view.item(withTitle: "Move Terminal into Panel"), "terminal items moved out of View")
        Keybindings.defaults.apply(to: menu)
        let new = try XCTUnwrap(menu.item(withTitle: "New Terminal"))
        XCTAssertEqual(new.keyEquivalent, "`")
        XCTAssertEqual(new.keyEquivalentModifierMask, [.control, .shift])
    }

    private func workspaceController() throws -> (MainWindowController, URL) {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-term-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let c = MainWindowController(bridge: CoreBridge())
        c.askSaveChoice = { _ in .discard }
        c.showWindow(nil)
        try c.openWorkspace(at: root)
        return (c, root)
    }

    func testSplitTerminalsShareThePanelAndListThem() throws {
        let (c, root) = try workspaceController()
        defer { c.window?.orderOut(nil); try? FileManager.default.removeItem(at: root) }
        c.toggleTerminal(nil)
        XCTAssertEqual(c.panelTerminals.count, 1)

        let item = NSMenuItem()
        item.representedObject = c.panelTerminals[0]
        c.splitTerminal(item)
        XCTAssertEqual(c.panelTerminals.count, 2)
        XCTAssertEqual(c.panelTerminals[0].group, c.panelTerminals[1].group, "a split joins its source's group")
        XCTAssertEqual(c.terminalPanelForTesting.panes.count, 2, "both terminals are side by side")
        XCTAssertFalse(c.terminalPanelForTesting.list.isHidden)
        XCTAssertTrue(c.bottomPanelHeaderForTesting.terminalChip.isHidden)

        c.createTerminal(nil)
        XCTAssertEqual(c.panelTerminals.count, 3)
        XCTAssertNotEqual(c.panelTerminals[2].group, c.panelTerminals[0].group, "a new terminal gets a pane of its own")
        XCTAssertEqual(c.terminalPanelForTesting.panes.count, 1)

        let kill = NSMenuItem()
        kill.representedObject = c.panelTerminals[2]
        c.killTerminal(kill)
        XCTAssertEqual(c.panelTerminals.count, 2)
        c.panelTerminals.forEach { $0.handle.kill() }
    }

    func testOpeningAnotherFolderStartsFreshTerminalsThere() throws {
        let (c, root) = try workspaceController()
        let other = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-ws-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: other, withIntermediateDirectories: true)
        defer {
            c.panelTerminals.forEach { $0.handle.kill() }
            c.window?.orderOut(nil)
            try? FileManager.default.removeItem(at: root)
            try? FileManager.default.removeItem(at: other)
        }
        c.toggleTerminal(nil)
        c.createTerminal(nil)
        let old = c.panelTerminals
        XCTAssertEqual(old.count, 2)

        try c.openWorkspace(at: other)
        XCTAssertTrue(old.allSatisfy { !$0.handle.isRunning() }, "the old folder's shells are gone")
        XCTAssertTrue(c.isBottomPanelVisible, "the panel stays open")
        XCTAssertEqual(c.bottomPanelTab, .terminal)
        XCTAssertEqual(c.panelTerminals.count, 1, "one new shell, in the new folder")
        XCTAssertFalse(old.contains { $0 === c.panelTerminals[0] })
        XCTAssertTrue(c.panelTerminals[0].handle.isRunning())
    }

    func testRenameColorAndIconStickToOneTerminal() throws {
        let (c, root) = try workspaceController()
        defer { c.window?.orderOut(nil); try? FileManager.default.removeItem(at: root) }
        c.toggleTerminal(nil)
        c.createTerminal(nil)
        let first = c.panelTerminals[0], second = c.panelTerminals[1]
        let shell = first.handle.shellName()

        c.renameTerminal(first, to: "  server ")
        XCTAssertEqual(first.displayName, "server")
        XCTAssertEqual(second.displayName, shell, "other terminals keep their shell name")
        c.renameTerminal(first, to: "")
        XCTAssertEqual(first.displayName, shell, "empty brings the shell name back")

        c.setTerminalColor(first, name: "Green")
        XCTAssertEqual(first.tint, .systemGreen)
        c.setTerminalColor(first, name: "Not a color")
        XCTAssertNil(first.tint, "unknown colors are ignored")

        c.setTerminalIcon(second, symbol: "hammer")
        XCTAssertEqual(second.icon, "hammer")
        XCTAssertEqual(first.icon, PanelTerminal.defaultIcon)
        c.panelTerminals.forEach { $0.handle.kill() }
    }

    func testTerminalMenuListsTheVSCodeItems() throws {
        let (c, root) = try workspaceController()
        defer { c.window?.orderOut(nil); try? FileManager.default.removeItem(at: root) }
        c.toggleTerminal(nil)
        let menu = try XCTUnwrap(c.bottomPanelHeaderForTesting.terminalChip.menu(for: NSEvent.mouseEvent(
            with: .rightMouseDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0,
            context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!))
        XCTAssertEqual(menu.items.filter { !$0.isSeparatorItem }.map(\.title), [
            "Split Terminal", "Move Terminal into Editor Area", "Move Terminal into New Window",
            "Change Color\u{2026}", "Change Icon\u{2026}", "Rename\u{2026}",
            "Toggle Size to Content Width", "Kill Terminal",
        ])
        XCTAssertEqual(menu.items.filter(\.isSeparatorItem).count, 2)
        XCTAssertEqual(menu.item(withTitle: "Split Terminal")?.keyEquivalent, "\\")
        c.panelTerminals.forEach { $0.handle.kill() }
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

    /// ⌘V goes through the window's key monitor and menu, not `keyDown`, so
    /// the terminal has to answer the paste command itself.
    func testCommandVPastesIntoTheFocusedTerminal() throws {
        let (c, root) = try workspaceController()
        defer { c.window?.orderOut(nil); try? FileManager.default.removeItem(at: root) }
        c.toggleTerminal(nil)
        let pane = c.terminalPanelForTesting.activePane
        pane.pasteboard = NSPasteboard(name: NSPasteboard.Name("galaxy-test-\(UUID().uuidString)"))
        pane.pasteboard.clearContents()
        pane.pasteboard.setString("echo galaxy_paste_ok", forType: .string)
        c.window?.makeFirstResponder(pane)
        XCTAssertTrue(c.window?.firstResponder === pane)

        // A real ⌘V event, so it takes the same route as a keypress: the
        // window's key monitor, then the menu.
        let event = try XCTUnwrap(NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: .command, timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: c.window?.windowNumber ?? 0, context: nil, characters: "v",
            charactersIgnoringModifiers: "v", isARepeat: false, keyCode: 9))
        NSApp.sendEvent(event)
        let deadline = Date().addingTimeInterval(5)
        var seen = false
        while Date() < deadline, !seen {
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
            seen = pane.screen?.lines.contains { $0.text.contains("galaxy_paste_ok") } == true
        }
        XCTAssertTrue(seen, "the pasted text reaches the shell")
        c.panelTerminals.forEach { $0.handle.kill() }
    }
}
