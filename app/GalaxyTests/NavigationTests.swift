import XCTest
@testable import Galaxy

@MainActor
final class NavigationTests: XCTestCase {
    private func makeController(text: String = "") throws -> MainWindowController {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        controller.newDocument(nil)
        let view = controller.editorView
        view.pasteboard = NSPasteboard(name: NSPasteboard.Name("galaxy-test-\(UUID().uuidString)"))
        if !text.isEmpty { view.document?.edit(command: .insertText(text: text)) }
        view.document?.moveCursors(motion: .docStart, extend: false)
        view.didChangeDocument()
        view.flushPendingUpdates()
        return controller
    }

    private func selection(_ view: EditorView) -> (Int, Int) {
        let r = view.selectedRange()
        return (r.location, r.length)
    }

    private func key(_ view: EditorView, _ selector: Selector) {
        view.doCommand(by: selector)
    }

    private func click(_ view: EditorView, line: Int, utf16Col: Int, count: Int = 1,
                       flags: NSEvent.ModifierFlags = [], type: NSEvent.EventType = .leftMouseDown) {
        let text = view.document?.lines(first: UInt64(line), count: 1).first ?? ""
        let x = view.textOriginX + view.xOffset(in: text, column: UInt64(utf16Col)) + 0.5
        let y = CGFloat(line) * view.metrics.lineHeight + view.metrics.lineHeight / 2
        let point = view.convert(NSPoint(x: x, y: y), to: nil)
        let event = NSEvent.mouseEvent(
            with: type, location: point, modifierFlags: flags, timestamp: 0,
            windowNumber: view.window?.windowNumber ?? 0, context: nil,
            eventNumber: 0, clickCount: count, pressure: 1)!
        if type == .leftMouseDragged { view.mouseDragged(with: event) } else { view.mouseDown(with: event) }
    }

    func testArrowKeysAndShiftSelection() throws {
        let c = try makeController(text: "hello world\nfoo")
        let v = c.editorView
        key(v, #selector(NSResponder.moveRight(_:)))
        key(v, #selector(NSResponder.moveRight(_:)))
        XCTAssertEqual(selection(v).0, 2)
        key(v, #selector(NSResponder.moveRightAndModifySelection(_:)))
        XCTAssertEqual(selection(v).0, 2)
        XCTAssertEqual(selection(v).1, 1)
        key(v, #selector(NSResponder.moveWordRightAndModifySelection(_:)))
        XCTAssertEqual(selection(v).1, 3, "extends to the end of 'hello'")
        key(v, #selector(NSResponder.moveLeft(_:)))
        XCTAssertEqual(selection(v).0, 2, "Left collapses to the selection start")
        XCTAssertEqual(selection(v).1, 0)
        key(v, #selector(NSResponder.moveDown(_:)))
        key(v, #selector(NSResponder.moveToEndOfLine(_:)))
        XCTAssertEqual(selection(v).0, 15)
        key(v, #selector(NSResponder.moveToBeginningOfDocumentAndModifySelection(_:)))
        XCTAssertEqual(selection(v).0, 0)
        XCTAssertEqual(selection(v).1, 15)
    }

    func testHomeEndPageKeysMoveTheCaret() throws {
        let lines = (0..<200).map { "line \($0)" }.joined(separator: "\n")
        let c = try makeController(text: lines)
        let v = c.editorView
        key(v, #selector(NSResponder.scrollToEndOfDocument(_:)))
        XCTAssertEqual(selection(v).0, 6, "End moves to the end of \"line 0\"")
        key(v, #selector(NSResponder.scrollPageDown(_:)))
        XCTAssertGreaterThan(selection(v).0, 20, "Page Down moves the caret down")
        key(v, #selector(NSResponder.moveToEndOfDocument(_:)))
        XCTAssertEqual(selection(v).0, (v.document?.lenUtf16()).map(Int.init))
        key(v, #selector(NSResponder.pageUp(_:)))
        XCTAssertLessThan(selection(v).0, Int(v.document!.lenUtf16()) - 20)
    }

    func testTypingAfterMovingInsertsAtTheCaret() throws {
        let c = try makeController(text: "abc")
        let v = c.editorView
        key(v, #selector(NSResponder.moveRight(_:)))
        v.insertText("X", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(v.document?.lines(first: 0, count: 1), ["aXbc"])
        XCTAssertEqual(selection(v).0, 2)
    }

    func testMouseClickDragWordLineAndMultiCaret() throws {
        let c = try makeController(text: "hello world\nsecond line")
        let v = c.editorView
        click(v, line: 0, utf16Col: 6)
        XCTAssertEqual(selection(v).0, 6)
        XCTAssertEqual(selection(v).1, 0)

        click(v, line: 1, utf16Col: 3, type: .leftMouseDragged)
        XCTAssertEqual(selection(v).0, 6)
        XCTAssertEqual(selection(v).1, 12 + 3 - 6)

        click(v, line: 0, utf16Col: 8, count: 2)
        XCTAssertEqual(selection(v).0, 6)
        XCTAssertEqual(selection(v).1, 5, "double-click selects 'world'")

        click(v, line: 1, utf16Col: 2, count: 3)
        XCTAssertEqual(selection(v).0, 12)
        XCTAssertEqual(selection(v).1, 11, "triple-click selects the last line")

        click(v, line: 0, utf16Col: 1)
        click(v, line: 1, utf16Col: 1, flags: .shift)
        XCTAssertEqual(selection(v).0, 1)
        XCTAssertEqual(selection(v).1, 12, "shift-click extends")

        click(v, line: 0, utf16Col: 0)
        click(v, line: 1, utf16Col: 0, flags: .option)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 2).selectionCount, 2)
        v.insertText("#", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(v.document?.lines(first: 0, count: 2), ["#hello world", "#second line"])
    }

    func testClickingPastTheEndsClampsToValidPositions() throws {
        let c = try makeController(text: "ab\ncd")
        let v = c.editorView
        let below = v.textPosition(at: NSPoint(x: 10_000, y: 10_000))
        XCTAssertEqual(below.line, 1)
        XCTAssertEqual(below.colUtf16, 2)
        let above = v.textPosition(at: NSPoint(x: -50, y: -50))
        XCTAssertEqual(above.line, 0)
        XCTAssertEqual(above.colUtf16, 0)
        let nan = v.textPosition(at: NSPoint(x: 0, y: CGFloat.nan))
        XCTAssertEqual(nan.line, 0)
    }

    func testSelectAllCopyCutPasteAndLinewise() throws {
        let c = try makeController(text: "one\ntwo\nthree")
        let v = c.editorView
        let pb = v.pasteboard

        v.selectAll(nil)
        XCTAssertEqual(selection(v).1, 13)
        v.copy(nil)
        XCTAssertEqual(pb.string(forType: .string), "one\ntwo\nthree")
        XCTAssertNil(pb.string(forType: EditorView.linewiseType))

        // No selection: copy takes the whole line and flags it.
        click(v, line: 1, utf16Col: 1)
        v.copy(nil)
        XCTAssertEqual(pb.string(forType: .string), "two\n")
        XCTAssertEqual(pb.string(forType: EditorView.linewiseType), "two\n")
        v.paste(nil)
        XCTAssertEqual(v.document?.lines(first: 0, count: 5), ["one", "two", "two", "three"])

        v.cut(nil)
        XCTAssertEqual(v.document?.lines(first: 0, count: 5), ["one", "two", "three"])
        XCTAssertEqual(pb.string(forType: .string), "two\n")

        // The same text copied from elsewhere is a plain paste.
        pb.clearContents()
        pb.setString("two\n", forType: .string)
        click(v, line: 0, utf16Col: 3)
        v.paste(nil)
        XCTAssertEqual(v.document?.lines(first: 0, count: 1), ["onetwo"])
    }

    func testMenuItemsAreValidatedAgainstState() throws {
        let c = try makeController(text: "x")
        let v = c.editorView
        func enabled(_ action: Selector) -> Bool {
            v.validateMenuItem(NSMenuItem(title: "", action: action, keyEquivalent: ""))
        }
        XCTAssertTrue(enabled(#selector(EditorView.undoEdit(_:))))
        XCTAssertFalse(enabled(#selector(EditorView.redoEdit(_:))))
        v.undoEdit(nil)
        XCTAssertTrue(enabled(#selector(EditorView.redoEdit(_:))))
        v.pasteboard.clearContents()
        XCTAssertFalse(enabled(#selector(EditorView.paste(_:))))
    }

    // MARK: Saving

    func testSaveWritesTheFileAndClearsTheEditedMark() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-save-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("a.txt")
        try "hi".write(to: file, atomically: true, encoding: .utf8)

        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.open(url: file)
        XCTAssertEqual(c.window?.isDocumentEdited, false)

        let v = c.editorView
        key(v, #selector(NSResponder.moveToEndOfDocument(_:)))
        v.insertText("!", replacementRange: NSRange(location: NSNotFound, length: 0))
        v.flushPendingUpdates()
        XCTAssertEqual(c.window?.isDocumentEdited, true, "typing marks the window as edited")

        c.saveDocument(nil)
        XCTAssertEqual(try String(contentsOf: file, encoding: .utf8), "hi!")
        XCTAssertEqual(c.window?.isDocumentEdited, false)
    }

    func testSavingAnUntitledDocumentAsksForALocation() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-saveas-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let target = dir.appendingPathComponent("new.txt")

        let c = try makeController(text: "draft")
        defer { c.window?.orderOut(nil) }
        var asked = 0
        c.askSaveLocation = { _ in asked += 1; return target }
        XCTAssertEqual(c.window?.isDocumentEdited, true, "the initial text already differs from a clean buffer")
        c.editorView.insertText("!", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        XCTAssertEqual(c.window?.isDocumentEdited, true)

        XCTAssertTrue(c.save())
        XCTAssertEqual(asked, 1)
        XCTAssertEqual(try String(contentsOf: target, encoding: .utf8), "!draft")
        XCTAssertEqual(c.window?.title, "new.txt")
        XCTAssertEqual(c.window?.isDocumentEdited, false)

        c.saveDocument(nil)
        XCTAssertEqual(asked, 1, "a second save reuses the path")

        // Cancelling the panel reports failure and leaves the text dirty.
        let d = try makeController(text: "zzz")
        defer { d.window?.orderOut(nil) }
        d.askSaveLocation = { _ in nil }
        XCTAssertFalse(d.save())
    }

    func testUnsavedChangesPromptBeforeReplacingOrClosing() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-prompt-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let a = dir.appendingPathComponent("a.txt")
        let b = dir.appendingPathComponent("b.txt")
        try "A".write(to: a, atomically: true, encoding: .utf8)
        try "B".write(to: b, atomically: true, encoding: .utf8)

        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        var asked: [String] = []
        var answer = MainWindowController.SaveChoice.cancel
        c.askSaveChoice = { name in asked.append(name); return answer }

        c.open(url: a)
        XCTAssertTrue(c.confirmDiscardChanges(), "clean documents never prompt")
        XCTAssertTrue(asked.isEmpty)

        // Opening another file adds a tab: nothing to ask yet.
        c.editorView.insertText("x", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.open(url: b)
        XCTAssertTrue(asked.isEmpty)
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["B"])
        XCTAssertFalse(c.windowShouldClose(c.window!))
        XCTAssertEqual(asked, ["a.txt"], "closing asks about the unsaved tab")
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["xA"], "Cancel shows the tab it asked about")

        answer = .discard
        XCTAssertTrue(c.closeTab(at: 0))
        XCTAssertEqual(try String(contentsOf: a, encoding: .utf8), "A", "discard leaves the file alone")
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["B"])

        c.editorView.insertText("y", replacementRange: NSRange(location: NSNotFound, length: 0))
        answer = .save
        XCTAssertTrue(c.windowShouldClose(c.window!))
        XCTAssertEqual(try String(contentsOf: b, encoding: .utf8), "yB", "save writes before closing")
    }

    // MARK: Multi-cursor

    func testCmdDAddsNextOccurrencesAndTypingEditsAll() throws {
        let c = try makeController(text: "foo bar\nfoo baz\nfoo")
        let v = c.editorView
        click(v, line: 0, utf16Col: 1)
        v.selectNextOccurrence(nil)
        XCTAssertEqual(selection(v).0, 0)
        XCTAssertEqual(selection(v).1, 3, "first press selects the word")
        v.selectNextOccurrence(nil)
        v.selectNextOccurrence(nil)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 3).selectionCount, 3)
        v.insertText("X", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(v.document?.lines(first: 0, count: 3), ["X bar", "X baz", "X"])
        v.undoEdit(nil)
        XCTAssertEqual(v.document?.lines(first: 0, count: 3), ["foo bar", "foo baz", "foo"])
    }

    func testSelectAllOccurrencesAndCursorAboveBelow() throws {
        let c = try makeController(text: "ab\nab\nab")
        let v = c.editorView
        click(v, line: 1, utf16Col: 1)
        v.selectAllOccurrences(nil)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 3).selectionCount, 3)

        click(v, line: 0, utf16Col: 1)
        v.addCursorBelow(nil)
        v.addCursorBelow(nil)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 3).selectionCount, 3)
        v.addCursorAbove(nil)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 3).selectionCount, 3, "no line above the first")
        v.insertText("-", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(v.document?.lines(first: 0, count: 3), ["a-b", "a-b", "a-b"])
    }

    func testOptionShiftDragMakesAColumnSelection() throws {
        let c = try makeController(text: "abcdef\nabcdef\nab")
        let v = c.editorView
        let flags: NSEvent.ModifierFlags = [.option, .shift]
        click(v, line: 0, utf16Col: 1, flags: flags)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 3).selectionCount, 1)
        click(v, line: 2, utf16Col: 2, flags: flags, type: .leftMouseDragged)
        XCTAssertEqual(v.document?.viewport(firstLine: 0, count: 3).selectionCount, 3)
        v.insertText("|", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(v.document?.lines(first: 0, count: 3), ["a|cdef", "a|cdef", "a|"])
        v.mouseUp(with: NSEvent.mouseEvent(
            with: .leftMouseUp, location: .zero, modifierFlags: [], timestamp: 0,
            windowNumber: v.window?.windowNumber ?? 0, context: nil, eventNumber: 0,
            clickCount: 1, pressure: 0)!)
        XCTAssertNil(v.columnAnchor)
    }

    func testColumnPositionKeepsColumnsBeyondTheLineEnd() throws {
        let c = try makeController(text: "abcdef\nab")
        let v = c.editorView
        let y = v.metrics.lineHeight * 1.5
        let far = v.columnPosition(at: NSPoint(x: v.textOriginX + v.metrics.spaceWidth * 5, y: y))
        XCTAssertEqual(far.line, 1)
        XCTAssertEqual(far.colUtf16, 5, "five columns in, three past the 2-char line")
        let inside = v.columnPosition(at: NSPoint(x: v.textOriginX + v.metrics.spaceWidth * 1, y: y))
        XCTAssertEqual(inside.colUtf16, 1)
    }

    func testSelectionMenuHasTheExpectedShortcuts() throws {
        let menu = try XCTUnwrap(
            MainMenu.build().items.compactMap(\.submenu).first { $0.title == "Selection" })
        let next = try XCTUnwrap(menu.item(withTitle: "Add Next Occurrence"))
        XCTAssertEqual(next.keyEquivalent, "d")
        XCTAssertEqual(next.keyEquivalentModifierMask, .command)
        let all = try XCTUnwrap(menu.item(withTitle: "Select All Occurrences"))
        XCTAssertEqual(all.keyEquivalent, "l")
        XCTAssertEqual(all.keyEquivalentModifierMask, [.command, .shift])
        let above = try XCTUnwrap(menu.item(withTitle: "Add Cursor Above"))
        XCTAssertEqual(above.keyEquivalentModifierMask, [.command, .option])
        XCTAssertNotNil(menu.item(withTitle: "Add Cursor Below"))
    }

    // MARK: Per-frame coalescing (M1-15)

    func testChangeNotificationsAreCoalescedPerFrame() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        let v = c.editorView
        var notified = 0
        v.onChange = { notified += 1 }
        let before = v.frameFlushes
        for _ in 0..<50 {
            v.insertText("x", replacementRange: NSRange(location: NSNotFound, length: 0))
        }
        XCTAssertEqual(notified, 0, "nothing runs until the frame")
        XCTAssertTrue(v.hasPendingChange)
        XCTAssertEqual(v.document?.lines(first: 0, count: 1), [String(repeating: "x", count: 50)],
                       "edits themselves are applied immediately")

        v.flushPendingUpdates()
        XCTAssertEqual(notified, 1, "fifty edits, one notification")
        XCTAssertEqual(v.frameFlushes, before + 1)
        v.flushPendingUpdates()
        XCTAssertEqual(notified, 1, "no extra work without a new change")
    }

    func testTheDisplayLinkDeliversPendingChangesOnItsOwn() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        let v = c.editorView
        let delivered = expectation(description: "onChange on the next frame")
        v.onChange = { delivered.fulfill() }
        v.insertText("y", replacementRange: NSRange(location: NSNotFound, length: 0))
        wait(for: [delivered], timeout: 2)
        XCTAssertFalse(v.hasPendingChange)
    }

    func testClosingTheWindowSeesTheLatestChange() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        var asked = 0
        c.askSaveChoice = { _ in asked += 1; return .cancel }
        c.editorView.insertText("z", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertFalse(c.windowShouldClose(c.window!))
        XCTAssertEqual(asked, 1)
        XCTAssertEqual(c.window?.isDocumentEdited, true)
    }
}

@MainActor
final class LatencyProbeTests: XCTestCase {
    private func keyEvent(_ chars: String, keyCode: UInt16, window: NSWindow) -> NSEvent {
        NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [],
                         timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
                         context: nil, characters: chars, charactersIgnoringModifiers: chars,
                         isARepeat: false, keyCode: keyCode)!
    }

    func testAKeyThatEditsDrawsImmediately() throws {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.newDocument(nil)
        let window = try XCTUnwrap(c.window)
        let probe = LatencyProbe.shared
        probe.reset()

        c.editorView.keyDown(with: keyEvent("a", keyCode: 0, window: window))
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["a"], "went through the real key path")
        XCTAssertEqual(probe.latencies.count, 1, "the edit must not wait for the next display frame")
        XCTAssertEqual(probe.drawTimes.count, 1)
        let sample = try XCTUnwrap(probe.latencies.first)
        XCTAssertGreaterThanOrEqual(sample, probe.drawTimes[0], "includes the draw itself")
        XCTAssertLessThan(sample, 1_000, "sanity bound, not a performance gate")

        // Another draw without a key (caret blink, scroll) is not a sample.
        c.editorView.needsDisplay = true
        c.editorView.displayIfNeeded()
        XCTAssertEqual(probe.latencies.count, 1)
        XCTAssertTrue(probe.report.hasPrefix("latency_samples=1 "), probe.report)
    }

    func testPercentilesAndTheRingBuffer() {
        XCTAssertEqual(LatencyProbe.percentile([1, 2, 3, 4], 0.5), 2)
        XCTAssertEqual(LatencyProbe.percentile([1, 2, 3, 4], 0.95), 4)
        XCTAssertEqual(LatencyProbe.percentile([], 0.5), 0)
        let probe = LatencyProbe.shared
        probe.reset()
        let event = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0,
                                     windowNumber: 0, context: nil, characters: "x",
                                     charactersIgnoringModifiers: "x", isARepeat: false, keyCode: 7)!
        for _ in 0..<(LatencyProbe.capacity + 10) {
            probe.keyDown(event)
            probe.drawFinished(drawStart: ProcessInfo.processInfo.systemUptime)
        }
        XCTAssertEqual(probe.latencies.count, LatencyProbe.capacity, "keeps the newest samples only")
        probe.keyDown(event)
        probe.cancelPending()
        probe.drawFinished(drawStart: ProcessInfo.processInfo.systemUptime)
        XCTAssertEqual(probe.latencies.count, LatencyProbe.capacity, "a cancelled key is not counted")
        probe.reset()
        XCTAssertNil(probe.summary)
    }

    func testArrowKeysDoNotRunWholeDocumentListeners() throws {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.newDocument(nil)
        let v = c.editorView
        v.document?.edit(command: .insertText(text: "abc\ndef"))
        v.didChangeDocument()
        v.flushPendingUpdates()
        var notified = 0
        v.onChange = { notified += 1 }
        v.doCommand(by: #selector(NSResponder.moveLeft(_:)))
        v.doCommand(by: #selector(NSResponder.moveUp(_:)))
        v.flushPendingUpdates()
        XCTAssertEqual(notified, 0, "moving the caret is not an edit")
        XCTAssertEqual(v.selectedRange().location, 2, "but the caret did move")
        v.doCommand(by: #selector(NSResponder.deleteBackward(_:)))
        v.flushPendingUpdates()
        XCTAssertEqual(notified, 1, "an edit still notifies")
    }
}
