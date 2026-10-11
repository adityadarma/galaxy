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
