import XCTest
@testable import Galaxy

/// M2-11: the folder, tabs, carets and scroll come back after a restart.
@MainActor
final class SessionTests: XCTestCase {
    // Set in setUp, which XCTest calls outside the main actor.
    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-session-\(UUID().uuidString)")
        let fm = FileManager.default
        try fm.createDirectory(at: root.appendingPathComponent("ws/src"), withIntermediateDirectories: true)
        let long = (0..<500).map { "line \($0)" }.joined(separator: "\n")
        try long.write(to: root.appendingPathComponent("ws/src/long.rs"), atomically: true, encoding: .utf8)
        try "fn main() {}\n".write(to: root.appendingPathComponent("ws/src/main.rs"), atomically: true, encoding: .utf8)
        try "outside\n".write(to: root.appendingPathComponent("outside.txt"), atomically: true, encoding: .utf8)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    private var sessionURL: URL { root.appendingPathComponent("session.json") }

    private func makeController() -> MainWindowController {
        let c = MainWindowController(bridge: CoreBridge())
        c.sessionURL = sessionURL
        c.window?.setContentSize(NSSize(width: 800, height: 500))
        c.showWindow(nil)
        c.askSaveChoice = { _ in .discard }
        return c
    }

    private func path(_ rel: String) -> String { root.appendingPathComponent(rel).path }

    func testTabsCaretsAndScrollComeBack() throws {
        let first = makeController()
        try first.openWorkspace(at: root.appendingPathComponent("ws"))
        try first.openFile(at: root.appendingPathComponent("ws/src/long.rs"))
        first.editorView.document?.goToLine(line: 300, col: 3)
        first.editorView.revealLine(300)
        try first.openFile(at: root.appendingPathComponent("outside.txt"))
        try first.openFile(at: root.appendingPathComponent("ws/src/main.rs"))
        first.editorView.document?.goToLine(line: 0, col: 2)
        first.selectTab(0)
        let savedScroll = try XCTUnwrap(first.currentSession().tabs.first?.scrollY)
        XCTAssertGreaterThan(savedScroll, 100, "scrolled down to line 300")
        first.newDocument(nil)  // Untitled: not saved
        first.selectTab(0)
        first.saveSession()  // closing a window starts fresh; only quitting saves
        first.window?.close()

        let saved = try XCTUnwrap(Session.load(from: sessionURL))
        XCTAssertEqual(saved.workspace, root.appendingPathComponent("ws").path)
        XCTAssertEqual(saved.tabs.map(\.path), [path("ws/src/long.rs"), path("outside.txt"), path("ws/src/main.rs")])
        XCTAssertEqual(saved.active, 0)

        let second = makeController()
        defer { second.window?.orderOut(nil) }
        XCTAssertTrue(second.restore(saved))
        XCTAssertEqual(second.workspace?.root(), root.appendingPathComponent("ws").path)
        XCTAssertEqual(second.tabBar.items.map(\.title), ["long.rs", "outside.txt", "main.rs"])
        XCTAssertEqual(second.activeIndex, 0)
        let caret = second.location(of: try XCTUnwrap(second.activeTab))
        XCTAssertEqual([caret.line, caret.col], [300, 3])
        let scroll = second.currentSession().tabs[0].scrollY ?? 0
        XCTAssertEqual(scroll, savedScroll, accuracy: 1, "same scroll position")

        second.selectTab(2)
        let mainCaret = second.location(of: try XCTUnwrap(second.activeTab))
        XCTAssertEqual([mainCaret.line, mainCaret.col], [0, 2])
        XCTAssertFalse(second.tabs.contains { $0.isEdited }, "restored tabs are clean")
    }

    func testMissingFilesAndFoldersAreSkipped() throws {
        var session = Session()
        session.workspace = path("gone")
        session.tabs = [
            Session.Tab(path: path("missing.txt"), isPreview: false, line: 0, col: 0, scrollY: nil),
            Session.Tab(path: path("outside.txt"), isPreview: false, line: 99, col: 99, scrollY: 50),
        ]
        session.active = 0
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        XCTAssertTrue(c.restore(session))
        XCTAssertNil(c.workspace)
        XCTAssertEqual(c.tabBar.items.map(\.title), ["outside.txt"])
        let caret = c.location(of: try XCTUnwrap(c.activeTab))
        XCTAssertEqual([caret.line, caret.col], [1, 0], "clamped to the last (empty) line")

        var empty = Session()
        empty.tabs = [Session.Tab(path: path("missing.txt"), isPreview: false, line: 0, col: 0, scrollY: nil)]
        let d = makeController()
        defer { d.window?.orderOut(nil) }
        XCTAssertFalse(d.restore(empty), "nothing restored: the caller opens an Untitled buffer")
    }

    func testLaunchRestoreBatchesWorkspaceTabs() throws {
        let first = makeController()
        try first.openWorkspace(at: root.appendingPathComponent("ws"))
        try first.openFile(at: root.appendingPathComponent("ws/src/long.rs"))
        try first.openFile(at: root.appendingPathComponent("ws/src/main.rs"))
        first.selectTab(2)
        let saved = first.currentSession()
        try saved.save(to: Session.workspaceURL(for: root.appendingPathComponent("ws").path))
        first.window?.close()

        let second = makeController()
        defer { second.window?.orderOut(nil) }
        XCTAssertTrue(second.restore(saved, incrementally: true))
        XCTAssertEqual(second.tabBar.items.map(\.title), ["main.rs"], "the last active tab is ready before deferred restoration")
        XCTAssertEqual(second.activeTab?.title, "main.rs")

        let restored = expectation(description: "workspace tabs restored")
        DispatchQueue.main.async {
            XCTAssertEqual(second.tabBar.items.map(\.title), ["long.rs", "main.rs"])
            XCTAssertEqual(second.activeTab?.title, "main.rs", "deferred tabs do not steal focus")
            restored.fulfill()
        }
        wait(for: [restored], timeout: 1)
    }

    func testABadOrNewerSessionFileIsIgnored() throws {
        try "{ not json".write(to: sessionURL, atomically: true, encoding: .utf8)
        XCTAssertNil(Session.load(from: sessionURL))
        try #"{"version": 99, "tabs": [], "sidebarVisible": true}"#.write(to: sessionURL, atomically: true, encoding: .utf8)
        XCTAssertNil(Session.load(from: sessionURL))
        XCTAssertNil(Session.load(from: root.appendingPathComponent("none.json")))
    }

    func testDeletedTabsAreNotSaved() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: root.appendingPathComponent("outside.txt"))
        try FileManager.default.removeItem(at: root.appendingPathComponent("outside.txt"))
        c.checkOpenFiles()
        XCTAssertEqual(c.currentSession().tabs, [])
    }
}
