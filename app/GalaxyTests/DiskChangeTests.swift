import XCTest
@testable import Galaxy

/// M2-03: files changed or deleted by another program while open.
@MainActor
final class DiskChangeTests: XCTestCase {
    // Set in setUp, which XCTest calls outside the main actor.
    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-disk-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        for name in ["a.txt", "b.txt"] {
            try "\(name) line 1\nline 2\n".write(to: root.appendingPathComponent(name), atomically: true, encoding: .utf8)
        }
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    private func makeController() -> MainWindowController {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        c.promptsOnlyWhenKey = false
        c.askSaveChoice = { _ in XCTFail("unexpected save prompt"); return .cancel }
        c.askDiskConflict = { _, _ in XCTFail("unexpected conflict prompt"); return .keepMine }
        return c
    }

    private func file(_ name: String) -> URL { root.appendingPathComponent(name) }

    /// Writes as another program would, with a later mtime so a change
    /// within one clock tick is still seen by the size and time check.
    private func writeExternally(_ name: String, _ text: String) throws {
        try text.write(to: file(name), atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.modificationDate: Date().addingTimeInterval(3)],
                                              ofItemAtPath: file(name).path)
    }

    private func text(_ doc: DocumentHandle?) -> String {
        guard let doc else { return "" }
        return doc.lines(first: 0, count: doc.lineCount()).joined(separator: "\n")
    }

    private func type(_ c: MainWindowController, _ s: String) {
        c.editorView.insertText(s, replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
    }

    func testAnUneditedFileReloadsAndTheReloadCanBeUndone() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        let a = c.tabs[0].doc

        try writeExternally("a.txt", "changed\nline 2\n")
        try writeExternally("b.txt", "b changed\n")
        c.checkOpenFiles()
        XCTAssertEqual(text(a), "changed\nline 2\n", "background tab reloaded too")
        XCTAssertEqual(text(c.editorView.document), "b changed\n")
        XCTAssertFalse(c.tabs.contains { $0.isEdited })
        XCTAssertTrue(c.statusLabel.stringValue.contains("reloaded from disk"))

        _ = c.editorView.document?.undo()
        XCTAssertEqual(text(c.editorView.document), "b.txt line 1\nline 2\n")
    }

    func testAnEditedFileAsksAndKeepingMineLeavesItUnsaved() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        type(c, "mine ")
        var asked: [String] = []
        c.askDiskConflict = { name, saving in asked.append("\(name) \(saving)"); return .keepMine }

        try writeExternally("a.txt", "theirs\n")
        c.checkOpenFiles()
        XCTAssertEqual(asked, ["a.txt false"])
        XCTAssertTrue(text(c.editorView.document).hasPrefix("mine "))
        XCTAssertTrue(c.tabs[0].isEdited)
        c.checkOpenFiles()
        XCTAssertEqual(asked.count, 1, "asked once per change")

        // Changed again, then saved: Save asks before overwriting.
        try writeExternally("a.txt", "theirs again\n")
        c.askDiskConflict = { name, saving in asked.append("\(name) \(saving)"); return .keepMine }
        XCTAssertTrue(c.save())
        XCTAssertEqual(asked.last, "a.txt true")
        XCTAssertTrue(try String(contentsOf: file("a.txt"), encoding: .utf8).hasPrefix("mine "))
        XCTAssertFalse(c.tabs[0].isEdited)
    }

    func testReloadingDiscardsEditsInOneUndoableStep() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        type(c, "mine ")
        c.askDiskConflict = { _, _ in .reload }
        try writeExternally("a.txt", "theirs\n")
        c.checkOpenFiles()
        XCTAssertEqual(text(c.editorView.document), "theirs\n")
        XCTAssertFalse(c.tabs[0].isEdited)
        _ = c.editorView.document?.undo()
        XCTAssertTrue(text(c.editorView.document).hasPrefix("mine "), "undo brings the edits back")
    }

    func testABackgroundConflictWaitsUntilItsTabIsShown() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        type(c, "mine ")
        try c.openFile(at: file("b.txt"))
        var asked = 0
        c.askDiskConflict = { _, _ in asked += 1; return .keepMine }
        try writeExternally("a.txt", "theirs\n")
        c.checkOpenFiles()
        XCTAssertEqual(asked, 0)
        c.selectTab(0)
        XCTAssertEqual(asked, 1)
    }

    func testADeletedFileKeepsItsTextAndSavingRecreatesIt() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        try FileManager.default.removeItem(at: file("a.txt"))
        c.checkOpenFiles()
        XCTAssertEqual(c.tabBar.items.map(\.title), ["a.txt (deleted)"])
        XCTAssertTrue(c.tabs[0].isEdited, "unsaved: closing asks first")
        XCTAssertEqual(c.tabBar.items.map(\.isEdited), [false], "no typed text: the tab keeps its close button")
        XCTAssertEqual(text(c.editorView.document), "a.txt line 1\nline 2\n")

        XCTAssertTrue(c.save())
        XCTAssertEqual(c.tabBar.items.map(\.title), ["a.txt"])
        XCTAssertFalse(c.tabs[0].isEdited)
        XCTAssertTrue(FileManager.default.fileExists(atPath: file("a.txt").path))
    }

    func testADeletedFileWithTypedTextKeepsTheUnsavedDot() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        type(c, "x")
        try FileManager.default.removeItem(at: file("a.txt"))
        c.checkOpenFiles()
        XCTAssertEqual(c.tabBar.items.map(\.title), ["a.txt (deleted)"])
        XCTAssertEqual(c.tabBar.items.map(\.isEdited), [true])
    }

    func testTheWatcherReloadsOpenFiles() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openWorkspace(at: root)
        try c.openFile(at: file("a.txt"))
        RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        let applied = expectation(description: "watcher batch")
        applied.assertForOverFulfill = false
        c.onWorkspaceChangesApplied = { batch in
            if batch.rescan || batch.paths.contains("a.txt") { applied.fulfill() }
        }
        try writeExternally("a.txt", "from the watcher\n")
        wait(for: [applied], timeout: 5)
        c.onWorkspaceChangesApplied = nil
        XCTAssertEqual(text(c.editorView.document), "from the watcher\n")
    }
}
