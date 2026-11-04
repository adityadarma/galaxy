import AppKit

@MainActor
final class DiagnosticScrollView: NSScrollView {
    let diagnosticScroller = DiagnosticScroller()

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        verticalScroller = diagnosticScroller
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }
}

@MainActor
final class DiagnosticScroller: NSScroller {
    // Without this AppKit refuses `.overlay` for a custom scroller class and
    // falls back to the wide legacy scroller.
    override class var isCompatibleWithOverlayScrollers: Bool { true }

    private var markers: [(line: UInt64, severity: UInt8)] = []
    private var gitMarkers: [(line: UInt64, kind: GitLineChangeKind)] = []
    private var lineCount: UInt64 = 1

    /// Problem markers: 0-based lines. Drawn in the right half of the track.
    func update(markers: [(line: UInt64, severity: UInt8)], lineCount: UInt64) {
        self.markers = markers
        self.lineCount = max(1, lineCount)
        needsDisplay = true
    }

    /// Git markers: 1-based lines, as in `EditorView.gitLineChanges`. Drawn in
    /// the left half of the track.
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
        switch kind {
        case .added: .systemGreen
        case .modified: .systemYellow
        case .deleted: .systemRed
        }
    }

    override func drawKnob() {
        super.drawKnob()
        guard lineCount > 1 else { return }
        let lanes = Self.lanes(in: bounds)
        func y(_ line: UInt64) -> CGFloat {
            bounds.maxY - CGFloat(min(line, lineCount - 1)) / CGFloat(lineCount - 1) * bounds.height
        }
        for marker in gitMarkers {
            Self.color(for: marker.kind).setFill()
            NSRect(x: lanes.git.minX, y: y(marker.line) - 1, width: lanes.git.width, height: 2).fill()
        }
        for marker in markers {
            Theme.diagnostic(marker.severity).setFill()
            NSRect(x: lanes.problems.minX, y: y(marker.line) - 1, width: lanes.problems.width, height: 2).fill()
        }
    }
}
