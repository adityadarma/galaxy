import AppKit

/// Effective keyboard shortcuts (M2-10): defaults from the Rust core, plus
/// the user's `keybindings.json` in VSCode's format.
///
/// Every binding is dispatched by the window's key monitor, so keys a menu
/// cannot express (`⌃⇧-`, a second key for one command) work the same as
/// the rest. Menu items show each command's primary key; a command only
/// runs when its menu item would be enabled.
@MainActor
struct Keybindings {
    /// Key id (`shift+cmd+p`) to command id.
    private(set) var commandByKey: [String: String] = [:]
    /// Each command's first binding: shown in menus and the palette.
    private(set) var primary: [String: KeyBindingInfo] = [:]
    private(set) var bindings: [KeyBindingInfo] = []
    private(set) var problems: [String] = []

    init(_ load: KeymapLoad) {
        bindings = load.bindings
        problems = load.problems
        for b in load.bindings {
            if commandByKey[b.keyId] == nil { commandByKey[b.keyId] = b.command }
            if primary[b.command] == nil { primary[b.command] = b }
        }
    }

    static let defaults = Keybindings(loadKeymap(userFile: nil))

    /// `~/Library/Application Support/Galaxy/User/keybindings.json`, the
    /// same place VSCode keeps its own.
    /// Under XCTest it points at a file that does not exist, so the
    /// user's own shortcuts never change what tests see.
    static var defaultURL: URL {
        if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil {
            return FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-no-keybindings.json")
        }
        return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Galaxy/User/keybindings.json")
    }

    /// Defaults plus the file at `url`. A missing file is not a problem.
    static func load(from url: URL) -> Keybindings {
        let text: String
        do {
            text = try String(contentsOf: url, encoding: .utf8)
        } catch CocoaError.fileReadNoSuchFile {
            return defaults
        } catch {
            var map = defaults
            map.problems = ["keybindings.json could not be read: \(error.localizedDescription)"]
            return map
        }
        return Keybindings(loadKeymap(userFile: text))
    }

    // MARK: Key events

    /// Canonical id for a key event, in the core's spelling and order
    /// (`ctrl+shift+alt+cmd+name`), or `nil` for keys we never bind.
    nonisolated static func keyID(for event: NSEvent) -> String? {
        let flags = event.modifierFlags
        let name: String
        if let named = namedKeys[event.keyCode] {
            name = named
        } else {
            // The character without Shift or Option, so ⌃⇧- is "-", not "_".
            let plain = event.characters(byApplyingModifiers: []) ?? event.charactersIgnoringModifiers ?? ""
            guard plain.count == 1, let c = plain.lowercased().first, c.isASCII, !c.isWhitespace else { return nil }
            name = String(c)
        }
        var id = ""
        if flags.contains(.control) { id += "ctrl+" }
        if flags.contains(.shift) { id += "shift+" }
        if flags.contains(.option) { id += "alt+" }
        if flags.contains(.command) { id += "cmd+" }
        return id + name
    }

    /// Hardware key codes of the keys spelled as words (`kVK_*`).
    nonisolated private static let namedKeys: [UInt16: String] = [
        48: "tab", 36: "enter", 76: "enter", 53: "escape", 49: "space", 51: "backspace", 117: "delete",
        126: "up", 125: "down", 123: "left", 124: "right", 115: "home", 119: "end",
        116: "pageup", 121: "pagedown",
        122: "f1", 120: "f2", 99: "f3", 118: "f4", 96: "f5", 97: "f6",
        98: "f7", 100: "f8", 101: "f9", 109: "f10", 103: "f11", 111: "f12",
    ]

    // MARK: Menus

    /// The menu key equivalent for a binding.
    static func menuKey(_ b: KeyBindingInfo) -> (key: String, modifiers: NSEvent.ModifierFlags) {
        let named: [String: String] = [
            "tab": "\t", "enter": "\r", "escape": "\u{1B}", "space": " ", "backspace": "\u{8}",
            "delete": "\u{F728}", "up": "\u{F700}", "down": "\u{F701}", "left": "\u{F702}", "right": "\u{F703}",
            "home": "\u{F729}", "end": "\u{F72B}", "pageup": "\u{F72C}", "pagedown": "\u{F72D}",
        ]
        var key = named[b.name] ?? b.name
        if b.name.count > 1, b.name.hasPrefix("f"), let n = Int(b.name.dropFirst()), (1...12).contains(n) {
            key = String(UnicodeScalar(0xF704 + n - 1)!)
        }
        var mods: NSEvent.ModifierFlags = []
        if b.ctrl { mods.insert(.control) }
        if b.shift { mods.insert(.shift) }
        if b.alt { mods.insert(.option) }
        if b.cmd { mods.insert(.command) }
        return (key, mods)
    }

    /// Command id for each menu action.
    static let commandByAction: [Selector: String] = {
        var out: [Selector: String] = [:]
        for (id, action) in CommandPaletteView.actions { out[action] = id }
        return out
    }()

    /// Shows each command's primary key on its menu item (none if it has
    /// no binding). Items that are not commands (Hide, …) keep theirs.
    func apply(to menu: NSMenu) {
        for item in menu.items {
            if let sub = item.submenu { apply(to: sub) }
            guard let action = item.action, let id = Self.commandByAction[action] else { continue }
            if let b = primary[id] {
                let (key, mods) = Self.menuKey(b)
                item.keyEquivalent = key
                item.keyEquivalentModifierMask = mods
            } else {
                item.keyEquivalent = ""
                item.keyEquivalentModifierMask = []
            }
        }
    }

    /// "⇧⌘P" style text for each bound command.
    var shortcutTexts: [String: String] {
        primary.mapValues { b in
            let (key, mods) = Self.menuKey(b)
            return CommandPaletteView.describe(key: key, modifiers: mods)
        }
    }
}
