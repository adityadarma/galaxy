import XCTest
@testable import Galaxy

final class CoreBridgeTests: XCTestCase {
    func testCoreVersionIsNotEmpty() {
        XCTAssertFalse(coreVersion().isEmpty)
    }

    @MainActor
    func testPingRoundTripsThroughListener() async {
        let bridge = CoreBridge()
        bridge.start()
        let received = expectation(description: "pong")
        bridge.onEvent = { event in
            if case let .pong(message) = event, message == "test" {
                received.fulfill()
            }
        }
        bridge.ping("test")
        await fulfillment(of: [received], timeout: 2)
    }

    @MainActor
    func testOpenDocumentReadsLines() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-\(UUID().uuidString).txt")
        try "alpha\nbeta\n".write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let doc = try CoreBridge().openDocument(path: url.path)
        XCTAssertEqual(doc.lineCount(), 3)
        XCTAssertEqual(doc.lines(first: 0, count: 2), ["alpha", "beta"])
    }

    @MainActor
    func testHoverRequestUsesTheHoverFFIMethod() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-\(UUID().uuidString).ts")
        try "const value = 1;\nvalue\n".write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let bridge = CoreBridge()
        let doc = try bridge.openDocument(path: url.path)
        XCTAssertTrue(bridge.requestHover(document: doc, line: 1, character: 1))
        XCTAssertTrue(bridge.hover(document: doc)?.contents.hasPrefix("**variable `value`**") ?? false)
    }

    @MainActor
    func testDefinitionRequestUsesTheDefinitionFFIMethodForImports() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }

        let dependency = directory.appendingPathComponent("dependency.ts")
        let source = directory.appendingPathComponent("source.ts")
        try "export const imported = 1;\n".write(to: dependency, atomically: true, encoding: .utf8)
        try "import { imported } from './dependency';\nimported;\n"
            .write(to: source, atomically: true, encoding: .utf8)

        let bridge = CoreBridge()
        let document = try bridge.openDocument(path: source.path)
        bridge.requestDefinition(document: document, line: 0, character: 10)

        let locations = bridge.definitionLocations(document: document)
        XCTAssertEqual(locations.count, 1)
        XCTAssertTrue(locations[0].uri.hasSuffix("/dependency.ts"))
        XCTAssertEqual(locations[0].line, 0)
        XCTAssertEqual(locations[0].column, 13)
    }

    @MainActor
    func testDefinitionFollowsTypeImportAndPropertyAnnotation() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }

        let dependency = directory.appendingPathComponent("database.ts")
        let source = directory.appendingPathComponent("schema.d.ts")
        try "export interface QueryClientContract { connectionName: string }\n"
            .write(to: dependency, atomically: true, encoding: .utf8)
        try "import type { QueryClientContract } from './database';\nexport declare class Schema { db: QueryClientContract; }\n"
            .write(to: source, atomically: true, encoding: .utf8)

        let bridge = CoreBridge()
        let document = try bridge.openDocument(path: source.path)
        for (line, character) in [(0, 14), (1, 33)] {
            bridge.requestDefinition(document: document, line: UInt64(line), character: UInt64(character))
            let locations = bridge.definitionLocations(document: document)
            XCTAssertEqual(locations.count, 1, "cursor \(line):\(character)")
            XCTAssertTrue(locations[0].uri.hasSuffix("/database.ts"))
            XCTAssertEqual(locations[0].line, 0)
            XCTAssertEqual(locations[0].column, 7)
        }
    }

    @MainActor
    func testSavePreservesCrlfLineEndings() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-\(UUID().uuidString).txt")
        let original = Data("one\r\ntwo\r\n".utf8)
        try original.write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }

        let doc = try CoreBridge().openDocument(path: url.path)
        XCTAssertEqual(doc.lineEnding(), .crLf)
        XCTAssertEqual(doc.encoding(), .utf8)
        let start = TextPos(line: 0, colUtf16: 0)
        doc.setSelections(selections: [TextSelection(anchor: start, head: start)], primary: 0)
        doc.edit(command: .paste(text: "zero\n"))
        try doc.save()
        XCTAssertEqual(try Data(contentsOf: url), Data("zero\r\none\r\ntwo\r\n".utf8))
    }

    @MainActor
    func testViewportSnapshotWithMultiCursorAndUndo() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("galaxy-\(UUID().uuidString).txt")
        try "a😀\nb\n".write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }

        let doc = try CoreBridge().openDocument(path: url.path)
        // UTF-16 columns: after the emoji is column 3 (a + surrogate pair).
        let carets = [TextPos(line: 0, colUtf16: 3), TextPos(line: 1, colUtf16: 1)]
            .map { TextSelection(anchor: $0, head: $0) }
        doc.setSelections(selections: carets, primary: 0)
        doc.edit(command: .insertText(text: "!"))

        let snap = doc.viewport(firstLine: 0, count: 10)
        XCTAssertEqual(snap.lineCount, 3)
        XCTAssertEqual(snap.lines.map(\.text), ["a😀!", "b!", ""])
        XCTAssertEqual(snap.lines[0].carets, [4])
        XCTAssertEqual(snap.selectionCount, 2)
        // Column must index the Swift string's UTF-16 view directly.
        let line0 = snap.lines[0].text.utf16
        XCTAssertEqual(line0.count, Int(snap.lines[0].carets[0]))

        XCTAssertTrue(doc.undo())
        XCTAssertEqual(doc.lines(first: 0, count: 2), ["a😀", "b"])
        XCTAssertTrue(doc.canRedo())
    }

    @MainActor
    func testOpenMissingDocumentThrows() {
        XCTAssertThrowsError(try CoreBridge().openDocument(path: "/nonexistent/file"))
    }
}
