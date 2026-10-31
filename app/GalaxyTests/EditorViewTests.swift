import XCTest
@testable import Galaxy

final class TwoGenerationCacheTests: XCTestCase {
    func testKeepsRecentEntriesAndBoundsMemory() {
        var cache = TwoGenerationCache<Int, Int>(capacity: 4)
        for i in 0..<100 { cache.insert(i, for: i) }
        XCTAssertLessThanOrEqual(cache.count, 8)
        XCTAssertEqual(cache.value(for: 99), 99)
        XCTAssertNil(cache.value(for: 0))
    }

    func testHitPromotesEntryFromPreviousGeneration() {
        var cache = TwoGenerationCache<Int, Int>(capacity: 2)
        cache.insert(1, for: 1)
        cache.insert(2, for: 2)
        cache.insert(3, for: 3)              // 1 and 2 move to previous
        XCTAssertEqual(cache.value(for: 1), 1) // promoted back
        cache.insert(4, for: 4)
        cache.insert(5, for: 5)
        XCTAssertEqual(cache.value(for: 1), 1)
    }
}

@MainActor
final class EditorViewTests: XCTestCase {
    func testWhitespaceColumnsRenderAllAndBoundaryModes() {
        XCTAssertEqual(EditorView.whitespaceColumns(in: "a  b\t", mode: .all), [1, 2, 4])
        XCTAssertEqual(EditorView.whitespaceColumns(in: "a b", mode: .boundary), [])
        XCTAssertEqual(EditorView.whitespaceColumns(in: "a  b", mode: .boundary), [1, 2])
        XCTAssertEqual(EditorView.whitespaceColumns(in: "a b", mode: .none), [])
    }

    private func makeDocument(lines: Int) throws -> (DocumentHandle, URL) {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-\(UUID().uuidString).txt")
        let text = (0..<lines).map { "line \($0)" }.joined(separator: "\n")
        try text.write(to: url, atomically: true, encoding: .utf8)
        return (try CoreBridge().openDocument(path: url.path), url)
    }

    /// A line wider than the window makes the document wider than the
    /// viewport, so it can scroll sideways. The line numbers keep their place.
    func testLongLinesScrollSidewaysAndKeepTheGutterAtTheEdge() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-\(UUID().uuidString).txt")
        let long = String(repeating: "abcdefghij", count: 150)
        try "short\n\(long)\nend".write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }
        let doc = try CoreBridge().openDocument(path: url.path)

        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 500, height: 300),
                              styleMask: [.titled], backing: .buffered, defer: false)
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 500, height: 300))
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 500, height: 300))
        view.autoresizingMask = [.width]
        scroll.documentView = view
        window.contentView = scroll
        view.document = doc
        window.orderFrontRegardless()
        defer { window.orderOut(nil) }
        func settle() { RunLoop.current.run(until: Date().addingTimeInterval(0.3)) }

        let visibleWidth = scroll.contentSize.width
        // The width follows the first draw by one main-queue hop; wait for it
        // instead of guessing how long a busy test run needs.
        let deadline = Date().addingTimeInterval(5)
        while view.frame.width <= visibleWidth, Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
        XCTAssertGreaterThan(view.contentTextWidth, visibleWidth, "the long line was measured")
        XCTAssertGreaterThan(view.frame.width, visibleWidth, "the document is wider than the window")
        XCTAssertEqual(view.gutterShift, 0)

        // Scrolled to the right: the gutter is drawn at the left edge of the window.
        scroll.contentView.scroll(to: NSPoint(x: 400, y: 0))
        scroll.reflectScrolledClipView(scroll.contentView)
        settle()
        XCTAssertEqual(view.gutterShift, 400)
        // A click on the line numbers is still a click on the gutter.
        XCTAssertLessThan(view.textPosition(at: NSPoint(x: 400 + view.gutterWidth + 1, y: 1)).colUtf16, 100)

        // The caret at the end of the long line brings that end into view.
        doc.setCaret(pos: TextPos(line: 1, colUtf16: UInt64(long.utf16.count)))
        view.reloadDocument()
        scroll.contentView.scroll(to: NSPoint(x: 0, y: 0))
        scroll.reflectScrolledClipView(scroll.contentView)
        view.scrollCaretToVisible(primary: doc.primaryCursor(), lineText: long)
        settle()
        let caret = view.rect(forLine: 1, column: UInt64(long.utf16.count))
        XCTAssertGreaterThan(view.gutterShift, 0, "scrolled right to follow the caret")
        XCTAssertLessThanOrEqual(caret.maxX, scroll.contentView.bounds.maxX + 1)
        XCTAssertGreaterThanOrEqual(caret.minX, scroll.contentView.bounds.minX + view.gutterWidth)

        // Soft wrap fits the text to the window: nothing to scroll sideways.
        view.wrapEnabled = true
        settle()
        XCTAssertEqual(view.gutterShift, 0)
        XCTAssertEqual(view.frame.width, visibleWidth, accuracy: 1)
    }

    func testViewHeightCoversWholeDocument() throws {
        let (doc, url) = try makeDocument(lines: 50_000)
        defer { try? FileManager.default.removeItem(at: url) }
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 600, height: 400))
        view.document = doc
        XCTAssertEqual(view.lineCount, 50_000)
        XCTAssertEqual(view.frame.height, 50_000 * view.metrics.lineHeight)
    }

    func testAccessibilityExposesTextSelectionAndCaretLine() throws {
        let (doc, url) = try makeDocument(lines: 3)
        defer { try? FileManager.default.removeItem(at: url) }
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 600, height: 400))
        view.document = doc

        // Select "in" on the second line. The core stores positions in
        // UTF-16 units, the same coordinate system used by AppKit's
        // accessibility text APIs.
        let start = doc.positionAt(offset: 8)
        let end = doc.positionAt(offset: 10)
        doc.setSelections(selections: [TextSelection(anchor: start, head: end)], primary: 0)

        XCTAssertEqual(view.accessibilityRole(), .textArea)
        XCTAssertEqual(view.accessibilityValue() as? String, "line 0\nline 1\nline 2")
        XCTAssertEqual(view.accessibilityNumberOfCharacters(), 20)
        XCTAssertEqual(view.accessibilitySelectedTextRange(), NSRange(location: 8, length: 2))
        XCTAssertEqual(view.accessibilitySelectedText(), "in")
        XCTAssertEqual(view.accessibilityInsertionPointLineNumber(), 1)
    }

    func testLineRangeIsClampedToVisibleRect() throws {
        let (doc, url) = try makeDocument(lines: 100)
        defer { try? FileManager.default.removeItem(at: url) }
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 600, height: 400))
        view.document = doc
        let h = view.metrics.lineHeight
        XCTAssertEqual(view.lineRange(for: NSRect(x: 0, y: 10 * h, width: 600, height: 5 * h)), 10..<15)
        XCTAssertEqual(view.lineRange(for: NSRect(x: 0, y: 10 * h + 1, width: 600, height: 1)), 10..<11)
        XCTAssertEqual(view.lineRange(for: NSRect(x: 0, y: 99 * h, width: 600, height: 50 * h)), 99..<100)
        XCTAssertTrue(view.lineRange(for: NSRect(x: 0, y: 500 * h, width: 600, height: h)).isEmpty)
    }

    /// Regression: AppKit can hand `draw(_:)` an enormous rect, which used to
    /// trap on the Double -> Int conversion.
    func testLineRangeSurvivesHugeAndInfiniteRects() throws {
        let (doc, url) = try makeDocument(lines: 100)
        defer { try? FileManager.default.removeItem(at: url) }
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 600, height: 400))
        view.document = doc
        XCTAssertEqual(view.lineRange(for: .infinite), 0..<100)
        XCTAssertEqual(view.lineRange(for: NSRect(x: 0, y: -1e300, width: 1e300, height: 2e300)), 0..<100)
        XCTAssertTrue(view.lineRange(for: .null).isEmpty)
        XCTAssertTrue(view.lineRange(for: NSRect(x: 0, y: 0, width: 600, height: 0)).isEmpty)
    }

    /// Renders the view into an offscreen bitmap, as a real redraw would.
    private func render(_ view: EditorView, rect: NSRect) throws {
        let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: rect))
        view.cacheDisplay(in: rect, to: rep)
    }

    func testDrawCachesLayoutsAcrossRedraws() throws {
        let (doc, url) = try makeDocument(lines: 200)
        defer { try? FileManager.default.removeItem(at: url) }
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 600, height: 400))
        view.document = doc
        let rect = NSRect(x: 0, y: 0, width: 600, height: 400)

        try render(view, rect: rect)
        let first = view.layoutsCreated
        XCTAssertGreaterThan(first, 0)
        XCTAssertLessThanOrEqual(first, 2 * 200, "at most one layout per line of text plus one per line number")

        try render(view, rect: rect)
        XCTAssertEqual(view.layoutsCreated, first, "second draw must reuse cached CTLines")
    }

    /// In a real scroll view only the visible lines are drawn, even for a
    /// 50k-line document, and scrolling far away draws lines near the new
    /// offset only.
    func testOnlyVisibleLinesAreDrawnInScrollView() throws {
        let (doc, url) = try makeDocument(lines: 50_000)
        defer { try? FileManager.default.removeItem(at: url) }

        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 600, height: 400),
                              styleMask: [.titled], backing: .buffered, defer: false)
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 600, height: 400))
        scroll.hasVerticalScroller = true
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 600, height: 400))
        view.autoresizingMask = [.width]
        scroll.documentView = view
        window.contentView = scroll
        view.document = doc
        window.orderFrontRegardless()
        defer { window.orderOut(nil) }

        func settle() { RunLoop.current.run(until: Date().addingTimeInterval(0.3)) }
        let visibleLines = Int(ceil(400 / view.metrics.lineHeight)) + 1

        settle()
        XCTAssertLessThanOrEqual(view.lastDrawnLines.count, visibleLines)
        XCTAssertEqual(view.lastDrawnLines.lowerBound, 0)

        let target = 25_000 * view.metrics.lineHeight
        scroll.contentView.scroll(to: NSPoint(x: 0, y: target))
        scroll.reflectScrolledClipView(scroll.contentView)
        settle()
        XCTAssertEqual(view.lastDrawnLines.lowerBound, 25_000)
        XCTAssertLessThanOrEqual(view.lastDrawnLines.count, visibleLines)
        XCTAssertLessThan(view.layoutsCreated, 4 * visibleLines, "layouts must stay proportional to what was shown")
    }

    // MARK: Pixel-level drawing checks (M1-09)

    private func makeView(text: String) throws -> (EditorView, DocumentHandle, URL) {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-\(UUID().uuidString).txt")
        try text.write(to: url, atomically: true, encoding: .utf8)
        let doc = try CoreBridge().openDocument(path: url.path)
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 600, height: 200))
        view.document = doc
        return (view, doc, url)
    }

    private func bitmap(_ view: EditorView) throws -> NSBitmapImageRep {
        let rect = view.bounds
        let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: rect))
        view.cacheDisplay(in: rect, to: rep)
        return rep
    }

    /// Luminance 0...1 of the pixel at view coordinates (x, y).
    private func luminance(_ rep: NSBitmapImageRep, _ view: EditorView, x: CGFloat, y: CGFloat) throws -> CGFloat {
        let scale = CGFloat(rep.pixelsWide) / view.bounds.width
        let color = try XCTUnwrap(
            rep.colorAt(x: Int(x * scale), y: Int(y * scale))?.usingColorSpace(.sRGB))
        return 0.2126 * color.redComponent + 0.7152 * color.greenComponent + 0.0722 * color.blueComponent
    }

    /// A hint widens the character before its argument, so the argument and
    /// everything after it move right by the label's room, and clicks map
    /// through the same layout.
    func testInlineHintsReserveRoomBeforeTheirArgument() throws {
        let text = "add(1, 2)"
        let (view, _, url) = try makeView(text: text)
        defer { try? FileManager.default.removeItem(at: url) }
        let before = view.xOffset(in: text, column: 4)
        let afterComma = view.xOffset(in: text, column: 7)
        view.setInlineHints([text: [EditorView.InlineHint(column: 4, label: "a:"),
                                    EditorView.InlineHint(column: 7, label: "b:")]])
        let room = view.hintAdvance("a:")
        XCTAssertGreaterThan(room, 0)
        XCTAssertEqual(view.xOffset(in: text, column: 3), view.xOffset(in: "add", column: 3), accuracy: 0.01)
        XCTAssertEqual(view.xOffset(in: text, column: 4), before + room, accuracy: 0.5)
        XCTAssertEqual(view.xOffset(in: text, column: 7), afterComma + room + view.hintAdvance("b:"), accuracy: 0.5)
        // Other lines are unaffected, and clearing restores the layout.
        XCTAssertEqual(view.xOffset(in: "add(9, 2)", column: 4), before, accuracy: 0.01)
        view.setInlineHints([:])
        XCTAssertEqual(view.xOffset(in: text, column: 4), before, accuracy: 0.01)
    }

    func testGutterGrowsWithDigitCount() throws {
        let (small, _, u1) = try makeView(text: "a\nb")
        let (large, _, u2) = try makeView(text: (0..<12_000).map { "l\($0)" }.joined(separator: "\n"))
        defer { [u1, u2].forEach { try? FileManager.default.removeItem(at: $0) } }
        // Minimum of three digits, then one digit width per extra digit.
        XCTAssertEqual(small.gutterWidth, ceil(3 * small.metrics.digitWidth) + EditorView.gutterPadding + EditorView.gutterTrailingPadding)
        XCTAssertGreaterThan(large.gutterWidth, small.gutterWidth)
        XCTAssertGreaterThanOrEqual(large.textOriginX, large.gutterWidth)
    }

    func testFoldingRemovesRowsAndHitTestingSkipsThem() throws {
        let (view, doc, url) = try makeView(text: "fn a() {\n    one;\n    two;\n}\nfn b() {\n    three;\n}")
        defer { try? FileManager.default.removeItem(at: url) }
        XCTAssertEqual(view.rowCount, 7)

        view.toggleFold(line: 0)
        XCTAssertTrue(view.hasFolds)
        XCTAssertEqual(view.rowCount, 5)
        XCTAssertEqual(view.rowTop(of: 3), view.metrics.lineHeight, "the closing brace follows the header")
        XCTAssertEqual(view.line(atY: view.metrics.lineHeight * 1.5).line, 3)

        view.toggleFold(line: 0)
        XCTAssertFalse(view.hasFolds)
        XCTAssertEqual(view.rowCount, 7)
        XCTAssertEqual(view.rowTop(of: 3), 3 * view.metrics.lineHeight)
        XCTAssertEqual(doc.foldState(line: 0), .expanded)
    }

    func testClickingTheChevronFoldsAndTheDotsUnfold() throws {
        let (view, doc, url) = try makeView(text: "fn a() {\n    one;\n    two;\n}\nfn b() {\n    three;\n}")
        defer { try? FileManager.default.removeItem(at: url) }
        var breakpoints: [UInt64] = []
        view.onToggleBreakpoint = { breakpoints.append($0) }
        func click(_ point: NSPoint) throws {
            let event = try XCTUnwrap(NSEvent.mouseEvent(
                with: .leftMouseDown, location: view.convert(point, to: nil), modifierFlags: [], timestamp: 0,
                windowNumber: 0, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
            view.mouseDown(with: event)
        }
        let y = view.metrics.lineHeight / 2

        try click(NSPoint(x: view.foldZoneMinX + 3, y: y))
        XCTAssertEqual(doc.foldState(line: 0), .folded)
        XCTAssertTrue(breakpoints.isEmpty, "the chevron column does not set a breakpoint")

        try click(NSPoint(x: 4, y: y))
        XCTAssertEqual(breakpoints, [0], "the line numbers still do")
        XCTAssertEqual(doc.foldState(line: 0), .folded)

        view.display()
        let pill = try XCTUnwrap(view.foldPillRects[0], "a folded line draws its dots")
        try click(NSPoint(x: pill.midX, y: pill.midY))
        XCTAssertEqual(doc.foldState(line: 0), .expanded)

        // Leaf lines have no chevron, so the same spot is a breakpoint click.
        try click(NSPoint(x: view.foldZoneMinX + 3, y: view.metrics.lineHeight * 1.5))
        XCTAssertEqual(breakpoints, [0, 1])
    }

    func testChevronToolTipFollowsTheFoldState() throws {
        let (view, doc, url) = try makeView(text: "fn a() {\n    one;\n}")
        defer { try? FileManager.default.removeItem(at: url) }
        let point = NSPoint(x: view.foldZoneMinX + 3, y: view.metrics.lineHeight / 2)
        XCTAssertEqual(view.view(view, stringForToolTip: 0, point: point, userData: nil), "Click to collapse the range.")
        _ = doc.toggleFold(line: 0)
        view.reloadDocument()
        XCTAssertEqual(view.view(view, stringForToolTip: 0, point: point, userData: nil), "Click to expand the range.")
        let leaf = NSPoint(x: point.x, y: view.metrics.lineHeight * 1.5)
        XCTAssertEqual(view.view(view, stringForToolTip: 0, point: leaf, userData: nil), "")
    }

    func testDiffPanesKeepOneRowPerLine() throws {
        let (view, doc, url) = try makeView(text: "fn a() {\n    one;\n}")
        defer { try? FileManager.default.removeItem(at: url) }
        view.toggleFold(line: 0)
        XCTAssertEqual(view.rowCount, 2)
        view.wrapSuspended = true
        XCTAssertEqual(view.rowCount, 3)
        XCTAssertFalse(doc.hasFolds())
        view.toggleFold(line: 0)
        XCTAssertFalse(doc.hasFolds(), "a suspended view does not fold")
    }

    func testDividerShowsOnlyBesideIndentedText() throws {
        // 0 "x {", 1 "    a", 2 "" (inside the block), 3 "    b", 4 "}", 5 "" (outside), 6 "c"
        let (view, _, url) = try makeView(text: "x {\n    a\n\n    b\n}\n\nc")
        defer { try? FileManager.default.removeItem(at: url) }
        view.forcedCaretVisible = false
        view.appearance = NSAppearance(named: .aqua)
        let rep = try bitmap(view)
        let h = view.metrics.lineHeight
        let dividerX = view.gutterWidth - 0.5
        let backgroundX = view.textOriginX + 400
        func hasDivider(line: Int) throws -> Bool {
            let y = (CGFloat(line) + 0.5) * h
            return abs(try luminance(rep, view, x: dividerX, y: y) - luminance(rep, view, x: backgroundX, y: y)) > 0.01
        }
        XCTAssertFalse(try hasDivider(line: 0), "text at column 0 touches the divider")
        XCTAssertTrue(try hasDivider(line: 1), "indented text")
        XCTAssertTrue(try hasDivider(line: 2), "blank line inside the block keeps it")
        XCTAssertTrue(try hasDivider(line: 3))
        XCTAssertFalse(try hasDivider(line: 4))
        XCTAssertFalse(try hasDivider(line: 5), "blank line outside any block")
        XCTAssertFalse(try hasDivider(line: 6))
    }

    func testCaretIsDrawnOnlyWhenVisible() throws {
        let (view, _, url) = try makeView(text: "      \nsecond")
        defer { try? FileManager.default.removeItem(at: url) }
        let x = view.textOriginX + 1          // inside the 2 pt caret at column 0
        let y = view.metrics.lineHeight / 2   // middle of line 0

        view.forcedCaretVisible = true
        let on = try luminance(try bitmap(view), view, x: x, y: y)
        view.forcedCaretVisible = false
        let off = try luminance(try bitmap(view), view, x: x, y: y)
        XCTAssertGreaterThan(abs(on - off), 0.3, "caret pixel must differ from the line background")
    }

    func testSelectionIsPaintedBehindText() throws {
        // Spaces draw no glyphs, so the pixel colour is pure background.
        let (view, doc, url) = try makeView(text: "      \n")
        defer { try? FileManager.default.removeItem(at: url) }
        view.forcedCaretVisible = false
        let a = TextPos(line: 0, colUtf16: 1), b = TextPos(line: 0, colUtf16: 3)
        doc.setSelections(selections: [TextSelection(anchor: a, head: b)], primary: 0)
        view.reloadDocument()

        let sw = view.metrics.spaceWidth
        let y = view.metrics.lineHeight / 2
        let rep = try bitmap(view)
        let selected = try luminance(rep, view, x: view.textOriginX + 2 * sw, y: y)
        let plain = try luminance(rep, view, x: view.textOriginX + 5 * sw, y: y)
        XCTAssertGreaterThan(abs(selected - plain), 0.05, "selected columns must be tinted")
        // Edges: just before the start and just after the end stay plain.
        let before = try luminance(rep, view, x: view.textOriginX + 0.1 * sw, y: y)
        // Column 3 contains a whitespace marker; sample the unmarked column 5
        // instead of treating every space cell as a flat background.
        let after = try luminance(rep, view, x: view.textOriginX + 3.1 * sw, y: y)
        XCTAssertEqual(before, plain, accuracy: 0.02)
        XCTAssertEqual(after, plain, accuracy: 0.02)
    }

    func testSelectionCoversEmptyLineAndExtendsAtLineEnd() throws {
        // Select from the middle of line 0 through the empty line 1.
        let (view, doc, url) = try makeView(text: "      \n\n      ")
        defer { try? FileManager.default.removeItem(at: url) }
        view.forcedCaretVisible = false
        doc.setSelections(selections: [TextSelection(
            anchor: TextPos(line: 0, colUtf16: 2), head: TextPos(line: 2, colUtf16: 2))], primary: 0)
        view.reloadDocument()

        let h = view.metrics.lineHeight
        let rep = try bitmap(view)
        let emptyLine = try luminance(rep, view, x: view.textOriginX + 1, y: h + h / 2)
        let outside = try luminance(rep, view, x: view.textOriginX + 300, y: h + h / 2)
        XCTAssertGreaterThan(abs(emptyLine - outside), 0.05, "empty line inside a selection shows a selected newline")
    }

    func testCurrentLineHighlightOnlyWithoutSelection() throws {
        let (view, doc, url) = try makeView(text: "      \n      \n      ")
        defer { try? FileManager.default.removeItem(at: url) }
        view.forcedCaretVisible = false
        let h = view.metrics.lineHeight
        let farX = view.textOriginX + 300

        // Caret on line 1: that line is tinted, its neighbours are not.
        doc.setSelections(selections: [TextSelection(
            anchor: TextPos(line: 1, colUtf16: 0), head: TextPos(line: 1, colUtf16: 0))], primary: 0)
        view.reloadDocument()
        var rep = try bitmap(view)
        let line0 = try luminance(rep, view, x: farX, y: h / 2)
        let line1 = try luminance(rep, view, x: farX, y: h + h / 2)
        let line2 = try luminance(rep, view, x: farX, y: 2 * h + h / 2)
        XCTAssertGreaterThan(abs(line1 - line0), 0.01)
        XCTAssertEqual(line0, line2, accuracy: 0.005)

        // A selection on the line replaces the highlight: far right of the
        // selection the line looks like any other.
        doc.setSelections(selections: [TextSelection(
            anchor: TextPos(line: 1, colUtf16: 0), head: TextPos(line: 1, colUtf16: 2))], primary: 0)
        view.reloadDocument()
        rep = try bitmap(view)
        let selectedLineFarRight = try luminance(rep, view, x: farX, y: h + h / 2)
        XCTAssertEqual(selectedLineFarRight, line0, accuracy: 0.005)
    }

    func testCaretLineNumberUsesTheAccent() throws {
        let (view, _, url) = try makeView(text: "      \n      ")
        defer { try? FileManager.default.removeItem(at: url) }
        view.forcedCaretVisible = false
        view.appearance = NSAppearance(named: .aqua)
        let rep = try bitmap(view)
        let scale = CGFloat(rep.pixelsWide) / view.bounds.width
        let h = view.metrics.lineHeight
        // Bluest pixel in each gutter cell: blue minus red.
        func warmth(line: Int) -> CGFloat {
            var best: CGFloat = 0
            for xi in stride(from: CGFloat(0), to: view.gutterWidth, by: 0.5) {
                for yi in stride(from: CGFloat(line) * h, to: CGFloat(line + 1) * h, by: 0.5) {
                    guard let c = rep.colorAt(x: Int(xi * scale), y: Int(yi * scale))?.usingColorSpace(.sRGB) else { continue }
                    best = max(best, c.blueComponent - c.redComponent)
                }
            }
            return best
        }
        XCTAssertGreaterThan(warmth(line: 0), 0.2, "caret line number uses the accent")
        XCTAssertLessThan(warmth(line: 1), 0.05, "other numbers stay neutral")
    }

    func testLineNumbersAreDrawnInGutter() throws {
        let (view, _, url) = try makeView(text: "      \n      ")
        defer { try? FileManager.default.removeItem(at: url) }
        view.forcedCaretVisible = false
        let rep = try bitmap(view)
        let h = view.metrics.lineHeight
        let background = try luminance(rep, view, x: view.textOriginX + 300, y: h * 1.5)
        // Some pixel in line 1's gutter cell must differ from the background.
        var inked = false
        for xi in stride(from: CGFloat(0), to: view.gutterWidth, by: 1) {
            for yi in stride(from: h, to: 2 * h, by: 1) {
                if abs(try luminance(rep, view, x: xi, y: yi) - background) > 0.2 { inked = true }
            }
        }
        XCTAssertTrue(inked, "line number glyphs should appear in the gutter")
    }

    /// Regression: Core Text draws black unless told to use the context
    /// colour, which made text invisible in dark mode. Glyph pixels must
    /// contrast with the background in both appearances.
    func testTextIsReadableInLightAndDarkAppearance() throws {
        for name in [NSAppearance.Name.aqua, .darkAqua] {
            let (view, _, url) = try makeView(text: "WWWWWWWWWWWW\nWWWWWWWWWWWW")
            defer { try? FileManager.default.removeItem(at: url) }
            view.forcedCaretVisible = false
            view.appearance = NSAppearance(named: name)
            let rep = try bitmap(view)
            let h = view.metrics.lineHeight
            let background = try luminance(rep, view, x: view.textOriginX + 400, y: h / 2)
            var strongest: CGFloat = 0
            for xi in stride(from: view.textOriginX, to: view.textOriginX + 100, by: 1) {
                for yi in stride(from: CGFloat(0), to: h, by: 1) {
                    strongest = max(strongest, abs(try luminance(rep, view, x: xi, y: yi) - background))
                }
            }
            XCTAssertGreaterThan(strongest, 0.5, "text must contrast with the background in \(name.rawValue)")
        }
    }

    func testAccessibilityCountNeverExceedsTheExposedValue() throws {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.newDocument(nil)
        let v = c.editorView
        v.document?.edit(command: .insertText(text: String(repeating: "a", count: EditorView.accessibilityTextLimit + 10)))
        v.didChangeDocument()
        v.flushPendingUpdates()
        let value = try XCTUnwrap(v.accessibilityValue() as? String)
        XCTAssertEqual(value.utf16.count, EditorView.accessibilityTextLimit)
        XCTAssertEqual(v.accessibilityNumberOfCharacters(), value.utf16.count)
    }
}
