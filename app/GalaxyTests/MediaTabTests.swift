import PDFKit
import XCTest
@testable import Galaxy

/// Pictures and PDFs open in a viewer tab; files dropped on the window open
/// like files picked from the Open panel.
@MainActor
final class MediaTabTests: XCTestCase {
    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-media-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    private func makeController() -> MainWindowController {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        c.askSaveChoice = { _ in .discard }
        return c
    }

    @discardableResult
    private func writePNG(_ name: String, width: Int = 4, height: Int = 3) throws -> URL {
        let rep = try XCTUnwrap(NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
            bytesPerRow: 0, bitsPerPixel: 0))
        let data = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
        let url = root.appendingPathComponent(name)
        try data.write(to: url)
        return url
    }

    @discardableResult
    private func writePDF(_ name: String) throws -> URL {
        let document = PDFDocument()
        document.insert(try XCTUnwrap(PDFPage(image: NSImage(size: NSSize(width: 40, height: 40), flipped: false) { rect in
            NSColor.red.setFill()
            rect.fill()
            return true
        })), at: 0)
        let url = root.appendingPathComponent(name)
        XCTAssertTrue(document.write(to: url))
        return url
    }

    func testKindIsDecidedByTheFileExtension() {
        func kind(_ name: String) -> MediaKind? { MediaKind.kind(for: URL(fileURLWithPath: "/tmp/\(name)")) }
        XCTAssertEqual(kind("a.png"), .image)
        XCTAssertEqual(kind("a.JPG"), .image)
        XCTAssertEqual(kind("a.webp"), .image)
        XCTAssertEqual(kind("Doc.PDF"), .pdf)
        XCTAssertNil(kind("a.svg"), "SVG is XML and stays editable as text")
        XCTAssertNil(kind("a.txt"))
        XCTAssertNil(kind("Makefile"))
    }

    func testImageOpensInTheViewerWithoutAnEditorDocument() throws {
        let png = try writePNG("pic.png", width: 40, height: 30)
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: png)

        let tab = try XCTUnwrap(c.activeTab)
        XCTAssertEqual(tab.mediaURL, png)
        XCTAssertFalse(tab.isDocument)
        XCTAssertEqual(c.tabBar.items.map(\.title), ["pic.png"])
        XCTAssertFalse(c.mediaViewer.isHidden)
        XCTAssertEqual(c.mediaViewer.kind, .image)
        XCTAssertEqual(c.mediaViewer.url, png)
        XCTAssertTrue(c.mediaViewer.summary.hasPrefix("PNG image, 40 × 30 px"), c.mediaViewer.summary)
        XCTAssertEqual(c.statusLabel.stringValue, c.mediaViewer.summary)
    }

    func testPDFOpensInTheViewer() throws {
        let pdf = try writePDF("paper.pdf")
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: pdf)

        XCTAssertEqual(c.activeTab?.mediaURL, pdf)
        XCTAssertEqual(c.mediaViewer.kind, .pdf)
        XCTAssertFalse(c.mediaViewer.isHidden)
        XCTAssertTrue(c.mediaViewer.summary.hasPrefix("PDF, 1 page"), c.mediaViewer.summary)
    }

    func testOpeningTheSameFileAgainShowsItsTab() throws {
        let png = try writePNG("pic.png")
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: png)
        try c.openFile(at: png)
        XCTAssertEqual(c.tabs.count, 1)
    }

    func testTheViewerFollowsTheActiveTab() throws {
        let png = try writePNG("pic.png")
        let text = root.appendingPathComponent("note.txt")
        try "hello\n".write(to: text, atomically: true, encoding: .utf8)
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: png)
        try c.openFile(at: text)
        XCTAssertEqual(c.tabs.count, 2)
        XCTAssertTrue(c.mediaViewer.isHidden, "a text tab hides the viewer")
        XCTAssertEqual(c.editorView.document?.text(), "hello\n")

        c.selectTab(0)
        XCTAssertFalse(c.mediaViewer.isHidden)
        c.closeTab(at: 0)
        XCTAssertTrue(c.mediaViewer.isHidden)
        XCTAssertEqual(c.activeTab?.title, "note.txt")
    }

    func testSavingAMediaTabDoesNothing() throws {
        let png = try writePNG("pic.png")
        let before = try Data(contentsOf: png)
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: png)
        XCTAssertTrue(c.save())
        XCTAssertEqual(try Data(contentsOf: png), before)
    }

    func testAnUnreadablePictureShowsAMessage() throws {
        let broken = root.appendingPathComponent("broken.png")
        try Data("not a png".utf8).write(to: broken)
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: broken)
        XCTAssertEqual(c.mediaViewer.summary, "Cannot display broken.png")
    }

    func testViewerReloadsAfterTheFileChanges() throws {
        let png = try writePNG("pic.png", width: 4, height: 3)
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openFile(at: png)
        XCTAssertFalse(c.mediaViewer.isStale(for: png))

        try writePNG("pic.png", width: 8, height: 6)
        try FileManager.default.setAttributes([.modificationDate: Date().addingTimeInterval(5)], ofItemAtPath: png.path)
        XCTAssertTrue(c.mediaViewer.isStale(for: png))
        c.selectTab(0)
        XCTAssertTrue(c.mediaViewer.summary.hasPrefix("PNG image, 8 × 6 px"), c.mediaViewer.summary)
    }

    // MARK: Dropping files on the window

    func testTheWindowAcceptsDroppedFiles() throws {
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        let content = try XCTUnwrap(c.window?.contentView as? FileDropView)
        XCTAssertTrue(content.registeredDraggedTypes.contains(.fileURL))
        XCTAssertNotNil(content.onDrop)
    }

    func testDroppedFilesOpenAsTabs() throws {
        let png = try writePNG("pic.png")
        let pdf = try writePDF("paper.pdf")
        let text = root.appendingPathComponent("note.txt")
        try "hi\n".write(to: text, atomically: true, encoding: .utf8)
        let c = makeController()
        defer { c.window?.orderOut(nil) }

        c.open(urls: [png, pdf, text])
        XCTAssertEqual(c.tabBar.items.map(\.title), ["pic.png", "paper.pdf", "note.txt"])
        XCTAssertEqual(c.tabs.map { $0.mediaURL != nil }, [true, true, false])
        XCTAssertEqual(c.activeTab?.title, "note.txt", "the last dropped file is shown")
    }

    func testADroppedFolderBecomesTheWorkspaceBeforeItsFilesOpen() throws {
        let png = try writePNG("pic.png")
        let folder = root.appendingPathComponent("project")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let c = makeController()
        defer { c.window?.orderOut(nil) }

        c.open(urls: [png, folder])
        XCTAssertEqual(c.workspace.map { URL(fileURLWithPath: $0.root()).resolvingSymlinksInPath().path },
                       folder.resolvingSymlinksInPath().path)
        XCTAssertEqual(c.tabs.map(\.title), ["pic.png"])
    }
}
