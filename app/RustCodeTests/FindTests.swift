import XCTest
@testable import RustCode

@MainActor
final class FindTests: XCTestCase {
    private func makeController(text: String) -> MainWindowController {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        controller.newDocument(nil)
        controller.editorView.document?.edit(command: .insertText(text: text))
        controller.editorView.document?.moveCursors(motion: .docStart, extend: false)
        controller.editorView.didChangeDocument()
        controller.editorView.flushPendingUpdates()
        return controller
    }

    private func selection(_ c: MainWindowController) -> (Int, Int) {
        let r = c.editorView.selectedRange()
        return (r.location, r.length)
    }

    private func text(_ c: MainWindowController) -> [String] {
        let doc = c.editorView.document!
        return doc.lines(first: 0, count: doc.lineCount())
    }

    func testFindBarIsHiddenUntilRequestedAndShowsResultCount() {
        let c = makeController(text: "cat hat cat")
        defer { c.window?.orderOut(nil) }
        XCTAssertFalse(c.isFindVisible)
        c.showFind(nil)
        XCTAssertTrue(c.isFindVisible)
        c.window?.contentView?.layoutSubtreeIfNeeded()
        XCTAssertEqual(c.findBar.frame.height, FindBar.findOnlyHeight, "find row only")
        c.showFindAndReplace(nil)
        c.window?.contentView?.layoutSubtreeIfNeeded()
        XCTAssertEqual(c.findBar.frame.height, FindBar.withReplaceHeight, "grows for the replace row")
        XCTAssertGreaterThan(c.editorView.enclosingScrollView?.frame.height ?? 0, 100, "editor keeps its space")

        c.findBar.setPattern("cat")
        XCTAssertEqual(selection(c).0, 0)
        XCTAssertEqual(selection(c).1, 3, "typing selects the nearest match")
        XCTAssertEqual(c.findBar.countLabel.stringValue, "1 of 2")

        c.findBar.setPattern("zzz")
        XCTAssertEqual(c.findBar.countLabel.stringValue, "No results")
        c.findBar.setPattern("")
        XCTAssertEqual(c.findBar.countLabel.stringValue, "")
    }

    func testNextAndPreviousWalkMatchesAndWrap() {
        let c = makeController(text: "ab x ab\nab")
        defer { c.window?.orderOut(nil) }
        c.showFind(nil)
        c.findBar.setPattern("ab")
        XCTAssertEqual(selection(c).0, 0)
        c.findNext(nil)
        XCTAssertEqual(selection(c).0, 5)
        XCTAssertEqual(c.findBar.countLabel.stringValue, "2 of 3")
        c.findNext(nil)
        c.findNext(nil)
        XCTAssertEqual(selection(c).0, 0, "wraps to the first match")
        c.findPrevious(nil)
        XCTAssertEqual(selection(c).0, 8, "and back to the last")
    }

    func testOptionsChangeTheResults() {
        let c = makeController(text: "Cat cat concat")
        defer { c.window?.orderOut(nil) }
        c.showFind(nil)
        c.findBar.setPattern("cat")
        XCTAssertTrue(c.findBar.countLabel.stringValue.hasPrefix("1 of 3"))

        c.findBar.caseButton.state = .on
        c.findBar.onPatternChange?()
        XCTAssertTrue(c.findBar.countLabel.stringValue.contains("of 2"), c.findBar.countLabel.stringValue)

        c.findBar.wordButton.state = .on
        c.findBar.onPatternChange?()
        XCTAssertTrue(c.findBar.countLabel.stringValue.contains("of 1"), c.findBar.countLabel.stringValue)

        c.findBar.caseButton.state = .off
        c.findBar.wordButton.state = .off
        c.findBar.regexButton.state = .on
        c.findBar.setPattern("c.t$")
        XCTAssertEqual(selection(c).0, 11, "regex anchors at the end of the line")
    }

    func testInvalidRegexShowsTheErrorInsteadOfCrashing() {
        let c = makeController(text: "a(b")
        defer { c.window?.orderOut(nil) }
        c.showFind(nil)
        c.findBar.regexButton.state = .on
        c.findBar.setPattern("(")
        XCTAssertTrue(c.findBar.countLabel.stringValue.contains("unclosed"), c.findBar.countLabel.stringValue)
        c.findNext(nil)
        XCTAssertEqual(selection(c).1, 0)
    }

    func testMatchesAreHighlightedInTheViewport() {
        let c = makeController(text: "ab ab\ncd")
        defer { c.window?.orderOut(nil) }
        c.showFind(nil)
        c.findBar.setPattern("ab")
        let lines = c.editorView.document!.viewport(firstLine: 0, count: 2).lines
        XCTAssertEqual(lines[0].matches.map { Int($0.start) }, [0, 3])
        XCTAssertTrue(lines[1].matches.isEmpty)
        // Drawing with matches must not trap.
        c.editorView.display()
        XCTAssertFalse(c.editorView.lastDrawnLines.isEmpty)

        c.closeFind(nil)
        XCTAssertFalse(c.isFindVisible)
        XCTAssertTrue(c.editorView.document!.viewport(firstLine: 0, count: 2).lines[0].matches.isEmpty,
                      "closing the bar clears the highlights")
    }

    func testOpeningTheBarSeedsItFromTheSelection() {
        let c = makeController(text: "hello world")
        defer { c.window?.orderOut(nil) }
        c.editorView.document?.selectWordAt(pos: TextPos(line: 0, colUtf16: 7))
        c.showFind(nil)
        XCTAssertEqual(c.findBar.pattern, "world")
        XCTAssertEqual(selection(c).0, 6)
        XCTAssertEqual(selection(c).1, 5)
    }

    func testReplaceOneAndReplaceAll() {
        let c = makeController(text: "a1 a2 a3")
        defer { c.window?.orderOut(nil) }
        c.showFindAndReplace(nil)
        XCTAssertTrue(c.findBar.isReplaceVisible)
        c.findBar.regexButton.state = .on
        c.findBar.setPattern(#"a(\d)"#)
        c.findBar.replaceField.stringValue = "<$1>"

        c.findBar.onReplace?()
        XCTAssertEqual(text(c), ["<1> a2 a3"], "first match replaced")
        XCTAssertEqual(selection(c).0, 4, "moved on to the next match")

        c.findBar.onReplaceAll?()
        XCTAssertEqual(text(c), ["<1> <2> <3>"])
        XCTAssertTrue(c.statusLabel.stringValue.contains("Replaced 2"), c.statusLabel.stringValue)
        c.editorView.undoEdit(nil)
        XCTAssertEqual(text(c), ["<1> a2 a3"], "replace all is a single undo step")
    }

    func testSelectAllMatchesEditsEveryOccurrenceAtOnce() {
        let c = makeController(text: "ab x ab\nab")
        defer { c.window?.orderOut(nil) }
        c.showFind(nil)
        c.findBar.setPattern("ab")
        c.findBar.onSelectAll?()
        XCTAssertEqual(c.editorView.document?.viewport(firstLine: 0, count: 2).selectionCount, 3)
        c.editorView.insertText("Z", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(text(c), ["Z x Z", "Z"])
    }

    func testCountStaysCorrectWhileTheTextChanges() {
        let c = makeController(text: "ab ab")
        defer { c.window?.orderOut(nil) }
        c.showFind(nil)
        c.findBar.setPattern("ab")
        XCTAssertTrue(c.findBar.countLabel.stringValue.contains("of 2"))
        c.editorView.document?.moveCursors(motion: .docEnd, extend: false)
        c.editorView.insertText(" ab", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        XCTAssertTrue(c.findBar.countLabel.stringValue.contains("3"), c.findBar.countLabel.stringValue)
    }

    func testFindMenuShortcuts() throws {
        let menu = try XCTUnwrap(MainMenu.build().items.compactMap(\.submenu).first { $0.title == "Find" })
        let find = try XCTUnwrap(menu.item(withTitle: "Find…"))
        XCTAssertEqual(find.keyEquivalent, "f")
        XCTAssertEqual(find.keyEquivalentModifierMask, .command)
        let replace = try XCTUnwrap(menu.item(withTitle: "Find and Replace…"))
        XCTAssertEqual(replace.keyEquivalentModifierMask, [.command, .option])
        let next = try XCTUnwrap(menu.item(withTitle: "Find Next"))
        XCTAssertEqual(next.keyEquivalent, "g")
        let previous = try XCTUnwrap(menu.item(withTitle: "Find Previous"))
        XCTAssertEqual(previous.keyEquivalentModifierMask, [.command, .shift])
    }
}

@MainActor
final class LargeFileTests: XCTestCase {
    private func tempFile(_ text: String) throws -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("rcode-large-\(UUID().uuidString).txt")
        try text.write(to: url, atomically: true, encoding: .utf8)
        return url
    }

    /// Waits until a background count has updated the bar and the label
    /// satisfies `ok`.
    private func waitForCount(_ c: MainWindowController, timeout: TimeInterval = 30,
                              file: StaticString = #filePath, line: UInt = #line,
                              _ ok: @escaping (String) -> Bool) {
        let done = expectation(description: "background count")
        done.assertForOverFulfill = false
        c.onFindCountFinished = {
            if ok(c.findBar.countLabel.stringValue) { done.fulfill() }
        }
        if !c.isCountingFind, ok(c.findBar.countLabel.stringValue) { done.fulfill() }
        wait(for: [done], timeout: timeout)
        c.onFindCountFinished = nil
    }

    func testLargeFileShowsTheBannerAndCountsInTheBackground() throws {
        var text = "first line ab\n"
        text += String(repeating: "x", count: 15_000) + " ab\n"
        text += String(repeating: "let value = 1; // ab\n", count: 1_050_000)
        let url = try tempFile(text)
        defer { try? FileManager.default.removeItem(at: url) }
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }

        c.open(url: url)
        let doc = try XCTUnwrap(c.editorView.document)
        XCTAssertTrue(doc.isLarge())
        XCTAssertFalse(c.largeFileBanner.isHidden)

        c.showFind(nil)
        c.findBar.setPattern("ab")
        XCTAssertEqual(c.findBar.countLabel.stringValue, "Counting…", "the main thread does not count")
        XCTAssertTrue(c.isCountingFind)
        XCTAssertEqual(c.editorView.selectedRange().length, 2, "the nearest match is selected at once")
        waitForCount(c) { $0 == "1 of 20000+" }

        c.findNext(nil)
        XCTAssertEqual(doc.positionAt(offset: UInt64(c.editorView.selectedRange().location)).line, 1)
        waitForCount(c) { $0 == "2 of 20000+" }

        c.newDocument(nil)
        XCTAssertTrue(c.largeFileBanner.isHidden)
    }

    func testAStaleCountNeverOverwritesANewerOne() throws {
        // Over the 1 MB sync limit with exactly one match.
        let text = "ab\n" + String(repeating: "filler line without it\n", count: 60_000)
        let url = try tempFile(text)
        defer { try? FileManager.default.removeItem(at: url) }
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.open(url: url)

        c.showFind(nil)
        c.findBar.setPattern("ab")
        XCTAssertEqual(c.findBar.countLabel.stringValue, "Counting…")
        // Edit before the first count finishes: that count is now stale.
        let doc = try XCTUnwrap(c.editorView.document)
        doc.moveCursors(motion: .docEnd, extend: false)
        c.editorView.insertText("ab", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        waitForCount(c) { $0 == "2 found" }
        // Give any stale task time to (wrongly) land, then check again.
        RunLoop.current.run(until: Date().addingTimeInterval(0.5))
        XCTAssertEqual(c.findBar.countLabel.stringValue, "2 found")
        XCTAssertFalse(c.isCountingFind)
    }

    func testClosingTheBarCancelsTheCount() throws {
        let text = String(repeating: "ab ab ab ab\n", count: 200_000)
        let url = try tempFile(text)
        defer { try? FileManager.default.removeItem(at: url) }
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.open(url: url)
        c.showFind(nil)
        c.findBar.setPattern("ab")
        XCTAssertTrue(c.isCountingFind)
        var finished = 0
        c.onFindCountFinished = { finished += 1 }
        c.closeFind(nil)
        XCTAssertFalse(c.isCountingFind)
        RunLoop.current.run(until: Date().addingTimeInterval(1))
        XCTAssertEqual(finished, 0, "a cancelled count never updates the bar")
    }

    func testClicksOnACutLineStayInsideTheShownText() throws {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        c.newDocument(nil)
        let doc = try XCTUnwrap(c.editorView.document)
        doc.edit(command: .insertText(text: String(repeating: "y", count: 12_000)))
        c.editorView.didChangeDocument()

        let snap = doc.viewport(firstLine: 0, count: 1)
        XCTAssertTrue(snap.lines[0].truncated)
        let pos = c.editorView.textPosition(at: NSPoint(x: 1_000_000, y: 2))
        XCTAssertEqual(pos.colUtf16, 10_000, "clicking far right lands at the cut")
        c.editorView.display()
        XCTAssertFalse(c.editorView.lastDrawnLines.isEmpty)
    }
}
