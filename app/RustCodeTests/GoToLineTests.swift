import XCTest
@testable import RustCode

/// M2-08: Go to Line (⌃G) and back/forward history (⌃- / ⌃⇧-).
@MainActor
final class GoToLineTests: XCTestCase {
    // Set in setUp, which XCTest calls outside the main actor.
    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("rcode-gtl-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let long = (1...300).map { "    line \($0)" }.joined(separator: "\n")
        try long.write(to: root.appendingPathComponent("long.txt"), atomically: true, encoding: .utf8)
        try "a\nb\n".write(to: root.appendingPathComponent("short.txt"), atomically: true, encoding: .utf8)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    private func makeController() -> MainWindowController {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        c.askSaveChoice = { _ in XCTFail("unexpected save prompt"); return .cancel }
        return c
    }

    private func file(_ name: String) -> URL { root.appendingPathComponent(name) }

    private func caret(_ c: MainWindowController) -> (UInt64, UInt64) {
        let p = c.editorView.document!.viewport(firstLine: 0, count: 0).primary
        return (p.line, p.col)
    }

    private func command(_ panel: GoToLine, _ selector: Selector) throws {
        let editor = try XCTUnwrap(panel.field.currentEditor() as? NSTextView)
        _ = panel.control(panel.field, textView: editor, doCommandBy: selector)
    }

    func testReturnJumpsToTheLineAndScrollsItIntoView() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("long.txt"))
        c.showGoToLine(nil)
        let panel = c.goToLine
        XCTAssertTrue(c.isGoToLineVisible)
        XCTAssertTrue(c.window?.firstResponder === panel.field.currentEditor(), "the field has focus")
        XCTAssertTrue(panel.field.placeholderString?.contains("Line 1 of 300") == true)

        panel.setInput("250")
        XCTAssertEqual(panel.message.stringValue, "Go to line 250")
        try command(panel, #selector(NSResponder.insertNewline(_:)))
        XCTAssertFalse(c.isGoToLineVisible)
        XCTAssertEqual(caret(c).0, 249)
        XCTAssertEqual(caret(c).1, 4, "first non-blank character")
        XCTAssertTrue(c.window?.firstResponder === c.editorView)
        let top = CGFloat(249) * c.editorView.metrics.lineHeight
        XCTAssertTrue(c.editorView.visibleRect.minY <= top && top < c.editorView.visibleRect.maxY,
                      "line 250 is on screen")

        c.showGoToLine(nil)
        c.goToLine.setInput("3:2")
        try command(c.goToLine, #selector(NSResponder.insertNewline(_:)))
        XCTAssertEqual(caret(c).0, 2)
        XCTAssertEqual(caret(c).1, 1)
    }

    func testInvalidInputExplainsAndEscapeCloses() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("short.txt"))
        c.showGoToLine(nil)
        let panel = c.goToLine
        panel.setInput("abc")
        XCTAssertNil(panel.target)
        XCTAssertTrue(panel.message.stringValue.hasPrefix("Not a line number"))
        try command(panel, #selector(NSResponder.insertNewline(_:)))
        XCTAssertTrue(c.isGoToLineVisible, "Return on bad input keeps the panel open")
        panel.setInput("-1")
        XCTAssertEqual(panel.target?.line, 2, "last line")
        try command(panel, #selector(NSResponder.cancelOperation(_:)))
        XCTAssertFalse(c.isGoToLineVisible)
        XCTAssertEqual(caret(c).0, 0, "Esc does not move the caret")
    }

    func testWithoutADocumentItExplains() {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        c.showGoToLine(nil)
        XCTAssertFalse(c.isGoToLineVisible)
        XCTAssertTrue(c.statusLabel.stringValue.contains("Open a file"))
    }

    func testBackAndForwardRetraceJumpsWithinAFile() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("long.txt"))
        for line in ["100", "200"] {
            c.showGoToLine(nil)
            c.goToLine.setInput(line)
            try command(c.goToLine, #selector(NSResponder.insertNewline(_:)))
        }
        XCTAssertEqual(caret(c).0, 199)

        c.navigateBack(nil)
        XCTAssertEqual(caret(c).0, 99)
        c.navigateBack(nil)
        XCTAssertEqual(caret(c).0, 0)
        XCTAssertFalse(c.navHistory.canGoBack())
        c.navigateForward(nil)
        XCTAssertEqual(caret(c).0, 99)
        c.navigateForward(nil)
        XCTAssertEqual(caret(c).0, 199)
        XCTAssertFalse(c.navHistory.canGoForward())
    }

    func testSmallMovesAreNotRecorded() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("long.txt"))
        c.showGoToLine(nil)
        c.goToLine.setInput("5")
        try command(c.goToLine, #selector(NSResponder.insertNewline(_:)))
        XCTAssertFalse(c.navHistory.canGoBack(), "4 lines is not a jump")
    }

    func testBackReturnsToThePreviousTabAndReopensAClosedFile() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("long.txt"))
        c.showGoToLine(nil)
        c.goToLine.setInput("120")
        try command(c.goToLine, #selector(NSResponder.insertNewline(_:)))
        try c.openFile(at: file("short.txt"))
        XCTAssertEqual(c.activeTab?.title, "short.txt")

        c.navigateBack(nil)
        XCTAssertEqual(c.activeTab?.title, "long.txt")
        XCTAssertEqual(caret(c).0, 119, "where the caret was when the tab was left")
        c.navigateForward(nil)
        XCTAssertEqual(c.activeTab?.title, "short.txt")

        // Close long.txt; Back opens it again at the same line.
        c.closeTab(at: 0)
        XCTAssertEqual(c.tabs.map(\.title), ["short.txt"])
        c.navigateBack(nil)
        XCTAssertEqual(c.activeTab?.title, "long.txt")
        XCTAssertEqual(caret(c).0, 119)
    }

    func testClosedUntitledBuffersAreSkipped() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("short.txt"))
        c.newDocument(nil)
        c.closeTab(at: c.activeIndex!)
        try c.openFile(at: file("long.txt"))
        c.navigateBack(nil)
        XCTAssertEqual(c.activeTab?.title, "short.txt")
    }

    func testMenuShortcuts() throws {
        let menu = try XCTUnwrap(MainMenu.build().items.compactMap(\.submenu).first { $0.title == "Go" })
        let line = try XCTUnwrap(menu.item(withTitle: "Go to Line…"))
        XCTAssertEqual(line.keyEquivalent, "g")
        XCTAssertEqual(line.keyEquivalentModifierMask, .control)
        let back = try XCTUnwrap(menu.item(withTitle: "Back"))
        XCTAssertEqual(back.keyEquivalentModifierMask, .control)
        let forward = try XCTUnwrap(menu.item(withTitle: "Forward"))
        XCTAssertEqual(forward.keyEquivalentModifierMask, [.control, .shift])
    }
}
