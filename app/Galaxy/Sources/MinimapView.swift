import AppKit

/// Zoomed-out overview of the document at the editor's right edge. Each
/// line is two points tall and each character one point wide, drawn as bars
/// for runs of non-blank text. A slider shows the part of the document the
/// editor displays; clicking or dragging moves it.
///
/// When the document is taller than the view, the overview scrolls along
/// with the editor so the slider always stays inside it.
@MainActor
final class MinimapView: NSView {
    static let width: CGFloat = 96
    static let rowHeight: CGFloat = 2
    static let leftPadding: CGFloat = 6
    static let maxColumns = 80

    /// Where the overview sits for one scroll position, in minimap points.
    struct Geometry: Equatable {
        /// How far the overview itself is scrolled up.
        var offset: CGFloat
        var sliderTop: CGFloat
        var sliderHeight: CGFloat
    }

    /// `topLine`/`bottomLine` are the fractional lines at the edges of the
    /// editor's visible text.
    static func geometry(lineCount: Int, height: CGFloat, topLine: Double, bottomLine: Double) -> Geometry {
        let lines = Double(max(lineCount, 1))
        let top = min(max(topLine, 0), lines)
        let bottom = min(max(bottomLine, top), lines)
        let visible = bottom - top
        let total = CGFloat(lines) * rowHeight
        let scrollable = lines - visible
        let fraction = scrollable > 0 ? min(max(top / scrollable, 0), 1) : 0
        let offset = max(0, total - height) * CGFloat(fraction)
        return Geometry(offset: offset,
                        sliderTop: CGFloat(top) * rowHeight - offset,
                        sliderHeight: max(CGFloat(visible) * rowHeight, 8))
    }

    /// Fractional document line under minimap `y`.
    static func line(atY y: CGFloat, offset: CGFloat, lineCount: Int) -> Double {
        min(max(Double((y + offset) / rowHeight), 0), Double(lineCount))
    }

    /// Runs of non-blank text as `(start, end)` minimap columns, at most
    /// `maxColumns` wide. A tab counts as four columns.
    static func runs(in text: String) -> [(start: Int, end: Int)] {
        var runs: [(start: Int, end: Int)] = []
        var column = 0
        var runStart: Int?
        for scalar in text.unicodeScalars {
            if column >= maxColumns { break }
            switch scalar {
            case " ", "\t":
                if let start = runStart { runs.append((start, column)); runStart = nil }
                column += scalar == "\t" ? 4 : 1
            default:
                if runStart == nil { runStart = column }
                column += 1
            }
        }
        if let start = runStart { runs.append((start, min(column, maxColumns))) }
        return runs
    }

    weak var editor: EditorView?
    weak var scrollView: NSScrollView?
    /// Problems in the active document, shown as VS Code-style overview-ruler
    /// marks at the minimap's right edge.
    private var diagnosticMarkers: [(line: UInt64, severity: UInt8)] = []
    private var isDragging = false
    private var isHovering = false
    private var cache: (document: ObjectIdentifier, revision: UInt64, first: Int, count: Int, lines: [String], styles: [StyleRun])?

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { true }
    override var acceptsFirstResponder: Bool { false }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override init(frame: NSRect) {
        super.init(frame: frame)
        autoresizingMask = [.minXMargin, .height]
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityIdentifier("minimap")
        setAccessibilityLabel("Minimap")
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: bounds, options: [.activeInKeyWindow, .mouseEnteredAndExited, .inVisibleRect], owner: self))
    }

    override func mouseEntered(with event: NSEvent) { isHovering = true; needsDisplay = true }
    override func mouseExited(with event: NSEvent) { isHovering = false; needsDisplay = true }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    // MARK: State from the editor

    /// Fractional lines at the top and bottom of what the editor shows.
    private func visibleLines() -> (top: Double, bottom: Double)? {
        guard let editor, let scrollView else { return nil }
        let clip = scrollView.contentView
        let inset = scrollView.contentInsets.top
        let topY = max(0, clip.bounds.origin.y + inset)
        let bottomY = max(topY, clip.bounds.origin.y + clip.bounds.height)
        return (editor.fractionalLine(atY: topY), editor.fractionalLine(atY: bottomY))
    }

    private func currentGeometry() -> Geometry? {
        guard let editor, let visible = visibleLines() else { return nil }
        return Self.geometry(lineCount: editor.lineCount, height: bounds.height,
                             topLine: visible.top, bottomLine: visible.bottom)
    }

    private func fetchContent(_ document: DocumentHandle, first: Int, count: Int) -> (lines: [String], styles: [StyleRun]) {
        let id = ObjectIdentifier(document)
        let revision = document.revision()
        if let c = cache, c.document == id, c.revision == revision, c.first == first, c.count == count {
            return (c.lines, c.styles)
        }
        let snapshot = document.viewport(firstLine: UInt64(first), count: UInt64(count))
        let lines = snapshot.lines.map(\.text)
        cache = (id, revision, first, count, lines, snapshot.styles)
        return (lines, snapshot.styles)
    }

    func updateDiagnosticMarkers(_ markers: [(line: UInt64, severity: UInt8)]) {
        guard diagnosticMarkers.map(\ .line) != markers.map(\ .line)
            || diagnosticMarkers.map(\ .severity) != markers.map(\ .severity) else { return }
        diagnosticMarkers = markers
        needsDisplay = true
    }

    /// Y of a document line in the currently scrolled minimap overview.
    static func markerY(line: UInt64, offset: CGFloat) -> CGFloat {
        CGFloat(line) * rowHeight - offset
    }

    // MARK: Drawing

    override func draw(_ dirtyRect: NSRect) {
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        bounds.fill()
        NSColor.separatorColor.withAlphaComponent(0.4).setFill()
        NSRect(x: 0, y: 0, width: 1, height: bounds.height).fill()

        guard let editor, let document = editor.document, editor.lineCount > 0,
              let geometry = currentGeometry(), let context = NSGraphicsContext.current?.cgContext else { return }

        let first = min(Int(geometry.offset / Self.rowHeight), editor.lineCount - 1)
        let capacity = Int(ceil(bounds.height / Self.rowHeight)) + 1
        let content = fetchContent(document, first: first, count: min(capacity, editor.lineCount - first))
        let originY = CGFloat(first) * Self.rowHeight - geometry.offset

        var bars: [NSColor: [CGRect]] = [:]
        let stylesByLine = Dictionary(grouping: content.styles, by: { Int($0.line) })
        for (index, text) in content.lines.enumerated() where !text.isEmpty {
            let y = originY + CGFloat(index) * Self.rowHeight + 0.25
            for run in Self.runs(in: text) {
                var boundaries = [run.start, run.end]
                for style in stylesByLine[first + index] ?? [] {
                    boundaries.append(min(max(Int(style.start), run.start), run.end))
                    boundaries.append(min(max(Int(style.end), run.start), run.end))
                }
                boundaries.sort()
                for (start, end) in zip(boundaries, boundaries.dropFirst()) where start < end {
                    let scope = (stylesByLine[first + index] ?? []).last {
                        Int($0.start) <= start && Int($0.end) >= end
                    }?.scope
                    let color = scope.map { Theme.syntax($0, appearance: effectiveAppearance) }
                        ?? Theme.editorForeground.resolved(for: effectiveAppearance)
                    bars[color, default: []].append(CGRect(x: Self.leftPadding + CGFloat(start), y: y,
                                                             width: CGFloat(end - start), height: Self.rowHeight - 0.5))
                }
            }
        }
        for (color, rects) in bars {
            context.setFillColor(color.withAlphaComponent(0.55).cgColor)
            context.fill(rects)
        }

        // Error, warning, and info stay visible on the overview ruler even
        // when the corresponding editor line is outside the text viewport.
        for marker in diagnosticMarkers {
            let y = Self.markerY(line: marker.line, offset: geometry.offset)
            guard y + Self.rowHeight >= 0, y <= bounds.height else { continue }
            Theme.diagnostic(marker.severity).setFill()
            NSRect(x: bounds.maxX - 5, y: y, width: 5, height: max(Self.rowHeight, 3)).fill()
        }

        let alpha: CGFloat = isDragging ? 0.2 : (isHovering ? 0.14 : 0.09)
        context.setFillColor(NSColor.labelColor.resolved(for: effectiveAppearance).withAlphaComponent(alpha).cgColor)
        context.fill(CGRect(x: 1, y: geometry.sliderTop, width: bounds.width - 1, height: geometry.sliderHeight))
    }

    // MARK: Mouse

    override func mouseDown(with event: NSEvent) {
        isDragging = true
        scroll(toY: convert(event.locationInWindow, from: nil).y)
        needsDisplay = true
    }

    override func mouseDragged(with event: NSEvent) {
        scroll(toY: convert(event.locationInWindow, from: nil).y)
    }

    override func mouseUp(with event: NSEvent) {
        isDragging = false
        needsDisplay = true
    }

    override func scrollWheel(with event: NSEvent) {
        scrollView?.scrollWheel(with: event)
    }

    /// Centers the editor on the document line under minimap `y`.
    func scroll(toY y: CGFloat) {
        guard let editor, let scrollView, let visible = visibleLines(), let geometry = currentGeometry() else { return }
        let line = Self.line(atY: y, offset: geometry.offset, lineCount: editor.lineCount)
        let target = max(0, line - (visible.bottom - visible.top) / 2)
        let clip = scrollView.contentView
        let wanted = NSPoint(x: clip.bounds.origin.x,
                             y: editor.y(forFractionalLine: target) - scrollView.contentInsets.top)
        clip.scroll(to: clip.constrainBoundsRect(NSRect(origin: wanted, size: clip.bounds.size)).origin)
        scrollView.reflectScrolledClipView(clip)
    }
}
