import AppKit

/// Galaxy's own look. Everything else uses system colors, so light and
/// dark mode, increased contrast and the user's selection color still work.
enum Theme {
    private struct SyntaxTheme: Decodable {
        let scopes: [String: String]
    }

    private static let lightSyntax = loadSyntaxTheme(named: "Galaxy Light")
    private static let darkSyntax = loadSyntaxTheme(named: "Galaxy Dark")

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
    /// Soft copper: caret, find matches, the active activity item and tab.
    static let accent = NSColor(name: "GalaxyAccent") { appearance in
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        return dark
            ? NSColor(srgbRed: 0.91, green: 0.60, blue: 0.40, alpha: 1)
            : NSColor(srgbRed: 0.72, green: 0.42, blue: 0.24, alpha: 1)
    }

    static let caret = accent
    static let findMatch = NSColor(name: "GalaxyFindMatch") { appearance in
        accent.resolved(for: appearance).withAlphaComponent(0.32)
    }

    /// Lines with a caret and no selection: a faint warm wash.
    static let currentLine = NSColor(name: "GalaxyCurrentLine") { appearance in
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        return dark
            ? NSColor(srgbRed: 1, green: 0.92, blue: 0.85, alpha: 0.06)
            : NSColor(srgbRed: 0.45, green: 0.30, blue: 0.20, alpha: 0.06)
    }
    /// Line numbers: quieter than body text; the caret line uses `accent`.
    static let gutterText = NSColor.secondaryLabelColor
    static let indentGuide = NSColor.separatorColor.withAlphaComponent(0.55)
    static let bracketMatch = NSColor(name: "GalaxyBracketMatch") { appearance in
        accent.resolved(for: appearance).withAlphaComponent(0.28)
    }
    /// Space above the first line, so text does not touch the path bar.
    static let editorTopInset: CGFloat = 6

    /// Title bar, activity bar, tab strip and status bar: one tone, a step
    /// away from the editor so the text area reads as the main surface.
    static let chrome = NSColor(name: "GalaxyChrome") { appearance in
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        return dark
            ? NSColor(srgbRed: 0.145, green: 0.141, blue: 0.137, alpha: 1)
            : NSColor(srgbRed: 0.949, green: 0.945, blue: 0.937, alpha: 1)
    }
    /// Sidebar: between chrome and editor.
    static let sidebar = NSColor(name: "GalaxySidebar") { appearance in
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        return dark
            ? NSColor(srgbRed: 0.125, green: 0.122, blue: 0.118, alpha: 1)
            : NSColor(srgbRed: 0.969, green: 0.965, blue: 0.957, alpha: 1)
    }
    static let tabHeight: CGFloat = 34

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
    fileprivate convenience init?(hex: String) {
        guard hex.count == 7, hex.first == "#", let value = UInt32(hex.dropFirst(), radix: 16) else { return nil }
        self.init(srgbRed: CGFloat((value >> 16) & 0xff) / 255,
                  green: CGFloat((value >> 8) & 0xff) / 255,
                  blue: CGFloat(value & 0xff) / 255, alpha: 1)
    }
}
