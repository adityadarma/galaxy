import XCTest
@testable import Galaxy

final class DocumentFormatterTests: XCTestCase {
    private var dir: URL!

    override func setUpWithError() throws {
        dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-format-\(UUID().uuidString)")
            .resolvingSymlinksInPath()
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: dir)
    }

    private func script(_ name: String, _ body: String) throws -> URL {
        let url = dir.appendingPathComponent(name)
        try "#!/bin/sh\n\(body)\n".write(to: url, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        return url
    }

    @MainActor
    private func apply(_ edit: RenameEdit?, to text: String) throws -> String {
        let url = dir.appendingPathComponent("edit-\(UUID().uuidString).txt")
        try text.write(to: url, atomically: true, encoding: .utf8)
        let doc = try CoreBridge().openDocument(path: url.path)
        if let edit { _ = doc.applyRenameEdits(edits: [edit]) }
        return doc.text()
    }

    @MainActor
    func testEditTurnsTheOldTextIntoTheNewText() throws {
        let cases: [(String, String)] = [
            ("a\nb\nc\n", "a\nB\nc\n"),
            ("fn main(){}", "fn main() {}\n"),
            ("a\nb", "a\nb\n"),
            ("a\nb\n\n", "a\nb\n"),
            ("a\n", "x\ny\nz\n"),
            ("x\ny\nz\n", "x\n"),
            ("é😀\nb\n", "é😀\nbb\n"),
            ("", "a\n"),
        ]
        for (old, new) in cases {
            XCTAssertEqual(try apply(DocumentFormatter.edit(from: old, to: new), to: old), new, "\(old.debugDescription)")
        }
        XCTAssertNil(DocumentFormatter.edit(from: "same\n", to: "same\n"))
    }

    func testEditKeepsUnchangedLinesOutOfTheReplacement() throws {
        let edit = try XCTUnwrap(DocumentFormatter.edit(from: "a\nb\nc\nd\n", to: "a\nB\nc\nd\n"))
        XCTAssertEqual(edit.startLine, 1)
        XCTAssertEqual(edit.endLine, 2)
        XCTAssertEqual(edit.newText, "B\n")
    }

    func testCommandsPickTheLanguagesFormatterAndPreferProjectTools() throws {
        let project = dir.appendingPathComponent("project")
        let src = project.appendingPathComponent("src")
        try FileManager.default.createDirectory(at: src, withIntermediateDirectories: true)
        try "[package]\nedition = \"2024\"\n".write(to: project.appendingPathComponent("Cargo.toml"),
                                                    atomically: true, encoding: .utf8)
        try "{}".write(to: project.appendingPathComponent("composer.json"), atomically: true, encoding: .utf8)
        var asked: [String: [URL]] = [:]
        let find: (String, [URL]) -> URL? = { name, bins in
            asked[name] = bins
            return name == "php-cs-fixer" ? nil : URL(fileURLWithPath: "/tools/\(name)")
        }

        let rust = try XCTUnwrap(DocumentFormatter.command(for: src.appendingPathComponent("main.rs").path,
                                                           root: project.path, find: find))
        XCTAssertEqual(rust.arguments, ["--emit", "stdout", "--edition", "2024"])
        XCTAssertFalse(rust.formatsFile)

        let ts = src.appendingPathComponent("app.ts").path
        let prettier = try XCTUnwrap(DocumentFormatter.command(for: ts, root: project.path, find: find))
        XCTAssertEqual(prettier.name, "prettier")
        XCTAssertEqual(prettier.arguments, ["--stdin-filepath", ts])
        XCTAssertEqual(asked["prettier"], [src, project].map { $0.appendingPathComponent("node_modules/.bin") },
                       "the search stops at the workspace root")

        let php = try XCTUnwrap(DocumentFormatter.command(for: src.appendingPathComponent("a.php").path,
                                                          root: project.path, find: find))
        XCTAssertEqual(php.name, "pint")
        XCTAssertTrue(php.formatsFile)
        XCTAssertEqual(php.directory.path, project.path, "PHP tools run from the Composer project")

        XCTAssertNil(DocumentFormatter.command(for: src.appendingPathComponent("notes.txt").path, root: project.path, find: find))
        XCTAssertNil(DocumentFormatter.command(for: src.appendingPathComponent("main.go").path, root: project.path) { _, _ in nil },
                     "no formatter installed")
    }

    func testRunReadsStdinOrFormatsACopyOfTheFile() throws {
        let upper = DocumentFormatter.Command(name: "upper", executable: try script("upper", "tr a-z A-Z"),
                                              arguments: [], directory: dir)
        XCTAssertEqual(try DocumentFormatter.run(upper, text: "abc\n", filename: "a.txt"), "ABC\n")

        let rewrite = DocumentFormatter.Command(name: "rewrite", executable: try script("rewrite", #"printf 'x\n' > "$1""#),
                                                arguments: [], formatsFile: true, directory: dir)
        XCTAssertEqual(try DocumentFormatter.run(rewrite, text: "abc\n", filename: "a.php"), "x\n")
    }

    func testRunReportsFailuresAndTimeouts() throws {
        let failing = DocumentFormatter.Command(name: "bad", executable: try script("bad", "echo 'syntax error' >&2; exit 2"),
                                                arguments: [], directory: dir)
        XCTAssertThrowsError(try DocumentFormatter.run(failing, text: "x", filename: "a")) {
            XCTAssertEqual($0 as? DocumentFormatter.Failure, .failed("bad", "syntax error"))
        }
        let slow = DocumentFormatter.Command(name: "slow", executable: try script("slow", "sleep 5"),
                                             arguments: [], directory: dir)
        XCTAssertThrowsError(try DocumentFormatter.run(slow, text: "x", filename: "a", timeout: 0.2)) {
            XCTAssertEqual($0 as? DocumentFormatter.Failure, .timedOut("slow"))
        }
    }

    @MainActor
    func testFormattedSaveWritesTheFormatterOutput() throws {
        let url = dir.appendingPathComponent("main.rs")
        try "fn main(){}\n".write(to: url, atomically: true, encoding: .utf8)
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        try controller.openFile(at: url)
        let tab = try XCTUnwrap(controller.activeTab)
        let doc = tab.doc

        controller.finishFormattedSave(tab: tab, revision: doc.revision(), original: doc.text(),
                                       outcome: .init(name: "rustfmt", result: .success("fn main() {}\n")))
        XCTAssertEqual(try String(contentsOf: url, encoding: .utf8), "fn main() {}\n")
        XCTAssertEqual(doc.text(), "fn main() {}\n")
        XCTAssertFalse(doc.isModified())
    }

    @MainActor
    func testFormattedSaveStillSavesWhenTheFormatterFailsOrTheTextChanged() throws {
        let url = dir.appendingPathComponent("main.rs")
        try "fn main(){}\n".write(to: url, atomically: true, encoding: .utf8)
        let controller = MainWindowController(bridge: CoreBridge())
        controller.showWindow(nil)
        defer { controller.window?.orderOut(nil) }
        try controller.openFile(at: url)
        let tab = try XCTUnwrap(controller.activeTab)
        let doc = tab.doc

        let original = doc.text(), revision = doc.revision()
        doc.goToLine(line: 0, col: 0)
        controller.editorView.insertText("// a\n", replacementRange: NSRange(location: NSNotFound, length: 0))
        controller.finishFormattedSave(tab: tab, revision: revision, original: original,
                                       outcome: .init(name: "rustfmt", result: .success("fn main() {}\n")))
        XCTAssertEqual(try String(contentsOf: url, encoding: .utf8), "// a\nfn main(){}\n", "a stale result is dropped")
        XCTAssertTrue(controller.statusLabel.stringValue.contains("the file changed while rustfmt ran"))

        controller.editorView.insertText("x", replacementRange: NSRange(location: NSNotFound, length: 0))
        controller.finishFormattedSave(tab: tab, revision: doc.revision(), original: doc.text(),
                                       outcome: .init(name: "rustfmt", result: .failure(.failed("rustfmt", "error: bad"))))
        XCTAssertFalse(doc.isModified())
        XCTAssertTrue(controller.statusLabel.stringValue.contains("rustfmt: error: bad"))
    }
}
