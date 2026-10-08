import XCTest
@testable import RustCode

/// Exercises the real window wiring (scroll view + editor view), which the
/// view-level tests bypass by giving the view a hand-picked frame.
@MainActor
final class MainWindowTests: XCTestCase {
    private func settle() { RunLoop.current.run(until: Date().addingTimeInterval(0.4)) }

    func testNewDocumentIsVisibleFocusedAndTypable() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }

        controller.newDocument(nil)
        settle()

        let view = controller.editorView
        XCTAssertGreaterThan(view.frame.width, 100, "editor must fill the scroll view")
        XCTAssertFalse(view.lastDrawnLines.isEmpty, "the empty buffer still draws its first line")
        XCTAssertTrue(controller.window?.firstResponder === view, "typing must work immediately")
        XCTAssertEqual(controller.window?.title, "Untitled")

        view.insertText("hello", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(view.document?.lines(first: 0, count: 1), ["hello"])
    }

    func testOpenedDocumentIsDrawnInTheRealWindow() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("rcode-\(UUID().uuidString).txt")
        try (0..<100).map { "line \($0)" }.joined(separator: "\n").write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let bridge = CoreBridge()
        let controller = MainWindowController(bridge: bridge)
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }

        controller.editorView.document = try bridge.openDocument(path: url.path)
        settle()

        let view = controller.editorView
        XCTAssertGreaterThan(view.frame.width, 100, "editor must fill the scroll view width")
        XCTAssertEqual(view.lastDrawnLines.lowerBound, 0)
        XCTAssertGreaterThan(view.lastDrawnLines.count, 10)
    }
}
