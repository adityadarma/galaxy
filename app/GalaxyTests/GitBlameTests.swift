import XCTest
@testable import Galaxy

/// Inline git blame: who last changed the caret line, shown after its text.
@MainActor
final class GitBlameTests: XCTestCase {
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
