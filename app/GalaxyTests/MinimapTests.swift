import XCTest
@testable import Galaxy

@MainActor
final class MinimapTests: XCTestCase {
    func testGeometryOfASmallDocumentFitsWithoutScrolling() {
        // 10 lines (20pt) in 200pt: nothing to scroll, slider covers what is visible.
        let g = MinimapView.geometry(lineCount: 10, height: 200, topLine: 0, bottomLine: 10)
        XCTAssertEqual(g.offset, 0)
        XCTAssertEqual(g.sliderTop, 0)
        XCTAssertEqual(g.sliderHeight, 20)
    }

    func testGeometryOfALongDocumentScrollsTheOverview() {
        // 1000 lines = 2000pt in a 400pt view; the editor shows 50 lines.
        let top = MinimapView.geometry(lineCount: 1000, height: 400, topLine: 0, bottomLine: 50)
        XCTAssertEqual(top.offset, 0)
        XCTAssertEqual(top.sliderTop, 0)
        XCTAssertEqual(top.sliderHeight, 100)

        let end = MinimapView.geometry(lineCount: 1000, height: 400, topLine: 950, bottomLine: 1000)
        XCTAssertEqual(end.offset, 1600, "overview scrolled to its end")
        XCTAssertEqual(end.sliderTop + end.sliderHeight, 400, accuracy: 0.001, "slider ends at the bottom")

        let middle = MinimapView.geometry(lineCount: 1000, height: 400, topLine: 475, bottomLine: 525)
        XCTAssertGreaterThan(middle.offset, 0)
        XCTAssertLessThan(middle.offset, 1600)
        XCTAssertGreaterThanOrEqual(middle.sliderTop, 0)
        XCTAssertLessThanOrEqual(middle.sliderTop + middle.sliderHeight, 400)
    }

    func testSliderHasAMinimumHeight() {
        let g = MinimapView.geometry(lineCount: 100_000, height: 400, topLine: 0, bottomLine: 1)
        XCTAssertGreaterThanOrEqual(g.sliderHeight, 8)
    }

    func testLineUnderAPointAccountsForTheOffset() {
        XCTAssertEqual(MinimapView.line(atY: 40, offset: 0, lineCount: 100), 20)
        XCTAssertEqual(MinimapView.line(atY: 40, offset: 100, lineCount: 100), 70)
        XCTAssertEqual(MinimapView.line(atY: -50, offset: 0, lineCount: 100), 0)
        XCTAssertEqual(MinimapView.line(atY: 9_999, offset: 0, lineCount: 100), 100)
    }

    func testRunsSkipBlanksAndCapTheWidth() {
        func pairs(_ text: String) -> [[Int]] { MinimapView.runs(in: text).map { [$0.start, $0.end] } }
        XCTAssertEqual(pairs(""), [])
        XCTAssertEqual(pairs("   "), [])
        XCTAssertEqual(pairs("ab cd"), [[0, 2], [3, 5]])
        XCTAssertEqual(pairs("\tx"), [[4, 5]], "a tab is four columns")
        let long = String(repeating: "x", count: 500)
        XCTAssertEqual(pairs(long), [[0, MinimapView.maxColumns]])
    }

    private func makeWindow(lines: Int) throws -> MainWindowController {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        c.newDocument(nil)
        let text = (0..<lines).map { "line \($0) with some text" }.joined(separator: "\n")
        c.editorView.insertText(text, replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        return c
    }

    func testMinimapNarrowsTheEditorAndTogglesOff() throws {
        let defaults = UserDefaults.standard
        let saved = defaults.object(forKey: "Minimap")
        defer { defaults.set(saved, forKey: "Minimap") }
        defaults.set(true, forKey: "Minimap")

        let c = try makeWindow(lines: 50)
        defer { c.window?.orderOut(nil) }
        let item = NSMenuItem(title: "", action: #selector(MainWindowController.toggleMinimap(_:)), keyEquivalent: "")
        _ = c.validateMenuItem(item)
        XCTAssertEqual(item.state, .on)
        XCTAssertEqual(c.editorView.rightInset, MinimapView.width + OverviewRulerView.width)
        let scroll = try XCTUnwrap(c.editorView.enclosingScrollView)
        XCTAssertEqual(c.editorView.frame.width, scroll.contentSize.width - MinimapView.width - OverviewRulerView.width, accuracy: 1)

        c.toggleMinimap(nil)
        _ = c.validateMenuItem(item)
        XCTAssertEqual(item.state, .off)
        XCTAssertEqual(c.editorView.rightInset, OverviewRulerView.width, "the ruler stays without the minimap")
        XCTAssertEqual(c.editorView.frame.width, scroll.contentSize.width - OverviewRulerView.width, accuracy: 1)
        XCTAssertFalse(defaults.bool(forKey: "Minimap"))
    }

    func testRulerSitsAtTheRightEdgeAndTheMinimapLeftOfIt() throws {
        let defaults = UserDefaults.standard
        let saved = defaults.object(forKey: "Minimap")
        defer { defaults.set(saved, forKey: "Minimap") }
        defaults.set(true, forKey: "Minimap")

        let c = try makeWindow(lines: 50)
        defer { c.window?.orderOut(nil) }
        let scroll = c.scrollView
        let ruler = scroll.ruler

        XCTAssertFalse(ruler.isHidden)
        XCTAssertEqual(ruler.frame.maxX, scroll.bounds.width, accuracy: 0.5)
        XCTAssertEqual(ruler.frame.width, OverviewRulerView.width)
        XCTAssertEqual(c.minimap.frame.maxX, ruler.frame.minX, accuracy: 0.5, "no overlap between the two")

        c.toggleMinimap(nil)
        XCTAssertTrue(c.minimap.isHidden)
        XCTAssertFalse(ruler.isHidden, "Git and problem ticks do not depend on the minimap")
        XCTAssertEqual(ruler.frame.maxX, scroll.bounds.width, accuracy: 0.5)
    }

    func testRulerTicksStayInsideTheStrip() {
        XCTAssertEqual(OverviewRulerView.tickY(line: 0, lineCount: 100, height: 200), 0)
        XCTAssertEqual(OverviewRulerView.tickY(line: 99, lineCount: 100, height: 200), 198, "last tick ends at the bottom")
        XCTAssertEqual(OverviewRulerView.tickY(line: 5_000, lineCount: 100, height: 200), 198)
        XCTAssertEqual(OverviewRulerView.tickY(line: 0, lineCount: 1, height: 200), 0)
    }

    func testClickingTheMinimapScrollsTheEditor() throws {
        let defaults = UserDefaults.standard
        let saved = defaults.object(forKey: "Minimap")
        defer { defaults.set(saved, forKey: "Minimap") }
        defaults.set(true, forKey: "Minimap")

        let c = try makeWindow(lines: 2000)
        defer { c.window?.orderOut(nil) }
        let scroll = try XCTUnwrap(c.editorView.enclosingScrollView)
        let minimap = try XCTUnwrap(scroll.subviews.compactMap { $0 as? MinimapView }.first)
        scroll.contentView.scroll(to: .zero)
        let before = scroll.contentView.bounds.origin.y

        minimap.scroll(toY: minimap.bounds.height * 0.9)
        let after = scroll.contentView.bounds.origin.y
        XCTAssertGreaterThan(after, before + 500, "a click far down moves the editor far down")
        XCTAssertLessThanOrEqual(after, c.editorView.frame.height)

        // Draws without crashing at the new position.
        let rep = try XCTUnwrap(minimap.bitmapImageRepForCachingDisplay(in: minimap.bounds))
        minimap.cacheDisplay(in: minimap.bounds, to: rep)
    }

    func testScrollDecorationUpdatesAreCoalesced() throws {
        let c = try makeWindow(lines: 100)
        defer { c.window?.orderOut(nil) }
        c.scheduleScrollDecorationUpdate()
        c.scheduleScrollDecorationUpdate()
        XCTAssertTrue(c.scrollDecorationUpdatePending)
        RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        XCTAssertFalse(c.scrollDecorationUpdatePending)
    }
}
