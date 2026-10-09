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

    // MARK: Layout

    func testSidebarHasANavigatorAndTheStatusBarTogglesIt() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))

        XCTAssertTrue(controller.isSidebarVisible, "sidebar is open by default")
        XCTAssertFalse(controller.emptyState.isHidden, "prompt to open a folder before one is open")
        XCTAssertTrue(controller.explorer.scrollView.isHidden)
        XCTAssertGreaterThan(controller.sidebar.frame.width, 150)

        // Navigator: Explorer selected, the others not built yet.
        XCTAssertEqual(controller.navigator.selected, .explorer)
        XCTAssertTrue(controller.navigator.isEnabled(.explorer))
        XCTAssertFalse(controller.navigator.isEnabled(.search), "not built yet")
        XCTAssertFalse(controller.navigator.isEnabled(.sourceControl), "not built yet")
        XCTAssertTrue(controller.navigator.isDescendant(of: controller.sidebar), "lives inside the sidebar")

        // Status bar along the bottom, full width.
        let window = try XCTUnwrap(controller.window)
        let bar = controller.statusBar
        XCTAssertEqual(bar.frame.height, Theme.statusHeight)
        XCTAssertEqual(bar.frame.minY, 0, "at the bottom of the window")
        XCTAssertEqual(bar.frame.width, window.contentView?.bounds.width ?? 0, accuracy: 0.5)

        bar.sidebarButton.performClick(nil)
        XCTAssertFalse(controller.isSidebarVisible)
        let editorWide = controller.editorView.frame.width
        bar.sidebarButton.performClick(nil)
        XCTAssertTrue(controller.isSidebarVisible)
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        XCTAssertGreaterThan(editorWide, controller.editorView.frame.width - 1, "editor regains space when hidden")
    }

    func testStatusBarShowsDocumentFactsAndFollowsTheCursor() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        controller.newDocument(nil)
        let bar = controller.statusBar
        XCTAssertEqual(bar.encoding.stringValue, "UTF-8")
        XCTAssertEqual(bar.lineEnding.stringValue, "LF")
        XCTAssertEqual(bar.indent.stringValue, "Spaces: 4")
        XCTAssertEqual(bar.cursor.stringValue, "Ln 1, Col 1")
        controller.window?.contentView?.layoutSubtreeIfNeeded()
        let encodingFrame = bar.convert(bar.encoding.bounds, from: bar.encoding)
        XCTAssertGreaterThan(encodingFrame.maxX, bar.bounds.width - 40, "facts sit at the right edge")

        let v = controller.editorView
        v.insertText("ab\ncd", replacementRange: NSRange(location: NSNotFound, length: 0))
        v.flushPendingUpdates()
        v.display()
        RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        XCTAssertEqual(bar.cursor.stringValue, "Ln 2, Col 3", "updated from the drawn frame")

        v.document?.addCursorVertical(above: true)
        v.didChangeSelection()
        v.display()
        RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        XCTAssertTrue(bar.cursor.stringValue.hasSuffix("(2 cursors)"), bar.cursor.stringValue)
    }

    func testTitleBarIsUnifiedAndThePathBarFollowsTheDocument() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        let window = try XCTUnwrap(controller.window)
        XCTAssertTrue(window.styleMask.contains(.fullSizeContentView))
        XCTAssertEqual(window.titleVisibility, .hidden)
        XCTAssertTrue(window.titlebarAppearsTransparent)

        controller.open(url: root)
        controller.explorer.outlineView.selectRowIndexes([1], byExtendingSelection: false) // src
        controller.explorer.outlineView.selectRowIndexes([3], byExtendingSelection: false) // src/main.rs
        window.contentView?.layoutSubtreeIfNeeded()
        let bar = controller.pathBar
        XCTAssertEqual(bar.components, [root.lastPathComponent, "src", "main.rs"])
        XCTAssertTrue(window.title.hasPrefix("main.rs"), "the hidden title is still kept for the Window menu")

        // The sidebar reaches the top edge; the path bar fills the title bar.
        let content = try XCTUnwrap(window.contentView)
        let sidebarTop = content.convert(controller.sidebar.bounds, from: controller.sidebar).maxY
        XCTAssertEqual(sidebarTop, content.bounds.maxY, accuracy: 0.5)
        let barFrame = content.convert(bar.bounds, from: bar)
        XCTAssertEqual(barFrame.maxY, content.bounds.maxY, accuracy: 0.5)
        XCTAssertEqual(barFrame.minY, window.contentLayoutRect.maxY, accuracy: 0.5, "ends where the title bar ends")

        XCTAssertFalse(bar.isEdited)
        controller.editorView.insertText("x", replacementRange: NSRange(location: NSNotFound, length: 0))
        controller.editorView.flushPendingUpdates()
        XCTAssertTrue(bar.isEdited)

        XCTAssertFalse(bar.clearsTrafficLights)
        controller.toggleSidebar(nil)
        XCTAssertTrue(bar.clearsTrafficLights, "makes room for the window buttons")
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
