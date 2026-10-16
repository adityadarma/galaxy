import AppKit

/// One selectable color theme: syntax colors plus the surfaces around them.
struct ThemeDefinition: Sendable {
    let name: String
    let isDark: Bool
    let syntaxColors: [String: NSColor]
    let editorBackground: NSColor
    let editorForeground: NSColor
    /// Title bar, activity bar, tab strip and status bar.
    let chrome: NSColor
    let sidebar: NSColor
}

/// Galaxy's own look. Controls not listed here use system colors, so
/// increased contrast and the user's selection color still work.
enum Theme {
    /// Tracks the macOS appearance: Galaxy Light or Galaxy Dark.
    static let systemName = "System"
    static let lightName = "Galaxy Light"
    static let darkName = "Galaxy Dark"

    private struct File: Decodable {
        struct UI: Decodable {
            let editorBackground: String?
            let editorForeground: String?
            let chrome: String?
            let sidebar: String?
        }
        let name: String
        let type: String?
        let scopes: [String: String]
        let ui: UI?
    }

    /// Every theme in the app bundle. The two built-in defaults come first,
    /// the rest follow by name. A JSON file is a theme when it has `scopes`.
    static let all: [ThemeDefinition] = {
        let urls = Bundle(for: AppDelegate.self).urls(forResourcesWithExtension: "json", subdirectory: nil) ?? []
        let themes = urls.compactMap { url -> ThemeDefinition? in
            guard let data = try? Data(contentsOf: url),
                  let file = try? JSONDecoder().decode(File.self, from: data) else { return nil }
            return definition(from: file)
        }
        let builtIn = [lightName, darkName]
        return themes.sorted {
            let a = builtIn.firstIndex(of: $0.name) ?? builtIn.count
            let b = builtIn.firstIndex(of: $1.name) ?? builtIn.count
            return a != b ? a < b : $0.name.localizedStandardCompare($1.name) == .orderedAscending
        }
    }()

    /// What the settings list offers: System first, then every theme.
    static var names: [String] { [systemName] + all.map(\.name) }

    private static func definition(from file: File) -> ThemeDefinition {
        let dark = file.type != "light"
        func color(_ hex: String?, _ fallback: NSColor) -> NSColor {
            hex.flatMap(NSColor.init(hex:)) ?? fallback
        }
        let ui = file.ui
        return ThemeDefinition(
            name: file.name, isDark: dark,
            syntaxColors: file.scopes.compactMapValues(NSColor.init(hex:)),
            editorBackground: color(ui?.editorBackground, .textBackgroundColor),
            editorForeground: color(ui?.editorForeground, .textColor),
            chrome: color(ui?.chrome, dark ? NSColor(srgbRed: 0.129, green: 0.145, blue: 0.169, alpha: 1)
                                           : NSColor(srgbRed: 0.949, green: 0.945, blue: 0.937, alpha: 1)),
            sidebar: color(ui?.sidebar, dark ? NSColor(srgbRed: 0.157, green: 0.173, blue: 0.204, alpha: 1)
                                             : NSColor(srgbRed: 0.969, green: 0.965, blue: 0.957, alpha: 1)))
    }

    /// Used only if a bundled theme file is missing or unreadable.
    private static func fallback(dark: Bool) -> ThemeDefinition {
        definition(from: File(name: dark ? darkName : lightName, type: dark ? "dark" : "light", scopes: [:], ui: nil))
    }

    // The choice is read on every draw, so it lives in memory. Main thread only.
    nonisolated(unsafe) private static var selection = storedSelection()

    /// "System" or a theme name.
    static var selectedName: String { selection }

    private static func storedSelection() -> String {
        // Tests must not depend on what the developer picked in the app.
        if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil { return systemName }
        let defaults = UserDefaults.standard
        if let name = defaults.string(forKey: "Theme") { return name }
        // Before themes had names the choice was Light, Dark or System.
        switch defaults.string(forKey: "Appearance") {
        case "Light": return lightName
        case "Dark": return darkName
        default: return systemName
        }
    }

    static func select(_ name: String, persist: Bool = true) {
        selection = name
        if persist { UserDefaults.standard.set(name, forKey: "Theme") }
    }

    /// The theme to draw with under `appearance`. An explicit choice wins;
    /// System follows the appearance.
    static func definition(for appearance: NSAppearance) -> ThemeDefinition {
        if selection != systemName, let chosen = all.first(where: { $0.name == selection }) {
            return chosen
        }
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let name = dark ? darkName : lightName
        return all.first { $0.name == name } ?? fallback(dark: dark)
    }

    /// Makes the app's controls (labels, separators, scrollers) match the
    /// chosen theme, then redraws everything. System follows macOS again.
    @MainActor
    static func apply() {
        if selection != systemName, let chosen = all.first(where: { $0.name == selection }) {
            NSApp.appearance = NSAppearance(named: chosen.isDark ? .darkAqua : .aqua)
        } else {
            NSApp.appearance = nil
        }
        // Two dark themes share an appearance, so AppKit sends no change.
        func invalidate(_ view: NSView) {
            view.needsDisplay = true
            view.subviews.forEach(invalidate)
        }
        for window in NSApp.windows {
            if let content = window.contentView { invalidate(content) }
        }
    }

    /// A semantic syntax color for the current theme. JSON keeps the palette
    /// editable without coupling the renderer to Rust scopes.
    static func syntax(_ scope: String, appearance: NSAppearance) -> NSColor {
        let theme = definition(for: appearance)
        return theme.syntaxColors[scope] ?? theme.editorForeground
    }

    /// Editor surface and plain text, shared by everything that sits flush
    /// with the editor (path bar, selected tab, panels, terminal).
    static let editorBackground = NSColor(name: "GalaxyEditorBackground") { definition(for: $0).editorBackground }
    static let editorForeground = NSColor(name: "GalaxyEditorForeground") { definition(for: $0).editorForeground }

    /// Blue accent: caret, find matches, active tab and editor decorations.
    static let accent = NSColor(name: "GalaxyAccent") { appearance in
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        return dark
            ? NSColor(srgbRed: 0.18, green: 0.55, blue: 0.95, alpha: 1)
            : NSColor(srgbRed: 0.00, green: 0.38, blue: 0.78, alpha: 1)
    }

    static let caret = accent
    static let activityActive = accent
    static let findMatch = NSColor(name: "GalaxyFindMatch") { appearance in
        accent.resolved(for: appearance).withAlphaComponent(0.32)
    }

    /// Lines with a caret and no selection: a faint blue wash.
    static let currentLine = NSColor(name: "GalaxyCurrentLine") { appearance in
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        return dark
            ? NSColor(srgbRed: 0.42, green: 0.70, blue: 1, alpha: 0.08)
            : NSColor(srgbRed: 0.00, green: 0.38, blue: 0.78, alpha: 0.08)
    }
    /// Diff tab: lines added or changed (right), removed or replaced (left).
    static let diffAdded = NSColor.systemGreen.withAlphaComponent(0.16)
    static let diffRemoved = NSColor.systemRed.withAlphaComponent(0.16)
    /// Line numbers: quieter than body text; the caret line uses `accent`.
    static let gutterText = NSColor.secondaryLabelColor
    static let indentGuide = NSColor.separatorColor.withAlphaComponent(0.55)
    /// Space/tab markers: visible on close look, quieter than comments.
    static let whitespace = NSColor.secondaryLabelColor.withAlphaComponent(0.38)
    static let bracketMatch = NSColor(name: "GalaxyBracketMatch") { appearance in
        accent.resolved(for: appearance).withAlphaComponent(0.28)
    }
    static func diagnostic(_ severity: UInt8) -> NSColor {
        switch severity {
        case 1: .systemRed
        case 2: .systemOrange
        case 3: .systemBlue
        default: .secondaryLabelColor
        }
    }
    /// Space above the first line, so text does not touch the path bar.
    static let editorTopInset: CGFloat = 6

    /// Title bar, activity bar, tab strip and status bar: one tone, a step
    /// away from the editor so the text area reads as the main surface.
    static let chrome = NSColor(name: "GalaxyChrome") { definition(for: $0).chrome }
    /// Sidebar: between chrome and editor.
    static let sidebar = NSColor(name: "GalaxySidebar") { definition(for: $0).sidebar }
    static let tabHeight: CGFloat = 34

    static var editorFont: NSFont {
        let defaults = UserDefaults.standard
        let size = max(10, min(24, defaults.object(forKey: "EditorFontSize") as? CGFloat ?? 13))
        guard let name = defaults.string(forKey: "EditorFontName"),
              name != "System Monospaced",
              let selected = NSFont(name: name, size: size) else {
            return .monospacedSystemFont(ofSize: size, weight: .regular)
        }
        return selected
    }

    /// Status bar: compact, secondary text.
    static var statusFont: NSFont { .systemFont(ofSize: 11) }
    static let statusHeight: CGFloat = 24
}

extension NSColor {
    /// `self` as seen under `appearance` (dynamic colors resolve lazily).
    func resolved(for appearance: NSAppearance) -> NSColor {
        var color = self
        appearance.performAsCurrentDrawingAppearance { color = self.usingColorSpace(.sRGB) ?? self }
        return color
    }
}

extension NSColor {
    convenience init?(hex: String) {
        guard hex.count == 7, hex.first == "#", let value = UInt32(hex.dropFirst(), radix: 16) else { return nil }
        self.init(srgbRed: CGFloat((value >> 16) & 0xff) / 255,
                  green: CGFloat((value >> 8) & 0xff) / 255,
                  blue: CGFloat(value & 0xff) / 255, alpha: 1)
    }
}

/// Git modification status and colors matching VS Code conventions.
enum GitFileStatus: Equatable {
    case modified
    case added
    case deleted
    case renamed
    case untracked
    case none

    static func from(code: String) -> GitFileStatus {
        if code.contains("?") { return .untracked }
        if code.contains("A") { return .added }
        if code.contains("D") { return .deleted }
        if code.contains("R") { return .renamed }
        if code.contains("U") { return .untracked }
        if code.contains("M") { return .modified }
        let trimmed = code.trimmingCharacters(in: .whitespaces)
        if !trimmed.isEmpty { return .modified }
        return .none
    }

    var color: NSColor? {
        switch self {
        case .modified:
            // VS Code modified amber/yellow #e2c08d
            return NSColor(srgbRed: 226/255.0, green: 192/255.0, blue: 141/255.0, alpha: 1.0)
        case .added, .untracked, .renamed:
            // VS Code added/new green #73c991
            return NSColor(srgbRed: 115/255.0, green: 201/255.0, blue: 145/255.0, alpha: 1.0)
        case .deleted:
            // VS Code deleted red #c74e39
            return NSColor(srgbRed: 199/255.0, green: 78/255.0, blue: 57/255.0, alpha: 1.0)
        case .none:
            return nil
        }
    }
}

