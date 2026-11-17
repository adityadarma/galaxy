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

    func testWorkspaceIndexShowsFileProgressAndIgnoresStaleWorkspace() {
        let bar = StatusBar()
        bar.resetWorkspaceScan(root: "/one")
        bar.workspaceIndexStarted(root: "/one", total: 120)
        XCTAssertFalse(bar.workspaceScan.isHidden)
        XCTAssertEqual(bar.workspaceScan.stringValue, "Scanning workspace 0 of 120 files")
        bar.workspaceIndexProgress(root: "/one", completed: 0, total: 120)
        XCTAssertEqual(bar.workspaceScan.stringValue, "Scanning workspace 0 of 120 files")
        bar.workspaceIndexProgress(root: "/one", completed: 64, total: 120)
        XCTAssertEqual(bar.workspaceScan.stringValue, "Scanning workspace 64 of 120 files")
        bar.workspaceIndexProgress(root: "/one", completed: 120, total: 120)
        XCTAssertEqual(bar.workspaceScan.stringValue, "Indexing workspace after scanning 120 files...")
        XCTAssertFalse(bar.workspaceScan.isHidden)
        bar.workspaceIndexPhase(root: "/one", phase: "Indexing Rust symbols", completed: 0, total: 0)
        XCTAssertEqual(bar.workspaceScan.stringValue, "Indexing Rust symbols...")
        bar.workspaceIndexPhase(root: "/one", phase: "Checking Rust diagnostics", completed: 48, total: 120)
        XCTAssertEqual(bar.workspaceScan.stringValue, "Checking Rust diagnostics 48 of 120 files")
        bar.resetWorkspaceScan(root: "/two")
        XCTAssertFalse(bar.workspaceScan.isHidden)
        XCTAssertEqual(bar.workspaceScan.stringValue, "Scanning workspace...")
        bar.workspaceIndexFinished(root: "/one", total: 120, changed: 3)
        XCTAssertFalse(bar.workspaceScan.isHidden, "a stale finish must not hide the current workspace scan")
        bar.workspaceIndexStarted(root: "/two", total: 1)
        bar.workspaceIndexFinished(root: "/two", total: 1, changed: 1)
        XCTAssertTrue(bar.workspaceScan.isHidden)
        bar.phpIndexFinished(root: "/two", files: 49_260)
        // The grouping separator follows the system locale ("49.260" or "49,260").
        let files = UInt64(49_260).formatted(.number.grouping(.automatic))
        XCTAssertEqual(bar.workspaceScan.stringValue, "PHP Composer indexed \(files) files")
        XCTAssertFalse(bar.workspaceScan.isHidden)
        RunLoop.current.run(until: Date().addingTimeInterval(3.1))
        XCTAssertTrue(bar.workspaceScan.isHidden)
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
