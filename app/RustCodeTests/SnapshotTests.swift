import XCTest
@testable import RustCode

/// Renders the main window to PNG for a visual check. Runs only when
/// `RCODE_SNAPSHOT_DIR` is set (pass `TEST_RUNNER_RCODE_SNAPSHOT_DIR=<dir>`
/// to xcodebuild).
@MainActor
final class SnapshotTests: XCTestCase {
    func testRenderMainWindow() throws {
        guard let dir = ProcessInfo.processInfo.environment["RCODE_SNAPSHOT_DIR"] else {
            throw XCTSkip("set RCODE_SNAPSHOT_DIR to render")
        }
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("rcode-snap-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: root.appendingPathComponent("src"), withIntermediateDirectories: true)
        let code = "fn main() {\n    let greeting = \"hello\";\n    println!(\"{greeting}\");\n}\n"
        try code.write(to: root.appendingPathComponent("src/main.rs"), atomically: true, encoding: .utf8)
        try "# demo\n".write(to: root.appendingPathComponent("README.md"), atomically: true, encoding: .utf8)

        for (name, appearance) in [("light", NSAppearance.Name.aqua), ("dark", .darkAqua)] {
            let c = MainWindowController(bridge: CoreBridge())
            c.window?.appearance = NSAppearance(named: appearance)
            c.window?.setContentSize(NSSize(width: 900, height: 520))
            c.showWindow(nil)
            c.open(url: root)
            // Two tabs: README (preview, italic) and main.rs (active, edited).
            let ws = try XCTUnwrap(c.workspace)
            let readme = try XCTUnwrap(try ws.listDir(relPath: "").first { $0.name == "README.md" })
            c.openWorkspaceFile(ws, readme, preview: true)
            try c.openFile(at: root.appendingPathComponent("src/main.rs"))
            c.editorView.document?.edit(command: .insertText(text: "// "))
            c.editorView.didChangeDocument()
            c.editorView.flushPendingUpdates()
            c.showFind(nil)
            c.findBar.setPattern("greeting")
            RunLoop.current.run(until: Date().addingTimeInterval(0.3))
            let view = try XCTUnwrap(c.window?.contentView)
            view.layoutSubtreeIfNeeded()
            try render(view, to: dir, name: "window-\(name)")

            // Go to File over the same window.
            let deadline = Date().addingTimeInterval(5)
            while c.fileFinder == nil, Date() < deadline {
                RunLoop.current.run(until: Date().addingTimeInterval(0.02))
            }
            c.showGoToFile(nil)
            c.quickOpen.setQuery("mars")
            RunLoop.current.run(until: Date().addingTimeInterval(0.2))
            view.layoutSubtreeIfNeeded()
            try render(view, to: dir, name: "quickopen-\(name)")
            c.closeGoToFile()
            c.window?.orderOut(nil)
        }
    }

    private func render(_ view: NSView, to dir: String, name: String) throws {
        let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: rep)
        let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
        try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(name).png"))
    }
}
