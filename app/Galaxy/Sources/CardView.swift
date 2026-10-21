import AppKit
import os

/// A rounded panel with a hairline border, clipping what it holds to its
/// corners. The sidebar and the editor area are cards on the chrome colour.
@MainActor
class CardView: NSView {
    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = Theme.cardRadius
        layer?.cornerCurve = .continuous
        layer?.masksToBounds = true
        layer?.borderWidth = 1
        refreshBorder()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        refreshBorder()
    }

    /// Dynamic colours do not follow appearance changes on a layer by themselves.
    private func refreshBorder() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.borderColor = NSColor.separatorColor.cgColor
        }
    }
}

/// Holds the two cards: draws the chrome colour in the gap between them and
/// no divider line, but keeps the gap draggable.
@MainActor
final class CardSplitView: NSSplitView {
    override var dividerThickness: CGFloat { Theme.cardGap }
    override func drawDivider(in rect: NSRect) {}

    override func draw(_ dirtyRect: NSRect) {
        Theme.chrome.setFill()
        dirtyRect.intersection(bounds).fill()
        super.draw(dirtyRect)
    }
}

/// Solid sidebar background (no vibrancy): matches the flat chrome.
@MainActor
final class SidebarView: CardView {
    override func draw(_ dirtyRect: NSRect) {
        Theme.sidebar.setFill()
        dirtyRect.intersection(bounds).fill()
    }
}
