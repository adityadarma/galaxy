import AppKit

/// Galaxy's own look. Everything else uses system colors, so light and
/// dark mode, increased contrast and the user's selection color still work.
enum Theme {
    private struct SyntaxTheme: Decodable {
        let scopes: [String: String]
    }

    private static let lightSyntax = loadSyntaxTheme(named: "Galaxy Light")
    private static let darkSyntax = loadSyntaxTheme(named: "One Dark Pro")

    /// A semantic syntax color for the current system appearance. JSON keeps
    /// the palette editable without coupling the renderer to Rust scopes.
    static func syntax(_ scope: String, appearance: NSAppearance) -> NSColor {
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let hex = (dark ? darkSyntax : lightSyntax)[scope]
        return hex.flatMap(NSColor.init(hex:)) ?? .textColor
    }

    private static func loadSyntaxTheme(named name: String) -> [String: String] {
        guard let url = Bundle(for: AppDelegate.self).url(forResource: name, withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let theme = try? JSONDecoder().decode(SyntaxTheme.self, from: data) else {
            return [:]
        }
        return theme.scopes
    }
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
    static let chrome = NSColor(name: "GalaxyChrome") { appearance in
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        return dark
            ? NSColor(hex: "#21252B") ?? .windowBackgroundColor
            : NSColor(srgbRed: 0.949, green: 0.945, blue: 0.937, alpha: 1)
    }
    /// Sidebar: between chrome and editor.
    static let sidebar = NSColor(name: "GalaxySidebar") { appearance in
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        return dark
            ? NSColor(hex: "#282C34") ?? .controlBackgroundColor
            : NSColor(srgbRed: 0.969, green: 0.965, blue: 0.957, alpha: 1)
    }
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

