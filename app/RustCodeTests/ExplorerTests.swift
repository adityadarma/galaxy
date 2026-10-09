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

    func testActivityBarSidebarAndStatusBarLayout() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        let window = try XCTUnwrap(controller.window)
        let content = try XCTUnwrap(window.contentView)
        func frame(_ v: NSView) -> NSRect { content.convert(v.bounds, from: v) }

        XCTAssertTrue(controller.isSidebarVisible, "sidebar is open by default")
        XCTAssertFalse(controller.emptyState.isHidden, "prompt to open a folder before one is open")
        XCTAssertTrue(controller.explorer.scrollView.isHidden)
        XCTAssertGreaterThan(controller.sidebar.frame.width, 150)

        // Activity bar on the far left, under the title bar, above the status bar.
        let activity = controller.activityBar
        XCTAssertEqual(frame(activity).minX, 0)
        XCTAssertEqual(frame(activity).width, ActivityBar.width)
        XCTAssertEqual(frame(activity).maxY, frame(controller.titleBar).minY, accuracy: 0.5)
        XCTAssertEqual(frame(activity).minY, frame(controller.statusBar).maxY, accuracy: 0.5)
        XCTAssertEqual(frame(controller.sidebar).minX, ActivityBar.width, accuracy: 0.5)
        XCTAssertEqual(activity.active, .explorer)
        XCTAssertTrue(activity.isEnabled(.explorer))
        XCTAssertFalse(activity.isEnabled(.search), "not built yet")
        XCTAssertFalse(activity.isEnabled(.sourceControl), "not built yet")

        // Status bar along the bottom, full width.
        let bar = controller.statusBar
        XCTAssertEqual(bar.frame.height, Theme.statusHeight)
        XCTAssertEqual(bar.frame.minY, 0, "at the bottom of the window")
        XCTAssertEqual(bar.frame.width, content.bounds.width, accuracy: 0.5)

        // Clicking the active item collapses the sidebar; again brings it back.
        let explorerButton = try XCTUnwrap(activity.buttons[.explorer])
        explorerButton.performClick(nil)
        XCTAssertFalse(controller.isSidebarVisible)
        XCTAssertNil(activity.active)
        let editorWide = controller.editorView.frame.width
        controller.titleBar.sidebarButton.performClick(nil)
        XCTAssertTrue(controller.isSidebarVisible)
        XCTAssertEqual(activity.active, .explorer)
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

    func testTitleBarShowsProjectAndFileAndTabsSitBelowIt() throws {
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
        XCTAssertEqual(bar.components, ["src", "main.rs"], "path inside the folder; the folder is named elsewhere")
        XCTAssertTrue(window.title.hasPrefix("main.rs"), "the hidden title is still kept for the Window menu")

        // Title bar: full width, fills the window's title bar area, shows
        // the project.
        let content = try XCTUnwrap(window.contentView)
        let titleFrame = content.convert(controller.titleBar.bounds, from: controller.titleBar)
        XCTAssertEqual(titleFrame.maxY, content.bounds.maxY, accuracy: 0.5)
        XCTAssertEqual(titleFrame.width, content.bounds.width, accuracy: 0.5)
        XCTAssertEqual(titleFrame.minY, window.contentLayoutRect.maxY, accuracy: 0.5, "ends where the title bar ends")
        XCTAssertEqual(controller.titleBar.projectButton.title, root.lastPathComponent)
        XCTAssertLessThanOrEqual(controller.titleBar.projectButton.frame.width, 220)
        // The folder name appears once in the title bar and once as the
        // sidebar section, not in the breadcrumb or a centered title.
        XCTAssertEqual(controller.sectionHeader.label.stringValue, root.lastPathComponent.uppercased())

        // Tabs at the top of the editor column; breadcrumb right under them.
        let tabsFrame = content.convert(controller.tabBar.bounds, from: controller.tabBar)
        XCTAssertEqual(tabsFrame.maxY, titleFrame.minY, accuracy: 0.5)
        XCTAssertEqual(tabsFrame.height, Theme.tabHeight)
        let barFrame = content.convert(bar.bounds, from: bar)
        XCTAssertEqual(barFrame.maxY, tabsFrame.minY, accuracy: 0.5)

        XCTAssertFalse(bar.isEdited)
        controller.editorView.insertText("x", replacementRange: NSRange(location: NSNotFound, length: 0))
        controller.editorView.flushPendingUpdates()
        XCTAssertTrue(bar.isEdited)
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

    func testExplorerLooksLikeVSCode() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let ws = try CoreBridge().core.openWorkspace(path: root.path)
        let explorer = ExplorerController()
        explorer.scrollView.frame = NSRect(x: 0, y: 0, width: 240, height: 400)
        explorer.setWorkspace(ws)
        explorer.outlineView.layoutSubtreeIfNeeded()
        func cell(_ row: Int) throws -> ExplorerCell {
            try XCTUnwrap(explorer.outlineView.view(atColumn: 0, row: row, makeIfNecessary: true) as? ExplorerCell)
        }

        // Folders: chevron, no icon. Files: no chevron, a type glyph, and
        // the name lined up with folder names.
        let src = try cell(1)
        XCTAssertNotNil(src.chevron.image)
        XCTAssertTrue(src.icon.isHidden)
        let readme = try cell(2)
        XCTAssertNil(readme.chevron.image)
        XCTAssertFalse(readme.icon.isHidden)
        XCTAssertEqual(explorer.outlineView.frameOfOutlineCell(atRow: 1), .zero, "no system disclosure triangle")

        // Children are indented by one step and the chevron turns.
        let node = try XCTUnwrap(explorer.outlineView.item(atRow: 1))
        explorer.outlineView.expandItem(node)
        XCTAssertTrue(try cell(1).isExpanded)
        let parentX = explorer.outlineView.frameOfCell(atColumn: 0, row: 1).minX
        let childX = explorer.outlineView.frameOfCell(atColumn: 0, row: 2).minX
        XCTAssertEqual(childX - parentX, ExplorerOutlineView.indent)

        explorer.collapseAll()
        XCTAssertEqual(names(explorer), ["docs", "src", "README.md"])
    }

    func testRefreshKeepsOpenFoldersOpenAndShowsNewFiles() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let ws = try CoreBridge().core.openWorkspace(path: root.path)
        let explorer = ExplorerController()
        explorer.setWorkspace(ws)
        explorer.outlineView.expandItem(explorer.outlineView.item(atRow: 1)) // src
        try "x".write(to: root.appendingPathComponent("src/new.rs"), atomically: true, encoding: .utf8)
        explorer.refresh()
        XCTAssertEqual(names(explorer), ["docs", "src", "deep", "main.rs", "new.rs", "README.md"])
    }

    func testFolderSectionFoldsTheTree() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        XCTAssertTrue(controller.sectionHeader.isHidden, "no section before a folder is open")
        controller.open(url: root)
        XCTAssertFalse(controller.sectionHeader.isHidden)
        XCTAssertTrue(controller.sectionHeader.refreshButton.isHidden, "tools show on hover only")
        XCTAssertTrue(controller.sectionHeader.collapseButton.isHidden)
        controller.sectionHeader.toggle()
        XCTAssertTrue(controller.explorer.scrollView.isHidden)
        controller.sectionHeader.toggle()
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
