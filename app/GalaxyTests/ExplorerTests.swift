import XCTest
@testable import Galaxy

@MainActor
final class ExplorerTests: XCTestCase {
    /// Everyday developer file types get a glyph of their own, not the blank page.
    func testCommonDeveloperFilesHaveTheirOwnGlyph() {
        let generic = FileGlyph.style(for: "unknown-extension")
        XCTAssertEqual(generic.1, "doc")
        for key in ["sql", "sqlite", "csv", "graphql", "proto", "java", "kt", "rb", "cs", "lua", "ex", "zig",
                    "ps1", "bat", "pem", "key", "lock", "env", "diff", "pdf", "zip", "jsonc", "conf", "ipynb",
                    "pyi", "hcl", "mdx", "ejs", "ico", "heic", "woff2", "cc", "makefile", "gemfile", "go.mod",
                    "requirements.txt", "pyproject.toml", "cmakelists.txt", "dockerfile"] {
            let style = FileGlyph.style(for: key)
            XCTAssertTrue(style.0 != nil || style.1 != "doc", "\(key) still has the generic glyph")
        }
        XCTAssertEqual(FileGlyph.style(for: "sql").1, "cylinder")
        XCTAssertEqual(FileGlyph.namedFileKey("makefile"), "makefile")
        XCTAssertEqual(FileGlyph.namedFileKey("go.mod"), "go.mod")
        XCTAssertEqual(FileGlyph.style(for: "go.mod").0, "go")
        XCTAssertEqual(FileGlyph.style(for: "jsonc").0, "json")
        let sql = FileGlyph.for(name: "db_sdsd.sql")
        XCTAssertNotNil(sql.image)
        XCTAssertNotEqual(sql.color, FileGlyph.for(name: "notes.unknown").color)
    }

    func testMaterialIconMappingsCoverCommonFilesAndFolders() {
        XCTAssertEqual(FileGlyph.style(for: "package.json").0, "npm")
        XCTAssertEqual(FileGlyph.style(for: "dockerfile").0, "docker")
        XCTAssertEqual(FileGlyph.style(for: ".dockerignore").0, "docker")
        XCTAssertEqual(FileGlyph.style(for: ".editorconfig").0, "settings")
        XCTAssertEqual(FileGlyph.style(for: ".env.example").0, "lock")
        XCTAssertEqual(FileGlyph.style(for: ".gitlab-ci_release.yml").0, "gitlab")
        XCTAssertEqual(FileGlyph.style(for: ".prettierignore").0, "prettier")
        XCTAssertEqual(FileGlyph.namedFileKey("dockerfile.database"), "dockerfile")
        XCTAssertEqual(FileGlyph.namedFileKey(".env.backup"), ".env")
        XCTAssertEqual(FileGlyph.namedFileKey("nginx-logrotate.conf"), "logrotate")
        XCTAssertEqual(FileGlyph.namedFileKey("crontab.database"), "crontab")
        XCTAssertEqual(FileGlyph.style(for: ".npmignore").0, "npm")
        XCTAssertEqual(FileGlyph.style(for: ".npmrc").0, "npm")
        XCTAssertEqual(FileGlyph.style(for: "composer.json").0, "json")
        XCTAssertEqual(FileGlyph.style(for: "artisan").0, "laravel")
        XCTAssertEqual(FileGlyph.style(for: "nginx.conf").0, "nginx")
        XCTAssertNil(FileGlyph.style(for: "logrotate").0)
        XCTAssertEqual(FileGlyph.style(for: "www.conf").0, "php")
        XCTAssertEqual(FileGlyph.style(for: ".styleci.yml").1, "paintbrush.pointed")
        XCTAssertEqual(FileGlyph.style(for: "phpunit.xml").1, "checkmark.seal")
        XCTAssertEqual(FileGlyph.style(for: "pint.json").1, "paintbrush")
        XCTAssertEqual(FileGlyph.style(for: "firebase").0, "firebase")
        XCTAssertEqual(FileGlyph.style(for: "supabase").0, "supabase")
        XCTAssertEqual(FileGlyph.style(for: "vercel").0, "vercel")
        XCTAssertEqual(FileGlyph.style(for: "netlify").0, "netlify")
        XCTAssertEqual(FileGlyph.style(for: "tf").0, "terraform")
        XCTAssertEqual(FileGlyph.style(for: "tsx").0, "react_ts")
        XCTAssertEqual(FileGlyph.style(for: "yaml").0, "yaml")
        XCTAssertEqual(FileGlyph.style(for: "prisma").0, "prisma")
        XCTAssertEqual(ExplorerIcon.folderAsset(named: "components"), "folder-components")
        XCTAssertEqual(ExplorerIcon.folderAsset(named: ".docker"), "folder-docker")
        XCTAssertEqual(ExplorerIcon.folderAsset(named: "Http"), "folder-routes")
        XCTAssertEqual(ExplorerIcon.folderAsset(named: "bootstrap"), "folder-config")
        XCTAssertEqual(ExplorerIcon.folderAsset(named: "factories"), "folder-migrations")
        XCTAssertEqual(ExplorerIcon.folderAsset(named: "Feature"), "folder-test")
        XCTAssertEqual(ExplorerIcon.folderAsset(named: "vendor"), "folder-packages")
        XCTAssertEqual(ExplorerIcon.folderAsset(named: "migrations"), "folder-migrations")
        XCTAssertEqual(ExplorerIcon.folderAsset(named: "node_modules"), "folder-node")
    }

    private func makeTree() throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-ws-\(UUID().uuidString)")
        let fm = FileManager.default
        for dir in ["src/deep", "docs"] {
            try fm.createDirectory(at: root.appendingPathComponent(dir), withIntermediateDirectories: true)
        }
        try "fn main() {}\n".write(to: root.appendingPathComponent("src/main.rs"), atomically: true, encoding: .utf8)
        try "# hi\n".write(to: root.appendingPathComponent("README.md"), atomically: true, encoding: .utf8)
        return root
    }

    private func names(_ explorer: ExplorerController) -> [String] {
        (0..<explorer.outlineView.numberOfRows).compactMap {
            (explorer.outlineView.item(atRow: $0) as? ExplorerNode)?.entry.name
        }
    }

    func testGitIgnoredEntriesAreDimmedIncludingEverythingInsideAnIgnoredFolder() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        try "docs/\n*.log\n".write(to: root.appendingPathComponent(".gitignore"), atomically: true, encoding: .utf8)
        try "x".write(to: root.appendingPathComponent("debug.log"), atomically: true, encoding: .utf8)
        try "x".write(to: root.appendingPathComponent("docs/guide.md"), atomically: true, encoding: .utf8)
        let ws = try CoreBridge().core.openWorkspace(path: root.path)

        let explorer = ExplorerController()
        explorer.setWorkspace(ws)
        func node(_ name: String) throws -> ExplorerNode {
            try XCTUnwrap((0..<explorer.outlineView.numberOfRows).lazy
                .compactMap { explorer.outlineView.item(atRow: $0) as? ExplorerNode }
                .first { $0.entry.name == name }, name)
        }
        XCTAssertTrue(try node("docs").isIgnored, "folder rule")
        XCTAssertTrue(try node("debug.log").isIgnored, "file rule")
        XCTAssertFalse(try node("src").isIgnored)
        XCTAssertFalse(try node(".gitignore").isIgnored, ".gitignore itself is not ignored")

        explorer.outlineView.expandItem(try node("docs"))
        XCTAssertTrue(try node("guide.md").isIgnored, "files inside an ignored folder are dimmed too")

        let cell = try XCTUnwrap(explorer.outlineView.view(atColumn: 0, row: explorer.outlineView.row(forItem: try node("docs")),
                                                           makeIfNecessary: true) as? ExplorerCell)
        XCTAssertEqual(cell.alphaValue, ExplorerCell.ignoredAlpha, accuracy: 0.001)
        let normal = try XCTUnwrap(explorer.outlineView.view(atColumn: 0, row: explorer.outlineView.row(forItem: try node("src")),
                                                             makeIfNecessary: true) as? ExplorerCell)
        XCTAssertEqual(normal.alphaValue, 1, accuracy: 0.001, "a reused cell is restored")
    }

    func testOnlyTheRootIsListedUntilAFolderIsExpanded() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(
            at: root.appendingPathComponent("node_modules/pkg"), withIntermediateDirectories: true)
        let ws = try CoreBridge().core.openWorkspace(path: root.path)

        XCTAssertEqual(try ws.listDir(relPath: "").map(\.name), ["docs", "node_modules", "src", "README.md"],
                       "nothing is hidden until files.exclude says so")
        XCTAssertTrue(ws.setExplorerExcludes(globs: ["node_modules"]))

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

    func testEveryClickOnAFolderOpensOrClosesIt() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        controller.open(url: root)
        let explorer = controller.explorer
        let outline = explorer.outlineView
        outline.layoutSubtreeIfNeeded()
        let window = try XCTUnwrap(controller.window)

        func click(row: Int, count: Int) {
            let rect = outline.rect(ofRow: row)
            let p = outline.convert(NSPoint(x: rect.midX, y: rect.midY), to: nil)
            let e = NSEvent.mouseEvent(with: .leftMouseDown, location: p, modifierFlags: [],
                                       timestamp: ProcessInfo.processInfo.systemUptime,
                                       windowNumber: window.windowNumber, context: nil, eventNumber: 0,
                                       clickCount: count, pressure: 1)!
            outline.mouseDown(with: e)
        }
        let src = try XCTUnwrap(outline.item(atRow: 1))
        click(row: 1, count: 1)
        XCTAssertTrue(outline.isItemExpanded(src), "one click opens")
        click(row: 1, count: 2)
        XCTAssertFalse(outline.isItemExpanded(src), "a quick second click closes, not swallowed as a double-click")
        click(row: 1, count: 1)
        XCTAssertTrue(outline.isItemExpanded(src), "clicking the selected folder again toggles too")
        XCTAssertEqual(outline.selectedRow, 1)
        click(row: 1, count: 1)
        XCTAssertFalse(outline.isItemExpanded(src))
    }

    func testDoubleClickingTheTitleBarFillsTheScreenAndRestores() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        let window = try XCTUnwrap(controller.window)
        let screen = try XCTUnwrap(window.screen ?? NSScreen.main)
        // Wider than the window's layout minimum (900), or a layout pass
        // after the restore grows it and the frame no longer matches.
        let small = NSRect(x: screen.visibleFrame.minX + 40, y: screen.visibleFrame.minY + 40, width: 1000, height: 500)
        window.setFrame(small, display: true)

        controller.titleBar.toggleFill()
        XCTAssertEqual(window.frame, screen.visibleFrame, "fills the usable screen area")
        XCTAssertFalse(window.styleMask.contains(.fullScreen), "not full screen")
        controller.titleBar.toggleFill()
        XCTAssertEqual(window.frame, small, "second double-click restores")
        XCTAssertFalse(controller.titleBar.mouseDownCanMoveWindow, "the bar drags itself so double-clicks reach it")
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
        XCTAssertEqual(frame(controller.sidebar).minX, ActivityBar.width + Theme.cardGap / 2, accuracy: 0.5, "the sidebar card starts after a small gap")
        XCTAssertEqual(activity.active, .explorer)
        XCTAssertTrue(activity.isEnabled(.explorer))
        XCTAssertTrue(activity.isEnabled(.search))
        XCTAssertTrue(activity.isEnabled(.sourceControl))

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
        explorerButton.performClick(nil)
        XCTAssertTrue(controller.isSidebarVisible)
        XCTAssertEqual(activity.active, .explorer)
        XCTAssertTrue(
            controller.explorer.scrollView.superview === controller.sidebar,
            "toggling the sidebar must preserve the explorer view hierarchy"
        )
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        XCTAssertGreaterThan(editorWide, controller.editorView.frame.width - 1, "editor regains space when hidden")
    }

    func testOptionJPanelToggleDoesNotRequireSynchronousLayout() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }

        XCTAssertFalse(controller.isBottomPanelVisible)
        controller.toggleTerminal(nil)
        XCTAssertTrue(controller.isBottomPanelVisible)
        controller.toggleTerminal(nil)
        XCTAssertFalse(controller.isBottomPanelVisible)
    }

    func testGitLineMarkersMergeDuplicateLines() {
        let markers = MainWindowController.gitLineMarkers([
            GitLineChangeInfo(line: 8, kind: .added),
            GitLineChangeInfo(line: 8, kind: .modified),
            GitLineChangeInfo(line: 8, kind: .deleted),
        ])
        XCTAssertEqual(markers, [8: .modified])

        // No Added + Deleted -> Modified rule: the edit on the line shows.
        let replacement = MainWindowController.gitLineMarkers([
            GitLineChangeInfo(line: 26, kind: .deleted),
            GitLineChangeInfo(line: 26, kind: .added),
        ])
        XCTAssertEqual(replacement, [26: .added])
    }

    func testDraggingTheDividerResizesTheSidebar() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        let window = try XCTUnwrap(controller.window)
        let content = try XCTUnwrap(window.contentView)
        let sidebar = controller.sidebar
        let start = sidebar.frame.width
        // The point on the divider, in window coordinates.
        let edge = sidebar.convert(NSPoint(x: sidebar.bounds.maxX, y: sidebar.bounds.midY), to: nil)
        let down = NSPoint(x: edge.x + 0.5, y: edge.y)
        let hit = content.hitTest(content.convert(down, from: nil))
        XCTAssertTrue(hit is NSSplitView, "divider gets the click, got \(String(describing: hit))")

        func mouse(_ type: NSEvent.EventType, _ p: NSPoint) -> NSEvent {
            NSEvent.mouseEvent(with: type, location: p, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                               windowNumber: window.windowNumber, context: nil, eventNumber: 0,
                               clickCount: 1, pressure: type == .leftMouseUp ? 0 : 1)!
        }
        // NSSplitView tracks the drag in its own loop; queue the rest first.
        for dx in stride(from: 10.0, through: 80.0, by: 10.0) {
            window.postEvent(mouse(.leftMouseDragged, NSPoint(x: down.x + dx, y: down.y)), atStart: false)
        }
        window.postEvent(mouse(.leftMouseUp, NSPoint(x: down.x + 80, y: down.y)), atStart: false)
        (hit ?? content).mouseDown(with: mouse(.leftMouseDown, down))
        content.layoutSubtreeIfNeeded()
        XCTAssertEqual(sidebar.frame.width, start + 80, accuracy: 4, "sidebar follows the divider")
        XCTAssertEqual(start, MainWindowController.sidebarWidth, accuracy: 1, "starts at the default width")

        // Growing the window widens the editor, not the sidebar.
        var f = window.frame
        f.size.width += 100
        window.setFrame(f, display: true)
        content.layoutSubtreeIfNeeded()
        XCTAssertEqual(sidebar.frame.width, start + 80, accuracy: 4)

        // Limits: never narrower than the minimum.
        let split = try XCTUnwrap(sidebar.superview as? NSSplitView)
        XCTAssertEqual(controller.splitView(split, constrainMinCoordinate: 0, ofSubviewAt: 0),
                       MainWindowController.minSidebarWidth)
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
        // The label is set on the main queue right after the draw.
        func waitFor(_ ok: () -> Bool) {
            let deadline = Date().addingTimeInterval(2)
            while !ok(), Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        }
        waitFor { bar.cursor.stringValue == "Ln 2, Col 3" }
        XCTAssertEqual(bar.cursor.stringValue, "Ln 2, Col 3", "updated from the drawn frame")

        v.document?.addCursorVertical(above: true)
        v.didChangeSelection()
        v.display()
        waitFor { bar.cursor.stringValue.hasSuffix("(2 cursors)") }
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

        // Title bar: full width, fills the window's title bar area, empty.
        let content = try XCTUnwrap(window.contentView)
        let titleFrame = content.convert(controller.titleBar.bounds, from: controller.titleBar)
        XCTAssertEqual(titleFrame.maxY, content.bounds.maxY, accuracy: 0.5)
        XCTAssertEqual(titleFrame.width, content.bounds.width, accuracy: 0.5)
        XCTAssertEqual(titleFrame.height, TitleBar.minimumHeight, accuracy: 0.5, "keeps its own height, also in full screen")
        // The separator, the navigation and search group, the layout toggles.
        XCTAssertTrue(controller.titleBar.subviews.allSatisfy { $0 is NSBox || $0 is NSStackView },
                      "only navigation, search and layout toggles in the title bar")
        XCTAssertEqual(controller.titleBar.searchPill.text, try XCTUnwrap(controller.workspace).name())
        // The sidebar section stays "EXPLORER"; the folder name is in the title bar.
        XCTAssertEqual(controller.sectionHeader.label.stringValue, "EXPLORER")

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

    func testPathBarFolderMenuListsChildrenAndOpensFiles() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        controller.open(url: root)
        controller.explorer.outlineView.selectRowIndexes([1], byExtendingSelection: false) // src
        controller.explorer.outlineView.selectRowIndexes([3], byExtendingSelection: false) // src/main.rs

        XCTAssertEqual(controller.pathBar.components, ["src", "main.rs"])
        let menu = try XCTUnwrap(controller.pathBarMenu(for: 0))
        let deep = try XCTUnwrap(menu.items.first { $0.title == "deep" })
        XCTAssertNotNil(deep.submenu, "folder children remain submenus")
        let main = try XCTUnwrap(menu.items.first { $0.title == "main.rs" })
        XCTAssertEqual(main.representedObject as? FileEntry, try controller.workspace?.listDir(relPath: "src").first { $0.name == "main.rs" })

        XCTAssertTrue(NSApp.sendAction(try XCTUnwrap(main.action), to: main.target, from: main))
        XCTAssertEqual(controller.tabBar.items.last?.title, "main.rs")
        XCTAssertFalse(controller.tabBar.items.last?.isPreview ?? true)
        XCTAssertNil(controller.pathBarMenu(for: 1), "the file component has no menu")
    }

    func testOpeningAFolderReplacesTheEmptyState() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }

        controller.toggleSidebarVisibility(nil)              // hidden
        controller.open(url: root)                 // opening a folder reveals it
        XCTAssertTrue(controller.isSidebarVisible)
        XCTAssertTrue(controller.emptyState.isHidden)
        XCTAssertFalse(controller.explorer.scrollView.isHidden)
    }

    func testExplorerLooksLikeVSCode() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        // The "Folder icons" switch is a user preference; the test runs with it on.
        let defaults = UserDefaults.standard
        let savedPreference = defaults.object(forKey: ExplorerIcon.showFolderIconsKey)
        defaults.set(true, forKey: ExplorerIcon.showFolderIconsKey)
        defer {
            if let savedPreference { defaults.set(savedPreference, forKey: ExplorerIcon.showFolderIconsKey) }
            else { defaults.removeObject(forKey: ExplorerIcon.showFolderIconsKey) }
        }
        let ws = try CoreBridge().core.openWorkspace(path: root.path)
        let explorer = ExplorerController()
        explorer.scrollView.frame = NSRect(x: 0, y: 0, width: 240, height: 400)
        explorer.setWorkspace(ws)
        explorer.outlineView.layoutSubtreeIfNeeded()
        func cell(_ row: Int) throws -> ExplorerCell {
            try XCTUnwrap(explorer.outlineView.view(atColumn: 0, row: row, makeIfNecessary: true) as? ExplorerCell)
        }

        // Folders: chevron and folder icon. Files: no chevron, a type glyph, and
        // the name lined up with folder names.
        let src = try cell(1)
        XCTAssertNotNil(src.chevron.image)
        XCTAssertFalse(src.icon.isHidden)
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

    func testSidebarHeaderShowsTheFolderAndItsActions() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        let header = controller.sectionHeader
        XCTAssertEqual(header.label.stringValue, "EXPLORER", "before a folder is open")
        XCTAssertTrue(header.tools.allSatisfy(\.isHidden), "no actions without a folder")

        controller.open(url: root)
        controller.window?.contentView?.layoutSubtreeIfNeeded()
        XCTAssertEqual(header.label.stringValue, "EXPLORER", "the folder name is in the title bar")
        XCTAssertEqual(header.tools.map(\.toolTip), ["New File…", "New Folder…", "Refresh Explorer",
                                                      "Collapse Folders in Explorer", "More Actions…"])
        XCTAssertTrue(header.tools.allSatisfy { $0.image != nil })

        // Only "…" shows at rest; the rest appear while the pointer is over the header.
        func crossing(_ type: NSEvent.EventType) throws -> NSEvent {
            try XCTUnwrap(NSEvent.enterExitEvent(
                with: type, location: .zero, modifierFlags: [], timestamp: 0,
                windowNumber: controller.window?.windowNumber ?? 0, context: nil,
                eventNumber: 0, trackingNumber: 0, userData: nil))
        }
        header.mouseExited(with: try crossing(.mouseExited))
        XCTAssertEqual(header.tools.filter { !$0.isHidden }.map(\.toolTip), ["More Actions…"])
        header.mouseEntered(with: try crossing(.mouseEntered))
        XCTAssertTrue(header.tools.allSatisfy { !$0.isHidden }, "all actions while hovered")

        // One row at the top of the sidebar, as tall as the tab strip; the tree below it.
        let content = try XCTUnwrap(controller.window?.contentView)
        let headerFrame = content.convert(header.bounds, from: header)
        let tabsFrame = content.convert(controller.tabBar.bounds, from: controller.tabBar)
        XCTAssertEqual(headerFrame.maxY, tabsFrame.maxY, accuracy: 0.5)
        XCTAssertEqual(headerFrame.height, Theme.tabHeight, accuracy: 0.5)
        let tree = controller.explorer.scrollView
        XCTAssertEqual(content.convert(tree.bounds, from: tree).maxY, headerFrame.minY, accuracy: 0.5)

        // The buttons do what they say.
        controller.explorer.outlineView.expandItem(controller.explorer.outlineView.item(atRow: 1))
        header.collapseButton.performClick(nil)
        XCTAssertEqual(names(controller.explorer), ["docs", "src", "README.md"])
        try "x".write(to: root.appendingPathComponent("added.txt"), atomically: true, encoding: .utf8)
        header.refreshButton.performClick(nil)
        XCTAssertTrue(names(controller.explorer).contains("added.txt"))

        let more = controller.explorerMoreMenu().items.map(\.title).filter { !$0.isEmpty }
        XCTAssertEqual(more, ["New File…", "New Folder…", "Open Folder…", "Refresh Explorer",
                              "Collapse Folders in Explorer", "Reveal in Finder", "Copy Path"])
    }

    func testNewFileAndFolderGoIntoTheSelectedFolder() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        controller.open(url: root)
        let explorer = controller.explorer
        func nameNewItem(_ name: String) throws {
            let field = try XCTUnwrap(explorer.renameFieldForTesting)
            let editor = try XCTUnwrap(field.currentEditor() as? NSTextView)
            editor.string = name
            editor.doCommand(by: #selector(NSResponder.insertNewline(_:)))
        }

        // Nothing selected: root.
        controller.sectionHeader.newFileButton.performClick(nil)
        XCTAssertEqual(explorer.renameFieldForTesting?.stringValue, "")
        try nameNewItem("lib.rs")
        XCTAssertTrue(FileManager.default.fileExists(atPath: root.appendingPathComponent("lib.rs").path))
        XCTAssertEqual(explorer.selectedEntry?.relPath, "lib.rs", "and is selected in the tree")

        // Folder selected: inside it. A selected file means its folder.
        explorer.reveal(relPath: "src")
        controller.sectionHeader.newFolderButton.performClick(nil)
        XCTAssertEqual(explorer.renameFieldForTesting?.stringValue, "")
        try nameNewItem("net")
        var isDir: ObjCBool = false
        XCTAssertTrue(FileManager.default.fileExists(atPath: root.appendingPathComponent("src/net").path, isDirectory: &isDir))
        XCTAssertTrue(isDir.boolValue)
        explorer.reveal(relPath: "src/main.rs")
        controller.explorerNewFile(nil)
        try nameNewItem("util.rs")
        XCTAssertTrue(FileManager.default.fileExists(atPath: root.appendingPathComponent("src/util.rs").path))

        // Escape removes the temporary item; an existing name is refused, not overwritten.
        let before = try FileManager.default.contentsOfDirectory(atPath: root.path).sorted()
        controller.explorerNewFile(nil)
        let field = try XCTUnwrap(explorer.renameFieldForTesting)
        try XCTUnwrap(field.currentEditor() as? NSTextView)
            .doCommand(by: #selector(NSResponder.cancelOperation(_:)))
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: root.path).sorted(), before)
        XCTAssertThrowsError(try controller.workspace!.createFile(parentRelPath: "", name: "README.md"))
        XCTAssertEqual(try String(contentsOf: root.appendingPathComponent("README.md"), encoding: .utf8), "# hi\n")
    }

    func testTheExplorerFollowsChangesOnDisk() throws {
        let root = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        controller.open(url: root)
        let explorer = controller.explorer
        explorer.outlineView.expandItem(explorer.outlineView.item(atRow: 1)) // src
        explorer.reveal(relPath: "src/main.rs")
        RunLoop.current.run(until: Date().addingTimeInterval(0.3))

        func waitFor(_ ok: () -> Bool) {
            let deadline = Date().addingTimeInterval(5)
            while !ok(), Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.02)) }
        }
        try "x".write(to: root.appendingPathComponent("src/added.rs"), atomically: true, encoding: .utf8)
        try FileManager.default.removeItem(at: root.appendingPathComponent("docs"))
        waitFor { names(explorer).contains("added.rs") && !names(explorer).contains("docs") }
        XCTAssertEqual(names(explorer), ["src", "deep", "added.rs", "main.rs", "README.md"])
        XCTAssertEqual(explorer.selectedEntry?.relPath, "src/main.rs", "selection kept")
        XCTAssertTrue(explorer.outlineView.isItemExpanded(explorer.outlineView.item(atRow: 0)), "src still open")

        // Changes in folders never listed do not relist the tree.
        let calls = explorer.listCalls
        try "x".write(to: root.appendingPathComponent("src/deep/inner.rs"), atomically: true, encoding: .utf8)
        RunLoop.current.run(until: Date().addingTimeInterval(1))
        XCTAssertEqual(explorer.listCalls, calls, "src/deep is collapsed and was never listed")
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

    // MARK: Stale hover

    private func crossing(_ type: NSEvent.EventType) -> NSEvent {
        NSEvent.enterExitEvent(with: type, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0,
                               context: nil, eventNumber: 0, trackingNumber: 0, userData: nil)!
    }

    func testRecycledRowViewDoesNotKeepItsHoverTint() {
        let row = ExplorerRowView()
        row.mouseEntered(with: crossing(.mouseEntered))
        XCTAssertTrue(row.isHovered)
        // The table recycles the view for another row without a mouseExited.
        row.prepareForReuse()
        XCTAssertFalse(row.isHovered, "a recycled row starts without a hover tint")
    }

    func testRowWithoutAWindowIsNotHovered() {
        let row = ExplorerRowView()
        row.mouseEntered(with: crossing(.mouseEntered))
        row.updateTrackingAreas()
        XCTAssertFalse(row.isHovered, "no window means no pointer over the row")
    }

    // MARK: Indent guides

    func testGuidesAreHiddenUntilThePointerEntersTheExplorer() {
        let outline = ExplorerOutlineView()
        XCTAssertFalse(outline.isPointerInside, "guides start hidden")
        outline.mouseEntered(with: crossing(.mouseEntered))
        XCTAssertTrue(outline.isPointerInside)
        outline.mouseExited(with: crossing(.mouseExited))
        XCTAssertFalse(outline.isPointerInside, "guides hide again when the pointer leaves")
    }

    func testOutlineWithoutAWindowHasNoPointerInside() {
        let outline = ExplorerOutlineView()
        outline.mouseEntered(with: crossing(.mouseEntered))
        outline.updateTrackingAreas()
        XCTAssertFalse(outline.isPointerInside, "no window means no pointer over the explorer")
    }
}
