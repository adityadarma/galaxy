import AppKit
import QuartzCore
import CoreText

/// Custom text view (M1-07, M1-09). Not an `NSTextView`: it draws only the
/// lines that intersect the dirty rect, straight from Core Text.
///
/// The view is as tall as the whole document (`lineCount * lineHeight`), so
/// an enclosing `NSScrollView` scrolls it natively while drawing cost stays
/// proportional to what is on screen. Each draw asks the Rust core for one
/// `ViewportSnapshot` covering exactly the visible lines, then paints, per
/// line: current-line highlight, selection, text, carets, and the gutter
/// line number.
@MainActor
final class EditorView: NSView {
    static let defaultFont = Theme.editorFont
    struct Metrics: Equatable {
        let ascent: CGFloat
        let descent: CGFloat
        let leading: CGFloat
        /// Whole points so line tops land on pixel boundaries.
        let lineHeight: CGFloat
        let digitWidth: CGFloat
        let spaceWidth: CGFloat

        init(font: NSFont) {
            let ctFont = font as CTFont
            ascent = CTFontGetAscent(ctFont)
            descent = CTFontGetDescent(ctFont)
            leading = CTFontGetLeading(ctFont)
            lineHeight = ceil(ascent + descent + leading)
            digitWidth = ("0" as NSString).size(withAttributes: [.font: font]).width
            spaceWidth = (" " as NSString).size(withAttributes: [.font: font]).width
        }
    }

    /// Padding on each side of the line numbers.
    static let gutterPadding: CGFloat = 10
    /// Space between the gutter and the first column.
    static let textInset: CGFloat = 8
    static let caretWidth: CGFloat = 2
    static let blinkInterval: TimeInterval = 0.53
    static let matchColor = Theme.findMatch

    private(set) var font: NSFont
    private(set) var metrics: Metrics
    private(set) var lineCount = 0
    /// Visual rows: `lineCount` unless soft wrap splits long lines.
    private(set) var rowCount = 0
    /// Soft wrap (Toggle Word Wrap). Applies only to the view that sets it.
    var wrapEnabled = false {
        didSet { if wrapEnabled != oldValue { wrapSettingsChanged() } }
    }
    /// Wrap is off while this is set, e.g. in a side-by-side diff where both
    /// panes must keep one row per line.
    var wrapSuspended = false {
        didSet { if wrapSuspended != oldValue { wrapSettingsChanged() } }
    }
    /// Columns the core wraps at, or `nil` while wrap is not in effect.
    private(set) var appliedWrapColumns: Int?
    private var lastWrapWidth: CGFloat = 0
    var wrapActive: Bool { appliedWrapColumns != nil }
    /// 0-based lines with a breakpoint, drawn on their line numbers.
    var breakpointLines: Set<UInt64> = [] {
        didSet { if breakpointLines != oldValue { needsDisplay = true } }
    }
    /// 0-based line the debugged program is stopped on.
    var debugStopLine: Int? {
        didSet { if debugStopLine != oldValue { needsDisplay = true } }
    }
    /// A click on the line numbers; the window turns it into a breakpoint.
    var onToggleBreakpoint: ((UInt64) -> Void)?

    /// Width at the right edge kept clear for another view (the minimap).
    /// The document view is that much narrower than its scroll view.
    var rightInset: CGFloat = 0 {
        didSet { if rightInset != oldValue { applyRightInset() } }
    }

    /// Sizes the view to the scroll view's width minus `rightInset`. Safe to
    /// call again: it works from the scroll view, not from the current frame.
    func applyRightInset() {
        guard let width = enclosingScrollView?.contentSize.width else { return }
        // Autoresizing keeps this offset from the scroll view's width.
        setFrameSize(NSSize(width: max(width - rightInset, 1), height: frame.height))
    }
    /// The document, its size or its scroll range changed; overview views
    /// redraw from this.
    var onLayoutChange: (() -> Void)?
    /// Number of `CTLine`s built so far. Tests use it to prove caching.
    private(set) var layoutsCreated = 0
    /// Lines drawn by the most recent `draw(_:)`, for tests and diagnostics.
    private(set) var lastDrawnLines: Range<Int> = 0..<0
    /// The committed side of a diff: it shows and navigates, but never edits.
    var isReadOnly = false
    /// Background per 0-based line, for the changed lines of a diff.
    var lineTints: [UInt64: NSColor] = [:] {
        didSet { if lineTints != oldValue { needsDisplay = true } }
    }
    var gitLineChanges: [UInt64: GitLineChangeKind] = [:] {
        didSet { if gitLineChanges != oldValue { needsDisplay = true } }
    }
    /// LSP names (types, parameters, functions) colored over Tree-sitter's runs.
    var semanticTokens: [SemanticTokenInfo] = [] {
        didSet {
            guard semanticTokens != oldValue else { return }
            semanticStylesByLine = Dictionary(grouping: semanticTokens.map {
                StyleRun(line: $0.line, start: $0.start, end: $0.start + $0.length, scope: $0.scope)
            }, by: { $0.line })
            needsDisplay = true
        }
    }
    private(set) var semanticStylesByLine: [UInt64: [StyleRun]] = [:]
    /// `editor.renderWhitespace`; dots everywhere unless settings say otherwise.
    var renderWhitespace: WhitespaceRendering = .all {
        didSet { if renderWhitespace != oldValue { needsDisplay = true } }
    }
    var inlayHints: [InlayHintInfo] = [] {
        didSet { if inlayHints != oldValue { needsDisplay = true } }
    }

    /// Git blame shown after the text of one line (`line` is 0-based).
    struct BlameAnnotation: Equatable {
        var line: UInt64
        var text: String
    }
    var blameAnnotation: BlameAnnotation? {
        didSet { if blameAnnotation != oldValue { needsDisplay = true } }
    }
    /// Space between the end of the line and its blame text.
    static let blameGap: CGFloat = 40

    /// Where the blame text of the annotated line is drawn, in view
    /// coordinates, or `nil` when there is none.
    func blameAnnotationRect() -> NSRect? {
        guard let blame = blameAnnotation, let document else { return nil }
        let text = document.displayLine(line: blame.line)
        let end = point(column: text.utf16.count, text: text, rows: lineRows(blame.line),
                        top: rowTop(of: blame.line), originX: textOriginX)
        let width = ceil((blame.text as NSString).size(withAttributes: [.font: font]).width)
        return NSRect(x: end.x + Self.blameGap, y: end.y, width: width, height: metrics.lineHeight)
    }

    /// Drops derived Core Text/LSP decorations while retaining the document.
    func releaseNonEssentialCaches() {
        layouts.removeAll()
        semanticTokens = []
        inlayHints = []
        caretRects.removeAll(keepingCapacity: false)
        needsDisplay = true
    }

    /// Tests can pin the caret on or off; `nil` means real focus and blink.
    var forcedCaretVisible: Bool?

    private var layouts = TwoGenerationCache<String, CTLine>(capacity: 1024)
    /// Caret rectangles from the last draw, keyed by line, so a blink only
    /// invalidates those few rects instead of the whole view.
    private var caretRects: [Int: [NSRect]] = [:]
    private var blinkTimer: Timer?
    private var caretOn = true

    /// Pasteboard for copy/cut/paste; tests swap in a private one.
    var pasteboard = NSPasteboard.general
    /// Anchor of an Opt+Shift column drag in progress.
    var columnAnchor: TextPos?
    /// Called after changes made through the view (edit, undo, paste), at
    /// most once per display frame (M1-15). Listeners do whole-document
    /// work (modified flag, find counts), so a burst of keystrokes or key
    /// repeat must not run it per event.
    var onChange: (() -> Void)?
    /// Primary cursor and cursor count after a frame that moved them. Comes
    /// from the snapshot that frame already fetched, so it costs no FFI.
    var onCursorChange: ((CursorInfo, UInt64) -> Void)?
    /// Primary caret before and after a plain click, so the window can
    /// record a far click in the back/forward history (M2-08).
    var onClickMove: ((CursorInfo, CursorInfo) -> Void)?
    var onHover: ((TextPos, NSPoint) -> Void)?
    var onCompletionKey: ((NSEvent) -> Bool)?
    var onTextInput: ((String) -> Void)?
    var onSnippetInvalidated: (() -> Void)?
    var onDefinitionClick: ((TextPos) -> Bool)?
    /// Word under the mouse while Cmd is held: drawn as a link so the user
    /// sees what Cmd+click will open. UTF-16 columns on one line.
    var definitionLink: (line: UInt64, start: UInt64, end: UInt64)? {
        didSet {
            guard definitionLink?.line != oldValue?.line
                || definitionLink?.start != oldValue?.start
                || definitionLink?.end != oldValue?.end else { return }
            for link in [oldValue, definitionLink].compactMap({ $0 }) {
                setNeedsDisplay(NSRect(x: 0, y: rowTop(of: link.line), width: bounds.width,
                                       height: metrics.lineHeight * CGFloat(lineRows(link.line).count)))
            }
            // Cursor rects would reset a bare `set()` on the next move.
            window?.invalidateCursorRects(for: self)
            (definitionLink == nil ? NSCursor.iBeam : NSCursor.pointingHand).set()
        }
    }
    /// Last mouse location in view coordinates, for Cmd presses without movement.
    var lastMousePoint: NSPoint?
    /// A change is waiting for the next frame.
    private(set) var hasPendingChange = false
    /// How many times pending work was flushed (tests check coalescing).
    private(set) var frameFlushes = 0
    private var frameLink: CADisplayLink?
    /// Metadata is refreshed by the frame that renders it, rather than once
    /// for every edit in a burst. The viewport snapshot already carries the
    /// line count and primary caret (M1-15).
    private var metadataNeedsRefresh = false
    private var pendingCaretScroll = false
    private var lastImmediateInputDraw: TimeInterval = 0
    private var isHandlingKeyEvent = false
    private var keyEventRequestedRefresh = false
    /// Keeps the display link alive only while a background syntax tree is
    /// catching up; the completed tree then triggers one redraw.
    private var syntaxAwaiting = false

    var document: DocumentHandle? {
        didSet {
            // The core keeps one wrap width per document: release the old one
            // and give the new one this view's width.
            if appliedWrapColumns != nil { oldValue?.setWrapColumns(columns: nil) }
            appliedWrapColumns = nil
            applyWrapColumns()
            syntaxAwaiting = document?.syntaxLanguage() != nil
            startSyntaxPollingIfNeeded()
            reloadDocument()
        }
    }

    override init(frame: NSRect) {
        font = Self.defaultFont
        metrics = Metrics(font: Self.defaultFont)
        super.init(frame: frame)
        wantsLayer = true
        // Redraw only when told to; the scroll view handles scrolling blits.
        layerContentsRedrawPolicy = .onSetNeedsDisplay
        setAccessibilityElement(true)
        setAccessibilityRole(.textArea)
        setAccessibilityIdentifier("editor")
        setAccessibilityLabel("Editor")
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { true }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        let area = NSTrackingArea(rect: bounds, options: [.activeInKeyWindow, .mouseMoved, .mouseEnteredAndExited, .inVisibleRect], owner: self)
        addTrackingArea(area)
    }

    override func mouseMoved(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        lastMousePoint = point
        updateDefinitionLink(commandHeld: event.modifierFlags.contains(.command))
        onHover?(textPosition(at: point), point)
    }

    /// The pointer left the editor (the blame card uses it to close).
    var onMouseExit: (() -> Void)?

    override func mouseExited(with event: NSEvent) {
        lastMousePoint = nil
        definitionLink = nil
        onMouseExit?()
    }

    override func flagsChanged(with event: NSEvent) {
        super.flagsChanged(with: event)
        updateDefinitionLink(commandHeld: event.modifierFlags.contains(.command))
    }

    override func scrollWheel(with event: NSEvent) {
        super.scrollWheel(with: event)
        // The text moved under a still mouse; re-hit-test the link.
        guard let window else { return }
        lastMousePoint = convert(window.mouseLocationOutsideOfEventStream, from: nil)
        updateDefinitionLink(commandHeld: event.modifierFlags.contains(.command))
    }

    override var acceptsFirstResponder: Bool { true }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        // Syntax CTLines contain resolved theme colors, unlike the cached
        // monochrome layouts, so recreate them under the new appearance.
        needsDisplay = true
    }

    // MARK: Geometry

    /// Width of the line-number column; grows with the number of digits.
    var gutterWidth: CGFloat {
        let digits = max(3, String(max(lineCount, 1)).count)
        return ceil(CGFloat(digits) * metrics.digitWidth) + 2 * Self.gutterPadding
    }

    /// X of column 0 of the text.
    var textOriginX: CGFloat { gutterWidth + Self.textInset }

    /// Document lines that intersect `rect`, clamped to the document.
    func lineRange(for rect: NSRect) -> Range<Int> {
        // AppKit may pass a huge or infinite dirty rect when a layer-backed
        // view is drawn. Clip to our bounds first so the Double -> Int
        // conversions below can never overflow and trap.
        let visible = rect.intersection(bounds)
        guard !visible.isNull, !visible.isEmpty, lineCount > 0 else { return 0..<0 }
        let height = metrics.lineHeight
        if wrapActive, let document {
            let firstRow = UInt64(max(0, floor(visible.minY / height)))
            let lastRow = UInt64(max(0, ceil(visible.maxY / height) - 1))
            let first = min(lineCount, Int(document.lineAtRow(row: firstRow).line))
            let last = min(lineCount, Int(document.lineAtRow(row: lastRow).line) + 1)
            return first..<max(first, last)
        }
        let first = min(lineCount, max(0, Int(floor(visible.minY / height))))
        let last = min(lineCount, max(0, Int(ceil(visible.maxY / height))))
        return first..<max(first, last)
    }

    func setFont(_ newFont: NSFont) {
        font = newFont
        metrics = Metrics(font: newFont)
        layouts.removeAll()
        reloadDocument()
    }

    /// Reads the small metadata snapshot and resizes the virtual document.
    /// The same snapshot also contains the primary caret, so callers that
    /// need scroll-into-view must reuse the returned value rather than make
    /// another FFI request (M1-15).
    @discardableResult
    func reloadDocument() -> StatusSnapshot? {
        let snapshot = document?.statusSnapshot()
        applyDocumentMetadata(lineCount: snapshot?.lineCount)
        metadataNeedsRefresh = false
        pendingCaretScroll = false
        // A moving caret should be solid, like every text field.
        caretOn = true
        updateBlinking()
        needsDisplay = true
        onLayoutChange?()
        // Clearing the flag above stops `draw` from reporting the caret, so
        // report it here; mouse clicks and drags only come through this path.
        if let snapshot {
            let primary = snapshot.primary, count = snapshot.selectionCount
            DispatchQueue.main.async { [weak self] in self?.onCursorChange?(primary, count) }
        }
        return snapshot
    }

    private func applyDocumentMetadata(lineCount newCount: UInt64?) {
        lineCount = Int(newCount ?? 0)
        rowCount = wrapActive ? Int(document?.totalRows() ?? 0) : lineCount
        let virtual = CGFloat(rowCount) * metrics.lineHeight
        let visible = enclosingScrollView?.contentSize.height ?? 0
        setFrameSize(NSSize(width: frame.width, height: max(virtual, visible)))
    }

    // MARK: Soft wrap

    private func wrapSettingsChanged() {
        applyWrapColumns()
        reloadDocument()
    }

    /// Columns that fit between the gutter and the right edge.
    private var fittingColumns: Int {
        guard metrics.spaceWidth > 0 else { return 80 }
        let room = bounds.width - textOriginX - Self.caretWidth - 6
        return max(2, Int(floor(room / metrics.spaceWidth)))
    }

    /// Pushes the wrap width to the core. Returns `true` if it changed.
    @discardableResult
    private func applyWrapColumns() -> Bool {
        let columns: Int? = (wrapEnabled && !wrapSuspended) ? fittingColumns : nil
        guard columns != appliedWrapColumns else { return false }
        appliedWrapColumns = columns
        document?.setWrapColumns(columns: columns.map { UInt32($0) })
        return true
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        onLayoutChange?()
        guard newSize.width != lastWrapWidth else { return }
        lastWrapWidth = newSize.width
        // A new width re-wraps every line: resize to the new row count.
        if wrapEnabled, !wrapSuspended, applyWrapColumns() || wrapActive {
            let rows = Int(document?.totalRows() ?? 0)
            guard rows != rowCount else { needsDisplay = true; return }
            rowCount = rows
            let visible = enclosingScrollView?.contentSize.height ?? 0
            super.setFrameSize(NSSize(width: newSize.width, height: max(CGFloat(rows) * metrics.lineHeight, visible)))
            needsDisplay = true
            onLayoutChange?()
        }
    }

    /// Where the rows of one logical line start, in UTF-16 columns.
    struct LineRows {
        /// Columns at which rows after the first begin; empty for one row.
        let breaks: [Int]
        var count: Int { breaks.count + 1 }
        func start(of row: Int) -> Int { row == 0 ? 0 : breaks[row - 1] }
        func end(of row: Int) -> Int { row < breaks.count ? breaks[row] : Int.max }
        /// The row a caret at `column` is drawn on; a break column belongs
        /// to the row it starts.
        func row(containing column: Int) -> Int { breaks.firstIndex { $0 > column } ?? breaks.count }
    }

    func lineRows(_ line: UInt64) -> LineRows {
        guard wrapActive, let document else { return LineRows(breaks: []) }
        return LineRows(breaks: document.lineBreaks(line: line).map(Int.init))
    }

    /// Y of the first row of `line`.
    func rowTop(of line: UInt64) -> CGFloat {
        guard wrapActive, let document else { return CGFloat(line) * metrics.lineHeight }
        return CGFloat(document.rowOfLine(line: line)) * metrics.lineHeight
    }

    /// The line (and row within it) drawn at document-space `y`.
    func line(atY y: CGFloat) -> (line: UInt64, row: Int) {
        let row = y.isFinite ? max(0, floor(y / metrics.lineHeight)) : 0
        guard wrapActive, let document else { return (UInt64(min(row, CGFloat(max(lineCount - 1, 0)))), 0) }
        let position = document.lineAtRow(row: UInt64(row))
        return (position.line, Int(position.row))
    }

    /// Source line at `y` with the fraction of the way through it, for
    /// scroll sync with the Markdown preview.
    func fractionalLine(atY y: CGFloat) -> Double {
        guard wrapActive, let document else { return Double(y / metrics.lineHeight) }
        let rowFloat = max(0, y / metrics.lineHeight)
        if UInt64(rowFloat) >= document.totalRows() { return Double(lineCount) }
        let position = document.lineAtRow(row: UInt64(rowFloat))
        let rows = Double(max(1, document.rowsOfLine(line: position.line)))
        return Double(position.line) + (Double(position.row) + Double(rowFloat - floor(rowFloat))) / rows
    }

    func y(forFractionalLine line: Double) -> CGFloat {
        guard wrapActive, let document else { return CGFloat(line) * metrics.lineHeight }
        let whole = UInt64(max(0, floor(line)))
        let rows = Double(document.rowsOfLine(line: whole))
        let row = CGFloat(document.rowOfLine(line: whole)) + CGFloat((line - floor(line)) * rows)
        return row * metrics.lineHeight
    }

    /// Top-left of the cell at `column` of a line: x includes `originX`,
    /// y is the top of the row holding it.
    func point(column: Int, text: String, rows: LineRows, top: CGFloat, originX: CGFloat) -> CGPoint {
        let row = rows.row(containing: column)
        let shift = row == 0 ? 0 : xOffset(in: text, column: UInt64(rows.start(of: row)))
        return CGPoint(x: originX + xOffset(in: text, column: UInt64(column)) - shift,
                       y: top + CGFloat(row) * metrics.lineHeight)
    }

    /// One rect per row touched by `[start, end)`; a span that ends past a
    /// row's end stops at it. `trailing` widens the last row (line break).
    func spanRects(_ start: Int, _ end: Int, text: String, rows: LineRows, top: CGFloat, originX: CGFloat,
                   minWidth: CGFloat = 0, trailing: CGFloat = 0) -> [CGRect] {
        var out: [CGRect] = []
        for row in 0..<rows.count {
            let rowStart = rows.start(of: row)
            let a = max(start, rowStart), b = min(end, rows.end(of: row))
            // A single-row line keeps empty spans (empty-line selection).
            guard a < b || (rows.count == 1 && a <= b) else { continue }
            let shift = rowStart == 0 ? 0 : xOffset(in: text, column: UInt64(rowStart))
            let x0 = originX + xOffset(in: text, column: UInt64(a)) - shift
            var x1 = originX + xOffset(in: text, column: UInt64(b)) - shift
            if row == rows.count - 1 { x1 += trailing }
            out.append(CGRect(x: x0, y: top + CGFloat(row) * metrics.lineHeight,
                              width: max(minWidth, x1 - x0), height: metrics.lineHeight))
        }
        return out
    }

    /// Caret-sized rect at `column` of `line`, in view coordinates.
    func rect(forLine line: UInt64, column: UInt64, width: CGFloat = 1) -> NSRect {
        let text = document?.displayLine(line: line) ?? ""
        let p = point(column: Int(column), text: text, rows: lineRows(line), top: rowTop(of: line), originX: textOriginX)
        return NSRect(x: p.x, y: p.y, width: width, height: metrics.lineHeight)
    }

    /// Invalidates visual document state. In a window this is deliberately
    /// cheap: the next render fetches one viewport snapshot and uses it for
    /// both geometry and painting. Headless unit tests still get synchronous
    /// geometry so their observations remain deterministic.
    func requestDocumentRefresh(scrollCaret: Bool) {
        metadataNeedsRefresh = true
        pendingCaretScroll = pendingCaretScroll || scrollCaret
        caretOn = true
        updateBlinking()
        needsDisplay = true
        syntaxAwaiting = document?.syntaxLanguage() != nil
        startSyntaxPollingIfNeeded()
        if window == nil {
            let snapshot = reloadDocument()
            if scrollCaret, let primary = snapshot?.primary {
                scrollCaretToVisible(primary: primary)
            }
        }
    }

    func beginKeyEvent() {
        isHandlingKeyEvent = true
        keyEventRequestedRefresh = false
    }

    func endKeyEvent() -> Bool {
        isHandlingKeyEvent = false
        return keyEventRequestedRefresh
    }

    func noteDocumentRefreshFromInput() {
        if isHandlingKeyEvent { keyEventRequestedRefresh = true }
    }

    /// Keeps the low-latency path for normal typing while coalescing bursts
    /// faster than the display refresh into the pending display-link frame.
    func drawInputImmediatelyIfDue() {
        let now = ProcessInfo.processInfo.systemUptime
        guard now - lastImmediateInputDraw >= 1.0 / 60.0 else { return }
        lastImmediateInputDraw = now
        displayIfNeeded()
    }

    // MARK: Focus and blinking

    private var isActive: Bool {
        window?.isKeyWindow == true && window?.firstResponder === self
    }

    private var showsCaret: Bool {
        forcedCaretVisible ?? (isActive && caretOn)
    }

    override func becomeFirstResponder() -> Bool {
        updateBlinking()
        needsDisplay = true
        return true
    }

    override func resignFirstResponder() -> Bool {
        stopBlinking()
        needsDisplay = true
        return true
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        NotificationCenter.default.removeObserver(self)
        frameLink?.invalidate()
        frameLink = nil
        guard let window else {
            stopBlinking()
            flushPendingUpdates()
            return
        }
        for name in [NSWindow.didBecomeKeyNotification, NSWindow.didResignKeyNotification] {
            NotificationCenter.default.addObserver(
                self, selector: #selector(windowFocusChanged(_:)), name: name, object: window)
        }
        updateBlinking()
    }

    // MARK: Per-frame coalescing (M1-15)

    /// Runs `onChange` on the next display frame, once, however many
    /// changes arrive before it. Without a window it runs immediately.
    func scheduleChangeNotification() {
        hasPendingChange = true
        guard window != nil else { return flushPendingUpdates() }
        if frameLink == nil {
            let link = displayLink(target: self, selector: #selector(frameTick(_:)))
            link.add(to: .main, forMode: .common)
            frameLink = link
        }
        frameLink?.isPaused = false
    }

    private func startSyntaxPollingIfNeeded() {
        guard syntaxAwaiting, window != nil else { return }
        if frameLink == nil {
            let link = displayLink(target: self, selector: #selector(frameTick(_:)))
            link.add(to: .main, forMode: .common)
            frameLink = link
        }
        frameLink?.isPaused = false
    }

    @objc private func frameTick(_ link: CADisplayLink) {
        flushPendingUpdates()
        if syntaxAwaiting, document?.pollSyntax() == true {
            syntaxAwaiting = false
            needsDisplay = true
        }
        if !hasPendingChange && !syntaxAwaiting { link.isPaused = true }
    }

    /// Runs pending work now. Tests call this instead of waiting a frame.
    func flushPendingUpdates() {
        if !syntaxAwaiting { frameLink?.isPaused = true }
        if hasPendingChange {
            hasPendingChange = false
            frameFlushes += 1
            onChange?()
            // Once per frame, not per key: a client that reacts by reading
            // the whole value should not do so for every keystroke.
            NSAccessibility.post(element: self, notification: .valueChanged)
            NSAccessibility.post(element: self, notification: .selectedTextChanged)
        }
        // A burst that skipped direct draws gets one visual update here.
        // This path is also used by tests and hidden windows, where waiting
        // for AppKit to schedule a display pass is not reliable.
        if metadataNeedsRefresh { displayIfNeeded() }
    }

    @objc private func windowFocusChanged(_ notification: Notification) {
        // A hidden or occluded window may not get frame callbacks.
        flushPendingUpdates()
        updateBlinking()
        needsDisplay = true
    }

    private func updateBlinking() {
        if isActive { startBlinking() } else { stopBlinking() }
    }

    private func startBlinking() {
        blinkTimer?.invalidate()
        caretOn = true
        blinkTimer = Timer.scheduledTimer(withTimeInterval: Self.blinkInterval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.toggleCaret() }
        }
    }

    private func stopBlinking() {
        blinkTimer?.invalidate()
        blinkTimer = nil
        caretOn = true
    }

    private func toggleCaret() {
        caretOn.toggle()
        for rect in caretRects.values.joined() { setNeedsDisplay(rect) }
    }

    // MARK: Drawing

    override func draw(_ dirtyRect: NSRect) {
        let drawStart = ProcessInfo.processInfo.systemUptime
        defer { LatencyProbe.shared.drawFinished(drawStart: drawStart) }
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        dirtyRect.fill()

        guard let document, let context = NSGraphicsContext.current?.cgContext else { return }
        let range = lineRange(for: dirtyRect)
        lastDrawnLines = range
        guard !range.isEmpty else { return }

        let snapshot = document.viewport(
            firstLine: UInt64(range.lowerBound),
            count: UInt64(range.count)
        )
        if metadataNeedsRefresh {
            applyDocumentMetadata(lineCount: snapshot.lineCount)
            metadataNeedsRefresh = false
            DispatchQueue.main.async { [weak self] in self?.onLayoutChange?() }
            // After this draw: changing a label mid-draw can trigger layout.
            let primary = snapshot.primary, count = snapshot.selectionCount
            DispatchQueue.main.async { [weak self] in self?.onCursorChange?(primary, count) }
            if pendingCaretScroll {
                pendingCaretScroll = false
                let caretText = snapshot.lines.first(where: { Int($0.line) == Int(snapshot.primary.line) })?.text
                DispatchQueue.main.async { [weak self] in
                    self?.scrollCaretToVisible(primary: snapshot.primary, lineText: caretText)
                }
            }
        }
        for line in range { caretRects[line] = nil }

        let originX = textOriginX
        let gutterRight = gutterWidth - Self.gutterPadding
        let lineHeight = metrics.lineHeight
        let drawCarets = showsCaret
        let selectionColor = (isActive || forcedCaretVisible != nil)
            ? NSColor.selectedTextBackgroundColor
            : NSColor.unemphasizedSelectedTextBackgroundColor

        context.saveGState()
        defer { context.restoreGState() }
        // The view is flipped (y grows downward); flip glyphs back upright.
        context.textMatrix = CGAffineTransform(scaleX: 1, y: -1)

        let stylesByLine = Dictionary(grouping: snapshot.styles, by: { $0.line })
        for line in snapshot.lines {
            let rows = lineRows(line.line)
            let top = rowTop(of: line.line)
            let block = lineHeight * CGFloat(rows.count)
            let hasCaret = !line.carets.isEmpty
            let span = { (start: UInt64, end: UInt64, minWidth: CGFloat, trailing: CGFloat) in
                self.spanRects(Int(start), Int(end), text: line.text, rows: rows, top: top,
                               originX: originX, minWidth: minWidth, trailing: trailing)
            }
            if let tint = lineTints[line.line] {
                context.setFillColor(tint.cgColor)
                context.fill(CGRect(x: 0, y: top, width: bounds.width, height: block))
            }
            for hint in inlayHints where hint.line == line.line {
                let p = point(column: Int(hint.character), text: line.text, rows: rows, top: top, originX: originX)
                NSColor.secondaryLabelColor.withAlphaComponent(0.7).setFill()
                NSAttributedString(string: "  \(hint.label)", attributes: [.font: NSFont.monospacedSystemFont(ofSize: font.pointSize - 1, weight: .regular), .foregroundColor: NSColor.secondaryLabelColor]).draw(at: NSPoint(x: p.x, y: p.y + 1))
            }

            // Current line: lines with a caret and no selection on them.
            if hasCaret, line.selections.isEmpty {
                context.setFillColor(Theme.currentLine.cgColor)
                context.fill(CGRect(x: 0, y: top, width: bounds.width, height: block))
            }

            if debugStopLine == Int(line.line) {
                context.setFillColor(NSColor.systemYellow.withAlphaComponent(0.22).cgColor)
                context.fill(CGRect(x: 0, y: top, width: bounds.width, height: block))
            }

            // Subtle divider between line numbers and source text.
            context.setFillColor(NSColor.separatorColor.withAlphaComponent(0.28).cgColor)
            context.fill(CGRect(x: gutterWidth - 1, y: top, width: 1, height: block))

            // Indent guides sit behind text and whitespace markers. Draw one
            // guide for each complete four-space indentation level.
            let indentColumns = leadingIndentColumns(in: line.text)
            if indentColumns >= 4 {
                context.setFillColor(NSColor.separatorColor.withAlphaComponent(0.16).cgColor)
                var column = 4
                // The deepest guide sits immediately beside the text and is
                // visually noisy; keep only the parent-level guides.
                while column < indentColumns {
                    let x = originX + CGFloat(column) * metrics.spaceWidth - 0.5
                    context.fill(CGRect(x: x, y: top, width: 1, height: block))
                    column += 4
                }
            }

            // Matching delimiter cells, below selections and above text.
            context.setFillColor(Theme.bracketMatch.cgColor)
            for bracket in line.brackets {
                for rect in span(bracket.start, bracket.end, metrics.spaceWidth, 0) { context.fill(rect) }
            }

            // Find matches, under the selection so the current one stays clear.
            if !line.matches.isEmpty {
                context.setFillColor(Self.matchColor.cgColor)
                for match in line.matches {
                    for rect in span(match.start, match.end, 0, 0) { context.fill(rect) }
                }
            }

            // Whitespace markers sit under selection fills, so selecting a
            // line keeps the selection color uniform.
            if renderWhitespace != .none, !line.text.isEmpty {
                drawWhitespace(in: line.text, rows: rows, originX: originX, top: top, context: context)
            }

            // Selections. A span that continues past the line end is
            // extended by one space so empty lines show as selected too.
            context.setFillColor(selectionColor.cgColor)
            for selection in line.selections {
                let eol = selection.includesEol ? metrics.spaceWidth : 0
                for rect in span(selection.start, selection.end, 0, eol) { context.fill(rect) }
            }

            // Text uses a one-frame attributed layout only if Tree-sitter
            // supplied semantic runs; otherwise the shared plain cache wins.
            if !line.text.isEmpty {
                context.setFillColor(Theme.editorForeground.resolved(for: effectiveAppearance).cgColor)
                // Semantic runs come last so they win over Tree-sitter's.
                let styles = (stylesByLine[line.line] ?? []) + (semanticStylesByLine[line.line] ?? [])
                let ctLine = styles.isEmpty ? layout(for: line.text) : syntaxLayout(for: line.text, styles: styles)
                for row in 0..<rows.count {
                    let rowTopY = top + CGFloat(row) * lineHeight
                    let shift = row == 0 ? 0 : xOffset(in: line.text, column: UInt64(rows.start(of: row)))
                    if rows.count > 1 {
                        // The whole line is laid out once; clip to this row.
                        context.saveGState()
                        context.clip(to: CGRect(x: originX, y: rowTopY, width: bounds.width, height: lineHeight))
                    }
                    context.textPosition = CGPoint(x: originX - shift, y: rowTopY + metrics.ascent)
                    CTLineDraw(ctLine, context)
                    if rows.count > 1 { context.restoreGState() }
                }
            }

            // Git blame of the caret line, quiet and after the text.
            if let blame = blameAnnotation, blame.line == line.line {
                let fromContext = NSAttributedString.Key(kCTForegroundColorFromContextAttributeName as String)
                let note = CTLineCreateWithAttributedString(NSAttributedString(
                    string: blame.text, attributes: [.font: font, fromContext: true]))
                context.setFillColor(NSColor.secondaryLabelColor.withAlphaComponent(0.75).cgColor)
                let end = point(column: line.text.utf16.count, text: line.text, rows: rows, top: top, originX: originX)
                context.textPosition = CGPoint(x: end.x + Self.blameGap, y: end.y + metrics.ascent)
                CTLineDraw(note, context)
            }

            // LSP diagnostics sit above text decorations but beneath carets.
            // Cmd hover: underline the identifier Cmd+click would open.
            if let link = definitionLink, link.line == line.line {
                context.setFillColor(NSColor.linkColor.cgColor)
                for rect in span(link.start, link.end, 1, 0) {
                    context.fill(CGRect(x: rect.minX, y: rect.minY + metrics.ascent + 2, width: rect.width, height: 1))
                }
            }
            for diagnostic in line.diagnostics {
                context.setFillColor(Theme.diagnostic(diagnostic.severity).cgColor)
                for rect in span(diagnostic.start, diagnostic.end, 1, 0) {
                    context.fill(CGRect(x: rect.minX, y: rect.maxY - 2, width: rect.width, height: 1))
                }
            }

            // IME marked text: thin underline under the preedit.
            if let marked = line.marked {
                context.setFillColor(NSColor.textColor.cgColor)
                for rect in span(marked.start, marked.end, 0, 0) {
                    context.fill(CGRect(x: rect.minX, y: rect.maxY - 2, width: rect.width, height: 1))
                }
            }

            // Carets. Rects are recorded even while the caret is off so the
            // blink timer knows what to invalidate.
            for column in line.carets {
                let p = point(column: Int(column), text: line.text, rows: rows, top: top, originX: originX)
                let rect = CGRect(x: p.x, y: p.y, width: Self.caretWidth, height: lineHeight)
                caretRects[Int(line.line), default: []].append(rect)
                if drawCarets {
                    context.setFillColor(Theme.caret.cgColor)
                    context.fill(rect)
                }
            }

            // Gutter: right-aligned line number, copper on caret lines.
            let number = layout(for: String(line.line + 1))
            let width = CGFloat(CTLineGetTypographicBounds(number, nil, nil, nil))
            let hasBreakpoint = breakpointLines.contains(line.line)
            if hasBreakpoint {
                let tag = CGRect(x: gutterRight - width - 4, y: top + 1, width: width + 7, height: lineHeight - 2)
                context.addPath(CGPath(roundedRect: tag, cornerWidth: 4, cornerHeight: 4, transform: nil))
                context.setFillColor(NSColor.systemRed.cgColor)
                context.fillPath()
            }
            context.setFillColor(hasBreakpoint ? NSColor.white.cgColor : (hasCaret ? Theme.accent : Theme.gutterText).cgColor)
            context.textPosition = CGPoint(x: gutterRight - width, y: top + metrics.ascent)
            CTLineDraw(number, context)
            if let severity = line.diagnostics.map(\.severity).min() {
                context.setFillColor(Theme.diagnostic(severity).cgColor)
                context.fill(CGRect(x: gutterRight + 3, y: top + lineHeight / 2 - 2, width: 4, height: 4))
            }
            if let change = gitLineChanges[line.line + 1] {
                let color: NSColor = switch change {
                case .added: NSColor.systemGreen
                case .modified: NSColor.systemYellow
                case .deleted: NSColor.systemRed
                }
                context.setFillColor(color.cgColor)
                let markerHeight: CGFloat = change == .deleted ? 3 : block
                context.fill(CGRect(x: 1, y: top, width: 3, height: markerHeight))
            }
        }
    }

    /// UTF-16 columns of the whitespace that `renderWhitespace` marks in
    /// `text`. `.boundary` skips single spaces between words, as in VSCode.
    static func whitespaceColumns(in text: String, mode: WhitespaceRendering) -> [Int] {
        guard mode != .none else { return [] }
        let units = Array(text.utf16)
        let space = UInt16(UInt8(ascii: " ")), tab = UInt16(UInt8(ascii: "\t"))
        let isBlank = { (i: Int) in i >= 0 && i < units.count && (units[i] == space || units[i] == tab) }
        var columns: [Int] = []
        for i in units.indices where isBlank(i) {
            if mode == .boundary, units[i] == space, !isBlank(i - 1), !isBlank(i + 1), i > 0, i < units.count - 1 {
                continue
            }
            columns.append(i)
        }
        return columns
    }

    private func leadingIndentColumns(in text: String) -> Int {
        var columns = 0
        for character in text {
            switch character {
            case " ": columns += 1
            case "\t": columns += 4
            default: return columns
            }
        }
        return columns
    }

    private func drawWhitespace(in text: String, rows: LineRows, originX: CGFloat, top: CGFloat, context: CGContext) {
        let columns = Self.whitespaceColumns(in: text, mode: renderWhitespace)
        guard !columns.isEmpty else { return }
        let units = Array(text.utf16)
        let tab = UInt16(UInt8(ascii: "\t"))
        // Centered on the x-height, where VSCode puts its middle dots.
        let midOffset = metrics.ascent - max(font.xHeight, 1) / 2
        let radius = max(1, (font.pointSize / 13).rounded(.down)) * 0.9
        context.setFillColor(Theme.whitespace.cgColor)
        for column in columns {
            let origin = point(column: column, text: text, rows: rows, top: top, originX: originX)
            let x0 = origin.x
            let x1 = x0 + xOffset(in: text, column: UInt64(column + 1)) - xOffset(in: text, column: UInt64(column))
            let midY = origin.y + midOffset
            if units[column] == tab {
                context.fill(CGRect(x: x0 + 2, y: midY - 0.5, width: max(1, x1 - x0 - 4), height: 1))
            } else {
                let cx = (x0 + x1) / 2
                context.fillEllipse(in: CGRect(x: cx - radius, y: midY - radius, width: radius * 2, height: radius * 2))
            }
        }
    }

    /// X offset of a UTF-16 column within a line's text.
    func xOffset(in text: String, column: UInt64) -> CGFloat {
        guard column > 0, !text.isEmpty else { return 0 }
        return CGFloat(CTLineGetOffsetForStringIndex(layout(for: text), CFIndex(column), nil))
    }

    /// Cached `CTLine` for `text`. Identical strings (blank lines, repeated
    /// braces, line numbers) share one layout.
    func layout(for text: String) -> CTLine {
        if let cached = layouts.value(for: text) { return cached }
        // Without this attribute Core Text draws black regardless of the
        // context fill colour, which is unreadable in dark mode. With it,
        // one cached layout works for every colour and appearance.
        let fromContext = NSAttributedString.Key(kCTForegroundColorFromContextAttributeName as String)
        let attributed = NSAttributedString(string: text, attributes: [.font: font, fromContext: true])
        let line = CTLineCreateWithAttributedString(attributed)
        layouts.insert(line, for: text)
        layoutsCreated += 1
        return line
    }

    /// A colored layout is intentionally not cached: style runs, selection
    /// and macOS appearance change independently while plain layouts are
    /// shared for drawing, hit-testing and scrolling.
    func syntaxLayout(for text: String, styles: [StyleRun]) -> CTLine {
        // No `kCTForegroundColorFromContextAttributeName` here: it makes
        // CoreText draw every run in the context's fill color, so all syntax
        // colors came out as `textColor`. Unstyled text gets it explicitly.
        let base = Theme.editorForeground.resolved(for: effectiveAppearance)
        let attributed = NSMutableAttributedString(string: text, attributes: [.font: font, .foregroundColor: base])
        let length = (text as NSString).length
        for style in styles {
            let start = min(Int(style.start), length)
            let end = min(Int(style.end), length)
            guard start < end else { continue }
            attributed.addAttribute(.foregroundColor,
                                    value: Theme.syntax(style.scope, appearance: effectiveAppearance),
                                    range: NSRange(location: start, length: end - start))
        }
        return CTLineCreateWithAttributedString(attributed)
    }
}
