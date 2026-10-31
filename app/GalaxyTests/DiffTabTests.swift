import XCTest
@testable import Galaxy

/// Source Control opens a changed file as a diff: the committed text on the
/// left (read-only), the working copy on the right.
@MainActor
final class DiffTabTests: XCTestCase {
    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-diff-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try git("init", "-b", "main")
        try "one\ntwo\nthree\nfour\n".write(to: root.appendingPathComponent("tracked.txt"), atomically: true, encoding: .utf8)
        try "keep\n".write(to: root.appendingPathComponent("other.txt"), atomically: true, encoding: .utf8)
        try git("add", ".")
        try git("-c", "user.name=T", "-c", "user.email=t@example.com", "commit", "-m", "initial")
        try "one\nTWO\nthree\nfour\nfive\n".write(to: root.appendingPathComponent("tracked.txt"), atomically: true, encoding: .utf8)
        try "new\n".write(to: root.appendingPathComponent("untracked.txt"), atomically: true, encoding: .utf8)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    nonisolated private func git(_ args: String...) throws {
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

    private func waitForDiff(_ c: MainWindowController) {
        let deadline = Date().addingTimeInterval(5)
        while c.diffMap.hunks.isEmpty, Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.02))
        }
    }

    // MARK: Line mapping

    func testLineMapFollowsInsertionsAndDeletionsBothWays() {
        // Old lines 2-3 were replaced by one line; one line was added after old line 6.
        let map = DiffLineMap(hunks: [
            .init(oldStart: 2, oldLen: 2, newStart: 2, newLen: 1),
            .init(oldStart: 7, oldLen: 0, newStart: 6, newLen: 2),
        ])
        XCTAssertEqual(map.oldLine(forNew: 0), 0)
        XCTAssertEqual(map.oldLine(forNew: 2), 2, "inside a change: the change's first old line")
        XCTAssertEqual(map.oldLine(forNew: 3), 4, "after a 2-for-1 replacement the old side is one ahead")
        XCTAssertEqual(map.newLine(forOld: 4), 3)
        XCTAssertEqual(map.newLine(forOld: 3), 2, "inside a change: the change's first new line")
        XCTAssertEqual(map.oldLine(forNew: 9), 8, "the new side is net one line longer, so old runs one behind")
        XCTAssertEqual(map.newLine(forOld: 8), 9)
        XCTAssertEqual(map.oldChangedLines, [2, 3])
        XCTAssertEqual(map.newChangedLines, [2, 6, 7])
        XCTAssertEqual(DiffLineMap().oldLine(forNew: 5), 5)
    }

    // MARK: Opening

    func testChangedFileOpensCommittedTextBesideTheWorkingCopy() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        let ws = try XCTUnwrap(c.workspace)
        c.openWorkspaceDiff(ws, relPath: "tracked.txt")
        waitForDiff(c)

        let tab = try XCTUnwrap(c.activeTab)
        XCTAssertEqual(c.tabBar.items.map(\.title), ["tracked.txt (Working Tree)"])
        XCTAssertTrue(c.isDiffPaneShown)
        XCTAssertEqual(c.secondaryEditorView.document?.text(), "one\ntwo\nthree\nfour\n", "left pane is HEAD")
        XCTAssertEqual(c.editorView.document?.text(), "one\nTWO\nthree\nfour\nfive\n", "right pane is the working copy")
        XCTAssertNotNil(tab.diffBase)
        XCTAssertEqual(c.diffMap.oldChangedLines, [1])
        XCTAssertEqual(c.diffMap.newChangedLines, [1, 4])
        XCTAssertEqual(c.secondaryEditorView.lineTints.keys.sorted(), [1])
        XCTAssertEqual(c.editorView.lineTints.keys.sorted(), [1, 4])
    }

    func testCommittedSideIsReadOnlyAndTheWorkingSideStaysEditable() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        c.openWorkspaceDiff(try XCTUnwrap(c.workspace), relPath: "tracked.txt")
        waitForDiff(c)

        let none = NSRange(location: NSNotFound, length: 0)
        c.secondaryEditorView.insertText("X", replacementRange: none)
        XCTAssertEqual(c.secondaryEditorView.document?.text(), "one\ntwo\nthree\nfour\n", "typing is ignored")
        XCTAssertFalse(c.secondaryEditorView.document?.isModified() ?? true)

        c.editorView.insertText("Y", replacementRange: none)
        XCTAssertTrue(c.editorView.document?.text().contains("Y") ?? false, "the working copy still edits")
        XCTAssertTrue(c.activeTab?.doc.isModified() ?? false)
    }

    func testFilesWithoutACommittedVersionOpenAsPlainTabs() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        c.openWorkspaceDiff(try XCTUnwrap(c.workspace), relPath: "untracked.txt")
        XCTAssertEqual(c.tabBar.items.map(\.title), ["untracked.txt"])
        XCTAssertFalse(c.isDiffPaneShown)
        XCTAssertNil(c.activeTab?.diffBase)
    }

    func testOtherTabsGoBackToOnePaneAndTheDiffComesBack() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        let ws = try XCTUnwrap(c.workspace)
        c.openWorkspaceDiff(ws, relPath: "tracked.txt")
        waitForDiff(c)
        c.openWorkspaceFile(ws, FileEntry(name: "other.txt", relPath: "other.txt", isDir: false), preview: false)
        XCTAssertFalse(c.isDiffPaneShown, "a normal file shows a single pane")
        XCTAssertTrue(c.editorView.lineTints.isEmpty)
        XCTAssertNil(c.secondaryEditorView.document)

        c.selectTab(0)
        XCTAssertTrue(c.isDiffPaneShown, "back on the diff tab")
        XCTAssertEqual(c.secondaryEditorView.document?.text(), "one\ntwo\nthree\nfour\n")
    }

    func testAnAlreadyOpenFileKeepsItsTabWhenOpeningTheDiff() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        let ws = try XCTUnwrap(c.workspace)
        c.openWorkspaceFile(ws, FileEntry(name: "tracked.txt", relPath: "tracked.txt", isDir: false), preview: false)
        XCTAssertFalse(c.isDiffPaneShown)
        c.openWorkspaceDiff(ws, relPath: "tracked.txt")
        XCTAssertEqual(c.tabBar.items.count, 2, "the diff has its own tab")
        XCTAssertTrue(c.isDiffPaneShown)
    }

    func testBothPanesAreVisibleSideBySide() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        c.openWorkspaceDiff(try XCTUnwrap(c.workspace), relPath: "tracked.txt")
        waitForDiff(c)
        c.window?.contentView?.layoutSubtreeIfNeeded()
        let left = try XCTUnwrap(c.secondaryEditorView.enclosingScrollView)
        let right = try XCTUnwrap(c.editorView.enclosingScrollView)
        let split = try XCTUnwrap(right.superview as? NSSplitView)
        XCTAssertTrue(left.superview === split, "both panes live in the same split view")
        XCTAssertFalse(left.isHidden)
        XCTAssertGreaterThan(left.frame.width, 100, "left pane width \(left.frame)")
        XCTAssertGreaterThan(right.frame.width, 100, "right pane width \(right.frame)")
        XCTAssertLessThanOrEqual(left.frame.maxX, right.frame.minX + 1, "left of right: \(left.frame) \(right.frame)")
        XCTAssertGreaterThan(left.frame.height, 50, "left pane height \(left.frame)")
    }
}
