import AppKit

/// The editor's scroll view. It owns the overview ruler that sits in the
/// scroll track at the editor's right edge, apart from the minimap.
@MainActor
final class DiagnosticScrollView: NSScrollView {
    let ruler = OverviewRulerView()
}

/// A narrow strip down the right edge of the editor, always visible (an overlay
/// scroller hides itself). Each change in the document is a tick at its
/// proportional position: Git on the left lane, problems on the right lane.
@MainActor
final class OverviewRulerView: NSView {
    static let width: CGFloat = 14

    private var markers: [(line: UInt64, severity: UInt8)] = []
    private var gitMarkers: [(line: UInt64, kind: GitLineChangeKind)] = []
    private var lineCount: UInt64 = 1

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { true }
    override var acceptsFirstResponder: Bool { false }

    override init(frame: NSRect) {
        super.init(frame: frame)
        autoresizingMask = []
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityIdentifier("overview-ruler")
        setAccessibilityLabel("Overview ruler")
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Problem markers: 0-based lines. Drawn in the right lane.
    func update(markers: [(line: UInt64, severity: UInt8)], lineCount: UInt64) {
        self.markers = markers
        self.lineCount = max(1, lineCount)
        needsDisplay = true
    }

    /// Git markers: 1-based lines, as in `EditorView.gitLineChanges`. Drawn in
    /// the left lane.
    func updateGit(changes: [UInt64: GitLineChangeKind], lineCount: UInt64) {
        gitMarkers = changes.map { (line: $0.key - 1, kind: $0.value) }.sorted { $0.line < $1.line }
        self.lineCount = max(1, lineCount)
        needsDisplay = true
    }

    /// Left lane for Git, right lane for problems.
    static func lanes(in bounds: NSRect) -> (git: NSRect, problems: NSRect) {
        let half = floor(bounds.width / 2)
        return (NSRect(x: bounds.minX, y: bounds.minY, width: half, height: bounds.height),
                NSRect(x: bounds.minX + half, y: bounds.minY, width: bounds.width - half, height: bounds.height))
    }

    static func color(for kind: GitLineChangeKind) -> NSColor {
        Theme.gitChange(kind)
    }

    /// Top of a 0-based line's tick, kept inside the strip.
    static func tickY(line: UInt64, lineCount: UInt64, height: CGFloat, tickHeight: CGFloat = 2) -> CGFloat {
        guard lineCount > 1 else { return 0 }
        let fraction = CGFloat(min(line, lineCount - 1)) / CGFloat(lineCount - 1)
        return min(max(fraction * height - tickHeight / 2, 0), max(height - tickHeight, 0))
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    override func scrollWheel(with event: NSEvent) {
        enclosingScrollView?.scrollWheel(with: event)
    }

    override func draw(_ dirtyRect: NSRect) {
        // Opaque: text scrolled sideways must not show through the strip.
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        bounds.fill()
        let lanes = Self.lanes(in: bounds)
        for marker in gitMarkers {
            Self.color(for: marker.kind).setFill()
            NSRect(x: lanes.git.minX, y: Self.tickY(line: marker.line, lineCount: lineCount, height: bounds.height),
                   width: lanes.git.width, height: 2).fill()
        }
        for marker in markers {
            Theme.diagnostic(marker.severity).setFill()
            NSRect(x: lanes.problems.minX, y: Self.tickY(line: marker.line, lineCount: lineCount, height: bounds.height),
                   width: lanes.problems.width, height: 2).fill()
        }
    }
}
