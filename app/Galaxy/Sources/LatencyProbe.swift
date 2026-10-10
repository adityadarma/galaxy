import AppKit
import os

/// Keypress-to-render latency (M1-16).
///
/// Measures from the key event's timestamp to the end of the
/// `EditorView.draw` that shows its effect. For real key presses the
/// timestamp is set by the system when the key is pressed, so the number
/// includes event delivery, the Rust edit, the FFI snapshot and drawing.
/// It does not include the WindowServer composite that puts the frame on
/// screen (up to one display refresh more).
@MainActor
final class LatencyProbe {
    static let shared = LatencyProbe()
    static let capacity = 1_000
    private static let signposter = OSSignposter(subsystem: Log.subsystem, category: "latency")

    struct Summary: Equatable {
        var count: Int
        var p50: Double
        var p95: Double
        var max: Double
        /// Time spent inside `draw` itself.
        var drawP50: Double
        var drawP95: Double
    }

    /// Event-to-draw-end samples in milliseconds, oldest first.
    private(set) var latencies: [Double] = []
    private(set) var drawTimes: [Double] = []
    private var pending: (time: TimeInterval, signpost: OSSignpostIntervalState)?
    /// Every `draw` seen, sample or not (diagnostics).
    private(set) var drawCalls = 0
    private(set) var cancels = 0
    /// Called after each recorded sample (the self-driving benchmark).
    var onSample: (() -> Void)?

    func reset() {
        latencies.removeAll()
        drawTimes.removeAll()
        drawCalls = 0
        pending = nil
    }

    /// Start of a key event that may change the text.
    func keyDown(_ event: NSEvent) {
        let now = ProcessInfo.processInfo.systemUptime
        // Synthesized events can carry a zero timestamp.
        let time = event.timestamp > 0 && event.timestamp <= now ? event.timestamp : now
        let state = Self.signposter.beginInterval("keypress-to-render")
        pending = (time, state)
    }

    /// The key did not change anything that needs drawing.
    func cancelPending() {
        if pending != nil { cancels += 1 }
        if let pending { Self.signposter.endInterval("keypress-to-render", pending.signpost) }
        pending = nil
    }

    /// End of `EditorView.draw`. `drawStart` is when that draw began.
    func drawFinished(drawStart: TimeInterval) {
        drawCalls += 1
        guard let pending else { return }
        self.pending = nil
        let now = ProcessInfo.processInfo.systemUptime
        Self.signposter.endInterval("keypress-to-render", pending.signpost)
        append((now - pending.time) * 1000, to: &latencies)
        append((now - drawStart) * 1000, to: &drawTimes)
        onSample?()
    }

    var summary: Summary? {
        guard !latencies.isEmpty else { return nil }
        let l = latencies.sorted(), d = drawTimes.sorted()
        return Summary(count: l.count, p50: Self.percentile(l, 0.5), p95: Self.percentile(l, 0.95),
                       max: l.last ?? 0, drawP50: Self.percentile(d, 0.5), drawP95: Self.percentile(d, 0.95))
    }

    /// One line for logs and the benchmark script.
    var report: String {
        guard let s = summary else { return "latency_samples=0" }
        return String(format: "latency_samples=%d latency_p50_ms=%.2f latency_p95_ms=%.2f latency_max_ms=%.2f draw_p50_ms=%.2f draw_p95_ms=%.2f",
                      s.count, s.p50, s.p95, s.max, s.drawP50, s.drawP95)
    }

    static func percentile(_ sorted: [Double], _ p: Double) -> Double {
        guard !sorted.isEmpty else { return 0 }
        let rank = Int((p * Double(sorted.count)).rounded(.up)) - 1
        return sorted[min(max(rank, 0), sorted.count - 1)]
    }

    private func append(_ value: Double, to array: inout [Double]) {
        array.append(value)
        if array.count > Self.capacity { array.removeFirst(array.count - Self.capacity) }
    }
}

/// `GALAXY_LATENCY_BENCH=<n>`: after launch, types `n` keys into the editor
/// by itself (alternating "x" and Backspace, so the text stays the same),
/// prints `LatencyProbe.report`, and exits. Used by
/// `scripts/measure-latency.sh`; nobody has to touch the keyboard.
///
/// Events are sent with `NSWindow.sendEvent`, so they skip the hardware
/// and WindowServer queue but go through the same `keyDown` ->
/// `interpretKeyEvents` -> edit -> draw path as real typing.
@MainActor
final class LatencyBench {
    static var requestedCount: Int? {
        ProcessInfo.processInfo.environment["GALAXY_LATENCY_BENCH"].flatMap(Int.init).flatMap { $0 > 0 ? $0 : nil }
    }

    private let view: EditorView
    private let count: Int
    private var sent = 0
    private var startRevision: UInt64 = 0
    private var timer: Timer?
    private var deadline: Timer?
    private static var running: LatencyBench?

    static func start(in controller: MainWindowController, count: Int) {
        let bench = LatencyBench(view: controller.editorView, count: count)
        running = bench
        bench.begin()
    }

    private init(view: EditorView, count: Int) {
        self.view = view
        self.count = count
    }

    private func begin() {
        guard let window = view.window, let doc = view.document else { return finish() }
        // Launched from a script the app starts in the background, where its
        // window may not be redrawn at all. Bring it forward first.
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(view)
        // Type in the middle of the file, like real editing.
        doc.setCaret(pos: TextPos(line: doc.lineCount() / 2, colUtf16: 0))
        view.didChangeSelection()
        LatencyProbe.shared.reset()
        startRevision = doc.revision()
        // About two frames apart, so every key gets its own frame.
        timer = Timer.scheduledTimer(withTimeInterval: 0.03, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.tick() }
        }
        deadline = Timer.scheduledTimer(withTimeInterval: Double(count) * 0.1 + 10, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated { self?.finish() }
        }
    }

    private func tick() {
        guard sent < count, let window = view.window else {
            timer?.invalidate()
            // Let the last frame land.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { [weak self] in self?.finish() }
            return
        }
        let backspace = sent % 2 == 1
        let event = NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: [],
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
            context: nil, characters: backspace ? "\u{7F}" : "x",
            charactersIgnoringModifiers: backspace ? "\u{7F}" : "x",
            isARepeat: false, keyCode: backspace ? 51 : 7)
        sent += 1
        if let event { window.sendEvent(event) }
    }

    private func finish() {
        timer?.invalidate()
        deadline?.invalidate()
        // Diagnostics, so a run with no samples says why.
        let edits = (view.document?.revision() ?? startRevision) - startRevision
        let focused = view.window?.firstResponder === view
        print(LatencyProbe.shared.report + " sent=\(sent) edits=\(edits) editor_focused=\(focused) draws=\(LatencyProbe.shared.drawCalls) cancels=\(LatencyProbe.shared.cancels) window_key=\(view.window?.isKeyWindow ?? false)")
        fflush(stdout)
        // Skip the unsaved-changes prompt: this was a throwaway buffer.
        exit(0)
    }
}
