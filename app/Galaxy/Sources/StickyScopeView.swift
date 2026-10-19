import AppKit
import CoreText

/// Sticky scroll: the header lines of the blocks around the top visible line
/// (class, function, ...) stay pinned over the editor, drawn like the editor
/// draws them: line number in the gutter, syntax colors, same columns.
@MainActor
final class StickyScopeView: NSView {
    struct Row: Equatable {
        var line: UInt64
        var text: String
        var styles: [StyleRun]
    }

    /// Off: no scope header, whatever the scroll position.
    var isEnabled = true
    /// Whose font, gutter and layouts the rows are drawn with.
    weak var editor: EditorView?
    /// A click on a row: its line, and how many pinned rows sit above it.
    var onSelect: ((UInt64, Int) -> Void)?
    private(set) var rows: [Row] = []
    private var heightConstraint: NSLayoutConstraint!
    /// Row under the pointer, drawn with a faint wash like VSCode's hover.
    private var hoveredRow: Int? {
        didSet { if hoveredRow != oldValue { needsDisplay = true } }
    }

    override var isFlipped: Bool { true }
    override var mouseDownCanMoveWindow: Bool { false }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        heightConstraint = heightAnchor.constraint(equalToConstant: 0)
        heightConstraint.isActive = true
        isHidden = true
        setAccessibilityIdentifier("sticky-scope")
        setAccessibilityRole(.group)
        // Soft drop shadow under the pinned rows, so code scrolling beneath
        // reads as passing under them.
        layer?.masksToBounds = false
        layer?.shadowColor = NSColor.black.cgColor
        layer?.shadowOpacity = 0.28
        layer?.shadowRadius = 4
        layer?.shadowOffset = CGSize(width: 0, height: -2)
    }

    /// An explicit path spares Core Animation from deriving the shadow shape
    /// from the layer's pixels on every composited frame while scrolling.
    override func layout() {
        super.layout()
        layer?.shadowPath = CGPath(rect: bounds, transform: nil)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero,
                                       options: [.mouseMoved, .mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
                                       owner: self))
    }

    override func mouseMoved(with event: NSEvent) {
        hoveredRow = row(at: convert(event.locationInWindow, from: nil))
    }

    override func mouseExited(with event: NSEvent) { hoveredRow = nil }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Height of one pinned row.
    private var rowHeight: CGFloat { editor?.metrics.lineHeight ?? 0 }

    func show(_ newRows: [Row]) {
        let visible = isEnabled && !newRows.isEmpty
        if visible == isHidden { fade(in: visible) }
        let shown = visible ? newRows : []
        let height = CGFloat(shown.count) * rowHeight
        // A font change resizes the rows without changing them.
        if heightConstraint.constant != height {
            heightConstraint.constant = height
            needsDisplay = true
        }
        guard shown != rows else { return }
        rows = shown
        hoveredRow = nil
        setAccessibilityLabel("Scope: " + shown.map { $0.text.trimmingCharacters(in: .whitespaces) }.joined(separator: ", "))
        needsDisplay = true
    }

    /// Fades the bar in on first pin; hides at once otherwise, so no stale
    /// rows linger over the editor.
    private func fade(in visible: Bool) {
        guard visible, window != nil, !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion else {
            alphaValue = 1
            isHidden = !visible
            return
        }
        alphaValue = 0
        isHidden = false
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.12
            animator().alphaValue = 1
        }
    }

    /// The row under `point` (view coordinates), if any.
    func row(at point: NSPoint) -> Int? {
        guard rowHeight > 0, point.y >= 0 else { return nil }
        let index = Int(point.y / rowHeight)
        return index < rows.count ? index : nil
    }

    override func mouseDown(with event: NSEvent) {
        guard let index = row(at: convert(event.locationInWindow, from: nil)) else { return }
        onSelect?(rows[index].line, index)
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .pointingHand)
    }

    override func draw(_ dirtyRect: NSRect) {
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        dirtyRect.intersection(bounds).fill()
        guard let editor, let context = NSGraphicsContext.current?.cgContext else { return }
        let metrics = editor.metrics
        let textX = editor.textOriginX
        let gutterRight = editor.gutterRight
        context.saveGState()
        defer { context.restoreGState() }
        context.textMatrix = CGAffineTransform(scaleX: 1, y: -1)

        for (index, row) in rows.enumerated() {
            let top = CGFloat(index) * metrics.lineHeight
            if index == hoveredRow {
                Theme.currentLine.resolved(for: effectiveAppearance).setFill()
                NSRect(x: 0, y: top, width: bounds.width, height: metrics.lineHeight).fill()
            }
            let number = editor.layout(for: String(row.line + 1))
            let numberWidth = CGFloat(CTLineGetTypographicBounds(number, nil, nil, nil))
            context.setFillColor(Theme.gutterText.cgColor)
            context.textPosition = CGPoint(x: gutterRight - numberWidth, y: top + metrics.ascent)
            CTLineDraw(number, context)

            guard !row.text.isEmpty else { continue }
            context.setFillColor(Theme.editorForeground.resolved(for: effectiveAppearance).cgColor)
            let line = row.styles.isEmpty
                ? editor.layout(for: row.text)
                : editor.syntaxLayout(for: row.text, styles: row.styles)
            context.saveGState()
            context.clip(to: CGRect(x: textX, y: top, width: max(0, bounds.width - textX), height: metrics.lineHeight))
            context.textPosition = CGPoint(x: textX, y: top + metrics.ascent)
            CTLineDraw(line, context)
            context.restoreGState()
        }

        // The gutter divider carries on through the pinned rows.
        NSColor.separatorColor.withAlphaComponent(0.28).setFill()
        NSRect(x: editor.gutterWidth - 1, y: 0, width: 1, height: bounds.height).fill()
    }
}
