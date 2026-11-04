import XCTest
@testable import Galaxy

/// M2-05: rename in place, delete to Trash, right-click menu.
@MainActor
final class ExplorerFileOpsTests: XCTestCase {
    // Set in setUp, which XCTest calls outside the main actor.
    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-ops-\(UUID().uuidString)")
        let fm = FileManager.default
        try fm.createDirectory(at: root.appendingPathComponent("src/net"), withIntermediateDirectories: true)
        for (name, text) in [("src/main.rs", "fn main() {}\n"), ("src/net/tcp.rs", "tcp\n"), ("README.md", "# hi\n")] {
            try text.write(to: root.appendingPathComponent(name), atomically: true, encoding: .utf8)
        }
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    private func makeController() throws -> MainWindowController {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        c.askSaveChoice = { _ in XCTFail("unexpected save prompt"); return .cancel }
        c.askConfirmDelete = { _, _ in XCTFail("unexpected delete prompt"); return false }
        // Never touch the real Trash from tests.
        c.moveToTrash = { try FileManager.default.removeItem(at: $0) }
        try c.openWorkspace(at: root)
        return c
    }

    private func exists(_ rel: String) -> Bool {
        FileManager.default.fileExists(atPath: root.appendingPathComponent(rel).path)
    }

    /// Types `name` into the rename field and presses Return or Escape.
    private func finishTyping(_ c: MainWindowController, _ name: String, cancel: Bool = false) throws {
        let field = try XCTUnwrap(c.explorer.renameFieldForTesting)
        let editor = try XCTUnwrap(field.currentEditor() as? NSTextView, "the field is being edited")
        editor.string = name
        editor.doCommand(by: cancel ? #selector(NSResponder.cancelOperation(_:)) : #selector(NSResponder.insertNewline(_:)))
    }

    func testRenamingInPlaceSelectsTheStemAndFollowsOpenTabs() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: root.appendingPathComponent("src/main.rs"))
        c.editorView.insertText("// ", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()

        c.explorer.reveal(relPath: "src/main.rs")
        c.renameExplorerItem(nil)
        let field = try XCTUnwrap(c.explorer.renameFieldForTesting)
        XCTAssertEqual(field.currentEditor()?.selectedRange, NSRange(location: 0, length: 4), "\"main\" without .rs")

        try finishTyping(c, "app.rs")
        XCTAssertNil(c.explorer.renamingEntry)
        XCTAssertTrue(exists("src/app.rs"))
        XCTAssertFalse(exists("src/main.rs"))
        XCTAssertEqual(c.explorer.selectedEntry?.relPath, "src/app.rs")
        let tab = try XCTUnwrap(c.activeTab)
        XCTAssertEqual(tab.title, "app.rs")
        XCTAssertEqual(tab.relPath, "src/app.rs")
        XCTAssertTrue(tab.isEdited, "edits kept")
        XCTAssertTrue(c.save())
        XCTAssertEqual(try String(contentsOf: root.appendingPathComponent("src/app.rs"), encoding: .utf8),
                       "// fn main() {}\n", "saved to the new name")
        XCTAssertFalse(exists("src/main.rs"), "not recreated under the old name")
    }

    func testRenamingAFolderMovesTheTabsInside() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: root.appendingPathComponent("src/net/tcp.rs"))
        XCTAssertTrue(c.explorer.beginRename(relPath: "src"))
        XCTAssertEqual(c.explorer.renameFieldForTesting?.currentEditor()?.selectedRange,
                       NSRange(location: 0, length: 3), "folders: whole name")
        try finishTyping(c, "lib")
        XCTAssertTrue(exists("lib/net/tcp.rs"))
        XCTAssertEqual(c.activeTab?.relPath, "lib/net/tcp.rs")
        XCTAssertEqual(c.activeTab?.pathComponents, ["lib", "net", "tcp.rs"])
        XCTAssertEqual(c.editorView.document?.path(), root.appendingPathComponent("lib/net/tcp.rs").path)
    }

    func testEscapeCancelsAndAnExistingNameIsRefused() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        XCTAssertTrue(c.explorer.beginRename(relPath: "README.md"))
        try finishTyping(c, "other.md", cancel: true)
        XCTAssertTrue(exists("README.md"))
        XCTAssertFalse(exists("other.md"))

        // Same folder, taken name: nothing moves, nothing is overwritten.
        try "keep".write(to: root.appendingPathComponent("NOTES.md"), atomically: true, encoding: .utf8)
        c.explorer.refresh()
        let entry = FileEntry(name: "README.md", relPath: "README.md", isDir: false)
        c.renameEntry(entry, to: "NOTES.md")
        XCTAssertEqual(try String(contentsOf: root.appendingPathComponent("NOTES.md"), encoding: .utf8), "keep")
        XCTAssertTrue(exists("README.md"))
        c.window?.attachedSheet.map { c.window?.endSheet($0) }
    }

    func testNewFileAndFolderUseInlineNames() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        c.explorer.reveal(relPath: "src")

        c.explorerNewFile(nil)
        XCTAssertEqual(c.explorer.renameFieldForTesting?.currentEditor()?.selectedRange,
                       NSRange(location: 0, length: 0))
        XCTAssertEqual(c.explorer.renameFieldForTesting?.stringValue, "")
        try finishTyping(c, "new.rs")
        XCTAssertTrue(exists("src/new.rs"))

        c.explorerNewFolder(nil)
        try finishTyping(c, "generated")
        XCTAssertTrue(exists("src/generated"))
    }

    func testNewExplorerItemSurvivesAnEmptyFocusLoss() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        c.explorer.reveal(relPath: "src")

        c.explorerNewFile(nil)
        let entry = try XCTUnwrap(c.explorer.renamingEntry)
        let field = try XCTUnwrap(c.explorer.renameFieldForTesting)
        c.explorer.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: field))

        XCTAssertFalse(exists(entry.relPath), "an empty new-file field remains virtual until a name is confirmed")

        let resumed = expectation(description: "empty name field resumes")
        DispatchQueue.main.async {
            XCTAssertEqual(c.explorer.renamingEntry?.relPath, entry.relPath)
            XCTAssertEqual(c.explorer.renameFieldForTesting?.stringValue, "")
            resumed.fulfill()
        }
        wait(for: [resumed], timeout: 1)
    }

    func testDeletingAsksThenClosesCleanTabsAndKeepsEditedOnes() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: root.appendingPathComponent("src/main.rs"))
        try c.openFile(at: root.appendingPathComponent("src/net/tcp.rs"))
        c.editorView.insertText("x", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        try c.openFile(at: root.appendingPathComponent("README.md"))

        var asked: [String] = []
        c.askConfirmDelete = { name, folder in asked.append("\(name) \(folder)"); return false }
        c.explorer.reveal(relPath: "src")
        c.deleteExplorerItem(nil)
        XCTAssertEqual(asked, ["src true"])
        XCTAssertTrue(exists("src"), "cancel deletes nothing")

        c.askConfirmDelete = { _, _ in true }
        c.deleteExplorerItem(nil)
        XCTAssertFalse(exists("src"))
        XCTAssertEqual(c.tabBar.items.map(\.title), ["tcp.rs (deleted)", "README.md"],
                       "the clean tab closed; the edited one stays with its text")
        XCTAssertTrue(c.tabs[0].isEdited)
        XCTAssertFalse(c.explorer.outlineView.numberOfRows > 1 &&
                       (0..<c.explorer.outlineView.numberOfRows).contains {
                           (c.explorer.outlineView.item(atRow: $0) as? ExplorerNode)?.entry.name == "src"
                       }, "gone from the tree")
    }

    func testRightClickSelectsTheRowAndOffersItsActions() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        let readme = try XCTUnwrap((0..<c.explorer.outlineView.numberOfRows).first {
            (c.explorer.outlineView.item(atRow: $0) as? ExplorerNode)?.entry.name == "README.md"
        })
        let menu = try XCTUnwrap(c.explorer.menu(forRow: readme))
        XCTAssertEqual(c.explorer.selectedEntry?.relPath, "README.md")
        XCTAssertNil(c.activeTab, "right click does not open the file")
        XCTAssertEqual(menu.items.filter { !$0.isSeparatorItem }.map(\.title),
                       ["New File…", "New Folder…", "Reveal in Finder", "Copy", "Paste", "Copy Path", "Copy Relative Path",
                        "Rename…", "Move to Trash"])
        for item in menu.items where !item.isSeparatorItem {
            XCTAssertTrue(c.validateMenuItem(item), item.title)
        }

        // Below the rows: the root, so no rename or delete.
        let empty = try XCTUnwrap(c.explorer.menu(forRow: -1))
        XCTAssertNil(c.explorer.selectedEntry)
        XCTAssertFalse(empty.items.contains { $0.title == "Rename…" })

        let pasteboard = NSPasteboard.general
        let saved = pasteboard.string(forType: .string)
        defer { if let saved { pasteboard.clearContents(); pasteboard.setString(saved, forType: .string) } }
        c.explorer.reveal(relPath: "src/main.rs")
        c.copyExplorerRelativePath(nil)
        XCTAssertEqual(pasteboard.string(forType: .string), "src/main.rs")
    }

    func testCopyingExplorerFilePastesIntoAnotherWindow() throws {
        let source = try makeController()
        defer { source.window?.orderOut(nil) }
        let destinationRoot = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-destination-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: destinationRoot, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: destinationRoot) }
        let destination = MainWindowController(bridge: CoreBridge())
        destination.showWindow(nil)
        defer { destination.window?.orderOut(nil) }
        try destination.openWorkspace(at: destinationRoot)

        source.explorer.reveal(relPath: "src/net")
        source.copyExplorerItem(nil)
        destination.pasteExplorerItem(nil)

        let copied = destinationRoot.appendingPathComponent("net/tcp.rs")
        XCTAssertEqual(try String(contentsOf: copied, encoding: .utf8), "tcp\n")
    }
}
