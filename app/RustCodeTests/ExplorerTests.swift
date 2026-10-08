import XCTest
@testable import RustCode

@MainActor
final class ExplorerTests: XCTestCase {
    private func makeTree() throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("rcode-ws-\(UUID().uuidString)")
        let fm = FileManager.default
        for dir in ["src/deep", "node_modules/pkg", "docs"] {
            try fm.createDirectory(at: root.appendingPathComponent(dir), withIntermediateDirectories: true)
        }
        try "fn main() {}\n".write(to: root.appendingPathComponent("src/main.rs"), atomically: true, encoding: .utf8)
        try "# hi\n".write(to: root.appendingPathComponent("README.md"), atomically: true, encoding: .utf8)
        try "x".write(to: root.appendingPathComponent("node_modules/pkg/index.js"), atomically: true, encoding: .utf8)
        return root
    }

    private func names(_ explorer: ExplorerController) -> [String] {
        (0..<explorer.outlineView.numberOfRows).compactMap {
            (explorer.outlineView.item(atRow: $0) as? ExplorerNode)?.entry.name
        }
    }

    func testOnlyTheRootIsListedUntilAFolderIsExpanded() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let ws = try CoreBridge().core.openWorkspace(path: root.path)

        let explorer = ExplorerController()
        explorer.setWorkspace(ws)
        XCTAssertEqual(names(explorer), ["docs", "src", "README.md"], "node_modules is hidden")
        XCTAssertEqual(explorer.listCalls, 1, "opening a workspace lists only the root")

        let src = try XCTUnwrap(explorer.outlineView.item(atRow: 1) as? ExplorerNode)
        explorer.outlineView.expandItem(src)
        XCTAssertEqual(explorer.listCalls, 2)
        XCTAssertEqual(names(explorer), ["docs", "src", "deep", "main.rs", "README.md"])

        // Collapsing and re-expanding reuses the loaded children.
        explorer.outlineView.collapseItem(src)
        explorer.outlineView.expandItem(src)
        XCTAssertEqual(explorer.listCalls, 2)
    }

    func testSelectingAFileOpensItAndSelectingAFolderToggles() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let ws = try CoreBridge().core.openWorkspace(path: root.path)
        let explorer = ExplorerController()
        var opened: [String] = []
        explorer.onOpenFile = { _, entry in opened.append(entry.relPath) }
        explorer.setWorkspace(ws)

        explorer.outlineView.selectRowIndexes([1], byExtendingSelection: false) // src (folder)
        XCTAssertTrue(opened.isEmpty)
        XCTAssertEqual(names(explorer), ["docs", "src", "deep", "main.rs", "README.md"], "folder expanded")

        explorer.outlineView.selectRowIndexes([3], byExtendingSelection: false) // src/main.rs
        XCTAssertEqual(opened, ["src/main.rs"])
    }

    func testWindowOpensAFolderAndShowsAFileFromTheSidebar() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        XCTAssertTrue(controller.explorer.scrollView.isHidden, "no sidebar before a folder is opened")

        controller.open(url: root)
        RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        XCTAssertFalse(controller.explorer.scrollView.isHidden)
        XCTAssertGreaterThan(controller.explorer.scrollView.frame.width, 100)
        XCTAssertEqual(controller.window?.title, root.lastPathComponent)

        // Click README.md (row 2 after docs, src).
        controller.explorer.outlineView.selectRowIndexes([2], byExtendingSelection: false)
        RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        XCTAssertEqual(controller.editorView.document?.lines(first: 0, count: 1), ["# hi"])
        XCTAssertTrue(controller.window?.title.hasPrefix("README.md") ?? false)
        XCTAssertFalse(controller.editorView.lastDrawnLines.isEmpty)
        XCTAssertGreaterThan(controller.editorView.frame.width, 100)
    }

    func testOpeningASingleFileStillWorksAndMissingPathsAreReported() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }

        controller.open(url: root.appendingPathComponent("README.md"))
        XCTAssertEqual(controller.editorView.document?.lines(first: 0, count: 1), ["# hi"])
        XCTAssertTrue(controller.explorer.scrollView.isHidden, "a lone file does not open the sidebar")

        XCTAssertThrowsError(try controller.openWorkspace(at: root.appendingPathComponent("missing")))
    }

    // MARK: VSCode-style layout

    func testActivityBarTogglesSidebarAndShowsEmptyStateFirst() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))

        XCTAssertTrue(controller.isSidebarVisible, "sidebar is open by default, like VSCode")
        XCTAssertFalse(controller.emptyState.isHidden, "prompt to open a folder before one is open")
        XCTAssertTrue(controller.explorer.scrollView.isHidden)
        XCTAssertEqual(controller.activityBar.frame.width, ActivityBar.width)
        XCTAssertGreaterThan(controller.sidebar.frame.width, 150)

        let explorerButton = try XCTUnwrap(controller.activityBar.buttons[.explorer])
        XCTAssertTrue(explorerButton.isEnabled)
        XCTAssertFalse(try XCTUnwrap(controller.activityBar.buttons[.search]).isEnabled, "not built yet")

        explorerButton.performClick(nil)
        XCTAssertFalse(controller.isSidebarVisible)
        let editorWide = controller.editorView.frame.width
        explorerButton.performClick(nil)
        XCTAssertTrue(controller.isSidebarVisible)
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        XCTAssertGreaterThan(editorWide, controller.editorView.frame.width - 1, "editor regains space when hidden")
    }

    func testOpeningAFolderReplacesTheEmptyState() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }

        controller.toggleSidebar(nil)              // hidden
        controller.open(url: root)                 // opening a folder reveals it
        XCTAssertTrue(controller.isSidebarVisible)
        XCTAssertTrue(controller.emptyState.isHidden)
        XCTAssertFalse(controller.explorer.scrollView.isHidden)
    }

    func testExplorerRowsAreCompact() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let ws = try CoreBridge().core.openWorkspace(path: root.path)
        let explorer = ExplorerController()
        explorer.scrollView.frame = NSRect(x: 0, y: 0, width: 240, height: 400)
        explorer.setWorkspace(ws)
        explorer.outlineView.layoutSubtreeIfNeeded()

        XCTAssertEqual(ExplorerController.rowHeight, 22)
        XCTAssertEqual(explorer.outlineView.rowHeight, 22)
        let first = explorer.outlineView.rect(ofRow: 0)
        let second = explorer.outlineView.rect(ofRow: 1)
        XCTAssertEqual(second.minY - first.minY, 22, accuracy: 0.5, "rows must be 22 pt apart")
    }
}
