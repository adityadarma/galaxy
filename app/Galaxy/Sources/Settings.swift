import Foundation

/// Global settings are read from Application Support; a workspace may override
/// the same editor keys in `.galaxy/settings.json`.
struct GalaxySettings: Equatable {
    var insertSpaces: Bool?
    var tabSize: UInt32?
    var autoSave: AutoSaveMode?
    var autoSaveDelay: TimeInterval?
    var renderWhitespace: WhitespaceRendering? = nil

    static let empty = GalaxySettings(insertSpaces: nil, tabSize: nil, autoSave: nil, autoSaveDelay: nil)

    func merged(with override: GalaxySettings) -> GalaxySettings {
        GalaxySettings(
            insertSpaces: override.insertSpaces ?? insertSpaces,
            tabSize: override.tabSize ?? tabSize,
            autoSave: override.autoSave ?? autoSave,
            autoSaveDelay: override.autoSaveDelay ?? autoSaveDelay,
            renderWhitespace: override.renderWhitespace ?? renderWhitespace
        )
    }

    var effectiveRenderWhitespace: WhitespaceRendering { renderWhitespace ?? .all }

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
        return GalaxySettings(insertSpaces: insertSpaces, tabSize: tabSize,
                              autoSave: autoSave, autoSaveDelay: autoSaveDelay,
                              renderWhitespace: renderWhitespace)
    }

    static func completionItems() -> [CompletionInfo] {
        [
            CompletionInfo(label: "editor", detail: "object", insertText: "editor", isSnippet: false),
            CompletionInfo(label: "insertSpaces", detail: "boolean", insertText: "insertSpaces", isSnippet: false),
            CompletionInfo(label: "tabSize", detail: "number", insertText: "tabSize", isSnippet: false),
            CompletionInfo(label: "renderWhitespace", detail: "none | boundary | all", insertText: "renderWhitespace", isSnippet: false),
            CompletionInfo(label: "files.autoSave", detail: "off | afterDelay | onFocusChange", insertText: "files.autoSave", isSnippet: false),
            CompletionInfo(label: "files.autoSaveDelay", detail: "milliseconds", insertText: "files.autoSaveDelay", isSnippet: false),
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
}
