import Foundation

/// Global settings are read from Application Support; a workspace may override
/// the same editor keys in `.galaxy/settings.json`.
struct GalaxySettings: Equatable {
    var insertSpaces: Bool?
    var tabSize: UInt32?
    var autoSave: AutoSaveMode?
    var autoSaveDelay: TimeInterval?
    var renderWhitespace: WhitespaceRendering? = nil
    /// Add the closing tag when `>` or `</` is typed in HTML, PHP and JSX.
    var autoClosingTags: Bool? = nil
    /// Rename a tag's partner when its name is edited.
    var linkedEditing: Bool? = nil
    /// Author and age of the caret line, after the text (`editor.gitBlame`).
    var gitBlame: Bool? = nil
    /// Parameter names before call arguments (`editor.inlineParameterHints`).
    var inlineParameterHints: Bool? = nil
    /// `files.exclude` as written in the file: glob to on/off. Entries are
    /// layered over `defaultFilesExclude`, so `false` turns a default off.
    var filesExclude: [String: Bool]? = nil

    /// VSCode's defaults, plus the Flutter folders.
    static let defaultFilesExclude: [String] = [
        "**/.git", "**/.svn", "**/.hg", "**/.jj", "**/.DS_Store", "**/Thumbs.db",
        "**/flutter/ephemeral", "**/Flutter/ephemeral", "**/.symlinks", "**/.plugin_symlinks",
    ]

    static let empty = GalaxySettings(insertSpaces: nil, tabSize: nil, autoSave: nil, autoSaveDelay: nil)

    func merged(with override: GalaxySettings) -> GalaxySettings {
        GalaxySettings(
            insertSpaces: override.insertSpaces ?? insertSpaces,
            tabSize: override.tabSize ?? tabSize,
            autoSave: override.autoSave ?? autoSave,
            autoSaveDelay: override.autoSaveDelay ?? autoSaveDelay,
            renderWhitespace: override.renderWhitespace ?? renderWhitespace,
            autoClosingTags: override.autoClosingTags ?? autoClosingTags,
            linkedEditing: override.linkedEditing ?? linkedEditing,
            gitBlame: override.gitBlame ?? gitBlame,
            inlineParameterHints: override.inlineParameterHints ?? inlineParameterHints,
            filesExclude: filesExclude.map { $0.merging(override.filesExclude ?? [:]) { _, new in new } }
                ?? override.filesExclude
        )
    }

    var effectiveRenderWhitespace: WhitespaceRendering { renderWhitespace ?? .all }
    var effectiveAutoClosingTags: Bool { autoClosingTags ?? true }
    var effectiveLinkedEditing: Bool { linkedEditing ?? true }
    var effectiveGitBlame: Bool { gitBlame ?? true }
    var effectiveInlineParameterHints: Bool { inlineParameterHints ?? true }
    /// Globs the explorer hides: the defaults, then the file's entries on top.
    var effectiveFilesExclude: [String] {
        var all = Dictionary(uniqueKeysWithValues: Self.defaultFilesExclude.map { ($0, true) })
        all.merge(filesExclude ?? [:]) { _, new in new }
        return all.filter(\.value).keys.sorted()
    }

    var useTabs: Bool { !(insertSpaces ?? true) }
    var effectiveTabSize: UInt32 { max(tabSize ?? 4, 1) }
    var effectiveAutoSave: AutoSaveMode { autoSave ?? .off }
    var effectiveAutoSaveDelay: TimeInterval { min(max(autoSaveDelay ?? 1.0, 0.1), 60.0) }

    static func load(from url: URL) throws -> GalaxySettings {
        let data = try Data(contentsOf: url)
        let object = try JSONSerialization.jsonObject(with: data)
        guard let root = object as? [String: Any] else {
            throw CocoaError(.fileReadCorruptFile)
        }
        let editor = root["editor"] as? [String: Any] ?? root
        let files = root["files"] as? [String: Any] ?? [:]
        let insertSpaces = editor["insertSpaces"] as? Bool
        let tabSize: UInt32?
        if let number = editor["tabSize"] as? NSNumber, number.intValue > 0 {
            tabSize = UInt32(number.intValue)
        } else {
            tabSize = nil
        }
        let autoSave: AutoSaveMode?
        switch files["autoSave"] as? String {
        case "afterDelay": autoSave = .afterDelay
        case "onFocusChange": autoSave = .onFocusChange
        case "off": autoSave = .off
        default: autoSave = nil
        }
        let autoSaveDelay = (files["autoSaveDelay"] as? NSNumber).map { max($0.doubleValue / 1000, 0.1) }
        let renderWhitespace: WhitespaceRendering? = switch editor["renderWhitespace"] as? String {
        case "none": WhitespaceRendering.none
        case "boundary": .boundary
        case "all", "selection", "trailing": .all
        default: nil
        }
        // VSCode shape: { "**/.git": true, "**/*.log": false }.
        let filesExclude = (files["exclude"] as? [String: Any]).map { object in
            object.compactMapValues { $0 as? Bool }
        }
        return GalaxySettings(insertSpaces: insertSpaces, tabSize: tabSize,
                              autoSave: autoSave, autoSaveDelay: autoSaveDelay,
                              renderWhitespace: renderWhitespace,
                              autoClosingTags: editor["autoClosingTags"] as? Bool,
                              linkedEditing: editor["linkedEditing"] as? Bool,
                              gitBlame: editor["gitBlame"] as? Bool,
                              inlineParameterHints: editor["inlineParameterHints"] as? Bool,
                              filesExclude: filesExclude)
    }

    static func completionItems() -> [CompletionInfo] {
        [
            CompletionInfo(label: "editor", detail: "object", insertText: "editor", isSnippet: false),
            CompletionInfo(label: "insertSpaces", detail: "boolean", insertText: "insertSpaces", isSnippet: false),
            CompletionInfo(label: "tabSize", detail: "number", insertText: "tabSize", isSnippet: false),
            CompletionInfo(label: "renderWhitespace", detail: "none | boundary | all", insertText: "renderWhitespace", isSnippet: false),
            CompletionInfo(label: "autoClosingTags", detail: "boolean: add </tag> after > (HTML, PHP, JSX)", insertText: "autoClosingTags", isSnippet: false),
            CompletionInfo(label: "linkedEditing", detail: "boolean: rename the matching tag", insertText: "linkedEditing", isSnippet: false),
            CompletionInfo(label: "gitBlame", detail: "boolean: show who last changed the caret line", insertText: "gitBlame", isSnippet: false),
            CompletionInfo(label: "inlineParameterHints", detail: "boolean: show parameter names before call arguments", insertText: "inlineParameterHints", isSnippet: false),
            CompletionInfo(label: "files.autoSave", detail: "off | afterDelay | onFocusChange", insertText: "files.autoSave", isSnippet: false),
            CompletionInfo(label: "files.autoSaveDelay", detail: "milliseconds", insertText: "files.autoSaveDelay", isSnippet: false),
            CompletionInfo(label: "files.exclude", detail: "object: globs hidden in the explorer, e.g. {\"**/.git\": true}", insertText: "files.exclude", isSnippet: false),
        ]
    }
}

/// `editor.renderWhitespace`, with VSCode's values (`selection` and
/// `trailing` are not drawn differently yet and fall back to `all`).
enum WhitespaceRendering: String, Equatable {
    case none
    case boundary
    case all
}

enum AutoSaveMode: String, Equatable {
    case off
    case afterDelay
    case onFocusChange
}

extension GalaxySettings {
    static var globalURL: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Galaxy/settings.json")
    }

    static func workspaceURL(root: String) -> URL {
        URL(fileURLWithPath: root).appendingPathComponent(".galaxy/settings.json")
    }

    /// Keys `load` also accepts at the top level instead of under `editor`.
    private static let editorKeys: Set<String> = [
        "insertSpaces", "tabSize", "renderWhitespace", "autoClosingTags", "linkedEditing", "gitBlame",
        "inlineParameterHints",
    ]

    /// Sets `section.key` in the settings file at `url` (the Settings window
    /// writes here), keeping every other key. A file that does not parse is
    /// left alone and the error is thrown, so a typo is never overwritten.
    static func write(_ value: Any, key: String, section: String, to url: URL = globalURL) throws {
        var root: [String: Any] = [:]
        if let data = try? Data(contentsOf: url), !data.isEmpty {
            guard let parsed = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                throw CocoaError(.fileReadCorruptFile)
            }
            root = parsed
        }
        // `load` reads editor keys from the top level when there is no
        // `editor` object; keep writing there so the file stays consistent.
        let flatEditor = section == "editor" && root["editor"] == nil
            && !editorKeys.isDisjoint(with: root.keys)
        if flatEditor {
            root[key] = value
        } else {
            var object = root[section] as? [String: Any] ?? [:]
            object[key] = value
            root[section] = object
        }
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let data = try JSONSerialization.data(withJSONObject: root, options: [.prettyPrinted, .sortedKeys])
        try data.write(to: url, options: .atomic)
    }

    /// Turns one `files.exclude` glob on or off in the file at `url`. The
    /// other entries stay as written, including ones this app does not read.
    static func writeFilesExclude(glob: String, enabled: Bool, to url: URL = globalURL) throws {
        var current: [String: Any] = [:]
        if let data = try? Data(contentsOf: url), !data.isEmpty {
            guard let root = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                throw CocoaError(.fileReadCorruptFile)
            }
            current = (root["files"] as? [String: Any])?["exclude"] as? [String: Any] ?? [:]
        }
        current[glob] = enabled
        try write(current, key: "exclude", section: "files", to: url)
    }
}
