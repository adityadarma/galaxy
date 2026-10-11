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
}
