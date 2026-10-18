import XCTest
@testable import Galaxy

/// Editor groups: Split Right/Down, one tab list per group, tabs moving
/// between groups, shared buffers and the session.
@MainActor
final class EditorGroupTests: XCTestCase {
    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-groups-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        for name in ["a.txt", "b.txt", "c.txt"] {
            try name.write(to: root.appendingPathComponent(name), atomically: true, encoding: .utf8)
        }
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    private func makeController() -> MainWindowController {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        c.askSaveChoice = { _ in XCTFail("unexpected save prompt"); return .cancel }
        return c
    }

    private func file(_ name: String) -> URL { root.appendingPathComponent(name) }
    private func titles(_ group: EditorGroup) -> [String] { group.tabs.map(\.title) }

    func testSplitRightMakesASecondGroupSharingTheDocument() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        c.splitEditorRight(nil)

        XCTAssertEqual(c.groups.count, 2)
        XCTAssertTrue(c.isEditorSplit)
        XCTAssertEqual(c.activeGroupIndex, 1, "the new group takes focus")
        XCTAssertEqual(titles(c.groups[0]), ["a.txt", "b.txt"])
        XCTAssertEqual(titles(c.groups[1]), ["b.txt"])
        XCTAssertTrue(c.groups[0].tabs[1].doc === c.groups[1].tabs[0].doc, "one buffer for both tabs")
        XCTAssertEqual(c.tabBar.items.map(\.title), ["b.txt"], "the tab bar shows the active group")
        XCTAssertEqual(c.groups[0].pane?.tabBar.items.map(\.title), ["a.txt", "b.txt"])
    }

    func testEditsShowInBothTabsAndClosingOneDoesNotAsk() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        c.splitEditorRight(nil)
        c.editorView.insertText("X", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        c.updateEditedState()
        XCTAssertTrue(c.groups[0].tabs[0].isEdited, "the dirty dot reaches the other group's tab")
        XCTAssertTrue(c.groups[1].tabs[0].isEdited)

        // Another tab still shows the document: closing this one is not a discard.
        XCTAssertTrue(c.closeTab(at: 0))
        XCTAssertEqual(c.groups.count, 1, "an empty group closes")
        XCTAssertEqual(c.tabs.count, 1)
        XCTAssertTrue(c.tabs[0].doc.isModified())
    }

    func testClosingTheLastTabOfAGroupRemovesTheGroup() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        c.splitEditorRight(nil)
        XCTAssertEqual(c.groups.count, 2)
        c.closeTab(nil)
        XCTAssertEqual(c.groups.count, 1)
        XCTAssertEqual(c.activeGroupIndex, 0)
        XCTAssertEqual(c.tabBar.items.map(\.title), ["a.txt"])
    }

    func testClickingAGroupMakesItActive() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        c.splitEditorRight(nil)
        let first = c.groups[0]
        let bar = try XCTUnwrap(first.pane?.tabBar)
        bar.onSelect?(0)

        XCTAssertTrue(c.activeGroup === first)
        XCTAssertEqual(c.activeTab?.title, "a.txt")
        XCTAssertEqual(c.tabBar.items.map(\.title), ["a.txt", "b.txt"])
        XCTAssertEqual(c.groups[1].pane?.tabBar.items.map(\.title), ["b.txt"])
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["a.txt"])
    }

    func testMovingATabToAnotherGroup() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        try c.openFile(at: file("c.txt"))
        c.splitEditorRight(nil)            // group 1: c
        c.focusFirstEditorGroup(nil)
        let tab = c.groups[0].tabs[0]      // a
        c.moveTab(from: c.groups[0], at: 0, to: c.groups[1], at: nil)

        XCTAssertEqual(titles(c.groups[0]), ["b.txt", "c.txt"])
        XCTAssertEqual(titles(c.groups[1]), ["c.txt", "a.txt"])
        XCTAssertTrue(c.activeTab === tab)
        XCTAssertEqual(c.activeGroupIndex, 1)
    }

    func testMovingTheLastTabClosesItsGroup() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        c.splitEditorRight(nil)            // group 1: b
        let source = c.groups[1]
        c.moveTab(from: source, at: 0, to: c.groups[0], at: nil)

        XCTAssertEqual(c.groups.count, 1)
        XCTAssertEqual(c.tabs.map(\.title), ["a.txt", "b.txt"])
        XCTAssertEqual(c.activeTab?.title, "b.txt")
    }

    func testMoveEditorToNextGroupOpensAGroupWhenThereIsNone() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        c.moveEditorToNextGroup(nil)
        XCTAssertEqual(c.groups.count, 2)
        XCTAssertEqual(titles(c.groups[0]), ["a.txt"])
        XCTAssertEqual(titles(c.groups[1]), ["b.txt"])
        XCTAssertEqual(c.activeGroupIndex, 1)
    }

    func testCloseEditorSplitJoinsTheGroupsWithoutDuplicates() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        c.splitEditorRight(nil)
        c.closeEditorSplit(nil)
        XCTAssertEqual(c.groups.count, 1)
        XCTAssertFalse(c.isEditorSplit)
        XCTAssertEqual(c.tabs.map(\.title), ["a.txt", "b.txt"], "group order, b.txt once")
        XCTAssertEqual(c.activeTab?.title, "b.txt")
    }

    func testOpeningAFileOpenInAnotherGroupSharesItsBuffer() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        c.splitEditorRight(nil)
        try c.openFile(at: file("b.txt"))
        XCTAssertTrue(c.closeTab(at: 0))
        XCTAssertEqual(c.tabs.map(\.title), ["b.txt"])
        try c.openFile(at: file("a.txt"))
        let docs = c.allTabs.filter { $0.title == "a.txt" }.map(\.doc)
        XCTAssertEqual(docs.count, 2)
        XCTAssertTrue(docs[0] === docs[1], "the same file never has two buffers")
    }

    func testUnsavedChangesInAnotherGroupAreAskedAboutOnce() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        c.editorView.insertText("X", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        c.splitEditorRight(nil)
        try c.openFile(at: file("b.txt"))
        var asked = 0
        c.askSaveChoice = { _ in asked += 1; return .discard }
        XCTAssertTrue(c.confirmDiscardChanges())
        XCTAssertEqual(asked, 1, "one document, shown in two tabs, is asked about once")
    }

    func testSessionRoundTripKeepsTheGroups() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        c.splitEditorRight(nil)
        try c.openFile(at: file("c.txt"))
        let session = c.currentSession()
        XCTAssertEqual(session.tabs.map { ($0.path as NSString).lastPathComponent }, ["a.txt", "b.txt"])
        XCTAssertEqual(session.extraGroups?.count, 1)
        XCTAssertEqual(session.extraGroups?[0].tabs.map { ($0.path as NSString).lastPathComponent }, ["b.txt", "c.txt"])
        XCTAssertEqual(session.activeGroup, 1)

        let other = makeController()
        defer { other.window?.orderOut(nil) }
        other.restore(session)
        XCTAssertEqual(other.groups.count, 2)
        XCTAssertEqual(other.groups[0].tabs.map(\.title), ["a.txt", "b.txt"])
        XCTAssertEqual(other.groups[1].tabs.map(\.title), ["b.txt", "c.txt"])
        XCTAssertEqual(other.activeGroupIndex, 1)
        XCTAssertTrue(other.groups[0].tabs[1].doc === other.groups[1].tabs[0].doc)
    }

    func testOldSessionsWithoutGroupsStillDecode() throws {
        let json = #"{"version":1,"tabs":[],"sidebarVisible":true}"#
        let session = try JSONDecoder().decode(Session.self, from: Data(json.utf8))
        XCTAssertNil(session.extraGroups)
    }

    func testOpeningAnotherFolderCollapsesTheGroups() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        c.splitEditorRight(nil)
        let other = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-groups-other-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: other, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: other) }
        try c.openWorkspace(at: other)
        XCTAssertEqual(c.groups.count, 1)
        XCTAssertTrue(c.allTabs.isEmpty)
    }
}
