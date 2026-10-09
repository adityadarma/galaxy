import AppKit

/// Automated native-scroll smoke benchmark (M1-08).
///
/// It moves an `NSScrollView` through a large document at roughly 60 Hz and
/// forces each newly visible viewport to draw. The measured work is the
/// scroll + viewport redraw on the main thread; WindowServer composition is
/// intentionally outside this number. Use Instruments to inspect the latter.
@MainActor
final class ScrollBench {
    static var requestedFrames: Int? {
        ProcessInfo.processInfo.environment["RCODE_SCROLL_BENCH"]
            .flatMap(Int.init)
            .flatMap { $0 > 0 ? $0 : nil }
    }

    private let view: EditorView
    private let frames: Int
    private var completed = 0
    private var drawTimes: [Double] = []
    private var intervals: [Double] = []
    private var lastTick: TimeInterval?
    private var displayLink: CADisplayLink?
    private static var running: ScrollBench?

    static func start(in controller: MainWindowController, frames: Int) {
        let bench = ScrollBench(view: controller.editorView, frames: frames)
        running = bench
        bench.begin()
    }

    private init(view: EditorView, frames: Int) {
        self.view = view
        self.frames = frames
    }

    private func begin() {
        guard let window = view.window, let scroll = view.enclosingScrollView else { return finish() }
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(view)
        scroll.contentView.scroll(to: .zero)
        scroll.reflectScrolledClipView(scroll.contentView)
        // A display link samples the actual refresh cadence. A regular
        // `Timer` has enough scheduling jitter to produce false missed-frame
        // reports even when rendering work easily fits the frame budget.
        let link = view.displayLink(target: self, selector: #selector(displayTick(_:)))
        link.add(to: .main, forMode: .common)
        displayLink = link
    }

    @objc private func displayTick(_ link: CADisplayLink) {
        tick()
    }

    private func tick() {
        guard let scroll = view.enclosingScrollView else { return finish() }
        let now = ProcessInfo.processInfo.systemUptime
        if let lastTick { intervals.append((now - lastTick) * 1_000) }
        lastTick = now

        let maximumY = max(0, view.bounds.height - scroll.contentView.bounds.height)
        let progress = CGFloat(completed + 1) / CGFloat(frames)
        let start = ProcessInfo.processInfo.systemUptime
        scroll.contentView.scroll(to: NSPoint(x: 0, y: maximumY * progress))
        scroll.reflectScrolledClipView(scroll.contentView)
        // Rendering a whole viewport is the worst case when a large scroll
        // jump invalidates all lines, not merely an exposed strip.
        view.needsDisplay = true
        view.displayIfNeeded()
        drawTimes.append((ProcessInfo.processInfo.systemUptime - start) * 1_000)
        completed += 1
        if completed == frames { finish() }
    }

    private func finish() {
        displayLink?.invalidate()
        displayLink = nil
        let draw = drawTimes.sorted()
        let interval = intervals.sorted()
        let drawP50 = LatencyProbe.percentile(draw, 0.5)
        let drawP95 = LatencyProbe.percentile(draw, 0.95)
        let intervalP50 = LatencyProbe.percentile(interval, 0.5)
        let intervalP95 = LatencyProbe.percentile(interval, 0.95)
        print(String(
            format: "scroll_frames=%d scroll_draw_p50_ms=%.2f scroll_draw_p95_ms=%.2f scroll_interval_p50_ms=%.2f scroll_interval_p95_ms=%.2f scroll_last_line=%d scroll_line_count=%d",
            completed, drawP50, drawP95, intervalP50, intervalP95,
            // Proves the run really scrolled through the document.
            view.lastDrawnLines.upperBound, view.lineCount
        ))
        fflush(stdout)
        exit(completed == frames ? 0 : 1)
    }
}
