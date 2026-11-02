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
            // Font metrics carry float noise (16.00006 for Fira Code 13), which
            // a bare `ceil` turned into a whole extra point of space per line.
            lineHeight = ceil(ascent + descent + leading - 0.01)
            digitWidth = ("0" as NSString).size(withAttributes: [.font: font]).width
            spaceWidth = (" " as NSString).size(withAttributes: [.font: font]).width
        }
    }

    /// Padding on each side of the line numbers.
    static let gutterPadding: CGFloat = 10
    /// Space between the line numbers and the gutter divider: the diagnostic
    /// dot first, then the fold chevron.
    static let gutterTrailingPadding: CGFloat = 26
    /// Space between the gutter and the first column.
    static let textInset: CGFloat = 0
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
        didSet {
            guard wrapSuspended != oldValue else { return }
            // One row per line is also what folds would break.
            if wrapSuspended { document?.unfoldAll() }
            wrapSettingsChanged()
        }
    }
    /// Columns the core wraps at, or `nil` while wrap is not in effect.
    private(set) var appliedWrapColumns: Int?
    private var lastWrapWidth: CGFloat = 0
    var wrapActive: Bool { appliedWrapColumns != nil }
    /// Some lines are folded away, so rows no longer match lines one to one.
    private(set) var hasFolds = false
    /// Rows come from the core's row map instead of `line * lineHeight`.
    var rowMapped: Bool { wrapActive || hasFolds }
    /// The mouse is over the line numbers; fold chevrons show then.
    private var pointerInGutter = false
    /// Where the "..." after each folded line was drawn, by line.
    var foldPillRects: [Int: NSRect] = [:]
    private var foldToolTipRect: NSRect = .zero
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

    /// Sizes the view to the scroll view's width minus `rightInset`, or wider
    /// when a long line needs it. Safe to call again: it works from the scroll
    /// view and the measured text, not from the current frame.
    func applyRightInset() { updateDocumentWidth() }

    // MARK: Horizontal scrolling

    /// Widest line drawn since the document was set, in points. The core
    /// never measures a whole document, so the scroll width grows as long
    /// lines come into view.
    private(set) var contentTextWidth: CGFloat = 0
    /// Room kept after the longest line so its end is not at the very edge.
    private static let trailingRoom: CGFloat = 24
    private var lastClipWidth: CGFloat = 0
    private var lastScrollX: CGFloat = 0
    private var clipObserver: NSObjectProtocol?

    /// How far the text is scrolled to the right. The line numbers stay put
    /// at the left edge, so they are drawn this far into the document.
    var gutterShift: CGFloat { max(0, enclosingScrollView?.contentView.bounds.origin.x ?? 0) }

    /// Wide enough for the longest line seen, never narrower than the visible
    /// area. Soft wrap fits the text to the width, so nothing scrolls then.
    func updateDocumentWidth() {
        guard let scroll = enclosingScrollView else { return }
        var width = max(scroll.contentSize.width - rightInset, 1)
        if !wrapActive {
            // The minimap covers the last `rightInset` points of the clip.
            width = max(width, textOriginX + contentTextWidth + Self.trailingRoom + rightInset)
        }
        guard abs(width - frame.width) > 0.5 else { return }
        setFrameSize(NSSize(width: width, height: frame.height))
    }

    override func viewDidMoveToSuperview() {
        super.viewDidMoveToSuperview()
        if let clipObserver { NotificationCenter.default.removeObserver(clipObserver) }
        clipObserver = nil
        guard let clip = superview as? NSClipView else { return }
        clip.postsBoundsChangedNotifications = true
        lastClipWidth = clip.bounds.width
        lastScrollX = max(0, clip.bounds.origin.x)
        clipObserver = NotificationCenter.default.addObserver(
            forName: NSView.boundsDidChangeNotification, object: clip, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.clipBoundsChanged() }
        }
    }

    private func clipBoundsChanged() {
        guard let clip = superview as? NSClipView else { return }
        if clip.bounds.width != lastClipWidth {
            lastClipWidth = clip.bounds.width
            updateDocumentWidth()
        }
        let x = max(0, clip.bounds.origin.x)
        guard x != lastScrollX else { return }
        lastScrollX = x
        // AppKit blits the old pixels on scroll, which would carry the line
        // numbers along; draw the whole view so they stay at the edge.
        setNeedsDisplay(visibleRect)
        updateFoldToolTip()
        window?.invalidateCursorRects(for: self)
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
    /// A changed hunk expanded below one working-tree line.
    struct InlineGitHunk: Equatable {
        let anchorLine: UInt64
        let oldLines: [String]
        let newLines: [String]
    }
    var inlineGitHunk: InlineGitHunk? {
        didSet { if inlineGitHunk != oldValue { reloadDocument() } }
    }
    /// A click on a Git change marker. The controller supplies the hunk from
    /// HEAD and toggles its inline representation.
    var onGitChangeClick: ((UInt64) -> Void)?
    /// `editor.renderWhitespace`; dots everywhere unless settings say otherwise.
    var renderWhitespace: WhitespaceRendering = .all {
        didSet { if renderWhitespace != oldValue { needsDisplay = true } }
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
    /// Explicit text colour avoids Core Text ignoring context fill alpha and
    /// drawing blame as bright editor text on dark themes.
    static let blameTextColor = NSColor.secondaryLabelColor.withAlphaComponent(0.52)

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

    /// Drops derived Core Text caches while retaining the document.
    func releaseNonEssentialCaches() {
        layouts.removeAll()
        syntaxLayouts.removeAll()
        caretRects.removeAll(keepingCapacity: false)
        needsDisplay = true
    }

    /// Tests can pin the caret on or off; `nil` means real focus and blink.
    var forcedCaretVisible: Bool?

    private var layouts = TwoGenerationCache<String, CTLine>(capacity: 1024)
    /// Parameter-name hints by displayed line text; see `setInlineHints`.
    private(set) var inlineHints: [String: [InlineHint]] = [:]
    private var hintLabels: [String: (line: CTLine, width: CGFloat)] = [:]
    /// Colored layouts include resolved theme colors, so they need their own
    /// cache key and are cleared when the font or appearance changes.
    private var syntaxLayouts = TwoGenerationCache<String, CTLine>(capacity: 512)
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
    /// Builds the right-click menu for the text area. Called after the
    /// caret has moved under the click.
    var onContextMenu: (() -> NSMenu?)?
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
            let inGutter = (lastMousePoint?.x ?? .infinity) < gutterWidth + gutterShift
            (inGutter ? NSCursor.arrow : definitionLink == nil ? NSCursor.iBeam : NSCursor.pointingHand).set()
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
            contentTextWidth = 0
            if document !== oldValue { setInlineHints([:]) }
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
        setPointerInGutter(point.x < gutterWidth + gutterShift)
        updateDefinitionLink(commandHeld: event.modifierFlags.contains(.command))
        onHover?(textPosition(at: point), point)
    }

    /// The pointer left the editor (the blame card uses it to close).
    var onMouseExit: (() -> Void)?

    override func mouseExited(with event: NSEvent) {
        lastMousePoint = nil
        definitionLink = nil
        setPointerInGutter(false)
        onMouseExit?()
    }

    private func setPointerInGutter(_ inside: Bool) {
        guard inside != pointerInGutter else { return }
        pointerInGutter = inside
        setNeedsDisplay(NSRect(x: gutterShift, y: visibleRect.minY, width: gutterWidth, height: visibleRect.height))
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
        syntaxLayouts.removeAll()
        needsDisplay = true
    }

    // MARK: Geometry

    /// Width of the line-number column; grows with the number of digits.
    var gutterWidth: CGFloat {
        let digits = max(3, String(max(lineCount, 1)).count)
        return ceil(CGFloat(digits) * metrics.digitWidth) + Self.gutterPadding + Self.gutterTrailingPadding
    }

    /// Right edge of the line numbers.
    var gutterRight: CGFloat { gutterWidth - Self.gutterTrailingPadding }

    /// Left edge of the fold chevron's column; clicks right of it fold.
    var foldZoneMinX: CGFloat { gutterRight + 9 }

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
        let first = min(lineCount, Int(line(atY: visible.minY).line))
        let last = min(lineCount, Int(line(atY: max(visible.minY, visible.maxY - 1)).line) + 1)
        return first..<max(first, last)
    }

    func setFont(_ newFont: NSFont) {
        font = newFont
        metrics = Metrics(font: newFont)
        layouts.removeAll()
        syntaxLayouts.removeAll()
        hintLabels.removeAll()
        contentTextWidth = 0
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
        hasFolds = !wrapSuspended && (document?.hasFolds() ?? false)
        rowCount = rowMapped ? Int(document?.totalRows() ?? 0) : lineCount
        let inlineRows = inlineGitHunk.map { $0.oldLines.count + $0.newLines.count } ?? 0
        let virtual = CGFloat(rowCount + inlineRows) * metrics.lineHeight
        let visible = enclosingScrollView?.contentSize.height ?? 0
        setFrameSize(NSSize(width: frame.width, height: max(virtual, visible)))
        updateDocumentWidth()
        updateFoldToolTip()
    }

    // MARK: Folding

    /// Folds or unfolds the range under `line`. Does nothing where lines
    /// must stay one row each (diff panes).
    func toggleFold(line: UInt64) {
        guard let document, !wrapSuspended, document.toggleFold(line: line) else { return }
        reloadDocument()
    }

    /// Hover text over the chevron column, kept in step with the gutter width.
    private func updateFoldToolTip() {
        let rect = NSRect(x: foldZoneMinX + gutterShift, y: 0, width: gutterWidth - foldZoneMinX, height: frame.height)
        guard rect != foldToolTipRect else { return }
        foldToolTipRect = rect
        // The gutter edge moved too (more digits), and the cursor rects follow it.
        window?.invalidateCursorRects(for: self)
        removeAllToolTips()
        addToolTip(rect, owner: self, userData: nil)
    }

    // MARK: Soft wrap

    private func wrapSettingsChanged() {
        applyWrapColumns()
        // Wrapped text fits the width: no sideways scroll, back to column 0.
        if wrapActive, let clip = superview as? NSClipView, clip.bounds.origin.x != 0 {
            clip.scroll(to: NSPoint(x: 0, y: clip.bounds.origin.y))
            enclosingScrollView?.reflectScrolledClipView(clip)
        }
        updateDocumentWidth()
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
        updateFoldToolTip()
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
        let row: CGFloat
        if rowMapped, let document {
            row = CGFloat(document.rowOfLine(line: line))
        } else {
            row = CGFloat(line)
        }
        let insertedRows: Int
        if let hunk = inlineGitHunk, line > hunk.anchorLine {
            insertedRows = hunk.oldLines.count + hunk.newLines.count
        } else {
            insertedRows = 0
        }
        return (row + CGFloat(insertedRows)) * metrics.lineHeight
    }

    /// The line (and row within it) drawn at document-space `y`.
    func line(atY y: CGFloat) -> (line: UInt64, row: Int) {
        var row = y.isFinite ? max(0, floor(y / metrics.lineHeight)) : 0
        if let hunk = inlineGitHunk {
            let anchor = rowMapped ? CGFloat(document?.rowOfLine(line: hunk.anchorLine) ?? hunk.anchorLine) : CGFloat(hunk.anchorLine)
            let inserted = CGFloat(hunk.oldLines.count + hunk.newLines.count)
            if row > anchor { row = max(anchor, row - inserted) }
        }
        guard rowMapped, let document else { return (UInt64(min(row, CGFloat(max(lineCount - 1, 0)))), 0) }
        let position = document.lineAtRow(row: UInt64(row))
        return (position.line, Int(position.row))
    }

    /// Source line at `y` with the fraction of the way through it, for
    /// scroll sync with the Markdown preview.
    func fractionalLine(atY y: CGFloat) -> Double {
        guard rowMapped, let document else { return Double(y / metrics.lineHeight) }
        let rowFloat = max(0, y / metrics.lineHeight)
        if UInt64(rowFloat) >= document.totalRows() { return Double(lineCount) }
        let position = document.lineAtRow(row: UInt64(rowFloat))
        let rows = Double(max(1, document.rowsOfLine(line: position.line)))
        return Double(position.line) + (Double(position.row) + Double(rowFloat - floor(rowFloat))) / rows
    }

    func y(forFractionalLine line: Double) -> CGFloat {
        guard rowMapped, let document else { return CGFloat(line) * metrics.lineHeight }
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
        for line in range {
            caretRects[line] = nil
            foldPillRects[line] = nil
        }

        let originX = textOriginX
        // Scrolled sideways, the line numbers are drawn this far to the right.
        let shift = gutterShift
        let gutterRight = self.gutterRight + shift
        var drawnWidth: CGFloat = 0
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
        // Blank lines inside an indented block keep the divider and guides.
        // Their indent is the smaller of the nearest non-blank lines around
        // them; neighbours outside the viewport are read once per draw.
        let visibleTexts = Dictionary(snapshot.lines.map { (Int($0.line), $0.text) }, uniquingKeysWith: { first, _ in first })
        let scanLimit = 100
        let total = lineCount
        var padded: (first: Int, lines: [String])?
        let textOfLine = { (index: Int) -> String? in
            guard index >= 0, index < total else { return nil }
            if let text = visibleTexts[index] { return text }
            if padded == nil {
                let first = max(0, range.lowerBound - scanLimit)
                let last = min(total, range.upperBound + scanLimit)
                padded = (first, document.lines(first: UInt64(first), count: UInt64(last - first)))
            }
            guard let padded, index >= padded.first, index - padded.first < padded.lines.count else { return nil }
            return padded.lines[index - padded.first]
        }
        let scopeIndent = { (line: Int) -> Int in
            func nearestIndent(step: Int) -> Int? {
                var index = line + step
                while index >= 0, index < total, abs(index - line) <= scanLimit {
                    guard let text = textOfLine(index) else { return nil }
                    if !text.allSatisfy(\.isWhitespace) { return Self.leadingIndentColumns(in: text) }
                    index += step
                }
                return nil
            }
            guard let above = nearestIndent(step: -1), let below = nearestIndent(step: 1) else { return 0 }
            return min(above, below)
        }
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

            // Current line: lines with a caret and no selection on them.
            if hasCaret, line.selections.isEmpty {
                context.setFillColor(Theme.currentLine.cgColor)
                context.fill(CGRect(x: 0, y: top, width: bounds.width, height: block))
            }

            if debugStopLine == Int(line.line) {
                context.setFillColor(NSColor.systemYellow.withAlphaComponent(0.22).cgColor)
                context.fill(CGRect(x: 0, y: top, width: bounds.width, height: block))
            }

            // Subtle divider between line numbers and source text. Only lines
            // with indented text get it: empty and blank lines have nothing
            // beside it, and text at column 0 would touch it.
            let isBlank = line.text.allSatisfy(\.isWhitespace)
            let startsAtDivider = line.text.first.map { $0 != " " && $0 != "\t" } ?? false
            // A blank line is part of the block around it, so it takes that indent.
            let indentColumns = isBlank
                ? max(Self.leadingIndentColumns(in: line.text), scopeIndent(Int(line.line)))
                : Self.leadingIndentColumns(in: line.text)
            let drawsDivider = isBlank ? indentColumns > 0 : !startsAtDivider

            // Indent guides sit behind text and whitespace markers. Draw one
            // guide for each complete four-space indentation level.
            if indentColumns >= 4 {
                context.setFillColor(NSColor.separatorColor.withAlphaComponent(0.08).cgColor)
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

            // Other uses of the identifier at the caret, fainter than find matches.
            if !line.occurrences.isEmpty {
                context.setFillColor(Theme.symbolOccurrence.cgColor)
                for occurrence in line.occurrences {
                    for rect in span(occurrence.start, occurrence.end, 0, 0) { context.fill(rect) }
                }
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
                let styles = (stylesByLine[line.line] ?? [])
                // Unused code (the `Unnecessary` diagnostic tag) is faded, not underlined.
                let faded = line.diagnostics.filter(\.unnecessary).map { Int($0.start)..<Int($0.end) }
                let ctLine = styles.isEmpty && faded.isEmpty
                    ? layout(for: line.text)
                    : syntaxLayout(for: line.text, styles: styles, faded: faded)
                drawnWidth = max(drawnWidth, CGFloat(CTLineGetTypographicBounds(ctLine, nil, nil, nil)))
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
                drawInlineHints(text: line.text, rows: rows, top: top, originX: originX, context: context)
            }

            // A folded range is stood in for by a small "..." after its header.
            if line.folded, !wrapSuspended {
                let end = point(column: line.text.utf16.count, text: line.text, rows: rows, top: top, originX: originX)
                let pill = CGRect(x: end.x + 6, y: end.y + 3, width: 26, height: lineHeight - 6)
                foldPillRects[Int(line.line)] = pill
                context.addPath(CGPath(roundedRect: pill, cornerWidth: 3, cornerHeight: 3, transform: nil))
                context.setFillColor(NSColor.separatorColor.withAlphaComponent(0.35).cgColor)
                context.fillPath()
                context.setFillColor(Theme.gutterText.cgColor)
                for dot in -1...1 {
                    context.fillEllipse(in: CGRect(x: pill.midX + CGFloat(dot) * 6 - 1.5, y: pill.midY - 1.5, width: 3, height: 3))
                }
            }

            // Git blame of the caret line, quiet and after the text.
            if let blame = blameAnnotation, blame.line == line.line {
                let note = CTLineCreateWithAttributedString(NSAttributedString(
                    string: blame.text,
                    attributes: [.font: font, .foregroundColor: Self.blameTextColor]
                ))
                let end = point(column: line.text.utf16.count, text: line.text, rows: rows, top: top, originX: originX)
                context.textPosition = CGPoint(x: end.x + Self.blameGap, y: end.y + metrics.ascent)
                CTLineDraw(note, context)
            }

            // Diagnostics sit above text decorations but beneath carets.
            // Cmd hover: underline the identifier Cmd+click would open.
            if let link = definitionLink, link.line == line.line {
                context.setFillColor(NSColor.linkColor.cgColor)
                for rect in span(link.start, link.end, 1, 0) {
                    context.fill(CGRect(x: rect.minX, y: rect.minY + metrics.ascent + 2, width: rect.width, height: 1))
                }
            }
            // Only errors and warnings are underlined. Information and hints
            // stay as the gutter dot so missing type hints do not clutter text.
            for diagnostic in line.diagnostics where !diagnostic.unnecessary && diagnostic.severity <= 2 {
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

            if shift > 0 {
                // Scrolled sideways: cover the text that ran under the line numbers.
                let gutterRect = CGRect(x: shift, y: top, width: gutterWidth, height: block)
                context.setFillColor(Theme.editorBackground.resolved(for: effectiveAppearance).cgColor)
                context.fill(gutterRect)
                if hasCaret, line.selections.isEmpty {
                    context.setFillColor(Theme.currentLine.cgColor)
                    context.fill(gutterRect)
                }
            }
            if drawsDivider {
                context.setFillColor(NSColor.separatorColor.withAlphaComponent(0.28).cgColor)
                context.fill(CGRect(x: shift + gutterWidth - 1, y: top, width: 1, height: block))
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
            if let severity = line.diagnostics.filter({ !$0.unnecessary }).map(\.severity).min() {
                context.setFillColor(Theme.diagnostic(severity).cgColor)
                context.fill(CGRect(x: gutterRight + 3, y: top + lineHeight / 2 - 2, width: 4, height: 4))
            }
            if line.foldable, !wrapSuspended, line.folded || pointerInGutter {
                drawFoldChevron(in: context, folded: line.folded,
                                center: CGPoint(x: gutterRight + 16, y: top + lineHeight / 2))
            }
            if let change = gitLineChanges[line.line + 1] {
                let color: NSColor = switch change {
                case .added: NSColor.systemGreen
                case .modified: NSColor.systemYellow
                case .deleted: NSColor.systemRed
                }
                context.setFillColor(color.cgColor)
                let markerHeight: CGFloat = change == .deleted ? 3 : block
                context.fill(CGRect(x: shift, y: top, width: 4, height: markerHeight))
            }
            if let hunk = inlineGitHunk, hunk.anchorLine == line.line {
                drawInlineGitHunk(hunk, below: top + block, context: context)
            }
        }
        if drawnWidth > contentTextWidth {
            contentTextWidth = drawnWidth
            // Not while drawing: resizing the view would start another layout.
            DispatchQueue.main.async { [weak self] in self?.updateDocumentWidth() }
        }
    }

    private func drawInlineGitHunk(_ hunk: InlineGitHunk, below top: CGFloat, context: CGContext) {
        let lineHeight = metrics.lineHeight
        let numberWidth = gutterRight - Self.gutterPadding
        let lines = hunk.oldLines.map { (text: $0, color: Theme.diffRemoved, prefix: "-") }
            + hunk.newLines.map { (text: $0, color: Theme.diffAdded, prefix: "+") }
        for (index, line) in lines.enumerated() {
            let y = top + CGFloat(index) * lineHeight
            context.setFillColor(line.color.cgColor)
            context.fill(CGRect(x: 0, y: y, width: bounds.width, height: lineHeight))
            context.setFillColor(line.prefix == "-" ? NSColor.systemRed.cgColor : NSColor.systemGreen.cgColor)
            context.fill(CGRect(x: gutterShift, y: y, width: 4, height: lineHeight))
            let marker = layout(for: line.prefix)
            context.setFillColor(Theme.gutterText.cgColor)
            context.textPosition = CGPoint(x: gutterShift + numberWidth - 8, y: y + metrics.ascent)
            CTLineDraw(marker, context)
            let source = layout(for: line.text)
            context.setFillColor(Theme.editorForeground.resolved(for: effectiveAppearance).cgColor)
            context.textPosition = CGPoint(x: textOriginX, y: y + metrics.ascent)
            CTLineDraw(source, context)
        }
    }

    /// A small chevron: points right while folded, down while open.
    private func drawFoldChevron(in context: CGContext, folded: Bool, center c: CGPoint) {
        context.saveGState()
        context.setStrokeColor(Theme.gutterText.cgColor)
        context.setLineWidth(1.4)
        context.setLineCap(.round)
        context.setLineJoin(.round)
        if folded {
            context.move(to: CGPoint(x: c.x - 1.5, y: c.y - 3.5))
            context.addLine(to: CGPoint(x: c.x + 2, y: c.y))
            context.addLine(to: CGPoint(x: c.x - 1.5, y: c.y + 3.5))
        } else {
            context.move(to: CGPoint(x: c.x - 3.5, y: c.y - 1.5))
            context.addLine(to: CGPoint(x: c.x, y: c.y + 2))
            context.addLine(to: CGPoint(x: c.x + 3.5, y: c.y - 1.5))
        }
        context.strokePath()
        context.restoreGState()
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

    private nonisolated static func leadingIndentColumns(in text: String) -> Int {
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
            var cellWidth = xOffset(in: text, column: UInt64(column + 1))
                - xOffset(in: text, column: UInt64(column))
            // The kern on this character reserves the following hint's room;
            // keep its whitespace marker inside the character's real cell.
            if let hint = inlineHints[text]?.first(where: { $0.column == column + 1 }) {
                cellWidth -= hintAdvance(hint.label)
            }
            let x1 = x0 + max(0, cellWidth)
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
        let line = layout(for: text)
        // Core Text puts the caret halfway through a kerned gap; a hinted
        // argument starts after the label, where its glyph is drawn.
        if let hints = inlineHints[text], hints.contains(where: { $0.column == Int(column) }),
           let x = Self.glyphX(in: line, stringIndex: CFIndex(column)) {
            return x
        }
        return CGFloat(CTLineGetOffsetForStringIndex(line, CFIndex(column), nil))
    }

    /// X of the glyph drawn for `stringIndex`, if one starts there.
    private static func glyphX(in line: CTLine, stringIndex: CFIndex) -> CGFloat? {
        for run in CTLineGetGlyphRuns(line) as? [CTRun] ?? [] {
            let range = CTRunGetStringRange(run)
            guard stringIndex >= range.location, stringIndex < range.location + range.length else { continue }
            let count = CTRunGetGlyphCount(run)
            var indices = [CFIndex](repeating: 0, count: count)
            var positions = [CGPoint](repeating: .zero, count: count)
            CTRunGetStringIndices(run, CFRange(location: 0, length: count), &indices)
            CTRunGetPositions(run, CFRange(location: 0, length: count), &positions)
            if let i = indices.firstIndex(of: stringIndex) { return positions[i].x }
        }
        return nil
    }

    /// Cached `CTLine` for `text`. Identical strings (blank lines, repeated
    /// braces, line numbers) share one layout.
    func layout(for text: String) -> CTLine {
        if let cached = layouts.value(for: text) { return cached }
        // Without this attribute Core Text draws black regardless of the
        // context fill colour, which is unreadable in dark mode. With it,
        // one cached layout works for every colour and appearance.
        let fromContext = NSAttributedString.Key(kCTForegroundColorFromContextAttributeName as String)
        let attributed = NSMutableAttributedString(string: text, attributes: [.font: font, fromContext: true])
        addHintSpacing(to: attributed, text: text)
        let line = CTLineCreateWithAttributedString(attributed)
        layouts.insert(line, for: text)
        layoutsCreated += 1
        return line
    }

    static let fadedAlpha: CGFloat = 0.45

    func syntaxLayout(for text: String, styles: [StyleRun], faded: [Range<Int>] = []) -> CTLine {
        // No `kCTForegroundColorFromContextAttributeName` here: it makes
        // CoreText draw every run in the context's fill color, so all syntax
        // colors came out as `textColor`. Unstyled text gets it explicitly.
        let key = "\(effectiveAppearance.name.rawValue)\u{1f}\(text)\u{1f}\(styles.map { "\($0.start):\($0.end):\($0.scope)" }.joined(separator: ","))\u{1f}\(faded.map { "\($0.lowerBound):\($0.upperBound)" }.joined(separator: ","))"
        if let cached = syntaxLayouts.value(for: key) { return cached }
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
        for range in faded {
            let start = min(range.lowerBound, length)
            let end = min(range.upperBound, length)
            guard start < end else { continue }
            let target = NSRange(location: start, length: end - start)
            // Keep each run's own colour, only lower its opacity.
            attributed.enumerateAttribute(.foregroundColor, in: target) { value, run, _ in
                let color = (value as? NSColor) ?? base
                attributed.addAttribute(.foregroundColor, value: color.withAlphaComponent(Self.fadedAlpha), range: run)
            }
        }
        addHintSpacing(to: attributed, text: text)
        let line = CTLineCreateWithAttributedString(attributed)
        syntaxLayouts.insert(line, for: key)
        layoutsCreated += 1
        return line
    }
}

// MARK: Inline parameter hints

extension EditorView {
    /// A parameter name drawn before the argument at UTF-16 `column`.
    struct InlineHint: Equatable {
        let column: Int
        let label: String
    }

    static let hintPadding: CGFloat = 3
    static let hintGap: CGFloat = 2

    /// Replaces the hints, keyed by the displayed text of their line. Keying
    /// by text keeps an edited line from showing hints made for its old text.
    func setInlineHints(_ hints: [String: [InlineHint]]) {
        guard hints != inlineHints else { return }
        inlineHints = hints
        // Layouts carry the space reserved for the labels.
        layouts.removeAll()
        syntaxLayouts.removeAll()
        needsDisplay = true
        onLayoutChange?()
    }

    /// Room a label takes in the line: the label, its padding and a gap on
    /// both sides.
    func hintAdvance(_ label: String) -> CGFloat {
        hintLabel(label).width + 2 * Self.hintPadding + 2 * Self.hintGap
    }

    /// Widens the character before each hinted argument (Core Text kerning),
    /// so carets, selections and clicks all measure through the labels.
    func addHintSpacing(to attributed: NSMutableAttributedString, text: String) {
        guard let hints = inlineHints[text] else { return }
        let string = text as NSString
        for hint in hints where hint.column > 0 && hint.column <= string.length {
            let previous = string.rangeOfComposedCharacterSequence(at: hint.column - 1)
            // Only the last unit of a composed character may carry the space.
            guard NSMaxRange(previous) == hint.column else { continue }
            attributed.addAttribute(.kern, value: hintAdvance(hint.label), range: previous)
        }
    }

    /// Draws the labels of `text` into the space `addHintSpacing` reserved.
    func drawInlineHints(text: String, rows: LineRows, top: CGFloat, originX: CGFloat, context: CGContext) {
        guard let hints = inlineHints[text] else { return }
        let hintFont = self.hintFont
        let pillHeight = max(0, metrics.lineHeight - 4)
        let textHeight = hintFont.ascender - hintFont.descender
        for hint in hints {
            let row = rows.row(containing: hint.column)
            // A soft wrap between the space and its argument: skip the label.
            guard hint.column > 0, rows.row(containing: hint.column - 1) == row else { continue }
            let at = point(column: hint.column, text: text, rows: rows, top: top, originX: originX)
            let label = hintLabel(hint.label)
            let pill = CGRect(x: at.x - Self.hintGap - label.width - 2 * Self.hintPadding,
                              y: at.y + 2, width: label.width + 2 * Self.hintPadding, height: pillHeight)
            context.addPath(CGPath(roundedRect: pill, cornerWidth: 3, cornerHeight: 3, transform: nil))
            context.setFillColor(NSColor.secondaryLabelColor.withAlphaComponent(0.12).cgColor)
            context.fillPath()
            context.setFillColor(NSColor.secondaryLabelColor.cgColor)
            context.textPosition = CGPoint(x: pill.minX + Self.hintPadding,
                                           y: at.y + (metrics.lineHeight - textHeight) / 2 + hintFont.ascender)
            CTLineDraw(label.line, context)
        }
    }

    private var hintFont: NSFont {
        NSFont(descriptor: font.fontDescriptor, size: max(8, (font.pointSize * 0.85).rounded())) ?? font
    }

    private func hintLabel(_ label: String) -> (line: CTLine, width: CGFloat) {
        if let cached = hintLabels[label] { return cached }
        let fromContext = NSAttributedString.Key(kCTForegroundColorFromContextAttributeName as String)
        let line = CTLineCreateWithAttributedString(NSAttributedString(
            string: label, attributes: [.font: hintFont, fromContext: true]))
        let entry = (line: line, width: ceil(CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil))))
        if hintLabels.count > 512 { hintLabels.removeAll() }
        hintLabels[label] = entry
        return entry
    }
}

extension EditorView: NSViewToolTipOwner {
    nonisolated func view(_ view: NSView, stringForToolTip tag: NSView.ToolTipTag,
                          point: NSPoint, userData data: UnsafeMutableRawPointer?) -> String {
        MainActor.assumeIsolated {
            guard let document, !wrapSuspended else { return "" }
            switch document.foldState(line: line(atY: point.y).line) {
            case .plain: return ""
            case .expanded: return "Click to collapse the range."
            case .folded: return "Click to expand the range."
            }
        }
    }
}
