import XCTest
@testable import Galaxy

@MainActor
final class SettingsTests: XCTestCase {
    func testWorkspaceSettingsOverrideGlobalEditorValues() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-settings-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }

        let globalURL = dir.appendingPathComponent("global.json")
        let workspaceURL = dir.appendingPathComponent("workspace.json")
        try #"{"editor":{"insertSpaces":true,"tabSize":2}}"#.write(to: globalURL, atomically: true, encoding: .utf8)
        try #"{"editor":{"tabSize":8}}"#.write(to: workspaceURL, atomically: true, encoding: .utf8)

        let global = try GalaxySettings.load(from: globalURL)
        let workspace = try GalaxySettings.load(from: workspaceURL)
        let settings = global.merged(with: workspace)
        XCTAssertFalse(settings.useTabs)
        XCTAssertEqual(settings.effectiveTabSize, 8)
    }

    func testTopLevelEditorKeysAndInvalidTabSizeUseDefaults() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-settings-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: url) }
        try #"{"insertSpaces":false,"tabSize":0}"#.write(to: url, atomically: true, encoding: .utf8)

        let settings = try GalaxySettings.load(from: url)
        XCTAssertTrue(settings.useTabs)
        XCTAssertEqual(settings.effectiveTabSize, 4)
    }

    func testTagEditingSettingsDefaultToOnAndWorkspaceCanTurnThemOff() throws {
        XCTAssertTrue(GalaxySettings.empty.effectiveAutoClosingTags)
        XCTAssertTrue(GalaxySettings.empty.effectiveLinkedEditing)

        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-tags-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let globalURL = dir.appendingPathComponent("global.json")
        let workspaceURL = dir.appendingPathComponent("workspace.json")
        try #"{"editor":{"autoClosingTags":false,"linkedEditing":false}}"#.write(to: globalURL, atomically: true, encoding: .utf8)
        try #"{"editor":{"linkedEditing":true}}"#.write(to: workspaceURL, atomically: true, encoding: .utf8)

        let settings = try GalaxySettings.load(from: globalURL).merged(with: GalaxySettings.load(from: workspaceURL))
        XCTAssertFalse(settings.effectiveAutoClosingTags)
        XCTAssertTrue(settings.effectiveLinkedEditing)
    }

    func testTypingInAnHTMLDocumentClosesAndRenamesTags() throws {
        let bridge = CoreBridge()
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("tags-\(UUID().uuidString).html")
        try "<div".write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }
        let doc = try bridge.core.openDocument(path: url.path)
        doc.setSelections(selections: [TextSelection(anchor: TextPos(line: 0, colUtf16: 4), head: TextPos(line: 0, colUtf16: 4))], primary: 0)
        doc.edit(command: .insertText(text: ">"))
        XCTAssertEqual(doc.text(), "<div></div>")
        doc.edit(command: .insertText(text: "s"))
        XCTAssertEqual(doc.text(), "<div>s</div>", "the letter goes between the tags")

        doc.setSelections(selections: [TextSelection(anchor: TextPos(line: 0, colUtf16: 4), head: TextPos(line: 0, colUtf16: 4))], primary: 0)
        doc.edit(command: .insertText(text: "x"))
        XCTAssertEqual(doc.text(), "<divx>s</divx>")

        bridge.setTagEditing(document: doc, autoClose: false, autoRename: false)
        doc.edit(command: .insertText(text: "y"))
        XCTAssertEqual(doc.text(), "<divxy>s</divx>")
    }

    func testSettingsCompletionContainsEditorKeys() {
        let labels = Set(GalaxySettings.completionItems().map(\.label))
        XCTAssertTrue(labels.isSuperset(of: ["editor", "insertSpaces", "tabSize"]))
    }

    func testAutoSaveSettingsUseMillisecondsAndWorkspaceOverride() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-autosave-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let globalURL = dir.appendingPathComponent("global.json")
        let workspaceURL = dir.appendingPathComponent("workspace.json")
        try #"{"files":{"autoSave":"afterDelay","autoSaveDelay":2500}}"#.write(to: globalURL, atomically: true, encoding: .utf8)
        try #"{"files":{"autoSave":"onFocusChange"}}"#.write(to: workspaceURL, atomically: true, encoding: .utf8)
        let settings = try GalaxySettings.load(from: globalURL).merged(with: try GalaxySettings.load(from: workspaceURL))
        XCTAssertEqual(settings.effectiveAutoSave, .onFocusChange)
        XCTAssertEqual(settings.effectiveAutoSaveDelay, 2.5, accuracy: 0.001)
    }

    func testAutoSaveDefaultsStayConservative() {
        XCTAssertEqual(GalaxySettings.empty.effectiveAutoSave, .off)
        XCTAssertEqual(GalaxySettings.empty.effectiveAutoSaveDelay, 1.0, accuracy: 0.001)
    }
}
