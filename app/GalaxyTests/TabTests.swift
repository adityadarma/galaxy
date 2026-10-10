import XCTest
@testable import Galaxy

/// M2-06: tabs, preview tabs, dirty markers, reorder and save prompts.
@MainActor
final class TabTests: XCTestCase {
    // Set in setUp, which XCTest calls outside the main actor.
    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-tabs-\(UUID().uuidString)")
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

    private func titles(_ c: MainWindowController) -> [String] { c.tabBar.items.map(\.title) }
    private func file(_ name: String) -> URL { root.appendingPathComponent(name) }
    private func type(_ c: MainWindowController, _ text: String) {
        c.editorView.insertText(text, replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
    }

    /// Opens the workspace and returns the explorer entry for `name`.
    private func entry(_ c: MainWindowController, _ name: String) throws -> (Workspace, FileEntry) {
        if c.workspace == nil { try c.openWorkspace(at: root) }
        let ws = try XCTUnwrap(c.workspace)
        let e = try XCTUnwrap(try ws.listDir(relPath: "").first { $0.name == name })
        return (ws, e)
    }

    func testOpeningFilesAddsTabsAndReopeningSelectsTheExistingOne() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        c.newDocument(nil)
        XCTAssertEqual(titles(c), ["Untitled"])

        try c.openFile(at: file("a.txt"))
        XCTAssertEqual(titles(c), ["a.txt"], "an empty, untouched Untitled tab is reused")
        try c.openFile(at: file("b.txt"))
        XCTAssertEqual(titles(c), ["a.txt", "b.txt"])
        XCTAssertEqual(c.activeIndex, 1)

        try c.openFile(at: file("a.txt"))
        XCTAssertEqual(titles(c), ["a.txt", "b.txt"], "no duplicate tab")
        XCTAssertEqual(c.activeIndex, 0)
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["a.txt"])
        XCTAssertEqual(c.window?.title, "a.txt")

        // New tabs go right after the active one.
        c.newDocument(nil)
        XCTAssertEqual(titles(c), ["a.txt", "Untitled", "b.txt"])
    }

    func testPreviewTabIsReplacedUntilEditedOrPinned() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        let (ws, a) = try entry(c, "a.txt")
        let (_, b) = try entry(c, "b.txt")
        let (_, cc) = try entry(c, "c.txt")

        c.openWorkspaceFile(ws, a, preview: true)
        XCTAssertEqual(titles(c), ["a.txt"])
        XCTAssertEqual(c.tabBar.items.first?.isPreview, true)
        c.openWorkspaceFile(ws, b, preview: true)
        XCTAssertEqual(titles(c), ["b.txt"], "a single click replaces the preview")

        // Editing keeps it.
        type(c, "!")
        XCTAssertEqual(c.tabBar.items.first?.isPreview, false)
        c.openWorkspaceFile(ws, cc, preview: true)
        XCTAssertEqual(titles(c), ["b.txt", "c.txt"])

        // Double-click (pin) on the current preview keeps it too.
        c.openWorkspaceFile(ws, cc, preview: false)
        XCTAssertEqual(c.tabBar.items[1].isPreview, false)
        c.openWorkspaceFile(ws, a, preview: true)
        XCTAssertEqual(titles(c), ["b.txt", "c.txt", "a.txt"])
    }

    func testEachTabKeepsItsOwnTextAndDirtyState() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        c.askSaveChoice = { _ in .discard }
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        type(c, "B")
        XCTAssertEqual(c.tabBar.items.map(\.isEdited), [false, true])
        XCTAssertTrue(c.pathBar.isEdited)
        XCTAssertEqual(c.window?.isDocumentEdited, true, "any unsaved tab marks the window")

        c.selectTab(0)
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["a.txt"])
        XCTAssertFalse(c.pathBar.isEdited)
        XCTAssertEqual(c.window?.isDocumentEdited, true)
        c.showNextTab(nil)
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["Bb.txt"])
        c.showNextTab(nil)
        XCTAssertEqual(c.activeIndex, 0, "wraps around")

        let tab = try XCTUnwrap(c.tabBar.tabViews[1])
        XCTAssertTrue(tab.showsEditedDot)
        XCTAssertTrue(tab.closeButton.isHidden, "the dot takes the close button's place")
    }

    func testTabsAndBreadcrumbShowTheFileTypeGlyph() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        let tab = try XCTUnwrap(c.tabBar.tabViews.first)
        XCTAssertNotNil(tab.icon.image, "tab shows the file glyph")
        XCTAssertLessThan(tab.icon.frame.maxX, tab.label.frame.minX + 1, "glyph before the name")
        XCTAssertTrue(c.pathBar.label.attributedStringValue.containsAttachments, "breadcrumb has chevrons and glyph")
        XCTAssertEqual(c.pathBar.label.accessibilityLabel(), c.pathBar.components.joined(separator: " / "))
    }

    func testUndoingBackToTheSavedTextClearsTheDots() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        let (ws, e) = try entry(c, "a.txt")
        c.openWorkspaceFile(ws, e, preview: false)
        type(c, "X")
        XCTAssertEqual(c.tabBar.items.map(\.isEdited), [true])
        XCTAssertEqual(c.explorer.dirtyPaths, ["a.txt"])

        c.editorView.undoEdit(nil)
        c.editorView.flushPendingUpdates()
        XCTAssertEqual(c.tabBar.items.map(\.isEdited), [false], "tab dot goes away")
        XCTAssertFalse(c.pathBar.isEdited)
        XCTAssertEqual(c.explorer.dirtyPaths, [], "sidebar dot goes away")
        XCTAssertEqual(c.window?.isDocumentEdited, false)

        c.editorView.redoEdit(nil)
        c.editorView.flushPendingUpdates()
        XCTAssertEqual(c.tabBar.items.map(\.isEdited), [true], "redo makes it dirty again")
    }

    func testClosingTabsPromptsOnlyForUnsavedOnesAndPicksANeighbour() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        try c.openFile(at: file("c.txt"))

        c.selectTab(1)
        XCTAssertTrue(c.closeTab(at: 1), "clean tabs close without asking")
        XCTAssertEqual(titles(c), ["a.txt", "c.txt"])
        XCTAssertEqual(c.activeIndex, 1, "the tab to the right takes over")

        type(c, "x")
        var asked: [String] = []
        var answer = MainWindowController.SaveChoice.cancel
        c.askSaveChoice = { name in asked.append(name); return answer }
        XCTAssertFalse(c.closeTab(at: 1))
        XCTAssertEqual(asked, ["c.txt"])
        XCTAssertEqual(titles(c), ["a.txt", "c.txt"], "Cancel keeps the tab")

        answer = .save
        c.closeTab(nil)
        XCTAssertEqual(titles(c), ["a.txt"])
        XCTAssertEqual(try String(contentsOf: file("c.txt"), encoding: .utf8), "xc.txt")

        c.closeTab(nil)
        XCTAssertTrue(c.tabs.isEmpty)
        XCTAssertNil(c.editorView.document)
        XCTAssertEqual(c.statusBar.cursor.stringValue, "")
    }

    func testClosingAnInactiveTabKeepsTheActiveOne() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        try c.openFile(at: file("c.txt"))
        XCTAssertTrue(c.closeTab(at: 0))
        XCTAssertEqual(titles(c), ["b.txt", "c.txt"])
        XCTAssertEqual(c.activeIndex, 1)
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["c.txt"])
    }

    func testClosingTheWindowAsksForEveryUnsavedTab() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        type(c, "1")
        try c.openFile(at: file("b.txt"))
        try c.openFile(at: file("c.txt"))
        type(c, "3")

        var asked: [String] = []
        c.askSaveChoice = { name in asked.append(name); return name == "a.txt" ? .discard : .cancel }
        XCTAssertFalse(c.windowShouldClose(c.window!))
        XCTAssertEqual(asked, ["a.txt", "c.txt"], "in tab order, stopping at Cancel")
        XCTAssertEqual(c.activeIndex, 2, "the tab being asked about is shown")

        asked = []
        c.askSaveChoice = { name in asked.append(name); return .discard }
        XCTAssertTrue(c.windowShouldClose(c.window!))
    }

    func testDraggingReordersTabs() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        try c.openFile(at: file("c.txt"))
        c.selectTab(0)

        c.moveTab(from: 0, to: 2)
        XCTAssertEqual(titles(c), ["b.txt", "c.txt", "a.txt"])
        XCTAssertEqual(c.activeIndex, 2, "the active tab moves with its document")
        XCTAssertEqual(c.editorView.document?.lines(first: 0, count: 1), ["a.txt"])

        // Drop index from an x position: past the middle of the last tab.
        c.window?.contentView?.layoutSubtreeIfNeeded()
        let bar = c.tabBar
        let last = bar.convert(bar.tabViews[2].bounds, from: bar.tabViews[2])
        XCTAssertEqual(bar.dropIndex(forX: last.maxX + 10, from: 0), 2)
        XCTAssertEqual(bar.dropIndex(forX: 0, from: 2), 0)
        let first = bar.convert(bar.tabViews[0].bounds, from: bar.tabViews[0])
        XCTAssertEqual(bar.dropIndex(forX: first.midX + 1, from: 0), 0, "barely moved: stays put")
    }

    func testScrollPositionIsRestoredPerTab() throws {
        let long = (0..<500).map { "line \($0)" }.joined(separator: "\n")
        try long.write(to: file("long.txt"), atomically: true, encoding: .utf8)
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("long.txt"))
        let clip = try XCTUnwrap(c.editorView.enclosingScrollView?.contentView)
        clip.scroll(to: NSPoint(x: 0, y: 2000))
        try c.openFile(at: file("a.txt"))
        XCTAssertLessThan(clip.bounds.origin.y, 1, "a new tab starts at the top")
        c.selectTab(0)
        XCTAssertEqual(clip.bounds.origin.y, 2000, accuracy: 1)
    }

    func testTabMenuShortcuts() throws {
        let menus = MainMenu.build().items.compactMap(\.submenu)
        let file = try XCTUnwrap(menus.first { $0.title == "File" })
        let closeTab = try XCTUnwrap(file.item(withTitle: "Close Tab"))
        XCTAssertEqual(closeTab.keyEquivalent, "w")
        XCTAssertEqual(closeTab.keyEquivalentModifierMask, .command)
        let closeWindow = try XCTUnwrap(file.item(withTitle: "Close Window"))
        XCTAssertEqual(closeWindow.keyEquivalentModifierMask, [.command, .shift])
        let view = try XCTUnwrap(menus.first { $0.title == "View" })
        XCTAssertEqual(view.item(withTitle: "Show Next Tab")?.keyEquivalent, "]")
        XCTAssertEqual(view.item(withTitle: "Show Previous Tab")?.keyEquivalent, "[")
    }
}
