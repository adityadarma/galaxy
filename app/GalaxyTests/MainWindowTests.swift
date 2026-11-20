import XCTest
@testable import Galaxy

/// Exercises the real window wiring (scroll view + editor view), which the
/// view-level tests bypass by giving the view a hand-picked frame.
@MainActor
final class MainWindowTests: XCTestCase {
    private func settle() { RunLoop.current.run(until: Date().addingTimeInterval(0.4)) }

    /// The window edges and the strip above the status bar show the content
    /// view, which must be the chrome colour, not the window's white.
    func testWindowBackgroundIsTheThemeChrome() throws {
        for name in [NSAppearance.Name.aqua, .darkAqua] {
            let view = FileDropView(frame: NSRect(x: 0, y: 0, width: 20, height: 20))
            view.appearance = NSAppearance(named: name)
            let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
            view.cacheDisplay(in: view.bounds, to: rep)
            let drawn = try XCTUnwrap(rep.colorAt(x: 2, y: 2)?.usingColorSpace(.sRGB))
            let chrome = Theme.chrome.resolved(for: NSAppearance(named: name)!)
            XCTAssertEqual(drawn.redComponent, chrome.redComponent, accuracy: 0.01, "\(name.rawValue)")
            XCTAssertEqual(drawn.greenComponent, chrome.greenComponent, accuracy: 0.01, "\(name.rawValue)")
            XCTAssertEqual(drawn.blueComponent, chrome.blueComponent, accuracy: 0.01, "\(name.rawValue)")
        }
    }

    func testReferencesPeekIsLazyAtStartup() {
        let controller = MainWindowController(bridge: CoreBridge())
        defer { controller.window?.orderOut(nil) }

        XCTAssertNil(controller.loadedReferencesPanel)
        _ = controller.referencesPanel
        XCTAssertNotNil(controller.loadedReferencesPanel)
    }

    func testNewDocumentIsVisibleFocusedAndTypable() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }

        controller.newDocument(nil)
        settle()

        let view = controller.editorView
        XCTAssertGreaterThan(view.frame.width, 100, "editor must fill the scroll view")
        XCTAssertFalse(view.lastDrawnLines.isEmpty, "the empty buffer still draws its first line")
        XCTAssertTrue(controller.window?.firstResponder === view, "typing must work immediately")
        XCTAssertEqual(controller.window?.title, "Untitled")

        view.insertText("hello", replacementRange: NSRange(location: NSNotFound, length: 0))
        XCTAssertEqual(view.document?.lines(first: 0, count: 1), ["hello"])
    }

    func testMethodCompletionPlacesCaretInsideParentheses() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        controller.newDocument(nil)
        let document = try XCTUnwrap(controller.editorView.document)
        document.edit(command: .insertText(text: "Aset::wher"))
        controller.editorView.didChangeDocument()
        controller.completionQuery = "wher"
        controller.completionPosition = TextPos(line: 0, colUtf16: 10)

        controller.acceptCompletion(CompletionInfo(
            label: "whereHas", detail: "Builder", insertText: "whereHas($0)",
            isSnippet: true, kind: nil
        ))

        XCTAssertEqual(document.displayLine(line: 0), "Aset::whereHas()")
        XCTAssertEqual(document.primaryCursor().colUtf16, 15, "caret is before the closing parenthesis")
    }

    func testViewMenuZoomPanelsAndToggles() throws {
        let defaults = UserDefaults.standard
        let savedSize = defaults.object(forKey: "EditorFontSize")
        let savedCrumbs = defaults.object(forKey: "ShowBreadcrumbs")
        let savedSticky = defaults.object(forKey: "StickyScroll")
        defer {
            defaults.set(savedSize, forKey: "EditorFontSize")
            defaults.set(savedCrumbs, forKey: "ShowBreadcrumbs")
            defaults.set(savedSticky, forKey: "StickyScroll")
        }
        defaults.set(13.0, forKey: "EditorFontSize")
        defaults.set(true, forKey: "ShowBreadcrumbs")
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }

        c.zoomEditorIn(nil)
        XCTAssertEqual(c.editorView.font.pointSize, 14)
        c.zoomEditorOut(nil)
        c.zoomEditorOut(nil)
        XCTAssertEqual(c.editorView.font.pointSize, 12)
        c.zoomEditorReset(nil)
        XCTAssertEqual(c.editorView.font.pointSize, 13)
        defaults.set(24.0, forKey: "EditorFontSize")
        c.zoomEditorIn(nil)
        XCTAssertEqual(c.editorView.font.pointSize, 24, "stays within the Settings range")

        XCTAssertFalse(c.isBottomPanelVisible)
        c.showProblemsPanel(nil)
        XCTAssertTrue(c.isBottomPanelVisible)
        XCTAssertEqual(c.bottomPanelTab, .problems)
        c.showOutputPanel(nil)
        XCTAssertEqual(c.bottomPanelTab, .output)

        let crumbs = NSMenuItem(title: "", action: #selector(MainWindowController.toggleBreadcrumbs(_:)), keyEquivalent: "")
        XCTAssertTrue(c.validateMenuItem(crumbs))
        XCTAssertEqual(crumbs.state, .on)
        c.toggleBreadcrumbs(nil)
        XCTAssertTrue(c.pathBar.isHidden)
        _ = c.validateMenuItem(crumbs)
        XCTAssertEqual(crumbs.state, .off)
        XCTAssertEqual(defaults.bool(forKey: "ShowBreadcrumbs"), false)

        let sticky = NSMenuItem(title: "", action: #selector(MainWindowController.toggleStickyScroll(_:)), keyEquivalent: "")
        _ = c.validateMenuItem(sticky)
        XCTAssertEqual(sticky.state, .on)
        c.toggleStickyScroll(nil)
        _ = c.validateMenuItem(sticky)
        XCTAssertEqual(sticky.state, .off)
    }

    func testEditorHasTopInsetOverlayScrollerAndCopperCaretLine() throws {
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        controller.newDocument(nil)
        settle()
        let view = controller.editorView
        let scroll = try XCTUnwrap(view.enclosingScrollView)
        XCTAssertEqual(scroll.scrollerStyle, .overlay)
        XCTAssertEqual(scroll.contentInsets.top, Theme.editorTopInset)
        // Line 1 starts below the inset, not flush with the path bar.
        let top = scroll.convert(view.bounds.origin, from: view).y
        XCTAssertEqual(top, Theme.editorTopInset, accuracy: 0.5)
        XCTAssertEqual(view.lastDrawnLines.lowerBound, 0)
    }

    func testOpenedDocumentIsDrawnInTheRealWindow() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-\(UUID().uuidString).txt")
        try (0..<100).map { "line \($0)" }.joined(separator: "\n").write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let bridge = CoreBridge()
        let controller = MainWindowController(bridge: bridge)
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }

        controller.editorView.document = try bridge.openDocument(path: url.path)
        settle()

        let view = controller.editorView
        XCTAssertGreaterThan(view.frame.width, 100, "editor must fill the scroll view width")
        XCTAssertEqual(view.lastDrawnLines.lowerBound, 0)
        XCTAssertGreaterThan(view.lastDrawnLines.count, 10)
    }

    func testOpeningWorkspaceHandledByAnotherWindowKeepsCurrentWorkspace() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-workspace-\(UUID().uuidString)", isDirectory: true)
        let other = root.appendingPathComponent("other", isDirectory: true)
        try FileManager.default.createDirectory(at: other, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }

        let controller = MainWindowController(bridge: CoreBridge())
        try controller.openWorkspace(at: root)
        var handledURL: URL?
        controller.openExistingWorkspace = { url in
            handledURL = url
            return true
        }

        try controller.openWorkspace(at: other)

        XCTAssertEqual(handledURL?.path, other.path)
        XCTAssertEqual(controller.workspace?.root(), root.path)
    }

    func testOpeningTheCurrentWorkspaceDoesNotReloadIt() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-workspace-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }

        let controller = MainWindowController(bridge: CoreBridge())
        try controller.openWorkspace(at: root)
        let workspace = try XCTUnwrap(controller.workspace)
        controller.openExistingWorkspace = { url in
            url.path == root.path
        }

        try controller.openWorkspace(at: root)

        XCTAssertTrue(controller.workspace === workspace)
    }
}
