import XCTest

/// End-to-end tests that drive the real app through the accessibility API.
/// Run with `scripts/ui-test.sh`. They need a logged-in GUI session and
/// Accessibility permission for the test runner.
@MainActor
final class EditorUITests: XCTestCase {
    private var app: XCUIApplication!
    private var workDir: URL!

    override func setUp() async throws {
        continueAfterFailure = false
        workDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("rcode-ui-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: workDir, withIntermediateDirectories: true)
    }

    override func tearDown() async throws {
        app?.terminate()
        try? FileManager.default.removeItem(at: workDir)
    }

    // MARK: Helpers

    @discardableResult
    private func write(_ name: String, _ text: String) throws -> URL {
        let url = workDir.appendingPathComponent(name)
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try text.write(to: url, atomically: true, encoding: .utf8)
        return url
    }

    // Keyboard input goes through `app.typeText`/`app.typeKey`. Calling them
    // on an element can click it first, which moves the caret to the end.

    /// Launches with an empty buffer, or with `open` as a file or folder.
    private func launch(open url: URL? = nil) {
        app = XCUIApplication()
        // A fresh window every run: no restored frame, no saved state.
        app.launchArguments = ["-ApplePersistenceIgnoreState", "YES", "-NSQuitAlwaysKeepsWindows", "NO"]
        if let url { app.launchArguments += ["-OpenPath", url.path] }
        app.launch()
        XCTAssertTrue(editor.waitForExistence(timeout: 10), "editor did not appear")
        if url == nil, !text.isEmpty {
            XCTFail("""
                The new buffer already contains \(text.debugDescription) before the test typed \
                anything. UI tests drive the real keyboard and mouse; something else (usually \
                someone typing) sent input to the app. Re-run with the machine idle.
                """)
        }
    }

    /// Looked up by accessibility identifier, whatever role AppKit reports.
    private var editor: XCUIElement {
        app.descendants(matching: .any).matching(identifier: "editor").firstMatch
    }
    private var status: XCUIElement { app.staticTexts["core-status"] }

    /// The editor's full text as exposed through accessibility.
    private var text: String { (editor.value as? String) ?? "" }

    private func waitForText(_ expected: String, timeout: TimeInterval = 5, line: UInt = #line) {
        let predicate = NSPredicate { _, _ in self.text == expected }
        let exp = XCTNSPredicateExpectation(predicate: predicate, object: nil)
        if XCTWaiter().wait(for: [exp], timeout: timeout) != .completed {
            XCTFail("editor text was \(text.debugDescription), expected \(expected.debugDescription)", line: line)
        }
    }

    private func attachScreenshot(_ name: String) {
        let shot = XCTAttachment(screenshot: app.windows.firstMatch.screenshot())
        shot.name = name
        shot.lifetime = .keepAlways
        add(shot)
    }

    // MARK: Tests

    func testLaunchShowsAnEmptyEditableBufferThatAcceptsTyping() throws {
        launch()
        XCTAssertEqual(text, "")
        editor.click()
        app.typeText("hello")
        waitForText("hello")
        app.typeText("\nworld")
        waitForText("hello\nworld")
        attachScreenshot("typing")
    }

    func testBackspaceArrowsAndUndoFromTheKeyboard() throws {
        launch()
        editor.click()
        app.typeText("abc")
        waitForText("abc")   // let the app consume the typed text before the next key
        app.typeKey(.leftArrow, modifierFlags: [])
        app.typeKey(.delete, modifierFlags: [])
        waitForText("ac")
        app.typeText("X")
        waitForText("aXc")
        app.typeKey("z", modifierFlags: .command)
        waitForText("ac")
        app.typeKey("z", modifierFlags: [.command, .shift])
        waitForText("aXc")
    }

    func testShiftArrowsSelectAndTypingReplacesTheSelection() throws {
        launch()
        editor.click()
        app.typeText("hello world")
        app.typeKey(.leftArrow, modifierFlags: [.option, .shift])
        app.typeText("there")
        waitForText("hello there")
        app.typeKey("a", modifierFlags: .command)
        app.typeText("x")
        waitForText("x")
    }

    func testCopyPasteAndLinewiseCut() throws {
        launch()
        editor.click()
        app.typeText("one\ntwo\nthree")
        app.typeKey(.upArrow, modifierFlags: [])
        app.typeKey("x", modifierFlags: .command)   // no selection: cuts the line
        waitForText("one\nthree")
        app.typeKey(.upArrow, modifierFlags: [])
        app.typeKey("v", modifierFlags: .command)   // pastes above the current line
        waitForText("two\none\nthree")
    }

    func testMultiCursorTypingWithCmdD() throws {
        launch()
        editor.click()
        app.typeText("foo bar\nfoo baz")
        app.typeKey("a", modifierFlags: .command)
        app.typeKey(.delete, modifierFlags: [])
        app.typeText("foo x\nfoo y")
        app.typeKey(.upArrow, modifierFlags: [])
        app.typeKey(.upArrow, modifierFlags: [])
        app.typeKey("d", modifierFlags: .command)   // selects "foo"
        app.typeKey("d", modifierFlags: .command)   // adds the next "foo"
        app.typeText("Z")
        waitForText("Z x\nZ y")
        attachScreenshot("multi-cursor")
    }

    func testOpeningAFileShowsItsContentAndSavingWritesIt() throws {
        let file = try write("note.txt", "first\nsecond\n")
        launch(open: file)
        waitForText("first\nsecond\n")
        XCTAssertTrue(app.windows.firstMatch.title.contains("note.txt"))

        editor.click()
        app.typeKey(.downArrow, modifierFlags: .command)
        app.typeText("third")
        waitForText("first\nsecond\nthird")
        app.typeKey("s", modifierFlags: .command)

        let saved = NSPredicate { _, _ in
            (try? String(contentsOf: file, encoding: .utf8)) == "first\nsecond\nthird"
        }
        let exp = XCTNSPredicateExpectation(predicate: saved, object: nil)
        XCTAssertEqual(XCTWaiter().wait(for: [exp], timeout: 5), .completed,
                       "file on disk: \((try? String(contentsOf: file, encoding: .utf8)) ?? "unreadable")")
    }

    func testOpeningAFolderShowsTheExplorerAndClickingAFileOpensIt() throws {
        try write("README.md", "# readme\n")
        try write("src/main.rs", "fn main() {}\n")
        try write("node_modules/dep/index.js", "ignored")
        launch(open: workDir)

        let explorer = app.outlines["explorer"]
        XCTAssertTrue(explorer.waitForExistence(timeout: 5))
        XCTAssertTrue(explorer.staticTexts["src"].exists)
        XCTAssertTrue(explorer.staticTexts["README.md"].exists)
        XCTAssertFalse(explorer.staticTexts["node_modules"].exists, "default excludes are hidden")

        explorer.staticTexts["src"].click()
        XCTAssertTrue(explorer.staticTexts["main.rs"].waitForExistence(timeout: 5), "clicking a folder expands it")
        explorer.staticTexts["main.rs"].click()
        waitForText("fn main() {}\n")
        attachScreenshot("explorer")
    }

    func testActivityBarTogglesTheSidebar() throws {
        try write("a.txt", "a")
        launch(open: workDir)
        let explorer = app.outlines["explorer"]
        XCTAssertTrue(explorer.waitForExistence(timeout: 5))

        let button = app.descendants(matching: .any).matching(identifier: "activity-explorer").firstMatch
        XCTAssertTrue(button.exists, "activity bar on the left")
        button.click()
        // A hidden view leaves the accessibility tree, so it must stop existing.
        let gone = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == false"), object: explorer)
        XCTAssertEqual(XCTWaiter().wait(for: [gone], timeout: 5), .completed,
                       "sidebar should disappear after toggling it off")
        button.click()
        XCTAssertTrue(explorer.waitForExistence(timeout: 5), "sidebar should come back")
    }

    func testUnsavedChangesPromptWhenClosingTheWindow() throws {
        let file = try write("keep.txt", "original")
        launch(open: file)
        waitForText("original")
        editor.click()   // the click lands below the text, so the caret goes to the end
        app.typeKey(.upArrow, modifierFlags: .command)
        app.typeText("edit ")
        waitForText("edit original")

        app.typeKey("w", modifierFlags: .command)
        let sheet = app.sheets.firstMatch.exists ? app.sheets.firstMatch : app.dialogs.firstMatch
        XCTAssertTrue(sheet.waitForExistence(timeout: 5), "a save prompt should appear")
        sheet.buttons["Cancel"].click()
        XCTAssertTrue(editor.exists, "Cancel keeps the window")
        XCTAssertEqual(try String(contentsOf: file, encoding: .utf8), "original")

        app.typeKey("w", modifierFlags: .command)
        XCTAssertTrue(sheet.waitForExistence(timeout: 5))
        sheet.buttons["Don’t Save"].exists ? sheet.buttons["Don’t Save"].click() : sheet.buttons["Don't Save"].click()
        XCTAssertEqual(try String(contentsOf: file, encoding: .utf8), "original", "discard leaves the file alone")
    }

    // MARK: Find and replace

    private var findCount: String {
        (app.staticTexts["find-count"].value as? String) ?? ""
    }

    private func waitForCount(_ expected: String, line: UInt = #line) {
        let predicate = NSPredicate { _, _ in self.findCount == expected }
        let exp = XCTNSPredicateExpectation(predicate: predicate, object: nil)
        if XCTWaiter().wait(for: [exp], timeout: 5) != .completed {
            XCTFail("find count was \(findCount.debugDescription), expected \(expected.debugDescription)", line: line)
        }
    }

    func testFindWalksMatchesAndEscapeClosesTheBar() throws {
        let file = try write("find.txt", "cat hat cat\ncat")
        launch(open: file)
        waitForText("cat hat cat\ncat")

        app.typeKey("f", modifierFlags: .command)
        let field = app.textFields["find-field"]
        XCTAssertTrue(field.waitForExistence(timeout: 5), "find bar did not open")
        app.typeText("cat")
        waitForCount("1 of 3")
        app.typeKey(.return, modifierFlags: [])
        waitForCount("2 of 3")
        app.typeKey("g", modifierFlags: [.command, .shift])
        waitForCount("1 of 3")
        attachScreenshot("find")

        app.typeKey(.escape, modifierFlags: [])
        let closed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: field)
        XCTAssertEqual(XCTWaiter().wait(for: [closed], timeout: 5), .completed, "Esc should close the bar")
        // Focus is back in the editor: typing replaces the selected match.
        app.typeText("dog")
        waitForText("dog hat cat\ncat")
    }

    func testReplaceAllIsOneUndoStep() throws {
        let file = try write("replace.txt", "a1 a2\na3")
        launch(open: file)
        waitForText("a1 a2\na3")

        app.typeKey("f", modifierFlags: [.command, .option])
        XCTAssertTrue(app.textFields["replace-field"].waitForExistence(timeout: 5))
        app.typeText("a")
        waitForCount("1 of 3")
        app.textFields["replace-field"].click()
        app.typeText("b")
        app.buttons["replace-all"].click()
        waitForText("b1 b2\nb3")

        app.typeKey(.escape, modifierFlags: [])
        app.typeKey("z", modifierFlags: .command)
        waitForText("a1 a2\na3")
    }

    func testInvalidRegexShowsAnErrorAndDoesNotCrash() throws {
        let file = try write("regex.txt", "a(b")
        launch(open: file)
        waitForText("a(b")
        app.typeKey("f", modifierFlags: .command)
        XCTAssertTrue(app.textFields["find-field"].waitForExistence(timeout: 5))
        // Toggle buttons are not reported with the plain button role, so
        // look the control up by identifier alone.
        app.descendants(matching: .any).matching(identifier: "find-regex").firstMatch.click()
        app.textFields["find-field"].click()
        app.typeText("(")
        let error = NSPredicate { _, _ in self.findCount.contains("unclosed") }
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: error, object: nil)], timeout: 5),
            .completed, "find count was \(findCount.debugDescription)")
        XCTAssertTrue(editor.exists, "app is still running")
    }
}
