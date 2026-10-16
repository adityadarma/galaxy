import XCTest
@testable import Galaxy

@MainActor
final class StatusBarTests: XCTestCase {
    func testLanguageDetectionCoversExtensionsAndSpecialNames() {
        XCTAssertEqual(StatusBar.language(for: nil), "Plain Text")
        XCTAssertEqual(StatusBar.language(for: "/work/main.RS"), "Rust")
        XCTAssertEqual(StatusBar.language(for: "/work/view.tsx"), "TypeScript React")
        XCTAssertEqual(StatusBar.language(for: "/work/settings.jsonc"), "JSON with Comments")
        XCTAssertEqual(StatusBar.language(for: "/work/Dockerfile"), "Dockerfile")
        XCTAssertEqual(StatusBar.language(for: "/work/Makefile"), "Makefile")
        XCTAssertEqual(StatusBar.language(for: "/work/.gitignore"), "Git Config")
        XCTAssertEqual(StatusBar.language(for: "/work/unknown.extension"), "Plain Text")
    }

    func testGitStatusShowsBranchAndChangedCount() {
        let bar = StatusBar()
        bar.showGit(branch: "main", changedCount: 2)
        XCTAssertEqual(bar.git.stringValue, "main")
        bar.showGit(branch: "main", changedCount: 0)
        XCTAssertEqual(bar.git.stringValue, "main")
        bar.showGit(branch: nil, changedCount: 0)
        XCTAssertEqual(bar.git.stringValue, "")
    }

    func testGitGraphButtonSitsBesideTheBranchAndOpensTheGraph() {
        let bar = StatusBar()
        XCTAssertTrue(bar.gitGraph.isHidden, "no repository, no graph button")
        bar.showGit(branch: "main", changedCount: 0)
        XCTAssertFalse(bar.gitGraph.isHidden)
        var opened = 0
        bar.onGitGraphClick = { opened += 1 }
        bar.gitGraph.performClick(nil)
        XCTAssertEqual(opened, 1)
        bar.showGit(branch: nil, changedCount: 0)
        XCTAssertTrue(bar.gitGraph.isHidden)
    }

    func testGitDecorationsAreRepresentedByViewModels() {
        let item = TabBar.Item(title: "main.rs", isPreview: false, isEdited: false, isGitChanged: true)
        XCTAssertTrue(item.isGitChanged)
        let clean = TabBar.Item(title: "main.rs", isPreview: false, isEdited: false, isGitChanged: false)
        XCTAssertNotEqual(item, clean)
    }

    func testDocumentFactsIncludeLanguageIndentEOLAndEncoding() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-status-\(UUID().uuidString).py")
        defer { try? FileManager.default.removeItem(at: url) }
        try "\u{FEFF}print('hi')\r\n".write(to: url, atomically: true, encoding: .utf8)

        let doc = try CoreBridge().openDocument(path: url.path)
        doc.setIndent(useTabs: true, tabSize: 8)
        let bar = StatusBar()
        bar.show(document: doc, path: doc.path())

        XCTAssertEqual(bar.language.stringValue, "Python")
        XCTAssertEqual(bar.indent.stringValue, "Tab Size: 8")
        XCTAssertEqual(bar.lineEnding.stringValue, "CRLF")
        XCTAssertEqual(bar.encoding.stringValue, "UTF-8 with BOM")
        XCTAssertEqual(bar.cursor.stringValue, "Ln 1, Col 1")

        bar.show(document: nil)
        XCTAssertEqual(bar.language.stringValue, "")
        XCTAssertEqual(bar.indent.stringValue, "")
    }
}
