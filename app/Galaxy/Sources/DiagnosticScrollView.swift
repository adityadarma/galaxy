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
    private var markers: [(line: UInt64, severity: UInt8)] = []
    private var lineCount: UInt64 = 1

    func update(markers: [(line: UInt64, severity: UInt8)], lineCount: UInt64) {
        self.markers = markers
        self.lineCount = max(1, lineCount)
        needsDisplay = true
    }

    override func drawKnob() {
        super.drawKnob()
        guard lineCount > 1 else { return }
        for marker in markers {
            let y = bounds.maxY - CGFloat(marker.line) / CGFloat(lineCount - 1) * bounds.height
            Theme.diagnostic(marker.severity).setFill()
            NSRect(x: 0, y: y - 1, width: bounds.width, height: 2).fill()
        }
    }
}
