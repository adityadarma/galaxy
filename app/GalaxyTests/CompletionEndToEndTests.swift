import XCTest
@testable import Galaxy

/// Opt-in: run with `TEST_RUNNER_GALAXY_LARAVEL_PATH=/path/to/laravel-app`
/// (served by the built-in PHP engine). Types a half-written class constant and
/// presses Ctrl+Space through the real key path, LSP poll timer and popup.
@MainActor
final class CompletionEndToEndTests: XCTestCase {
    func testCtrlSpaceSuggestsClassConstants() throws {
        guard let path = ProcessInfo.processInfo.environment["GALAXY_LARAVEL_PATH"] else {
            throw XCTSkip("GALAXY_LARAVEL_PATH not set")
        }
        let root = URL(fileURLWithPath: path)
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        try c.openWorkspace(at: root)
        try c.openFile(at: root.appendingPathComponent("app/Console/Commands/JatuhTempoPembayaran.php"))
        let doc = try XCTUnwrap(c.editorView.document)

        // Cut `STATUS_EXPIRED` down to `STATUS_EXPI`, caret right after it.
        var target: (line: UInt64, column: Int)?
        for line in 0..<doc.lineCount() {
            let text = doc.displayLine(line: line) as NSString
            let hit = text.range(of: "PermohonanPayment::STATUS_EXPIRED")
                if hit.location != NSNotFound { target = (line, hit.location + hit.length - 3); break }
        }
        let spot = try XCTUnwrap(target)
        let start = doc.utf16Offset(pos: TextPos(line: spot.line, colUtf16: UInt64(spot.column)))
        doc.commitText(text: "", replacement: Utf16Range(location: start, length: 3))
        doc.setSelections(selections: [TextSelection(anchor: doc.positionAt(offset: start),
                                                     head: doc.positionAt(offset: start))], primary: 0)
        c.editorView.didChangeDocument()
        // A person presses Ctrl+Space well after typing; let the coalesced
        // change notification (and its own completion request) settle first.
        RunLoop.main.run(until: Date().addingTimeInterval(0.5))

        let before = doc.displayLine(line: spot.line)
        XCTAssertTrue(before.contains("PermohonanPayment::STATUS_EXPI,"), "setup failed: \(before)")

        // Indexing is asynchronous; press Ctrl+Space again until a list shows.
        // One press only: the app itself must keep asking until the server,
        // which may still be indexing, has something to offer.
        XCTAssertTrue(c.performKeyBinding("ctrl+space"), "ctrl+space was not handled")
        let presses = 1
        let deadline = Date().addingTimeInterval(30)
        while c.completionPopup.isHidden, Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.2))
        }
        print("COMPLETION presses=\(presses) hidden=\(c.completionPopup.isHidden)")
        print("COMPLETION items=\(c.bridge.completions(document: doc).prefix(5).map(\.label))")
        XCTAssertFalse(c.completionPopup.isHidden, "no suggestion list after ctrl+space")
    }
}
