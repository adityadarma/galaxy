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
        XCTAssertEqual(Theme.syntax("keyword", appearance: dark).resolved(for: dark), NSColor(hex: "#C678DD")?.resolved(for: dark))
        XCTAssertEqual(Theme.syntax("function", appearance: dark).resolved(for: dark), NSColor(hex: "#61AFEF")?.resolved(for: dark))
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

    /// Regression: the editor's syntax layout used
    /// `kCTForegroundColorFromContextAttributeName`, so CoreText painted every
    /// run in the context fill color and all code rendered as plain text.
    func testEditorSyntaxLayoutDrawsThemeColorsNotContextFill() {
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 400, height: 100))
        view.appearance = NSAppearance(named: .darkAqua)
        let text = "fn main"
        let line = view.syntaxLayout(for: text, styles: [StyleRun(line: 0, start: 0, end: 2, scope: "keyword")])

        let width = 200, height = 40
        let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                                space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        // Same as the editor: plain text color as the context fill.
        context.setFillColor(NSColor.white.cgColor)
        context.textPosition = CGPoint(x: 2, y: 12)
        CTLineDraw(line, context)

        // Keyword is #FF9AA9 in Galaxy Dark: count clearly pink pixels.
        let pixels = context.data!.bindMemory(to: UInt8.self, capacity: width * height * 4)
        var pink = 0
        for i in 0..<(width * height) where pixels[i * 4 + 3] > 120 {
            let r = Int(pixels[i * 4]), g = Int(pixels[i * 4 + 1])
            if r - g > 40 { pink += 1 }
        }
        XCTAssertGreaterThan(pink, 10, "keyword run must use its theme color, not the context fill")
    }

    /// LSP names are colored through theme scopes, not drawn as underlines.
    func testSemanticTokensBecomeStyleRunsByLine() {
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 400, height: 100))
        view.semanticTokens = [
            SemanticTokenInfo(line: 2, start: 3, length: 4, scope: "type", isDeclaration: false),
            SemanticTokenInfo(line: 2, start: 9, length: 1, scope: "variable", isDeclaration: false),
        ]
        XCTAssertEqual(view.semanticStylesByLine[2], [
            StyleRun(line: 2, start: 3, end: 7, scope: "type"),
            StyleRun(line: 2, start: 9, end: 10, scope: "variable"),
        ])
        XCTAssertNil(view.semanticStylesByLine[0])
        view.releaseNonEssentialCaches()
        XCTAssertTrue(view.semanticStylesByLine.isEmpty)
    }
}
