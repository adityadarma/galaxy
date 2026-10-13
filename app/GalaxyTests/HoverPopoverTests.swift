import XCTest
@testable import Galaxy

@MainActor
final class HoverPopoverTests: XCTestCase {
    private let bounds = NSRect(x: 0, y: 0, width: 1000, height: 800)
    private let size = NSSize(width: 400, height: 200)

    func testPrefersAboveTheWordWhenThereIsRoom() {
        let anchor = NSRect(x: 100, y: 400, width: 1, height: 18)
        let frame = HoverPopover.placement(size: size, anchor: anchor, in: bounds)
        XCTAssertEqual(frame.minY, anchor.maxY + 4)
        XCTAssertEqual(frame.height, 200)
        XCTAssertEqual(frame.minX, 100)
    }

    func testFallsBackBelowNearTheTopEdge() {
        let anchor = NSRect(x: 100, y: 700, width: 1, height: 18)
        let frame = HoverPopover.placement(size: size, anchor: anchor, in: bounds)
        XCTAssertEqual(frame.maxY, anchor.minY - 4)
        XCTAssertGreaterThanOrEqual(frame.minY, bounds.minY)
    }

    func testShrinksIntoTheLargerSideWhenNeitherFits() {
        let small = NSRect(x: 0, y: 0, width: 1000, height: 300)
        let anchor = NSRect(x: 100, y: 100, width: 1, height: 18)
        let frame = HoverPopover.placement(size: size, anchor: anchor, in: small)
        // Above: 300 - 122 = 178, below: 96. Above wins and the card shrinks.
        XCTAssertEqual(frame.minY, anchor.maxY + 4)
        XCTAssertLessThanOrEqual(frame.maxY, small.maxY)
    }

    func testClampsHorizontallyInsideTheEditor() {
        let anchor = NSRect(x: 900, y: 300, width: 1, height: 18)
        let frame = HoverPopover.placement(size: size, anchor: anchor, in: bounds)
        XCTAssertEqual(frame.maxX, bounds.maxX)
    }

    func testShownCardHasVisibleTextAndBackground() {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 700),
                              styleMask: [.titled], backing: .buffered, defer: false)
        let parent = NSView(frame: window.contentLayoutRect)
        window.contentView = parent
        let popover = HoverPopover()
        parent.addSubview(popover)
        popover.show("```php\n<?php\nabstract class ServiceProvider { }\n```\n\nAll of the container bindings.")
        let size = popover.fittingSize()
        XCTAssertGreaterThan(size.height, 30)
        popover.frame = NSRect(origin: NSPoint(x: 50, y: 50), size: size)
        parent.layoutSubtreeIfNeeded()
        popover.displayIfNeeded()
        let text = try! XCTUnwrap(popover.debugTextView)
        XCTAssertGreaterThan(text.frame.height, 30, "text view must not collapse to zero height")
        XCTAssertGreaterThan(text.frame.width, 100)
        XCTAssertNotNil(popover.layer?.backgroundColor, "card needs an opaque background")
    }

    func testRendersMarkdownWithoutFencesOrPhpTag() {
        let markdown = "__Illuminate\\Support\\ServiceProvider__\n\n```php\n<?php\nabstract class ServiceProvider { }\n```\n\n_@property_ `array` $bindings"
        let text = HoverPopover.render(markdown).string
        XCTAssertFalse(text.contains("```"))
        XCTAssertFalse(text.contains("<?php"))
        XCTAssertFalse(text.contains("__"))
        XCTAssertTrue(text.contains("Illuminate\\Support\\ServiceProvider"))
        XCTAssertTrue(text.contains("abstract class ServiceProvider { }"))
        XCTAssertTrue(text.contains("@property array $bindings"))
    }
}
