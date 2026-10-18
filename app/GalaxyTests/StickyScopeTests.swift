import AppKit
import XCTest
@testable import Galaxy

@MainActor
final class StickyScopeTests: XCTestCase {
    private let source = "class A {\n    fn f() {\n        one;\n        two;\n    }\n}\n"

    private func makeView() throws -> (StickyScopeView, EditorView, URL) {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("sticky-\(UUID().uuidString).txt")
        try source.write(to: url, atomically: true, encoding: .utf8)
        let editor = EditorView(frame: NSRect(x: 0, y: 0, width: 600, height: 200))
        editor.document = try CoreBridge().openDocument(path: url.path)
        let sticky = StickyScopeView(frame: NSRect(x: 0, y: 0, width: 600, height: 0))
        sticky.editor = editor
        return (sticky, editor, url)
    }

    func testShowsRowsAndSizesToThem() throws {
        let (sticky, editor, url) = try makeView()
        defer { try? FileManager.default.removeItem(at: url) }
        XCTAssertTrue(sticky.isHidden)

        sticky.show([.init(line: 0, text: "class A {", styles: []), .init(line: 1, text: "    fn f() {", styles: [])])
        XCTAssertFalse(sticky.isHidden)
        sticky.layoutSubtreeIfNeeded()
        XCTAssertEqual(sticky.fittingSize.height, 2 * editor.metrics.lineHeight)
        XCTAssertEqual(sticky.row(at: NSPoint(x: 5, y: editor.metrics.lineHeight * 1.5)), 1)
        XCTAssertNil(sticky.row(at: NSPoint(x: 5, y: editor.metrics.lineHeight * 2.5)))

        sticky.show([])
        XCTAssertTrue(sticky.isHidden)
        sticky.isEnabled = false
        sticky.show([.init(line: 0, text: "class A {", styles: [])])
        XCTAssertTrue(sticky.isHidden, "off means never shown")
    }

    func testClickReportsTheLineAndTheRowsAboveIt() throws {
        let (sticky, editor, url) = try makeView()
        defer { try? FileManager.default.removeItem(at: url) }
        sticky.show([.init(line: 0, text: "class A {", styles: []), .init(line: 1, text: "    fn f() {", styles: [])])
        var clicked: (UInt64, Int)?
        sticky.onSelect = { clicked = ($0, $1) }
        sticky.frame = NSRect(x: 0, y: 0, width: 600, height: 2 * editor.metrics.lineHeight)
        let point = sticky.convert(NSPoint(x: 5, y: editor.metrics.lineHeight * 1.5), to: nil)
        let event = try XCTUnwrap(NSEvent.mouseEvent(
            with: .leftMouseDown, location: point, modifierFlags: [], timestamp: 0,
            windowNumber: 0, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        sticky.mouseDown(with: event)
        XCTAssertEqual(clicked?.0, 1)
        XCTAssertEqual(clicked?.1, 1)
    }

    func testDrawsTheRowTextInTheEditorFont() throws {
        let (sticky, editor, url) = try makeView()
        defer { try? FileManager.default.removeItem(at: url) }
        sticky.show([.init(line: 39, text: "class A {", styles: [])])
        sticky.frame = NSRect(x: 0, y: 0, width: 600, height: editor.metrics.lineHeight)
        let rep = try XCTUnwrap(sticky.bitmapImageRepForCachingDisplay(in: sticky.bounds))
        sticky.cacheDisplay(in: sticky.bounds, to: rep)
        // Ink in the number column (left of the text) and in the text itself.
        func hasInk(_ xs: Range<Int>) -> Bool {
            let scale = CGFloat(rep.pixelsWide) / sticky.bounds.width
            let background = rep.colorAt(x: 0, y: 2)?.usingColorSpace(.sRGB)
            for x in xs { for y in 0..<rep.pixelsHigh - 2 {
                guard let c = rep.colorAt(x: Int(CGFloat(x) * scale), y: y)?.usingColorSpace(.sRGB), let b = background else { continue }
                if abs(c.redComponent - b.redComponent) > 0.2 { return true }
            } }
            return false
        }
        XCTAssertTrue(hasInk(0..<Int(editor.gutterRight)), "line number")
        XCTAssertTrue(hasInk(Int(editor.textOriginX)..<Int(editor.textOriginX) + 60), "code")
    }

    /// A header pins the moment it reaches the top, with its copy on the
    /// original, and leaves when its block ends under the bar.
    func testHeadersPinAsTheyReachTheTopAndLeaveWithTheirBlock() throws {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("sticky-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let body = (0..<60).map { "        step\($0);" }.joined(separator: "\n")
        let file = dir.appendingPathComponent("a.txt")
        try "class A {\n    fn f() {\n\(body)\n    }\n    fn g() {\n        x;\n    }\n}\nafter;\n"
            .write(to: file, atomically: true, encoding: .utf8)
        try c.openFile(at: file)

        let editor = c.editorView
        let sticky = c.stickyScopeViewForTesting
        let scroll = c.editorScrollViewForTesting
        let height = editor.metrics.lineHeight
        // The bar covers the document from the clip's top edge, so rows are
        // measured from `bounds.origin.y`, not from the inset.
        func scrollTo(row: CGFloat) -> [UInt64] {
            scroll.contentView.scroll(to: NSPoint(x: 0, y: row * height))
            scroll.reflectScrolledClipView(scroll.contentView)
            c.updateStickyScope()
            return sticky.rows.map(\.line)
        }
        // Lines: 0 class, 1 fn f, 2...61 body, 62 closes f, 63 fn g, 64 body, 65 closes g, 66 closes class.
        XCTAssertEqual(scrollTo(row: 0), [], "nothing is pinned at the very top")
        XCTAssertEqual(scrollTo(row: 0.25), [0, 1], "headers pin as soon as they reach their slots")
        XCTAssertEqual(scrollTo(row: 1), [0, 1])
        XCTAssertEqual(scrollTo(row: 30), [0, 1])
        XCTAssertEqual(scrollTo(row: 59), [0, 1])
        XCTAssertEqual(scrollTo(row: 60), [0], "f leaves when its closing line is under the bar")
        XCTAssertEqual(scrollTo(row: 61.9), [0], "g not yet: its row is still below its slot")
        XCTAssertEqual(scrollTo(row: 62), [0, 63], "g pins exactly when its row reaches the slot")
        XCTAssertEqual(scrollTo(row: 64), [0], "g leaves once its whole block is under the bar")
        XCTAssertEqual(scrollTo(row: 66), [], "past the class nothing is pinned")
    }

    /// Allman style: the function pins while its `{` line is under the bar,
    /// not only after the body has scrolled in (which clipped the header).
    func testAllmanHeaderPinsBeforeItsBodyReachesTheBar() throws {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("sticky-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let body = (0..<40).map { "        step\($0);" }.joined(separator: "\n")
        let file = dir.appendingPathComponent("a.txt")
        // 0 class, 1 `{`, 2..4 filler, 5 function, 6 `{`, 7... body.
        try "class A\n{\n    a;\n    b;\n    c;\n    public function f(int $x)\n    {\n\(body)\n    }\n}\n"
            .write(to: file, atomically: true, encoding: .utf8)
        try c.openFile(at: file)

        let sticky = c.stickyScopeViewForTesting
        let scroll = c.editorScrollViewForTesting
        let height = c.editorView.metrics.lineHeight
        func scrollTo(row: CGFloat) -> [UInt64] {
            scroll.contentView.scroll(to: NSPoint(x: 0, y: row * height))
            scroll.reflectScrolledClipView(scroll.contentView)
            c.updateStickyScope()
            return sticky.rows.map(\.line)
        }
        XCTAssertEqual(scrollTo(row: 1), [0], "the class pins once its brace is under the bar")
        // The function takes the bar's second row, one below the top edge.
        XCTAssertEqual(scrollTo(row: 3.9), [0], "the function is not at its slot yet")
        XCTAssertEqual(scrollTo(row: 4), [0, 5], "the function pins as its line reaches the slot")
        XCTAssertEqual(scrollTo(row: 6), [0, 5])
        XCTAssertEqual(scrollTo(row: 20), [0, 5])
    }
}
