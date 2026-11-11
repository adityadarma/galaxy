import XCTest
@testable import Galaxy

@MainActor
final class SettingsTests: XCTestCase {
    func testWorkspaceRustCompilerRefreshDefaultsOffAndParsesEnabled() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-settings-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: url) }
        XCTAssertNil(GalaxySettings.empty.rustCompilerRefreshEnabled)
        try #"{"rust":{"compilerRefreshEnabled":true}}"#.write(to: url, atomically: true, encoding: .utf8)
        XCTAssertEqual(try GalaxySettings.load(from: url).rustCompilerRefreshEnabled, true)
    }

    func testPHPAnalyzerVersionParsesAndWorkspaceOverridesGlobal() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-settings-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: url) }
        try #"{"php":{"version":"8.1"}}"#.write(to: url, atomically: true, encoding: .utf8)
        XCTAssertEqual(try GalaxySettings.load(from: url).phpVersion, "8.1")
        var global = GalaxySettings.empty
        global.phpVersion = "8.3"
        var workspace = GalaxySettings.empty
        workspace.phpVersion = "8.1"
        XCTAssertEqual(global.merged(with: workspace).phpVersion, "8.1")
    }

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

    func testFilesExcludeDefaultsHideVersionControlFoldersAndFilesCanTurnThemOff() throws {
        let defaults = GalaxySettings.empty.effectiveFilesExclude
        XCTAssertEqual(Set(defaults), Set(GalaxySettings.defaultFilesExclude))
        XCTAssertTrue(defaults.contains("**/.git"))
        XCTAssertTrue(defaults.contains("**/.DS_Store"))

        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-settings-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let globalURL = dir.appendingPathComponent("global.json")
        let workspaceURL = dir.appendingPathComponent("workspace.json")
        try #"{"files":{"exclude":{"**/node_modules":true,"**/.svn":false}}}"#
            .write(to: globalURL, atomically: true, encoding: .utf8)
        try #"{"files":{"exclude":{"**/.git":false,"**/dist":true}}}"#
            .write(to: workspaceURL, atomically: true, encoding: .utf8)

        let global = try GalaxySettings.load(from: globalURL)
        XCTAssertTrue(global.effectiveFilesExclude.contains("**/node_modules"))
        XCTAssertFalse(global.effectiveFilesExclude.contains("**/.svn"), "false turns a default off")

        let merged = global.merged(with: try GalaxySettings.load(from: workspaceURL))
        let globs = merged.effectiveFilesExclude
        XCTAssertFalse(globs.contains("**/.git"), "the workspace shows .git again")
        XCTAssertTrue(globs.contains("**/dist"))
        XCTAssertTrue(globs.contains("**/node_modules"), "global entries still apply")
        XCTAssertTrue(globs.contains("**/.hg"))
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

    func testInlineParameterHintsDefaultToOnAndWorkspaceCanTurnThemOff() throws {
        XCTAssertTrue(GalaxySettings.empty.effectiveInlineParameterHints)
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-settings-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: url) }
        try #"{"editor":{"inlineParameterHints":false}}"#.write(to: url, atomically: true, encoding: .utf8)
        let workspace = try GalaxySettings.load(from: url)
        XCTAssertFalse(GalaxySettings.empty.merged(with: workspace).effectiveInlineParameterHints)
        XCTAssertTrue(GalaxySettings.completionItems().contains { $0.label == "inlineParameterHints" })
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

    func testWritingAKeyKeepsTheRestOfTheFile() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-write-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: url) }
        try #"{"editor":{"tabSize":2},"files":{"autoSave":"afterDelay"},"custom":1}"#
            .write(to: url, atomically: true, encoding: .utf8)
        try GalaxySettings.write(false, key: "gitBlame", section: "editor", to: url)
        try GalaxySettings.write("off", key: "autoSave", section: "files", to: url)
        let settings = try GalaxySettings.load(from: url)
        XCTAssertEqual(settings.effectiveTabSize, 2)
        XCTAssertFalse(settings.effectiveGitBlame)
        XCTAssertEqual(settings.effectiveAutoSave, .off)
        let root = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        XCTAssertEqual(root["custom"] as? Int, 1, "unknown keys survive")
    }

    func testWritingCreatesTheFileAndKeepsFlatEditorKeysFlat() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-write-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }
        let url = dir.appendingPathComponent("sub/settings.json")
        try GalaxySettings.write(8, key: "tabSize", section: "editor", to: url)
        XCTAssertEqual(try GalaxySettings.load(from: url).effectiveTabSize, 8)

        let flat = dir.appendingPathComponent("flat.json")
        try #"{"tabSize":2}"#.write(to: flat, atomically: true, encoding: .utf8)
        try GalaxySettings.write(false, key: "insertSpaces", section: "editor", to: flat)
        let settings = try GalaxySettings.load(from: flat)
        XCTAssertEqual(settings.effectiveTabSize, 2, "a nested editor object would hide the flat keys")
        XCTAssertTrue(settings.useTabs)
    }

    func testABrokenFileIsNotOverwritten() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-write-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: url) }
        try #"{"editor": {"#.write(to: url, atomically: true, encoding: .utf8)
        XCTAssertThrowsError(try GalaxySettings.write(4, key: "tabSize", section: "editor", to: url))
        XCTAssertEqual(try String(contentsOf: url, encoding: .utf8), #"{"editor": {"#)
    }

    func testSettingsWindowWritesTheEditorTab() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-prefs-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: url) }
        var changes = 0
        let controller = PreferencesWindowController(settingsURL: url) { changes += 1 }
        func control<T>(_ name: String) throws -> T {
            try XCTUnwrap(Mirror(reflecting: controller).children.first { $0.label == name }?.value as? T, name)
        }
        let blame: NSButton = try control("gitBlameBox")
        XCTAssertEqual(blame.state, .on, "defaults show when there is no file")
        blame.state = .off
        _ = blame.target?.perform(blame.action, with: blame)
        let tabSize: NSPopUpButton = try control("tabSizePopup")
        tabSize.selectItem(withTitle: "2")
        _ = tabSize.target?.perform(tabSize.action, with: tabSize)
        let settings = try GalaxySettings.load(from: url)
        XCTAssertFalse(settings.effectiveGitBlame)
        XCTAssertEqual(settings.effectiveTabSize, 2)
        XCTAssertGreaterThan(changes, 0, "open windows are told")
    }

    func testSettingsWindowListsTheDefaultExcludesAndWritesToggles() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-prefs-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: url) }
        try #"{"files":{"exclude":{"**/*.log":true,"**/.git":true}}}"#.write(to: url, atomically: true, encoding: .utf8)
        var changes = 0
        let controller = PreferencesWindowController(settingsURL: url) { changes += 1 }
        let boxes = try XCTUnwrap(Mirror(reflecting: controller).children.first { $0.label == "excludeBoxes" }?.value as? [NSButton])
        XCTAssertEqual(boxes.map(\.title), GalaxySettings.defaultFilesExclude, "every default is listed")
        XCTAssertTrue(boxes.allSatisfy { $0.state == .on }, "defaults are on without being written to the file")

        let git = try XCTUnwrap(boxes.first { $0.title == "**/.git" })
        git.state = .off
        _ = git.target?.perform(git.action, with: git)
        let settings = try GalaxySettings.load(from: url)
        XCTAssertEqual(settings.filesExclude?["**/.git"], false)
        XCTAssertEqual(settings.filesExclude?["**/*.log"], true, "the user's own entry survives")
        XCTAssertFalse(settings.effectiveFilesExclude.contains("**/.git"))
        XCTAssertTrue(settings.effectiveFilesExclude.contains("**/.DS_Store"))
        XCTAssertGreaterThan(changes, 0, "open windows are told")
    }

    func testAutoSaveDefaultsStayConservative() {
        XCTAssertEqual(GalaxySettings.empty.effectiveAutoSave, .off)
        XCTAssertEqual(GalaxySettings.empty.effectiveAutoSaveDelay, 1.0, accuracy: 0.001)
    }
}
