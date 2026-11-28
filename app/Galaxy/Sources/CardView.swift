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

    /// Three dots in the middle of the gap show that it can be dragged.
    /// Nothing is drawn while the sidebar is hidden and the gap is at the edge.
    override func drawDivider(in rect: NSRect) {
        guard arrangedSubviews.first?.isHidden == false else { return }
        GripDots.draw(centeredIn: rect, vertical: isVertical)
    }

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

/// The drag affordance of the gaps between the cards: three small dots
/// along the gap, centred in it.
enum GripDots {
    static let diameter: CGFloat = 3
    static let spacing: CGFloat = 4
    static var color: NSColor { .tertiaryLabelColor }

    /// Rects of the dots for a gap `rect`; `vertical` stacks them top to
    /// bottom, for the gap between side-by-side cards.
    static func rects(centeredIn rect: NSRect, vertical: Bool) -> [NSRect] {
        let length = 3 * diameter + 2 * spacing
        return (0..<3).map { index in
            let offset = CGFloat(index) * (diameter + spacing) - length / 2
            let origin = vertical
                ? NSPoint(x: rect.midX - diameter / 2, y: rect.midY + offset)
                : NSPoint(x: rect.midX + offset, y: rect.midY - diameter / 2)
            return NSRect(origin: origin, size: NSSize(width: diameter, height: diameter))
        }
    }

    @MainActor
    static func draw(centeredIn rect: NSRect, vertical: Bool) {
        color.setFill()
        for dot in rects(centeredIn: rect, vertical: vertical) {
            NSBezierPath(ovalIn: dot).fill()
        }
    }
}
