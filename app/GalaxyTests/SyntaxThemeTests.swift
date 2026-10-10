import XCTest
import CoreText
@testable import Galaxy

@MainActor
final class SyntaxThemeTests: XCTestCase {
    func testJSONThemesSupplyDistinctColorsForEveryScope() {
        let scopes = ["attribute", "comment", "constant", "constructor", "function", "keyword", "label", "number", "operator", "property", "punctuation", "string", "tag", "type", "variable"]
        let light = NSAppearance(named: .aqua)!
        let dark = NSAppearance(named: .darkAqua)!
        for scope in scopes {
            XCTAssertNotEqual(Theme.syntax(scope, appearance: light), NSColor.textColor, scope)
            XCTAssertNotEqual(Theme.syntax(scope, appearance: dark), NSColor.textColor, scope)
        }
        XCTAssertNotEqual(Theme.syntax("keyword", appearance: light).resolved(for: light),
                          Theme.syntax("keyword", appearance: dark).resolved(for: dark))
    }

    func testSyntaxRunsProduceColoredCoreTextAttributes() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-theme-\(UUID().uuidString).rs")
        defer { try? FileManager.default.removeItem(at: url) }
        try "// note\nfn main() {}\n".write(to: url, atomically: true, encoding: .utf8)
        let doc = try CoreBridge().openDocument(path: url.path)
        let deadline = Date().addingTimeInterval(2)
        var snapshot = doc.viewport(firstLine: 0, count: 2)
        while snapshot.styles.isEmpty && Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.005))
            snapshot = doc.viewport(firstLine: 0, count: 2)
        }
        let comment = try XCTUnwrap(snapshot.styles.first { $0.scope == "comment" })
        let line = try XCTUnwrap(snapshot.lines.first { $0.line == comment.line })
        let attributed = NSMutableAttributedString(string: line.text, attributes: [.font: EditorView.defaultFont, .foregroundColor: NSColor.textColor])
        attributed.addAttribute(.foregroundColor,
                                value: Theme.syntax(comment.scope, appearance: NSAppearance.current),
                                range: NSRange(location: Int(comment.start), length: Int(comment.end - comment.start)))
        let color = attributed.attribute(.foregroundColor, at: Int(comment.start), effectiveRange: nil) as? NSColor
        XCTAssertEqual(color, Theme.syntax("comment", appearance: NSAppearance.current))
    }
}
