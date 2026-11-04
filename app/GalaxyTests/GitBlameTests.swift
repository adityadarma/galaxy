import XCTest
@testable import Galaxy

/// Inline git blame: who last changed the caret line, shown after its text.
@MainActor
final class GitBlameTests: XCTestCase {
    func testInlineBlameUsesDimExplicitColor() {
        let color = EditorView.blameTextColor.usingColorSpace(.sRGB)
        XCTAssertNotNil(color)
        XCTAssertLessThan(color?.alphaComponent ?? 1, 0.7)
    }

    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-blame-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try git("init", "-b", "main")
        try "one\ntwo\n".write(to: root.appendingPathComponent("f.txt"), atomically: true, encoding: .utf8)
        try git("add", ".")
        try commit(as: "Alice", "first commit")
        try "one\nTWO\nthree\n".write(to: root.appendingPathComponent("f.txt"), atomically: true, encoding: .utf8)
        try git("add", ".")
        try commit(as: "Bob", "shout two")
        try "untracked\n".write(to: root.appendingPathComponent("new.txt"), atomically: true, encoding: .utf8)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    nonisolated private func git(_ args: String...) throws {
        try runGit(args)
    }

    nonisolated private func commit(as name: String, _ message: String) throws {
        try runGit(["-c", "user.name=\(name)", "-c", "user.email=\(name.lowercased())@example.com",
                    "commit", "-m", message])
    }

    /// `git diff --cached` of one file, empty when git fails.
    private func stagedDiff(_ path: String) -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["git", "-C", root.path, "diff", "--cached", "--unified=0", "--", path]
        let out = Pipe()
        process.standardOutput = out
        process.standardError = FileHandle.nullDevice
        guard (try? process.run()) != nil else { return "" }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return String(decoding: data, as: UTF8.self)
    }

    nonisolated private func runGit(_ args: [String]) throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["git", "-C", root.path] + args
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()
        process.waitUntilExit()
        XCTAssertEqual(process.terminationStatus, 0, "git \(args.joined(separator: " "))")
    }

    private func makeController() throws -> MainWindowController {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        c.askSaveChoice = { _ in .discard }
        try c.openWorkspace(at: root)
        return c
    }

    private func waitFor(_ condition: () -> Bool, timeout: TimeInterval = 8) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.02))
        }
    }

    private func open(_ c: MainWindowController, _ name: String) throws {
        try c.openFile(at: root.appendingPathComponent(name))
    }

    // MARK: Formatting

    func testAnnotationNamesAuthorAgeAndMessage() {
        let now = Date()
        let commit = BlameCommitInfo(
            sha: String(repeating: "a", count: 40), author: "Putu Adi", email: "p@example.com",
            time: Int64(now.addingTimeInterval(-3 * 86_400).timeIntervalSince1970),
            summary: "update sweetalert", uncommitted: false)
        let text = BlameFormat.annotation(for: commit, now: now)
        XCTAssertTrue(text.hasPrefix("Putu Adi, "), text)
        XCTAssertTrue(text.contains("3 days ago"), text)
        XCTAssertTrue(text.hasSuffix(" • update sweetalert"), text)
    }

    func testUncommittedAndFreshLinesReadPlainly() {
        let now = Date()
        var commit = BlameCommitInfo(sha: String(repeating: "0", count: 40), author: "Not Committed Yet",
                                     email: "", time: 0, summary: "", uncommitted: true)
        XCTAssertEqual(BlameFormat.annotation(for: commit, now: now), "You • Uncommitted changes")
        commit = BlameCommitInfo(sha: String(repeating: "b", count: 40), author: "Bob", email: "",
                                 time: Int64(now.timeIntervalSince1970), summary: "", uncommitted: false)
        XCTAssertEqual(BlameFormat.annotation(for: commit, now: now), "Bob, just now")
    }

    func testAbsoluteDateIsGMT() {
        XCTAssertEqual(BlameFormat.absolute(0), "Thu, 01 Jan 1970 00:00:00 GMT")
    }

    // MARK: Annotation

    func testCaretLineShowsItsAuthor() throws {
        let c = try makeController()
        try open(c, "f.txt")
        waitFor { c.editorView.blameAnnotation != nil }
        let first = try XCTUnwrap(c.editorView.blameAnnotation)
        XCTAssertEqual(first.line, 0)
        XCTAssertTrue(first.text.hasPrefix("Alice, "), first.text)
        XCTAssertTrue(first.text.hasSuffix("first commit"), first.text)

        // Moving the caret to line 2 shows the commit that changed it.
        c.activeTab?.doc.goToLine(line: 1, col: 0)
        c.editorView.didChangeSelection()
        c.updateBlameAnnotation()
        let second = try XCTUnwrap(c.editorView.blameAnnotation)
        XCTAssertEqual(second.line, 1)
        XCTAssertTrue(second.text.hasPrefix("Bob, "), second.text)
        XCTAssertTrue(second.text.hasSuffix("shout two"), second.text)
    }

    func testClickingAnotherLineMovesTheAnnotation() throws {
        let c = try makeController()
        try open(c, "f.txt")
        waitFor { c.editorView.blameAnnotation != nil }
        XCTAssertEqual(c.editorView.blameAnnotation?.line, 0)

        // Same path as a mouse click: set the caret, then reloadDocument.
        c.activeTab?.doc.setCaret(pos: TextPos(line: 1, colUtf16: 0))
        c.editorView.reloadDocument()
        waitFor { c.editorView.blameAnnotation?.line == 1 }
        XCTAssertEqual(c.editorView.blameAnnotation?.line, 1)
    }

    func testUntrackedFilesHaveNoAnnotation() throws {
        let c = try makeController()
        try open(c, "new.txt")
        RunLoop.current.run(until: Date().addingTimeInterval(0.6))
        XCTAssertNil(c.editorView.blameAnnotation)
    }

    func testToggleHidesAndRestoresIt() throws {
        let c = try makeController()
        try open(c, "f.txt")
        waitFor { c.editorView.blameAnnotation != nil }
        XCTAssertTrue(c.gitBlameEnabled)

        c.toggleGitBlame(nil)
        XCTAssertFalse(c.gitBlameEnabled)
        XCTAssertNil(c.editorView.blameAnnotation)

        c.toggleGitBlame(nil)
        waitFor { c.editorView.blameAnnotation != nil }
        XCTAssertNotNil(c.editorView.blameAnnotation)
    }

    func testUnsavedEditShowsAsUncommittedOnceBlameRefreshes() throws {
        let c = try makeController()
        try open(c, "f.txt")
        waitFor { c.editorView.blameAnnotation != nil }

        c.editorView.insertText("X", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        waitFor { c.editorView.blameAnnotation?.text == "You • Uncommitted changes" }
        XCTAssertEqual(c.editorView.blameAnnotation?.text, "You • Uncommitted changes")
    }

    func testAddingALineHidesItUntilBlameCatchesUp() throws {
        let c = try makeController()
        try open(c, "f.txt")
        waitFor { c.editorView.blameAnnotation != nil }

        c.editorView.insertText("\n", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        XCTAssertNil(c.editorView.blameAnnotation, "line numbers moved; the old blame no longer lines up")
        waitFor { c.editorView.blameAnnotation != nil }
        XCTAssertNotNil(c.editorView.blameAnnotation)
    }

    func testOpenInlineHunkFollowsEditsAndClosesWhenChangeIsUndone() throws {
        let c = try makeController()
        try open(c, "f.txt")
        let doc = try XCTUnwrap(c.activeTab?.doc)
        let original = doc.text()

        c.editorView.insertText("X", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        c.toggleInlineGitHunk(at: 0)
        waitFor { c.editorView.inlineGitHunk != nil }
        XCTAssertEqual(c.editorView.inlineGitHunk?.oldLines, ["one"])
        XCTAssertEqual(c.editorView.inlineGitHunk?.newLines, ["Xone"])

        c.editorView.insertText("Y", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        waitFor { c.editorView.inlineGitHunk?.newLines == ["XYone"] }
        XCTAssertEqual(c.editorView.inlineGitHunk?.newLines, ["XYone"], "the open hunk shows the edited text")

        while doc.text() != original, doc.undo() {}
        XCTAssertEqual(doc.text(), original)
        c.refreshGitDiff()
        waitFor { c.editorView.inlineGitHunk == nil }
        XCTAssertNil(c.editorView.inlineGitHunk, "no change left, so the hunk closes")
    }

    func testInlineHunkRowsCarrySyntaxStyles() throws {
        let url = root.appendingPathComponent("code.rs")
        try "fn a() {}\nfn b() { 1 }\nfn c() {}\n".write(to: url, atomically: true, encoding: .utf8)
        try git("add", ".")
        try commit(as: "Alice", "add code")
        try "fn a() {}\nfn b() { 2 }\nfn c() {}\n".write(to: url, atomically: true, encoding: .utf8)

        let c = try makeController()
        try open(c, "code.rs")
        c.toggleInlineGitHunk(at: 1)
        waitFor { c.editorView.inlineGitHunk != nil }
        let hunk = try XCTUnwrap(c.editorView.inlineGitHunk)

        for kind in [EditorView.InlineGitHunk.Kind.context, .removed, .added] {
            let rows = hunk.rows.filter { $0.kind == kind }
            XCTAssertFalse(rows.isEmpty, "\(kind) rows")
            XCTAssertTrue(rows.allSatisfy { !$0.styles.isEmpty }, "\(kind) rows are highlighted: \(rows)")
        }
    }

    func testHeaderButtonHitReachesTheActionCallback() throws {
        let c = try makeController()
        try open(c, "f.txt")
        c.editorView.insertText("X", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        c.toggleInlineGitHunk(at: 0)
        waitFor { c.editorView.inlineGitHunk != nil }
        let hunk = try XCTUnwrap(c.editorView.inlineGitHunk)

        let top = c.editorView.inlineHunkTop(hunk)
        let close = try XCTUnwrap(c.editorView.inlineHunkButtonRects(top: top).first { $0.action == .close }).rect
        let window = try XCTUnwrap(c.window)
        let windowPoint = c.editorView.convert(NSPoint(x: close.midX, y: close.midY), to: nil)
        let contentPoint = window.contentView!.convert(windowPoint, from: nil)
        XCTAssertTrue(
            window.contentView!.hitTest(contentPoint) === c.editorView,
            "the minimap must not receive header button clicks"
        )
        let action = expectation(description: "header action")
        c.editorView.onInlineHunkAction = { received in
            XCTAssertEqual(received, .close)
            action.fulfill()
        }
        let event = try XCTUnwrap(NSEvent.mouseEvent(
            with: .leftMouseDown, location: windowPoint, modifierFlags: [],
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
            context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        c.editorView.mouseDown(with: event)
        wait(for: [action], timeout: 1)
    }

    func testStageButtonStagesOnlyTheOpenHunk() throws {
        let url = root.appendingPathComponent("lines.txt")
        let original = (1...12).map { "line \($0)" }.joined(separator: "\n") + "\n"
        try original.write(to: url, atomically: true, encoding: .utf8)
        try git("add", ".")
        try commit(as: "Alice", "add lines")
        var edited = original.components(separatedBy: "\n")
        edited[1] = "second changed"
        edited[9] = "tenth changed"
        try edited.joined(separator: "\n").write(to: url, atomically: true, encoding: .utf8)

        let c = try makeController()
        try open(c, "lines.txt")
        c.toggleInlineGitHunk(at: 1)
        waitFor { c.editorView.inlineGitHunk != nil }
        let hunk = try XCTUnwrap(c.editorView.inlineGitHunk)
        XCTAssertEqual(hunk.total, 2)
        XCTAssertEqual(hunk.index, 0)

        c.handleInlineHunkAction(.stage)
        var diff = ""
        waitFor {
            diff = stagedDiff("lines.txt")
            return diff.contains("+second changed")
        }
        XCTAssertTrue(diff.contains("+second changed"), diff)
        XCTAssertFalse(diff.contains("tenth changed"), "the other hunk stays unstaged: \(diff)")
    }

    func testRevertButtonRestoresTheHeadLines() throws {
        let c = try makeController()
        try open(c, "f.txt")
        let doc = try XCTUnwrap(c.activeTab?.doc)
        let original = doc.text()

        // A changed line comes back as HEAD had it.
        c.editorView.insertText("X", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        c.toggleInlineGitHunk(at: 0)
        waitFor { c.editorView.inlineGitHunk != nil }
        let hunk = try XCTUnwrap(c.editorView.inlineGitHunk)
        XCTAssertEqual(hunk.total, 1)
        XCTAssertEqual(hunk.fileName, "f.txt")
        XCTAssertTrue(hunk.rows.contains { $0.kind == .context }, "unchanged neighbours are shown around the change")
        c.handleInlineHunkAction(.revert)
        XCTAssertEqual(doc.text(), original)
        // The hunk is gone with its change; the refresh closes the zone.
        waitFor { c.editorView.inlineGitHunk == nil }
        XCTAssertNil(c.editorView.inlineGitHunk)

        // An added line is removed again.
        c.editorView.insertText("\n", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        XCTAssertNotEqual(doc.text(), original)
        c.toggleInlineGitHunk(at: 1)
        waitFor { c.editorView.inlineGitHunk != nil }
        XCTAssertNotNil(c.editorView.inlineGitHunk)
        c.handleInlineHunkAction(.revert)
        XCTAssertEqual(doc.text(), original)
    }

    func testAnnotationSitsAfterTheLineText() throws {
        let c = try makeController()
        try open(c, "f.txt")
        waitFor { c.editorView.blameAnnotation != nil }
        let rect = try XCTUnwrap(c.editorView.blameAnnotationRect())
        let textEnd = c.editorView.textOriginX + c.editorView.xOffset(in: "one", column: 3)
        XCTAssertGreaterThan(rect.minX, textEnd)
        XCTAssertEqual(rect.minY, 0)
    }

    // MARK: Card

    func testCardShowsCommitDetailsAndCopiesTheSha() throws {
        let pasteboard = NSPasteboard(name: NSPasteboard.Name("galaxy-blame-test-\(UUID().uuidString)"))
        let card = BlamePopover()
        card.pasteboard = pasteboard
        let sha = "d604728" + String(repeating: "1", count: 33)
        card.show(BlameCommitInfo(sha: sha, author: "Aditya Darma", email: "a@example.com",
                                  time: 1_791_444_505, summary: "Initial commit", uncommitted: false))
        XCTAssertEqual(card.accessibilityValue() as? String, "Aditya Darma, Initial commit")
        XCTAssertGreaterThan(card.fittingSize().height, 80)

        card.copySha(nil)
        XCTAssertEqual(pasteboard.string(forType: .string), sha)
        pasteboard.releaseGlobally()
    }

    func testHoveringTheAnnotationOpensTheCard() throws {
        let c = try makeController()
        try open(c, "f.txt")
        waitFor { c.editorView.blameAnnotation != nil }
        let rect = try XCTUnwrap(c.editorView.blameAnnotationRect())
        XCTAssertTrue(c.blamePopover.isHidden)

        c.editorView.lastMousePoint = NSPoint(x: rect.midX, y: rect.midY)
        c.editorView.onHover?(TextPos(line: 0, colUtf16: 0), c.editorView.lastMousePoint!)
        waitFor { !c.blamePopover.isHidden }
        XCTAssertFalse(c.blamePopover.isHidden)
        XCTAssertEqual(c.blamePopover.sha.count, 40)

        // Away from the annotation, the card closes after its grace period.
        c.editorView.onHover?(TextPos(line: 0, colUtf16: 0), NSPoint(x: c.editorView.textOriginX + 2, y: rect.midY))
        waitFor { c.blamePopover.isHidden }
        XCTAssertTrue(c.blamePopover.isHidden)
    }

    func testBriefAnnotationHoverDoesNotOpenTheCard() throws {
        let c = try makeController()
        try open(c, "f.txt")
        waitFor { c.editorView.blameAnnotation != nil }
        let rect = try XCTUnwrap(c.editorView.blameAnnotationRect())

        c.editorView.lastMousePoint = NSPoint(x: rect.midX, y: rect.midY)
        c.editorView.onHover?(TextPos(line: 0, colUtf16: 0), c.editorView.lastMousePoint!)
        c.editorView.lastMousePoint = NSPoint(x: c.editorView.textOriginX + 2, y: rect.midY)
        c.editorView.onHover?(TextPos(line: 0, colUtf16: 0), c.editorView.lastMousePoint!)
        RunLoop.current.run(until: Date().addingTimeInterval(1.25))

        XCTAssertTrue(c.blamePopover.isHidden)
    }

    func testLeavingAnnotationDismissesTheCardPromptly() throws {
        let c = try makeController()
        try open(c, "f.txt")
        waitFor { c.editorView.blameAnnotation != nil }
        let rect = try XCTUnwrap(c.editorView.blameAnnotationRect())

        c.editorView.onHover?(TextPos(line: 0, colUtf16: 0), NSPoint(x: rect.midX, y: rect.midY))
        c.editorView.onHover?(TextPos(line: 0, colUtf16: 0), NSPoint(x: c.editorView.textOriginX + 2, y: rect.midY))
        RunLoop.current.run(until: Date().addingTimeInterval(0.5))

        XCTAssertTrue(c.blamePopover.isHidden)
    }
}
