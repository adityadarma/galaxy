import XCTest
@testable import Galaxy

/// Soft wrap: the core decides where rows break, the view lays them out,
/// hit-tests them and moves the caret across them.
@MainActor
final class WordWrapTests: XCTestCase {
    private let sentence = String(repeating: "lorem ipsum ", count: 40)

    private func makeView(text: String, width: CGFloat = 320) throws -> (EditorView, DocumentHandle) {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-wrap-\(UUID().uuidString).txt")
        try text.write(to: url, atomically: true, encoding: .utf8)
        addTeardownBlock { try? FileManager.default.removeItem(at: url) }
        let doc = try CoreBridge().openDocument(path: url.path)
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: width, height: 400))
        view.document = doc
        return (view, doc)
    }

    func testLongLineBecomesSeveralRows() throws {
        let (view, _) = try makeView(text: sentence)
        XCTAssertEqual(view.rowCount, 1)
        view.wrapEnabled = true
        XCTAssertEqual(view.lineCount, 1)
        XCTAssertGreaterThan(view.rowCount, 3)
        XCTAssertGreaterThanOrEqual(view.frame.height, CGFloat(view.rowCount) * view.metrics.lineHeight)
        XCTAssertEqual(view.lineRows(0).count, view.rowCount)

        view.wrapEnabled = false
        XCTAssertEqual(view.rowCount, 1, "off again: one row per line")
        XCTAssertFalse(view.wrapActive)
    }

    func testResizingRewraps() throws {
        let (view, _) = try makeView(text: sentence, width: 320)
        view.wrapEnabled = true
        let narrow = view.rowCount
        view.setFrameSize(NSSize(width: 900, height: 400))
        XCTAssertLessThan(view.rowCount, narrow)
        XCTAssertGreaterThanOrEqual(view.frame.height, CGFloat(view.rowCount) * view.metrics.lineHeight)
    }

    func testSuspendedWrapKeepsOneRowPerLine() throws {
        let (view, _) = try makeView(text: sentence)
        view.wrapEnabled = true
        view.wrapSuspended = true
        XCTAssertEqual(view.rowCount, view.lineCount)
        view.wrapSuspended = false
        XCTAssertGreaterThan(view.rowCount, 1)
    }

    func testHitTestAndCaretRectAgreeOnWrappedRows() throws {
        let (view, _) = try makeView(text: sentence)
        view.wrapEnabled = true
        let rows = view.lineRows(0)
        let second = try XCTUnwrap(rows.breaks.first)
        let h = view.metrics.lineHeight

        // Start of the second row.
        let start = view.textPosition(at: NSPoint(x: view.textOriginX, y: h + 1))
        XCTAssertEqual(start.line, 0)
        XCTAssertEqual(Int(start.colUtf16), second)

        // Far right of the first row stays on the first row.
        let end = view.textPosition(at: NSPoint(x: view.bounds.width - 1, y: 1))
        XCTAssertLessThan(Int(end.colUtf16), second)

        // The caret rect of a column on row two sits on row two, near the left.
        let rect = view.rect(forLine: 0, column: UInt64(second + 3), width: 2)
        XCTAssertEqual(rect.minY, h, accuracy: 0.5)
        XCTAssertEqual(rect.minX, view.textOriginX + 3 * view.metrics.spaceWidth, accuracy: 1.5)

        // And a click at that rect round-trips to the same column.
        let back = view.textPosition(at: NSPoint(x: rect.minX + 1, y: rect.midY))
        XCTAssertEqual(Int(back.colUtf16), second + 3)
    }

    func testArrowDownMovesByVisualRow() throws {
        let (view, doc) = try makeView(text: sentence + "\nnext")
        view.wrapEnabled = true
        let second = try XCTUnwrap(view.lineRows(0).breaks.first)
        doc.setCaret(pos: TextPos(line: 0, colUtf16: 0))
        doc.moveCursors(motion: .down, extend: false)
        let cursor = doc.primaryCursor()
        XCTAssertEqual(cursor.line, 0)
        XCTAssertEqual(Int(cursor.colUtf16), second)
        doc.moveCursors(motion: .up, extend: false)
        XCTAssertEqual(doc.primaryCursor().colUtf16, 0)
    }

    func testDrawingAWrappedDocumentWithSelectionDoesNotCrash() throws {
        let (view, doc) = try makeView(text: sentence + "\nsecond line")
        view.wrapEnabled = true
        doc.setSelections(selections: [TextSelection(anchor: TextPos(line: 0, colUtf16: 5),
                                                     head: TextPos(line: 1, colUtf16: 3))], primary: 0)
        view.reloadDocument()
        let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: rep)
        XCTAssertFalse(view.lastDrawnLines.isEmpty)
        XCTAssertEqual(view.lastDrawnLines.lowerBound, 0)
    }

    func testFractionalLineRoundTripsThroughRows() throws {
        let (view, _) = try makeView(text: sentence + "\nnext")
        view.wrapEnabled = true
        let rows = view.lineRows(0).count
        let y = view.y(forFractionalLine: 0.5)
        XCTAssertEqual(y, CGFloat(Double(rows) * 0.5) .rounded(.down) * view.metrics.lineHeight, accuracy: view.metrics.lineHeight)
        XCTAssertEqual(view.fractionalLine(atY: view.rowTop(of: 1)), 1.0, accuracy: 0.001)
    }

    /// A call that fits one row until the parameter labels take room: the
    /// core wraps with them, an argument that starts a row starts at its
    /// label, and clicks, carets and arrow keys all agree.
    func testInlineHintsWrapWithTheirArguments() throws {
        let (probe, _) = try makeView(text: "x")
        probe.wrapEnabled = true
        let columns = try XCTUnwrap(probe.appliedWrapColumns)

        var args = ["a0"]
        func source() -> String { "f(" + args.joined(separator: ", ") + ")" }
        while source().count < columns - 4 { args.append("a\(args.count)") }
        let text = source()
        var column = 2
        var hints: [EditorView.InlineHint] = []
        for arg in args {
            hints.append(EditorView.InlineHint(column: column, label: "value:"))
            column += arg.count + 2
        }

        let (view, doc) = try makeView(text: text)
        view.wrapEnabled = true
        XCTAssertEqual(view.rowCount, 1, "fits without labels")

        view.setInlineHints([text: hints])
        XCTAssertGreaterThan(view.rowCount, 1, "labels take room, so the line wraps")
        XCTAssertEqual(view.lineRows(0).count, view.rowCount)
        XCTAssertGreaterThanOrEqual(view.frame.height, CGFloat(view.rowCount) * view.metrics.lineHeight)

        // The break falls before a labelled argument, which starts its row at the label.
        let second = try XCTUnwrap(view.lineRows(0).breaks.first)
        XCTAssertTrue(hints.contains { $0.column == second })
        let h = view.metrics.lineHeight
        let rect = view.rect(forLine: 0, column: UInt64(second), width: 2)
        XCTAssertEqual(rect.minY, h, accuracy: 0.5)
        XCTAssertEqual(rect.minX, view.textOriginX + view.hintAdvance("value:") - EditorView.hintGap, accuracy: 1.5)
        let back = view.textPosition(at: NSPoint(x: rect.minX + 1, y: rect.midY))
        XCTAssertEqual(Int(back.colUtf16), second)

        // The core moves by the same rows.
        doc.setCaret(pos: TextPos(line: 0, colUtf16: 0))
        doc.moveCursors(motion: .down, extend: false)
        XCTAssertEqual(Int(doc.primaryCursor().colUtf16), second)

        // Drawing the labels on wrapped rows works, and clearing them unwraps.
        let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: rep)
        view.setInlineHints([:])
        XCTAssertEqual(view.rowCount, 1)
    }

    func testWordWrapMenuItemTogglesAndPersists() throws {
        let defaults = UserDefaults.standard
        let saved = defaults.object(forKey: "WordWrap")
        defer { defaults.set(saved, forKey: "WordWrap") }
        defaults.set(false, forKey: "WordWrap")

        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        let item = NSMenuItem(title: "", action: #selector(MainWindowController.toggleWordWrap(_:)), keyEquivalent: "")
        _ = c.validateMenuItem(item)
        XCTAssertEqual(item.state, .off)
        c.toggleWordWrap(nil)
        _ = c.validateMenuItem(item)
        XCTAssertEqual(item.state, .on)
        XCTAssertTrue(defaults.bool(forKey: "WordWrap"))
        XCTAssertTrue(c.editorView.wrapEnabled)
        c.toggleWordWrap(nil)
        XCTAssertFalse(c.editorView.wrapEnabled)
    }
}
