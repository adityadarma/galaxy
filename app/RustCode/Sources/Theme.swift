import AppKit

/// RustCode's own look. Everything else uses system colors, so light and
/// dark mode, increased contrast and the user's selection color still work.
enum Theme {
    /// Soft copper: caret, find matches, the active navigator and tab.
    static let accent = NSColor(name: "RustCodeAccent") { appearance in
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        return dark
            ? NSColor(srgbRed: 0.91, green: 0.60, blue: 0.40, alpha: 1)
            : NSColor(srgbRed: 0.72, green: 0.42, blue: 0.24, alpha: 1)
    }

    static let caret = accent
    static let findMatch = NSColor(name: "RustCodeFindMatch") { appearance in
        accent.resolved(for: appearance).withAlphaComponent(0.32)
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
