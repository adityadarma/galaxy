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

    func testDiffHunkButtonsRevertOneChangeAndStageAnother() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        c.openWorkspaceDiff(try XCTUnwrap(c.workspace), relPath: "tracked.txt")
        waitForDiff(c)
        XCTAssertEqual(c.diffMap.hunks.count, 2)
        XCTAssertFalse(c.diffHunkActions.isHidden)
        XCTAssertEqual(c.diffHunkActions.hunkButtons.count, 4)

        let revert = try XCTUnwrap(c.diffHunkActions.hunkButtons.first { $0.tag == 0 && $0.toolTip == "Revert Block" })
        let stage = try XCTUnwrap(c.diffHunkActions.hunkButtons.first { $0.tag == 0 && $0.toolTip == "Stage Block" })
        XCTAssertEqual(revert.frame.minX, stage.frame.minX)
        XCTAssertLessThan(revert.frame.minY, stage.frame.minY, "stage sits below revert")
        XCTAssertGreaterThanOrEqual(revert.frame.minX, c.secondaryScrollView.frame.maxX, "clear of the left scrollbar")
        XCTAssertLessThanOrEqual(revert.frame.maxX, c.scrollView.frame.minX, "clear of the right pane")

        let split = c.editorSplitView
        let gutter = try XCTUnwrap(split.diffGutterRect)
        let regions = split.diffGutterCursorRegions()
        XCTAssertFalse(regions.drag.isEmpty)
        for drag in regions.drag {
            XCTAssertEqual(drag.width, EditorSplitView.diffHandleWidth, "only the line resizes")
            XCTAssertEqual(drag.maxX, gutter.maxX, accuracy: 0.5, "the line runs down the gutter's right edge")
            for button in [revert, stage] {
                let frame = c.diffHunkActions.convert(button.frame, to: split)
                XCTAssertFalse(drag.intersects(frame), "buttons never show the resize cursor")
            }
        }
        XCTAssertTrue(regions.arrow.contains { $0.minX == gutter.minX }, "rest of the gutter uses the arrow")
        XCTAssertFalse(revert.frame.intersects(stage.frame), "stacked buttons do not overlap")
        XCTAssertFalse(revert.isBordered, "a bezel would paint past the frame")
        let handleX = gutter.maxX - EditorSplitView.diffHandleWidth
        for button in [revert, stage] {
            XCTAssertLessThanOrEqual(c.diffHunkActions.convert(button.frame, to: split).maxX, handleX,
                                     "buttons sit left of the line")
        }

        stage.performClick(nil)
        let deadline = Date().addingTimeInterval(5)
        while c.statusLabel.stringValue != "Staged change.", Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.02))
        }
        XCTAssertEqual(c.statusLabel.stringValue, "Staged change.")
        XCTAssertEqual(c.activeTab?.doc.text(), "one\nTWO\nthree\nfour\nfive\n")

        try XCTUnwrap(c.diffHunkActions.hunkButtons.first {
            $0.tag == 1 && $0.toolTip == "Revert Block"
        }).performClick(nil)
        XCTAssertEqual(c.activeTab?.doc.text(), "one\nTWO\nthree\nfour\n")
        XCTAssertTrue(c.activeTab?.doc.isModified() ?? false)
    }

    func testRevertHunkPreservesNewlinesAtFileEdges() throws {
        func revert(_ old: String, _ new: String, _ hunk: DiffLineMap.Hunk,
                    file: StaticString = #filePath, line: UInt = #line) throws -> String {
            let edit = try XCTUnwrap(MainWindowController.revertedDiffHunk(hunk, base: old, working: new),
                                     file: file, line: line)
            let result = (new as NSString).replacingCharacters(
                in: NSRange(location: Int(edit.range.location), length: Int(edit.range.length)), with: edit.text)
            return result
        }
        XCTAssertEqual(try revert("a\nb\nc\n", "a\nB\nc\n", .init(oldStart: 1, oldLen: 1, newStart: 1, newLen: 1)), "a\nb\nc\n")
        XCTAssertEqual(try revert("a\nc\n", "a\nb\nc\n", .init(oldStart: 1, oldLen: 0, newStart: 1, newLen: 1)), "a\nc\n")
        XCTAssertEqual(try revert("a\nb\nc\n", "a\nc\n", .init(oldStart: 1, oldLen: 1, newStart: 1, newLen: 0)), "a\nb\nc\n")
        XCTAssertEqual(try revert("a\nb\n", "a\n", .init(oldStart: 1, oldLen: 1, newStart: 1, newLen: 0)), "a\nb\n")
        XCTAssertEqual(try revert("a\nb", "a", .init(oldStart: 1, oldLen: 1, newStart: 1, newLen: 0)), "a\nb")
        XCTAssertEqual(try revert("a", "a\nb", .init(oldStart: 1, oldLen: 0, newStart: 1, newLen: 1)), "a")
        XCTAssertEqual(try revert("", "a\n", .init(oldStart: 0, oldLen: 0, newStart: 0, newLen: 1)), "")
    }

    func testDiffHunkButtonsFollowScrollAndDisappearOutsideDiff() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        let ws = try XCTUnwrap(c.workspace)
        c.openWorkspaceDiff(ws, relPath: "tracked.txt")
        waitForDiff(c)
        c.window?.contentView?.layoutSubtreeIfNeeded()
        c.layoutDiffHunkActions()
        let before = try XCTUnwrap(c.diffHunkActions.hunkButtons.first?.frame.minY)
        c.scrollView.contentView.scroll(to: NSPoint(x: 0, y: 20))
        c.scrollView.reflectScrolledClipView(c.scrollView.contentView)
        c.layoutDiffHunkActions()
        let after = try XCTUnwrap(c.diffHunkActions.hunkButtons.first?.frame.minY)
        if c.scrollView.contentView.bounds.minY > 0 { XCTAssertLessThan(after, before) }

        // A button scrolled partly above the editor is cut off at its top
        // edge and cannot be clicked through the tab bar.
        let actions = c.diffHunkActions
        XCTAssertTrue(actions.clipsToBounds)
        let button = try XCTUnwrap(actions.hunkButtons.first)
        button.frame.origin.y = -button.frame.height / 2
        let above = actions.convert(NSPoint(x: button.frame.midX, y: -2), to: actions.superview)
        XCTAssertNil(actions.hitTest(above))
        let inside = actions.convert(NSPoint(x: button.frame.midX, y: 2), to: actions.superview)
        XCTAssertTrue(actions.hitTest(inside) === button)

        c.openWorkspaceFile(ws, FileEntry(name: "other.txt", relPath: "other.txt", isDir: false), preview: false)
        XCTAssertTrue(c.diffHunkActions.isHidden)
        XCTAssertTrue(c.diffHunkActions.hunkButtons.isEmpty)
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
        XCTAssertEqual(map.oldSpacers, [.init(beforeLine: 7, rows: 2)])
        XCTAssertEqual(map.newSpacers, [.init(beforeLine: 3, rows: 1)])
        XCTAssertEqual(map.visualRow(forOld: 7), 9)
        XCTAssertEqual(map.visualRow(forNew: 3), 4)
        XCTAssertEqual(DiffLineMap().oldLine(forNew: 5), 5)
    }

    func testSpacerRowsGiveBothSidesTheSameVisualHeight() {
        let map = DiffLineMap(hunks: [
            .init(oldStart: 1, oldLen: 4, newStart: 1, newLen: 1),
            .init(oldStart: 8, oldLen: 0, newStart: 5, newLen: 2),
        ])
        let oldLines = 10
        let newLines = 9
        let oldRows = oldLines + map.oldSpacers.reduce(0) { $0 + $1.rows }
        let newRows = newLines + map.newSpacers.reduce(0) { $0 + $1.rows }
        XCTAssertEqual(oldRows, newRows)
        XCTAssertEqual(map.visualRow(forOld: 5), map.visualRow(forNew: 2),
                       "the unchanged line after a 4-for-1 hunk aligns")
        XCTAssertEqual(map.visualRow(forOld: 8), map.visualRow(forNew: 7),
                       "the unchanged line after a pure insertion aligns")
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
        XCTAssertEqual(c.secondaryEditorView.diffSpacers, [.init(beforeLine: 4, rows: 1)])
        XCTAssertTrue(c.editorView.diffSpacers.isEmpty)
        XCTAssertEqual(c.secondaryEditorView.diffSide, .removed)
        XCTAssertEqual(c.editorView.diffSide, .added)
        XCTAssertEqual(c.secondaryEditorView.diffMarker(for: 1), "-")
        XCTAssertEqual(c.editorView.diffMarker(for: 1), "+")
        XCTAssertNil(c.editorView.diffMarker(for: 0), "unchanged lines have no marker")
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
        XCTAssertNil(c.editorView.diffSide)
        XCTAssertNil(c.secondaryEditorView.diffSide)
        XCTAssertNil(c.secondaryEditorView.document)

        c.selectTab(0)
        XCTAssertTrue(c.isDiffPaneShown, "back on the diff tab")
        XCTAssertEqual(c.secondaryEditorView.document?.text(), "one\ntwo\nthree\nfour\n")
    }

    func testDiffScrollbarsRemainVisibleAndDoNotCoverTheOverviewRuler() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        let ws = try XCTUnwrap(c.workspace)
        c.openWorkspaceDiff(ws, relPath: "tracked.txt")
        waitForDiff(c)
        c.window?.contentView?.layoutSubtreeIfNeeded()

        for scroll in [c.secondaryScrollView, c.scrollView] {
            XCTAssertTrue(scroll.hasVerticalScroller)
            XCTAssertEqual(scroll.scrollerStyle, .legacy)
            XCTAssertFalse(scroll.autohidesScrollers)
        }
        let scroller = try XCTUnwrap(c.scrollView.verticalScroller)
        XCTAssertFalse(scroller.isHidden)
        XCTAssertEqual(c.scrollView.ruler.frame.maxX, scroller.frame.minX, accuracy: 1)
        for scroll in [c.secondaryScrollView, c.scrollView] {
            let vertical = try XCTUnwrap(scroll.verticalScroller)
            let backdrop = scroll.verticalScrollerBackdrop
            XCTAssertFalse(backdrop.isHidden, "text must not show through the scroller track")
            XCTAssertEqual(backdrop.frame, vertical.frame)
            let subviews = scroll.subviews
            XCTAssertLessThan(try XCTUnwrap(subviews.firstIndex(of: backdrop)),
                              try XCTUnwrap(subviews.firstIndex(of: vertical)))
            XCTAssertGreaterThan(try XCTUnwrap(subviews.firstIndex(of: backdrop)),
                                 try XCTUnwrap(subviews.firstIndex(of: scroll.contentView)))
        }

        c.openWorkspaceFile(ws, FileEntry(name: "other.txt", relPath: "other.txt", isDir: false), preview: false)
        XCTAssertEqual(c.scrollView.scrollerStyle, .overlay)
        XCTAssertTrue(c.scrollView.autohidesScrollers)
        c.window?.contentView?.layoutSubtreeIfNeeded()
        XCTAssertTrue(c.scrollView.verticalScrollerBackdrop.isHidden, "overlay scrollers keep floating over text")
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

    /// Scrolling sideways in either pane moves the other, even when only one
    /// side has a long line.
    func testHorizontalScrollFollowsInBothDirections() throws {
        let long = String(repeating: "wide ", count: 400)
        try "one\n\(long)\nthree\nfour\nfive\n".write(
            to: root.appendingPathComponent("tracked.txt"), atomically: true, encoding: .utf8)
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        c.openWorkspaceDiff(try XCTUnwrap(c.workspace), relPath: "tracked.txt")
        waitForDiff(c)
        c.window?.contentView?.layoutSubtreeIfNeeded()
        c.editorView.updateDocumentWidth()
        let left = try XCTUnwrap(c.secondaryEditorView.enclosingScrollView)
        let right = try XCTUnwrap(c.editorView.enclosingScrollView)
        XCTAssertGreaterThan(c.secondaryEditorView.frame.width, left.contentSize.width,
                             "the short side can scroll as far as the long one")

        right.contentView.scroll(to: NSPoint(x: 300, y: right.contentView.bounds.origin.y))
        right.reflectScrolledClipView(right.contentView)
        XCTAssertEqual(left.contentView.bounds.origin.x, 300, accuracy: 0.5, "left follows right")

        left.contentView.scroll(to: NSPoint(x: 120, y: left.contentView.bounds.origin.y))
        left.reflectScrolledClipView(left.contentView)
        XCTAssertEqual(right.contentView.bounds.origin.x, 120, accuracy: 0.5, "right follows left")
    }

    /// Both panes draw the line beside their numbers the same way, and it
    /// goes with the text when they scroll sideways.
    func testGutterLineLooksTheSameInBothPanes() throws {
        let body = (0..<8).map { "    " + String(repeating: "wide ", count: 120) + "\($0)" }
        try (["one"] + body).joined(separator: "\n").appending("\n").write(
            to: root.appendingPathComponent("tracked.txt"), atomically: true, encoding: .utf8)
        try git("add", "tracked.txt")
        try git("-c", "user.name=T", "-c", "user.email=t@example.com", "commit", "-m", "wide")
        try (["ONE"] + body).joined(separator: "\n").appending("\n").write(
            to: root.appendingPathComponent("tracked.txt"), atomically: true, encoding: .utf8)
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        c.openWorkspaceDiff(try XCTUnwrap(c.workspace), relPath: "tracked.txt")
        waitForDiff(c)
        c.window?.contentView?.layoutSubtreeIfNeeded()
        c.editorView.updateDocumentWidth()
        let left = try XCTUnwrap(c.secondaryEditorView.enclosingScrollView)
        let right = try XCTUnwrap(c.editorView.enclosingScrollView)

        /// Luminance across the gutter line of the second text row.
        func profile(_ view: EditorView) throws -> [Int] {
            let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
            view.cacheDisplay(in: view.bounds, to: rep)
            let scale = CGFloat(rep.pixelsWide) / view.bounds.width
            let y = Int((view.metrics.lineHeight * 2.5) * scale)
            let edge = view.gutterShift + view.gutterWidth
            return (-6...2).map { dx in
                let x = Int((edge + CGFloat(dx)) * scale)
                let color = rep.colorAt(x: x, y: y)?.usingColorSpace(.sRGB)
                return Int(((color?.brightnessComponent ?? 0) * 255).rounded())
            }
        }
        let past = c.editorView.gutterWidth + 10
        for x in [CGFloat(0), past] {
            for scroll in [left, right] {
                scroll.contentView.scroll(to: NSPoint(x: x, y: 0))
                scroll.reflectScrolledClipView(scroll.contentView)
            }
            let l = try profile(c.secondaryEditorView), r = try profile(c.editorView)
            XCTAssertEqual(c.secondaryEditorView.gutterShift, x, accuracy: 0.5)
            XCTAssertEqual(c.editorView.gutterShift, x, accuracy: 0.5)
            XCTAssertEqual(l, r, "scrolled \(x): left \(l) vs right \(r)")
            // Scrolled past the numbers the line is gone with the text. Only
            // samples up to the edge count: right of it, scrolled text can
            // put a glyph under the sample, depending on the font metrics.
            let gutterSide = l.prefix(7)
            XCTAssertEqual(gutterSide.filter { $0 != l[0] }.count, x == 0 ? 1 : 0, "scrolled \(x): \(l)")
        }
    }

    /// Only the working side (right) carries the overview ruler; the committed
    /// side (left) is a plain scroll view.
    func testOnlyTheWorkingSideShowsTheOverviewRuler() throws {
        let c = try makeController()
        defer { c.window?.orderOut(nil) }
        c.openWorkspaceDiff(try XCTUnwrap(c.workspace), relPath: "tracked.txt")
        waitForDiff(c)
        c.window?.contentView?.layoutSubtreeIfNeeded()
        let right = c.scrollView
        XCTAssertTrue(right.ruler.superview === right)
        XCTAssertFalse(right.ruler.isHidden, "the working side keeps its ruler")
        let scroller = try XCTUnwrap(right.verticalScroller)
        XCTAssertEqual(right.ruler.frame.maxX, scroller.frame.minX, accuracy: 0.5)
        XCTAssertEqual(scroller.frame.maxX, right.bounds.width, accuracy: 0.5)
        XCTAssertNil(c.secondaryScrollView.ruler.superview, "the committed side has none")
        XCTAssertEqual(c.secondaryEditorView.rightInset, 0)
        XCTAssertTrue(c.minimap.isHidden, "no minimap in a diff")
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
