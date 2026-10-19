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

    func testInactiveSyntaxIsEvictedAndResumedWhenSelected() throws {
        try "fn first() {}".write(to: file("first.rs"), atomically: true, encoding: .utf8)
        try "fn second() {}".write(to: file("second.rs"), atomically: true, encoding: .utf8)
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("first.rs"))
        try c.openFile(at: file("second.rs"))
        let first = c.tabs[0]
        XCTAssertTrue(first.doc.syntaxActive())
        first.lastActiveAt = Date.now.addingTimeInterval(-MainWindowController.syntaxEvictionInterval)
        c.evictInactiveSyntax(now: .now)
        XCTAssertFalse(first.doc.syntaxActive())
        c.selectTab(0)
        XCTAssertTrue(first.doc.syntaxActive())
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

    func testDoubleClickingAPreviewTabKeepsItAndDropsTheItalic() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        let (ws, a) = try entry(c, "a.txt")
        let (_, b) = try entry(c, "b.txt")

        c.openWorkspaceFile(ws, a, preview: true)
        XCTAssertEqual(c.tabBar.items.first?.isPreview, true)
        c.tabBar.onKeep?(0)
        XCTAssertEqual(c.tabBar.items.first?.isPreview, false, "no longer italic")
        c.openWorkspaceFile(ws, b, preview: true)
        XCTAssertEqual(titles(c), ["a.txt", "b.txt"], "the next preview no longer replaces it")
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

    private func perform(_ menu: NSMenu, _ title: String) throws {
        let item = try XCTUnwrap(menu.item(withTitle: title), title)
        XCTAssertTrue(item.isEnabled, title)
        _ = (item.target as? NSObject)?.perform(item.action, with: item)
    }

    func testTabMenuClosesRelativeToTheClickedTab() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        for name in ["a.txt", "b.txt", "c.txt"] { try c.openFile(at: file(name)) }
        let menu = try XCTUnwrap(c.menu(forTab: 1))
        XCTAssertEqual(menu.items.filter { !$0.isSeparatorItem }.map(\.title),
                       ["Close", "Close Others", "Close to the Right", "Close Saved", "Close All", "Pin",
                        "Copy Path", "Copy Relative Path", "Reveal in Finder", "Reveal in Explorer View",
                        "Split Right", "Split Down"])
        XCTAssertEqual(menu.item(withTitle: "Close Others")?.keyEquivalent, "t", "shortcut shown")
        XCTAssertFalse(try XCTUnwrap(c.menu(forTab: 2)?.item(withTitle: "Close to the Right")).isEnabled,
                       "nothing right of the last tab")

        try perform(menu, "Close to the Right")
        XCTAssertEqual(titles(c), ["a.txt", "b.txt"])
        try perform(try XCTUnwrap(c.menu(forTab: 1)), "Close Others")
        XCTAssertEqual(titles(c), ["b.txt"])
        try perform(try XCTUnwrap(c.menu(forTab: 0)), "Close")
        XCTAssertTrue(c.tabBar.items.isEmpty)
    }

    func testCloseSavedKeepsUnsavedTabsAndCloseAllStopsOnCancel() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        for name in ["a.txt", "b.txt", "c.txt"] { try c.openFile(at: file(name)) }
        c.selectTab(1)
        type(c, "edit")
        try perform(try XCTUnwrap(c.menu(forTab: 0)), "Close Saved")
        XCTAssertEqual(titles(c), ["b.txt"], "only the unsaved tab is left")

        c.askSaveChoice = { _ in .cancel }
        try perform(try XCTUnwrap(c.menu(forTab: 0)), "Close All")
        XCTAssertEqual(titles(c), ["b.txt"], "cancel keeps it")
        c.askSaveChoice = { _ in .discard }
        c.closeAllTabs(nil)
        XCTAssertTrue(c.tabBar.items.isEmpty)
    }

    func testTabMenuCopiesPathsAndRevealsInExplorer() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openWorkspace(at: root)
        try c.openFile(at: file("a.txt"))
        try c.openFile(at: file("b.txt"))
        let menu = try XCTUnwrap(c.menu(forTab: 0))
        let saved = NSPasteboard.general.string(forType: .string)
        defer {
            NSPasteboard.general.clearContents()
            if let saved { NSPasteboard.general.setString(saved, forType: .string) }
        }
        try perform(menu, "Copy Path")
        XCTAssertEqual(NSPasteboard.general.string(forType: .string).map { URL(fileURLWithPath: $0).resolvingSymlinksInPath() },
                       file("a.txt").resolvingSymlinksInPath(), "the clicked tab, not the active one")
        try perform(menu, "Copy Relative Path")
        XCTAssertEqual(NSPasteboard.general.string(forType: .string), "a.txt")
        try perform(menu, "Reveal in Explorer View")
        XCTAssertEqual(c.explorer.selectedEntry?.relPath, "a.txt")

        let untitled = MainWindowController(bridge: CoreBridge())
        untitled.newDocument(nil)
        let plain = try XCTUnwrap(untitled.menu(forTab: 0))
        XCTAssertFalse(try XCTUnwrap(plain.item(withTitle: "Copy Path")).isEnabled, "an Untitled buffer has no path")
        XCTAssertFalse(try XCTUnwrap(plain.item(withTitle: "Reveal in Finder")).isEnabled)
    }

    /// The menu a real right click on tab `index` gets, through the tab view.
    private func rightClickMenu(_ c: MainWindowController, tab index: Int) throws -> NSMenu {
        c.window?.contentView?.layoutSubtreeIfNeeded()
        let view = c.tabBar.tabViews[index]
        let point = view.convert(NSPoint(x: view.bounds.midX, y: view.bounds.midY), to: nil)
        let event = try XCTUnwrap(NSEvent.mouseEvent(with: .rightMouseDown, location: point, modifierFlags: [],
                                                     timestamp: 0, windowNumber: c.window?.windowNumber ?? 0,
                                                     context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        return try XCTUnwrap(view.menu(for: event), "no menu on tab \(index)")
    }

    /// Picks an item the way AppKit does when it is clicked.
    private func choose(_ menu: NSMenu, _ title: String) throws {
        let index = menu.indexOfItem(withTitle: title)
        XCTAssertGreaterThanOrEqual(index, 0, title)
        XCTAssertTrue(menu.items[index].isEnabled, "\(title) is disabled")
        menu.performActionForItem(at: index)
    }

    /// The tab bar sits in the transparent title bar; a click there must
    /// still land on the tab view, not on the window frame.
    func testRightClickOnATabHitsTheTabView() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        for name in ["a.txt", "b.txt"] { try c.openFile(at: file(name)) }
        c.window?.contentView?.layoutSubtreeIfNeeded()
        let frame = try XCTUnwrap(c.window?.contentView?.superview)
        for (i, view) in c.tabBar.tabViews.enumerated() {
            let point = view.convert(NSPoint(x: view.bounds.midX - 20, y: view.bounds.midY), to: nil)
            let hit = frame.hitTest(frame.superview?.convert(point, from: nil) ?? point)
            var v = hit
            while let current = v, !(current is TabItemView) { v = current.superview }
            XCTAssertTrue(v === view, "tab \(i): hit \(String(describing: hit))")
        }
    }

    /// Right click in the editor text gets VSCode's editor menu, and a click
    /// outside the selection moves the caret under the pointer first.
    func testEditorContextMenuHasVSCodeItems() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: file("a.txt"))
        c.window?.contentView?.layoutSubtreeIfNeeded()

        let menu = try XCTUnwrap(c.editorContextMenu())
        let titles = menu.items.filter { !$0.isSeparatorItem }.map(\.title)
        XCTAssertEqual(titles, [
            "Go to Definition", "Go to Type Definition", "Go to Implementations", "Go to References",
            "Find All References", "Rename Symbol", "Change All Occurrences", "Format Document",
            "Refactor…", "Source Action…", "Cut", "Copy", "Paste", "Command Palette…",
        ])
        XCTAssertEqual(menu.items.filter(\.isSeparatorItem).count, 4)

        let local = NSPoint(x: c.editorView.textOriginX + 2, y: c.editorView.metrics.lineHeight / 2)
        let point = c.editorView.convert(local, to: nil)
        let event = try XCTUnwrap(NSEvent.mouseEvent(with: .rightMouseDown, location: point, modifierFlags: [],
                                                     timestamp: 0, windowNumber: c.window?.windowNumber ?? 0,
                                                     context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        c.editorView.document?.selectAll()
        XCTAssertNotNil(c.editorView.menu(for: event))
        XCTAssertEqual(c.editorView.selectedRange().length, 5, "a click inside the selection keeps it")
    }

    func testPinnedTabsMoveFirstAndOnlyCloseThemselves() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        for name in ["a.txt", "b.txt", "c.txt"] { try c.openFile(at: file(name)) }

        try choose(try rightClickMenu(c, tab: 2), "Pin")
        XCTAssertEqual(titles(c), ["c.txt", "a.txt", "b.txt"], "pinned tab moves first")
        XCTAssertTrue(c.tabBar.items[0].isPinned)
        XCTAssertEqual(try rightClickMenu(c, tab: 0).item(withTitle: "Unpin")?.title, "Unpin")

        // Close Others/All from another tab skip the pinned one.
        try choose(try rightClickMenu(c, tab: 1), "Close Others")
        XCTAssertEqual(titles(c), ["c.txt", "a.txt"])
        try choose(try rightClickMenu(c, tab: 1), "Close All")
        XCTAssertEqual(titles(c), ["c.txt"])
        XCTAssertFalse(try XCTUnwrap(try rightClickMenu(c, tab: 0).item(withTitle: "Close All")).isEnabled,
                       "only pinned tabs left")

        // New tabs open after the pinned ones; dragging cannot cross the boundary.
        try c.openFile(at: file("a.txt"))
        c.selectTab(0)
        try c.openFile(at: file("b.txt"))
        XCTAssertEqual(titles(c), ["c.txt", "b.txt", "a.txt"], "opened from the pinned tab: right after it")
        c.moveTab(from: 2, to: 0)
        XCTAssertEqual(titles(c), ["c.txt", "a.txt", "b.txt"], "stops at the pinned boundary")

        // Its own Close still closes it.
        try choose(try rightClickMenu(c, tab: 0), "Close")
        XCTAssertEqual(titles(c), ["a.txt", "b.txt"])
    }

    func testClickingThePinUnpins() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        for name in ["a.txt", "b.txt"] { try c.openFile(at: file(name)) }
        c.setPinned(true, tab: c.tabs[1])
        XCTAssertEqual(titles(c), ["b.txt", "a.txt"])
        let button = c.tabBar.tabViews[0].closeButton
        XCTAssertFalse(button.isHidden, "the pin is always shown")
        button.performClick(nil)
        XCTAssertEqual(titles(c), ["b.txt", "a.txt"], "unpinned, not closed")
        XCTAssertFalse(c.tabs[0].isPinned)
    }

    func testRightClickMenuItemsWorkThroughAppKit() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openWorkspace(at: root)
        for name in ["a.txt", "b.txt", "c.txt"] { try c.openFile(at: file(name)) }
        let saved = NSPasteboard.general.string(forType: .string)
        defer {
            NSPasteboard.general.clearContents()
            if let saved { NSPasteboard.general.setString(saved, forType: .string) }
        }

        try choose(try rightClickMenu(c, tab: 0), "Copy Relative Path")
        XCTAssertEqual(NSPasteboard.general.string(forType: .string), "a.txt")
        try choose(try rightClickMenu(c, tab: 1), "Copy Path")
        XCTAssertEqual(NSPasteboard.general.string(forType: .string).map { URL(fileURLWithPath: $0).lastPathComponent }, "b.txt")
        try choose(try rightClickMenu(c, tab: 0), "Reveal in Explorer View")
        XCTAssertEqual(c.explorer.selectedEntry?.relPath, "a.txt")
        try choose(try rightClickMenu(c, tab: 1), "Split Right")
        XCTAssertTrue(c.isEditorSplit)
        c.closeEditorSplit(nil)

        try choose(try rightClickMenu(c, tab: 1), "Close to the Right")
        XCTAssertEqual(titles(c), ["a.txt", "c.txt"], "b.txt moved to the split and joined back last")
        try choose(try rightClickMenu(c, tab: 1), "Close Others")
        XCTAssertEqual(titles(c), ["c.txt"])
        try c.openFile(at: file("c.txt"))
        try choose(try rightClickMenu(c, tab: 0), "Close Saved")
        XCTAssertTrue(c.tabBar.items.isEmpty)
        for name in ["a.txt", "b.txt"] { try c.openFile(at: file(name)) }
        try choose(try rightClickMenu(c, tab: 0), "Close")
        XCTAssertEqual(titles(c), ["b.txt"])
        try choose(try rightClickMenu(c, tab: 0), "Close All")
        XCTAssertTrue(c.tabBar.items.isEmpty)
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

    // MARK: Crowded tab bar

    private func laidOutTab(title: String, width: CGFloat, selected: Bool = true) -> TabItemView {
        let tab = TabItemView(item: TabBar.Item(title: title, isPreview: false, isEdited: false, isGitChanged: false))
        tab.isSelected = selected
        let host = NSView(frame: NSRect(x: 0, y: 0, width: width, height: Theme.tabHeight))
        host.addSubview(tab)
        tab.frame = host.bounds
        host.layoutSubtreeIfNeeded()
        tab.layoutSubtreeIfNeeded()
        return tab
    }

    private func crowdedBar(tabs: Int, width: CGFloat) -> TabBar {
        let bar = TabBar(frame: NSRect(x: 0, y: 0, width: width, height: Theme.tabHeight))
        let items = (0..<tabs).map {
            TabBar.Item(title: "file-number-\($0).ts", isPreview: false, isEdited: false, isGitChanged: false)
        }
        bar.set(items: items, selected: 0)
        bar.layoutSubtreeIfNeeded()
        return bar
    }

    func testManyTabsKeepTheirWidthAndTheStripScrolls() {
        let bar = crowdedBar(tabs: 30, width: 600)
        XCTAssertGreaterThan(bar.contentWidth, 600, "tabs are not squeezed to fit")
        XCTAssertEqual(bar.scrollView.frame.width, 600, accuracy: 0.5, "the strip fills the bar")
        XCTAssertEqual(bar.scrollView.documentView?.frame.width ?? 0, bar.contentWidth, accuracy: 0.5)
        XCTAssertTrue(bar.tabViews.allSatisfy { $0.frame.width >= TabBar.minTabWidth - 0.5 }, "no tab is squeezed under the minimum")
        XCTAssertEqual(bar.scrollView.contentView.bounds.origin.x, 0)
    }

    func testActiveTabIsScrolledIntoView() {
        let bar = crowdedBar(tabs: 30, width: 600)
        let items = bar.items
        bar.set(items: items, selected: 29)
        bar.layoutSubtreeIfNeeded()
        let clip = bar.scrollView.contentView.bounds
        XCTAssertGreaterThan(clip.origin.x, 0)
        let last = bar.tabViews[29].frame
        XCTAssertGreaterThanOrEqual(last.minX, clip.minX - 0.5)
        XCTAssertLessThanOrEqual(last.maxX, clip.maxX + 0.5, "the last tab is fully visible")

        bar.set(items: items, selected: 0)
        bar.layoutSubtreeIfNeeded()
        XCTAssertEqual(bar.scrollView.contentView.bounds.origin.x, 0, accuracy: 0.5, "back to the first tab")
    }

    func testMouseWheelScrollsTheTabsSideways() throws {
        let bar = crowdedBar(tabs: 30, width: 600)
        let cg = try XCTUnwrap(CGEvent(scrollWheelEvent2Source: nil, units: .line, wheelCount: 1, wheel1: -3, wheel2: 0, wheel3: 0))
        let wheel = try XCTUnwrap(NSEvent(cgEvent: cg))
        bar.scrollView.scrollWheel(with: wheel)
        XCTAssertGreaterThan(bar.scrollView.contentView.bounds.origin.x, 0, "a vertical wheel moves the tabs along")
        bar.scrollView.scroll(toX: -500)
        XCTAssertEqual(bar.scrollView.contentView.bounds.origin.x, 0, "it stops at the first tab")
        bar.scrollView.scroll(toX: 1_000_000)
        XCTAssertEqual(bar.scrollView.contentView.bounds.origin.x, bar.contentWidth - 600, accuracy: 0.5, "and at the last")
    }

    func testFewTabsLeaveRoomToDragTheWindow() {
        let bar = crowdedBar(tabs: 2, width: 800)
        XCTAssertLessThan(bar.scrollView.frame.width, 800)
        XCTAssertEqual(bar.scrollView.frame.width, bar.contentWidth, accuracy: 0.5)
    }

    func testNarrowTabKeepsTheTitleAndShowsCloseOnlyOnHover() {
        let tab = laidOutTab(title: "AppServiceProvider.php", width: TabBar.minTabWidth)
        XCTAssertTrue(tab.closeButton.isHidden, "no room: the title wins over the X")
        XCTAssertGreaterThanOrEqual(tab.label.frame.width, 40, "title is not squeezed by the button")
        XCTAssertGreaterThan(tab.label.frame.maxX, tab.closeButton.frame.minX, "button overlays the title when hovered")
    }

    func testRoomyTabShowsCloseNextToTheFullTitle() {
        let tab = laidOutTab(title: "app.php", width: 200)
        XCTAssertFalse(tab.closeButton.isHidden)
        XCTAssertLessThanOrEqual(tab.label.frame.maxX, tab.closeButton.frame.minX, "X sits after the title, never over it")
        XCTAssertEqual(tab.label.stringValue, "app.php")
    }

    func testTabWantsEnoughWidthForTheWholeTitle() {
        let short = laidOutTab(title: "a.php", width: 200).intrinsicContentSize.width
        let long = laidOutTab(title: "ApplicationBuilder.php", width: 400).intrinsicContentSize.width
        XCTAssertGreaterThan(long, short)
    }

    /// A long path wrapped the file name onto a second line that overlapped
    /// the editor. It stays on one line and truncates at the head.
    func testLongPathStaysOnOneLine() {
        let bar = PathBar(frame: NSRect(x: 0, y: 0, width: 320, height: PathBar.height))
        bar.show(["node_modules", ".pnpm", "@types+node@24.12.0", "node_modules", "@types", "node", "child_process.d.ts"])
        XCTAssertEqual(bar.label.maximumNumberOfLines, 1)
        let style = bar.label.attributedStringValue.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle
        XCTAssertEqual(style?.lineBreakMode, .byTruncatingHead)
        let fitted = bar.label.cell?.cellSize(forBounds: NSRect(x: 0, y: 0, width: 280, height: 1000))
        XCTAssertLessThanOrEqual(fitted?.height ?? .infinity, PathBar.height, "one line fits in the bar")
    }
}
