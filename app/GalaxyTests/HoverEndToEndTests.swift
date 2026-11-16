import XCTest
@testable import Galaxy

/// Opt-in: run with `TEST_RUNNER_GALAXY_LARAVEL_PATH=/path/to/laravel-app`
/// (served by the built-in PHP engine). Drives a real mouse hover through the window
/// controller, LSP poll timer and popover placement.
@MainActor
final class HoverEndToEndTests: XCTestCase {
    func testHoveringAClassShowsTheCard() throws {
        guard let path = ProcessInfo.processInfo.environment["GALAXY_LARAVEL_PATH"] else {
            throw XCTSkip("GALAXY_LARAVEL_PATH not set")
        }
        let root = URL(fileURLWithPath: path)
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        try c.openWorkspace(at: root)
        try c.openFile(at: root.appendingPathComponent("bootstrap/providers.php"))
        let v = c.editorView
        v.flushPendingUpdates()

        let line = 2 // `use App\Providers\AppServiceProvider;`
        let text = try XCTUnwrap(v.document?.displayLine(line: UInt64(line)))
        let column = (text as NSString).range(of: "AppServiceProvider").location + 3
        let point = NSPoint(x: v.textOriginX + v.xOffset(in: text, column: UInt64(column)) + 0.5,
                            y: CGFloat(line) * v.metrics.lineHeight + v.metrics.lineHeight / 2)
        let event = try XCTUnwrap(NSEvent.mouseEvent(
            with: .mouseMoved, location: v.convert(point, to: nil), modifierFlags: .command, timestamp: 0,
            windowNumber: v.window?.windowNumber ?? 0, context: nil, eventNumber: 0, clickCount: 0, pressure: 0))

        // Servers start and index asynchronously; re-hover until a card shows.
        let deadline = Date().addingTimeInterval(60)
        while c.hoverPopover.isHidden, Date() < deadline {
            v.mouseMoved(with: event)
            RunLoop.main.run(until: Date().addingTimeInterval(0.5))
        }
        let popover = c.hoverPopover
        if let doc = v.document {
            let info = c.bridge.hover(document: doc)
            print("HOVER info=\(String(describing: info?.contents.prefix(80))) start=\(String(describing: info?.startLine))-\(String(describing: info?.endLine))")
        }
        XCTAssertFalse(popover.isHidden, "hover card never became visible")
        XCTAssertFalse(popover.renderedText.isEmpty)
        XCTAssertGreaterThan(popover.frame.height, 30)
        XCTAssertTrue(popover.superview?.bounds.intersects(popover.frame) ?? false,
                      "card is outside its parent: \(popover.frame)")
        print("HOVER frame=\(popover.frame) parent=\(popover.superview?.bounds ?? .zero) text=\(popover.renderedText.prefix(80))")
    }
}
