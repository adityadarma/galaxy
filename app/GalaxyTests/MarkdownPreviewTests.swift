import XCTest
@testable import Galaxy

/// Markdown preview: a pane beside the editor that follows the active tab.
@MainActor
final class MarkdownPreviewTests: XCTestCase {
    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-md-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try "# Title\n\nbody\n".write(to: root.appendingPathComponent("doc.md"), atomically: true, encoding: .utf8)
        try "fn main() {}\n".write(to: root.appendingPathComponent("main.rs"), atomically: true, encoding: .utf8)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    private func makeController() throws -> MainWindowController {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        c.askSaveChoice = { _ in .discard }
        try c.openWorkspace(at: root)
        return c
    }

    func testDetectsMarkdownExtensions() {
        XCTAssertTrue(MarkdownPreviewView.isMarkdown(path: "/a/README.md"))
        XCTAssertTrue(MarkdownPreviewView.isMarkdown(path: "/a/notes.MARKDOWN"))
        XCTAssertFalse(MarkdownPreviewView.isMarkdown(path: "/a/main.rs"))
        XCTAssertFalse(MarkdownPreviewView.isMarkdown(path: ""))
    }

    func testCoreRendersHtmlAndEscapesRawHtml() {
        XCTAssertTrue(markdownToHtml(source: "# Hi\n").contains("<h1>Hi</h1>"))
        XCTAssertFalse(markdownToHtml(source: "<script>x</script>\n").contains("<script>"))
    }

    func testPreviewShowsForMarkdownAndFollowsTabs() throws {
        let c = try makeController()
        try c.openFile(at: root.appendingPathComponent("doc.md"))
        XCTAssertFalse(c.isMarkdownPreviewShown)

        c.toggleMarkdownPreview(nil)
        XCTAssertTrue(c.isMarkdownPreviewOn)
        XCTAssertTrue(c.isMarkdownPreviewShown)

        // A non-Markdown tab hides the pane; coming back restores it.
        try c.openFile(at: root.appendingPathComponent("main.rs"))
        XCTAssertFalse(c.isMarkdownPreviewShown)
        try c.openFile(at: root.appendingPathComponent("doc.md"))
        XCTAssertTrue(c.isMarkdownPreviewShown)

        c.toggleMarkdownPreview(nil)
        XCTAssertFalse(c.isMarkdownPreviewOn)
        XCTAssertFalse(c.isMarkdownPreviewShown)
    }

    func testPreviewButtonAppearsOnlyForMarkdownAndTogglesThePane() throws {
        let c = try makeController()
        try c.openFile(at: root.appendingPathComponent("main.rs"))
        XCTAssertTrue(c.tabBar.previewButton.isHidden)

        try c.openFile(at: root.appendingPathComponent("doc.md"))
        XCTAssertFalse(c.tabBar.previewButton.isHidden)
        XCTAssertFalse(c.isMarkdownPreviewShown)

        c.tabBar.previewButton.performClick(nil)
        XCTAssertTrue(c.isMarkdownPreviewShown)
        c.tabBar.previewButton.performClick(nil)
        XCTAssertFalse(c.isMarkdownPreviewShown)

        try c.openFile(at: root.appendingPathComponent("main.rs"))
        XCTAssertTrue(c.tabBar.previewButton.isHidden)
    }

    func testTogglingOnNonMarkdownDoesNothing() throws {
        let c = try makeController()
        try c.openFile(at: root.appendingPathComponent("main.rs"))
        c.toggleMarkdownPreview(nil)
        XCTAssertFalse(c.isMarkdownPreviewOn)
        XCTAssertFalse(c.isMarkdownPreviewShown)
    }

    func testSplittingEditorReplacesPreview() throws {
        let c = try makeController()
        try c.openFile(at: root.appendingPathComponent("doc.md"))
        c.toggleMarkdownPreview(nil)
        c.splitEditorRight(nil)
        XCTAssertFalse(c.isMarkdownPreviewShown)
        XCTAssertFalse(c.isMarkdownPreviewOn)
    }
}
