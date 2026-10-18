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
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Height of one pinned row.
    private var rowHeight: CGFloat { editor?.metrics.lineHeight ?? 0 }

    func show(_ newRows: [Row]) {
        let visible = isEnabled && !newRows.isEmpty
        isHidden = !visible
        let shown = visible ? newRows : []
        let height = CGFloat(shown.count) * rowHeight
        // A font change resizes the rows without changing them.
        if heightConstraint.constant != height {
            heightConstraint.constant = height
            needsDisplay = true
        }
        guard shown != rows else { return }
        rows = shown
        setAccessibilityLabel("Scope: " + shown.map { $0.text.trimmingCharacters(in: .whitespaces) }.joined(separator: ", "))
        needsDisplay = true
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

        NSColor.separatorColor.setFill()
        NSRect(x: 0, y: bounds.height - 1, width: bounds.width, height: 1).fill()
        // The gutter divider carries on through the pinned rows.
        NSColor.separatorColor.withAlphaComponent(0.28).setFill()
        NSRect(x: editor.gutterWidth - 1, y: 0, width: 1, height: max(0, bounds.height - 1)).fill()
    }
}
