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

        init(font: NSFont, lineSpacing: CGFloat = Theme.editorLineSpacing) {
            let ctFont = font as CTFont
            ascent = CTFontGetAscent(ctFont)
            descent = CTFontGetDescent(ctFont)
            leading = CTFontGetLeading(ctFont)
            // Font metrics carry float noise (16.00006 for Fira Code 13), which
            // a bare `ceil` turned into a whole extra point of space per line.
            lineHeight = ceil(ascent + descent + leading - 0.01 + lineSpacing)
            digitWidth = ("0" as NSString).size(withAttributes: [.font: font]).width
            spaceWidth = (" " as NSString).size(withAttributes: [.font: font]).width
        }
    }

    /// Space before the line numbers. A test's run icon sits in it, with
    /// room on both sides.
    static let gutterPadding: CGFloat = 20
    /// Space after the line numbers: Git lane first, then the fold chevron.
    static let gutterTrailingPadding: CGFloat = 26
    /// Space between the gutter and the first column.
    static let textInset: CGFloat = 0
    /// Git change bar: gap between the line numbers and the bar.
    static let gitBarOffset: CGFloat = 4
    static let gitBarWidth: CGFloat = 3
    /// Width of the bar under the pointer. It grows leftwards, keeping its right edge.
    static let gitBarHoverWidth: CGFloat = 6
    /// Deleted lines: a triangle this wide and tall, like VS Code's.
    static let gitDeleteWidth: CGFloat = 4
    static let gitDeleteHeight: CGFloat = 8
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
    /// 0-based line whose Git bar the pointer is over; that bar widens.
    var hoveredGitLine: UInt64? {
        didSet {
            guard hoveredGitLine != oldValue else { return }
            setNeedsDisplay(NSRect(x: gutterShift, y: visibleRect.minY, width: gutterWidth, height: visibleRect.height))
        }
    }
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
    /// 0-based lines that declare a test, with the state of their run icon.
    /// The icon sits in the left padding of the gutter.
    var runMarkers: [UInt64: TestRunState] = [:] {
        didSet { if runMarkers != oldValue { needsDisplay = true } }
    }
    /// A click on a run icon, with its 0-based line.
    var onRunTest: ((UInt64) -> Void)?

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
    func updateDocumentWidth(propagate: Bool = true) {
        guard let scroll = enclosingScrollView else { return }
        var width = max(scroll.contentSize.width - rightInset, 1)
        if !wrapActive {
            // The minimap covers the last `rightInset` points of the clip.
            width = max(width, textOriginX + scrollTextWidth + Self.trailingRoom + rightInset)
        }
        // The two sides of a diff scroll together, so both must reach as far
        // right as the longer one.
        if let partner = diffPartner, let partnerScroll = partner.enclosingScrollView, !partner.wrapActive {
            width = max(width, max(partnerScroll.contentSize.width - partner.rightInset, 1),
                        partner.textOriginX + partner.scrollTextWidth + Self.trailingRoom + partner.rightInset)
        }
        if propagate { diffPartner?.updateDocumentWidth(propagate: false) }
        guard abs(width - frame.width) > 0.5 else { return }
        setFrameSize(NSSize(width: width, height: frame.height))
    }

    /// Width the text scrolls over: the widest line drawn so far, or the
    /// widest line of the document when the core knows it, so the scroll
    /// range is whole from the start and does not grow while scrolling.
    var scrollTextWidth: CGFloat {
        let columns = CGFloat(document?.maxLineColumns() ?? 0)
        return max(contentTextWidth, ceil(columns * metrics.spaceWidth))
    }

    /// The other side of an open diff, whose horizontal scroll range this
    /// side shares.
    weak var diffPartner: EditorView?

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
            onAppKitMainThread { self?.clipBoundsChanged() }
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
    enum DiffSide: Equatable { case removed, added }
    /// Side-by-side diff role. `nil` in a normal editor.
    var diffSide: DiffSide? {
        didSet { if diffSide != oldValue { needsDisplay = true } }
    }
    /// Background per 0-based line, for the changed lines of a diff.
    var lineTints: [UInt64: NSColor] = [:] {
        didSet { if lineTints != oldValue { needsDisplay = true } }
    }
    /// Empty visual rows used only by side-by-side diffs to align hunks.
    var diffSpacers: [DiffLineMap.Spacer] = [] {
        didSet {
            guard diffSpacers != oldValue else { return }
            reloadDocument()
        }
    }
    var gitLineChanges: [UInt64: GitLineChangeKind] = [:] {
        didSet { if gitLineChanges != oldValue { needsDisplay = true } }
    }
    /// A changed hunk expanded in a peek zone below `anchorLine`: a header row,
    /// then context, removed and added rows.
    struct InlineGitHunk: Equatable {
        enum Kind: Equatable { case context, removed, added, modified }
        struct Row: Equatable {
            var kind: Kind
            /// 1-based line numbers in HEAD and in the working tree.
            var oldNumber: Int?
            var newNumber: Int?
            var text: String
            /// Syntax runs of `text`, from the document the row belongs to.
            var styles: [StyleRun] = []
        }
        var anchorLine: UInt64
        var rows: [Row]
        /// First HEAD line of the hunk. It does not move while the file is
        /// edited, so a refresh finds the same hunk again by it.
        var oldStart: UInt64 = 0
        var newStart: UInt64 = 0
        var newLen: UInt64 = 0
        var fileName = ""
        var index = 0
        var total = 1

        static let headerVerticalPadding: CGFloat = 4
        /// The header plus every source row. Used by tests and accessibility.
        var rowCount: Int { rows.count + 1 }
        func headerHeight(lineHeight: CGFloat) -> CGFloat {
            lineHeight + Self.headerVerticalPadding * 2
        }
        func height(lineHeight: CGFloat) -> CGFloat {
            headerHeight(lineHeight: lineHeight) + CGFloat(rows.count) * lineHeight
        }
        var oldLines: [String] { rows.filter { $0.oldNumber != nil && $0.kind != .context }.map(\.text) }
        var newLines: [String] { rows.filter { $0.newNumber != nil && $0.kind != .context }.map(\.text) }

        func contains(line: UInt64) -> Bool {
            line >= newStart && line < newStart + max(newLen, 1)
        }
    }
    var inlineGitHunk: InlineGitHunk? {
        didSet {
            if inlineGitHunk != oldValue {
                reloadDocument()
                window?.invalidateCursorRects(for: self)
            }
        }
    }
    /// A click on a Git change marker. The controller supplies the hunk from
    /// HEAD and toggles its inline representation.
    var onGitChangeClick: ((UInt64) -> Void)?
    /// A double-click on an inferred return type: line and UTF-16 column
    /// of the hint, where the type goes.
    var onReturnTypeHintDoubleClick: ((UInt64, Int) -> Void)?
    /// The header buttons of the open hunk.
    enum InlineHunkAction: Equatable { case stage, revert, next, previous, close }
    var onInlineHunkAction: ((InlineHunkAction) -> Void)?
    /// `editor.renderWhitespace`; dots everywhere unless settings say otherwise.
    var renderWhitespace: WhitespaceRendering = .all {
        didSet { if renderWhitespace != oldValue { needsDisplay = true } }
    }
    private(set) var tabSize = 4

    func syncTabSize() {
        let next = max(Int(document?.indentSettings().tabSize ?? 4), 1)
        guard next != tabSize else { return }
        tabSize = next
        releaseNonEssentialCaches()
        onLayoutChange?()
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
        return NSRect(x: end.x + trailingHintExtent(text: text) + Self.blameGap, y: end.y,
                      width: width, height: metrics.lineHeight)
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
    /// Pointer moved: text position, point, and whether Cmd is held.
    var onHover: ((TextPos, NSPoint, Bool) -> Void)?
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
            if document !== oldValue {
                oldValue?.setWrapHints(lines: [])
                setInlineHints([:])
            }
            applyWrapColumns()
            syncTabSize()
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

    // AppKit asks these through Objective-C during hit testing. Constant
    // answers must avoid the main-actor thunk, whose executor check can crash.
    nonisolated override var isFlipped: Bool { true }
    nonisolated override var isOpaque: Bool { true }

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
        updateHoveredGitLine(at: point)
        let commandHeld = event.modifierFlags.contains(.command)
        updateDefinitionLink(commandHeld: commandHeld)
        onHover?(textPosition(at: point), point, commandHeld)
    }

    /// The pointer left the editor (the blame card uses it to close).
    var onMouseExit: (() -> Void)?

    override func mouseExited(with event: NSEvent) {
        lastMousePoint = nil
        definitionLink = nil
        setPointerInGutter(false)
        updateHoveredGitLine(at: nil)
        onMouseExit?()
    }

    /// Only the line under the pointer widens, as each bar does in VS Code.
    func updateHoveredGitLine(at point: NSPoint?) {
        var hovered: UInt64?
        if let point,
           point.x >= gutterShift + gitBarX - 2,
           point.x < gutterShift + gitBarX + Self.gitBarWidth + 2 {
            let line = textPosition(at: point).line
            if gitLineChanges[line + 1] != nil { hovered = line }
        }
        hoveredGitLine = hovered
    }

    private func setPointerInGutter(_ inside: Bool) {
        guard inside != pointerInGutter else { return }
        pointerInGutter = inside
        setNeedsDisplay(NSRect(x: gutterShift, y: visibleRect.minY, width: gutterWidth, height: visibleRect.height))
    }

    override func flagsChanged(with event: NSEvent) {
        super.flagsChanged(with: event)
        updateDefinitionLinkAtWindowMouse(commandHeld: event.modifierFlags.contains(.command))
    }

    /// Modifier events follow the first responder, which may be another view
    /// while the pointer remains over the editor. Re-read the window mouse
    /// position so Cmd-hover does not require an extra mouse movement.
    func updateDefinitionLinkAtWindowMouse(commandHeld: Bool) {
        guard let window else {
            updateDefinitionLink(commandHeld: false)
            return
        }
        let point = convert(window.mouseLocationOutsideOfEventStream, from: nil)
        lastMousePoint = bounds.contains(point) ? point : nil
        updateDefinitionLink(commandHeld: commandHeld)
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

    /// Git-diff bar, just right of the line numbers. It follows `gutterRight`,
    /// so it moves with the column when it grows from 999 to 1000.
    var gitBarX: CGFloat { gutterRight + Self.gitBarOffset }

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
        syncWrapHints()
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
        // The gutter grows with the digit count; the scroller starts after it.
        (enclosingScrollView as? DiagnosticScrollView)?.updateScrollerInset()
        hasFolds = !wrapSuspended && (document?.hasFolds() ?? false)
        rowCount = (rowMapped ? Int(document?.totalRows() ?? 0) : lineCount)
            + diffSpacers.reduce(0) { $0 + $1.rows }
        let inlineHeight = inlineGitHunk?.height(lineHeight: metrics.lineHeight) ?? 0
        let virtual = CGFloat(rowCount) * metrics.lineHeight + inlineHeight
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
        syncWrapHints()
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
        let insertedHeight: CGFloat
        if let hunk = inlineGitHunk, line > hunk.anchorLine {
            insertedHeight = hunk.height(lineHeight: metrics.lineHeight)
        } else {
            insertedHeight = 0
        }
        let spacerRows = diffSpacers
            .filter { $0.beforeLine <= Int(line) }
            .reduce(0) { $0 + $1.rows }
        return (row + CGFloat(spacerRows)) * metrics.lineHeight + insertedHeight
    }

    /// The line (and row within it) drawn at document-space `y`.
    func line(atY y: CGFloat) -> (line: UInt64, row: Int) {
        var row = y.isFinite ? max(0, floor(y / metrics.lineHeight)) : 0
        if let hunk = inlineGitHunk {
            let anchor = rowMapped ? CGFloat(document?.rowOfLine(line: hunk.anchorLine) ?? hunk.anchorLine) : CGFloat(hunk.anchorLine)
            let zoneStart = (anchor + 1) * metrics.lineHeight
            let zoneEnd = zoneStart + hunk.height(lineHeight: metrics.lineHeight)
            if y >= zoneStart, y < zoneEnd {
                row = anchor
            } else if y >= zoneEnd {
                row = max(anchor, floor((y - hunk.height(lineHeight: metrics.lineHeight)) / metrics.lineHeight))
            }
        }
        if !diffSpacers.isEmpty {
            var removed = 0
            for spacer in diffSpacers {
                let start = spacer.beforeLine + removed
                if Int(row) < start { break }
                if Int(row) < start + spacer.rows {
                    return (UInt64(min(spacer.beforeLine, max(lineCount - 1, 0))), 0)
                }
                removed += spacer.rows
            }
            row = max(0, row - CGFloat(removed))
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
        let shift = rowShift(in: text, rows: rows, row: row)
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
            let shift = rowShift(in: text, rows: rows, row: row)
            let x0 = originX + xOffset(in: text, column: UInt64(a)) - shift
            var x1 = originX + xOffset(in: text, column: UInt64(b)) - shift
            if row == rows.count - 1 {
                x1 += trailing
            } else if b == rows.end(of: row), let hint = inlineHints[text]?.first(where: { $0.column == b }) {
                // The next row starts at a labelled argument: the label's room
                // belongs to that row, not to the end of this one.
                x1 -= hintAdvance(hint.label)
            }
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
        let timer = Timer(timeInterval: Self.blinkInterval, target: self,
                          selector: #selector(toggleCaret), userInfo: nil, repeats: true)
        RunLoop.main.add(timer, forMode: .common)
        blinkTimer = timer
    }

    private func stopBlinking() {
        blinkTimer?.invalidate()
        blinkTimer = nil
        caretOn = true
    }

    @objc private func toggleCaret() {
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

        drawDiffSpacers(context: context, lineHeight: lineHeight)

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
            let tabSize = self.tabSize
            func nearestIndent(step: Int) -> Int? {
                var index = line + step
                while index >= 0, index < total, abs(index - line) <= scanLimit {
                    guard let text = textOfLine(index) else { return nil }
                    if !text.allSatisfy(\.isWhitespace) {
                        return Self.leadingIndentColumns(in: text, tabSize: tabSize)
                    }
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
                ? max(Self.leadingIndentColumns(in: line.text, tabSize: tabSize), scopeIndent(Int(line.line)))
                : Self.leadingIndentColumns(in: line.text, tabSize: tabSize)
            let drawsDivider = isBlank ? indentColumns > 0 : !startsAtDivider

            // Indent guides sit behind text and whitespace markers. Draw one
            // guide for each complete four-space indentation level.
            if indentColumns >= tabSize {
                context.setFillColor(NSColor.separatorColor.withAlphaComponent(0.08).cgColor)
                var column = tabSize
                // The deepest guide sits immediately beside the text and is
                // visually noisy; keep only the parent-level guides.
                while column < indentColumns {
                    let x = originX + CGFloat(column) * metrics.spaceWidth - 0.5
                    context.fill(CGRect(x: x, y: top, width: 1, height: block))
                    column += tabSize
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
                    let shift = rowShift(in: line.text, rows: rows, row: row)
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

            // Git blame of the caret line, quiet and after the text. A
            // diagnostic message follows it, not the source text, so the two
            // annotations never occupy the same pixels.
            let sourceEnd = point(column: line.text.utf16.count, text: line.text, rows: rows,
                                  top: top, originX: originX)
            var trailingAnnotationEnd = sourceEnd.x + trailingHintExtent(text: line.text)
            if let blame = blameAnnotation, blame.line == line.line {
                let note = CTLineCreateWithAttributedString(NSAttributedString(
                    string: blame.text,
                    attributes: [.font: font, .foregroundColor: Self.blameTextColor]
                ))
                let start = trailingAnnotationEnd + Self.blameGap
                context.textPosition = CGPoint(x: start, y: sourceEnd.y + metrics.ascent)
                CTLineDraw(note, context)
                trailingAnnotationEnd = start + CGFloat(CTLineGetTypographicBounds(note, nil, nil, nil))
            }

            // Diagnostics sit above text decorations but beneath carets.
            // Cmd hover: underline the identifier Cmd+click would open.
            if let link = definitionLink, link.line == line.line {
                context.setFillColor(NSColor.linkColor.cgColor)
                for rect in span(link.start, link.end, 1, 0) {
                    context.fill(CGRect(x: rect.minX, y: rect.minY + metrics.ascent + 2, width: rect.width, height: 1))
                }
            }
            // Errors, warnings and information use their severity color.
            for diagnostic in line.diagnostics where !diagnostic.unnecessary && diagnostic.severity <= 3 {
                context.setStrokeColor(Self.diagnosticUnderlineColor(diagnostic.severity).cgColor)
                for rect in span(diagnostic.start, diagnostic.end, 1, 0) {
                    drawDiagnosticUnderline(in: rect, context: context)
                }
            }

            // The first diagnostic message reads at the line's right edge,
            // without obscuring the minimap or a horizontally scrolled gutter.
            if let diagnostic = line.diagnostics.first(where: { !$0.unnecessary && !$0.message.isEmpty }) {
                let start = trailingAnnotationEnd + 24
                let limit = bounds.width - rightInset - 8
                if start < limit {
                    let note = CTLineCreateWithAttributedString(NSAttributedString(
                        string: diagnostic.message,
                        attributes: [.font: font, .foregroundColor: Theme.diagnostic(diagnostic.severity)]
                    ))
                    let available = limit - start
                    let rendered = CTLineCreateTruncatedLine(note, Double(available), .end, nil) ?? note
                    context.textPosition = CGPoint(x: start, y: sourceEnd.y + metrics.ascent)
                    CTLineDraw(rendered, context)
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

            // The line beside the numbers is the text's left edge: it scrolls
            // away with the text, and the cover below hides it on the way.
            if drawsDivider {
                context.setFillColor(NSColor.separatorColor.withAlphaComponent(0.28).cgColor)
                context.fill(CGRect(x: gutterWidth - 1, y: top, width: 1, height: block))
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
            if let state = runMarkers[line.line] {
                drawRunMarker(state, in: context, centerY: top + lineHeight / 2, shift: shift)
            }
            if let marker = diffMarker(for: line.line) {
                let glyph = layout(for: marker)
                context.setFillColor(Theme.gutterText.cgColor)
                context.textPosition = CGPoint(x: gutterRight + 10, y: top + metrics.ascent)
                CTLineDraw(glyph, context)
            }
            if line.foldable, !wrapSuspended, line.folded || pointerInGutter {
                drawFoldChevron(in: context, folded: line.folded,
                                center: CGPoint(x: gutterRight + 16, y: top + lineHeight / 2))
            }
            if let change = gitLineChanges[line.line + 1] {
                context.setFillColor(Theme.gitChange(change).resolved(for: effectiveAppearance).cgColor)
                if change == .deleted {
                    // As in VS Code: a small triangle on the edge where lines went
                    // missing, not a bar. It stays inside the row so the next
                    // row's background cannot paint over it.
                    let x = shift + gitBarX
                    context.move(to: CGPoint(x: x, y: top + block - Self.gitDeleteHeight))
                    context.addLine(to: CGPoint(x: x + Self.gitDeleteWidth, y: top + block - Self.gitDeleteHeight / 2))
                    context.addLine(to: CGPoint(x: x, y: top + block))
                    context.closePath()
                    context.fillPath()
                } else {
                    let width = hoveredGitLine == line.line ? Self.gitBarHoverWidth : Self.gitBarWidth
                    let bar = CGRect(x: shift + gitBarX + Self.gitBarWidth - width, y: top, width: width, height: block)
                    if change == .modified {
                        drawGitHatch(in: bar, color: Theme.gitChange(change).resolved(for: effectiveAppearance), context: context)
                    } else {
                        context.fill(bar)
                    }
                }
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

    /// Diagonal stripes for modified lines, VS Code's default pattern for them.
    /// The stripes lie on `x + y = k * step` in view coordinates, so they carry
    /// on from one row to the next.
    private func drawGitHatch(in rect: CGRect, color: NSColor, context: CGContext) {
        context.saveGState()
        defer { context.restoreGState() }
        context.clip(to: rect)
        context.setStrokeColor(color.cgColor)
        context.setLineWidth(1)
        let step = Self.gitBarWidth
        var k = ((rect.minX + rect.minY) / step).rounded(.down)
        while k * step <= rect.maxX + rect.maxY {
            let c = k * step
            context.move(to: CGPoint(x: c - rect.maxY, y: rect.maxY))
            context.addLine(to: CGPoint(x: c - rect.minY, y: rect.minY))
            k += 1
        }
        context.strokePath()
    }

    func diffMarker(for line: UInt64) -> String? {
        guard lineTints[line] != nil else { return nil }
        return switch diffSide {
        case .removed: "-"
        case .added: "+"
        case nil: nil
        }
    }

    private func drawDiffSpacers(context: CGContext, lineHeight: CGFloat) {
        guard !diffSpacers.isEmpty else { return }
        var preceding = 0
        for spacer in diffSpacers {
            let top = CGFloat(spacer.beforeLine + preceding) * lineHeight
            let rect = CGRect(x: gutterShift + gutterWidth, y: top,
                              width: max(0, bounds.width - gutterWidth),
                              height: CGFloat(spacer.rows) * lineHeight)
            preceding += spacer.rows
            guard rect.intersects(visibleRect) else { continue }
            context.setFillColor(Theme.editorBackground.resolved(for: effectiveAppearance).cgColor)
            context.fill(rect)
            context.saveGState()
            context.clip(to: rect)
            context.setStrokeColor(NSColor.separatorColor.withAlphaComponent(0.28).cgColor)
            context.setLineWidth(1)
            var x = rect.minX - rect.height
            while x < rect.maxX {
                context.move(to: CGPoint(x: x, y: rect.maxY))
                context.addLine(to: CGPoint(x: x + rect.height, y: rect.minY))
                x += 9
            }
            context.strokePath()
            context.restoreGState()
        }
    }

    /// A compact 1 pt zig-zag below diagnostics, rather than a flat underline.
    static func diagnosticUnderlineColor(_ severity: UInt8) -> NSColor {
        Theme.diagnostic(severity)
    }

    private func drawDiagnosticUnderline(in rect: CGRect, context: CGContext) {
        let y = rect.maxY - 2
        let step: CGFloat = 2
        context.setLineWidth(1)
        context.move(to: CGPoint(x: rect.minX, y: y))
        var x = rect.minX
        var up = true
        while x < rect.maxX {
            let next = min(x + step, rect.maxX)
            context.addLine(to: CGPoint(x: next, y: y + (up ? -1 : 1)))
            x = next
            up.toggle()
        }
        context.strokePath()
    }

    static let inlineHunkButtons: [(action: InlineHunkAction, symbol: String)] = [
        (.stage, "plus"),
        (.revert, "arrow.uturn.backward"),
        (.next, "arrow.down"),
        (.previous, "arrow.up"),
        (.close, "xmark"),
    ]

    static func inlineHunkActionIsEnabled(_ action: InlineHunkAction, hunk: InlineGitHunk) -> Bool {
        hunk.total > 1 || (action != .next && action != .previous)
    }

    /// Top of the open hunk's zone, in document space.
    func inlineHunkTop(_ hunk: InlineGitHunk) -> CGFloat {
        rowTop(of: hunk.anchorLine) + metrics.lineHeight * CGFloat(lineRows(hunk.anchorLine).count)
    }

    /// Header buttons, pinned to the visible right edge. The minimap covers
    /// the last `rightInset` points of the clip, so they sit left of it.
    func inlineHunkButtonRects(top: CGFloat) -> [(action: InlineHunkAction, rect: CGRect)] {
        let right = (enclosingScrollView?.contentView.bounds.maxX ?? bounds.width) - rightInset - 14
        let size: CGFloat = 30
        let count = Self.inlineHunkButtons.count
        let headerHeight = inlineGitHunk?.headerHeight(lineHeight: metrics.lineHeight) ?? metrics.lineHeight
        return Self.inlineHunkButtons.enumerated().map { index, button in
            (button.action, CGRect(x: right - CGFloat(count - index) * size, y: top, width: size,
                                   height: headerHeight))
        }
    }

    enum InlineHunkHit: Equatable {
        case button(InlineHunkAction)
        case zone
    }

    func inlineHunkHit(at point: NSPoint) -> InlineHunkHit? {
        guard let hunk = inlineGitHunk else { return nil }
        let top = inlineHunkTop(hunk)
        guard point.y >= top, point.y < top + hunk.height(lineHeight: metrics.lineHeight) else { return nil }
        if let hit = inlineHunkButtonRects(top: top).first(where: { $0.rect.contains(point) }) {
            return Self.inlineHunkActionIsEnabled(hit.action, hunk: hunk) ? .button(hit.action) : .zone
        }
        return .zone
    }

    private func drawInlineGitHunk(_ hunk: InlineGitHunk, below top: CGFloat, context: CGContext) {
        let lineHeight = metrics.lineHeight
        let headerHeight = hunk.headerHeight(lineHeight: lineHeight)
        let shift = gutterShift
        let background = Theme.editorBackground.resolved(for: effectiveAppearance)
        let foreground = Theme.editorForeground.resolved(for: effectiveAppearance)
        let digits = max(3, String(hunk.rows.map { max($0.oldNumber ?? 0, $0.newNumber ?? 0) }.max() ?? 0).count)
        let column = ceil(CGFloat(digits) * metrics.digitWidth)
        let oldRight = shift + Self.gutterPadding + column
        let newRight = oldRight + 16 + column
        // Code starts after both number columns, and scrolls under them.
        let textX = max(textOriginX, newRight + 16)
        let pinnedWidth = textX - shift
        let border = Theme.gitChange(.modified).withAlphaComponent(0.55)
        let width = bounds.width

        func tint(_ kind: InlineGitHunk.Kind) -> NSColor? {
            switch kind {
            case .context: nil
            case .removed: Theme.gitChange(.deleted).withAlphaComponent(0.26)
            case .added: Theme.gitChange(.added).withAlphaComponent(0.22)
            case .modified: Theme.gitChange(.modified).withAlphaComponent(0.22)
            }
        }
        func draw(_ text: String, at x: CGFloat, y: CGFloat, color: NSColor) {
            context.setFillColor(color.cgColor)
            context.textPosition = CGPoint(x: x, y: y + metrics.ascent)
            CTLineDraw(layout(for: text), context)
        }
        func number(_ value: Int?, right: CGFloat, y: CGFloat) {
            guard let value else { return }
            let line = layout(for: "\(value)")
            let numberWidth = CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil))
            draw("\(value)", at: right - numberWidth, y: y, color: Theme.gutterText)
        }

        // Header: file name, what is shown, and the buttons.
        context.setFillColor(NSColor.labelColor.withAlphaComponent(0.07).cgColor)
        context.fill(CGRect(x: 0, y: top, width: width, height: headerHeight))
        let name = hunk.fileName
        let nameWidth = CGFloat(CTLineGetTypographicBounds(layout(for: name), nil, nil, nil))
        let changes = hunk.total == 1 ? "1 of 1 change" : "\(hunk.index + 1) of \(hunk.total) changes"
        let headerTextY = top + InlineGitHunk.headerVerticalPadding
        draw(name, at: shift + Self.gutterPadding, y: headerTextY, color: foreground)
        draw("Git Local Changes (Working Tree) - \(changes)",
             at: shift + Self.gutterPadding + nameWidth + 12, y: headerTextY, color: Theme.gutterText)
        for button in Self.inlineHunkButtons.enumerated() {
            guard let rect = inlineHunkButtonRects(top: top).first(where: { $0.action == button.element.action })?.rect,
                  let image = NSImage(systemSymbolName: button.element.symbol, accessibilityDescription: nil)?
                    .withSymbolConfiguration(
                        NSImage.SymbolConfiguration(paletteColors: [.secondaryLabelColor])
                            .applying(NSImage.SymbolConfiguration(pointSize: 12, weight: .regular)))
            else { continue }
            let size = image.size
            let unavailable = !Self.inlineHunkActionIsEnabled(button.element.action, hunk: hunk)
            image.draw(
                in: CGRect(x: rect.midX - size.width / 2, y: rect.midY - size.height / 2, width: size.width, height: size.height),
                from: .zero, operation: .sourceOver, fraction: unavailable ? 0.3 : 1, respectFlipped: true, hints: nil)
        }

        for (index, row) in hunk.rows.enumerated() {
            let y = top + headerHeight + CGFloat(index) * lineHeight
            if let color = tint(row.kind) {
                context.setFillColor(color.cgColor)
                context.fill(CGRect(x: 0, y: y, width: width, height: lineHeight))
            }
            if row.styles.isEmpty {
                draw(row.text, at: textX, y: y, color: foreground)
            } else {
                // Colours come from the runs, not the context's fill.
                context.textPosition = CGPoint(x: textX, y: y + metrics.ascent)
                CTLineDraw(syntaxLayout(for: row.text, styles: row.styles), context)
            }
            // Scrolled sideways, text must not show through the number columns.
            context.setFillColor(background.cgColor)
            context.fill(CGRect(x: shift, y: y, width: pinnedWidth, height: lineHeight))
            if let color = tint(row.kind) {
                context.setFillColor(color.cgColor)
                context.fill(CGRect(x: shift, y: y, width: pinnedWidth, height: lineHeight))
            }
            number(row.oldNumber, right: oldRight, y: y)
            number(row.newNumber, right: newRight, y: y)
        }

        let height = hunk.height(lineHeight: lineHeight)
        context.setFillColor(border.cgColor)
        context.fill(CGRect(x: 0, y: top, width: width, height: 1))
        context.fill(CGRect(x: 0, y: top + height - 1, width: width, height: 1))
    }

    /// Width of the run icon's click target, from the gutter's left edge.
    static let runMarkerWidth: CGFloat = gutterPadding

    /// The icon of a test's run state in the left padding of the gutter:
    /// a green outlined triangle to run, a check or a cross once done.
    private func drawRunMarker(_ state: TestRunState, in context: CGContext, centerY y: CGFloat, shift: CGFloat) {
        let size: CGFloat = 10
        let x = shift + (Self.runMarkerWidth - size) / 2
        let rect = CGRect(x: x, y: y - size / 2, width: size, height: size)
        let green = NSColor.systemGreen.cgColor
        context.saveGState()
        context.setLineWidth(1.2)
        context.setLineJoin(.round)
        context.setLineCap(.round)
        switch state {
        case .ready:
            context.setStrokeColor(green)
            context.move(to: CGPoint(x: rect.minX + 1.5, y: rect.minY))
            context.addLine(to: CGPoint(x: rect.maxX, y: rect.midY))
            context.addLine(to: CGPoint(x: rect.minX + 1.5, y: rect.maxY))
            context.closePath()
            context.strokePath()
        case .running:
            context.setFillColor(Theme.accent.cgColor)
            context.fillEllipse(in: rect.insetBy(dx: 1.5, dy: 1.5))
        case .passed:
            context.setStrokeColor(green)
            context.strokeEllipse(in: rect.insetBy(dx: 0.5, dy: 0.5))
            context.move(to: CGPoint(x: rect.minX + 2.4, y: rect.midY))
            context.addLine(to: CGPoint(x: rect.midX - 0.4, y: rect.maxY - 2.6))
            context.addLine(to: CGPoint(x: rect.maxX - 2, y: rect.minY + 2.6))
            context.strokePath()
        case .failed:
            let red = NSColor.systemRed.cgColor
            context.setStrokeColor(red)
            context.strokeEllipse(in: rect.insetBy(dx: 0.5, dy: 0.5))
            context.move(to: CGPoint(x: rect.minX + 2.8, y: rect.minY + 2.8))
            context.addLine(to: CGPoint(x: rect.maxX - 2.8, y: rect.maxY - 2.8))
            context.move(to: CGPoint(x: rect.maxX - 2.8, y: rect.minY + 2.8))
            context.addLine(to: CGPoint(x: rect.minX + 2.8, y: rect.maxY - 2.8))
            context.strokePath()
        }
        context.restoreGState()
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

    private nonisolated static func leadingIndentColumns(in text: String, tabSize: Int) -> Int {
        var columns = 0
        let tabSize = max(tabSize, 1)
        for character in text {
            switch character {
            case " ": columns += 1
            case "\t": columns += tabSize - columns % tabSize
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
                let start = x0 + 2
                let end = max(start + 2, x1 - 3)
                context.setStrokeColor(Theme.whitespace.cgColor)
                context.setLineWidth(1)
                context.beginPath()
                context.move(to: CGPoint(x: start, y: midY))
                context.addLine(to: CGPoint(x: end, y: midY))
                context.addLine(to: CGPoint(x: end - 3, y: midY - 2.5))
                context.move(to: CGPoint(x: end, y: midY))
                context.addLine(to: CGPoint(x: end - 3, y: midY + 2.5))
                context.strokePath()
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
        attributed.addAttribute(.paragraphStyle, value: paragraphStyle(), range: NSRange(location: 0, length: attributed.length))
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
        attributed.addAttribute(.paragraphStyle, value: paragraphStyle(), range: NSRange(location: 0, length: attributed.length))
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

    private func paragraphStyle() -> NSParagraphStyle {
        let style = NSMutableParagraphStyle()
        style.tabStops = []
        style.defaultTabInterval = CGFloat(tabSize) * metrics.spaceWidth
        return style
    }
}

// MARK: Inline parameter hints

extension EditorView {
    /// A parameter name drawn before the argument at UTF-16 `column`.
    struct InlineHint: Equatable {
        let column: Int
        let label: String
        /// An inferred return type; a double-click inserts it into the code.
        var returnType = false
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
        if wrapActive {
            // Labels take room in their rows, so lines may wrap differently.
            syncWrapHints()
            applyDocumentMetadata(lineCount: UInt64(lineCount))
        }
        needsDisplay = true
        onLayoutChange?()
    }

    /// Tells the core how much room each label takes, so wrapped rows leave
    /// it free. Empty while wrap is off.
    func syncWrapHints() {
        guard let document else { return }
        var lines: [WrapHintLine] = []
        if wrapActive, metrics.spaceWidth > 0 {
            for (text, hints) in inlineHints {
                let length = text.utf16.count
                let slots = hints.filter { $0.column > 0 && $0.column <= length && !trailsLine($0, lineLength: length) }.map {
                    WrapHintSlot(column: UInt64($0.column),
                                 columns: UInt64(ceil(hintAdvance($0.label) / metrics.spaceWidth)))
                }
                if !slots.isEmpty { lines.append(WrapHintLine(text: text, slots: slots)) }
            }
        }
        document.setWrapHints(lines: lines)
    }

    /// X, within the whole line, where wrapped `row` starts. A row that starts
    /// at a labelled argument starts at its label.
    func rowShift(in text: String, rows: LineRows, row: Int) -> CGFloat {
        guard row > 0 else { return 0 }
        let column = rows.start(of: row)
        var x = xOffset(in: text, column: UInt64(column))
        if let hint = inlineHints[text]?.first(where: { $0.column == column }) {
            x -= hintAdvance(hint.label) - Self.hintGap
        }
        return x
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
        for hint in hints where hint.column > 0 && hint.column <= string.length && !trailsLine(hint, lineLength: string.length) {
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
        let textHeight = hintFont.ascender - hintFont.descender
        for hint in hints {
            // A label whose argument starts a wrapped row is drawn at that row's start.
            guard hint.column > 0 else { continue }
            let at = point(column: hint.column, text: text, rows: rows, top: top, originX: originX)
            let label = hintLabel(hint.label)
            let pill = hintPill(hint, at: at, lineLength: text.utf16.count)
            context.addPath(CGPath(roundedRect: pill, cornerWidth: 3, cornerHeight: 3, transform: nil))
            context.setFillColor(NSColor.secondaryLabelColor.withAlphaComponent(0.12).cgColor)
            context.fillPath()
            context.setFillColor(NSColor.secondaryLabelColor.cgColor)
            context.textPosition = CGPoint(x: pill.minX + Self.hintPadding,
                                           y: at.y + (metrics.lineHeight - textHeight) / 2 + hintFont.ascender)
            CTLineDraw(label.line, context)
        }
    }

    /// Where the label of `hint` is drawn, given the point of its column.
    func hintPill(_ hint: InlineHint, at: CGPoint, lineLength: Int) -> CGRect {
        let label = hintLabel(hint.label)
        let width = label.width + 2 * Self.hintPadding
        // Nothing follows a hint at the end of the line, and Core Text does not
        // kern the last character, so there is no reserved room: draw after the text.
        let x = trailsLine(hint, lineLength: lineLength)
            ? at.x + Self.hintGap + Self.hintPadding
            : at.x - Self.hintGap - width
        return CGRect(x: x, y: at.y + 2, width: width, height: max(0, metrics.lineHeight - 4))
    }

    /// How far past the last character of `text` its trailing return type
    /// pill reaches, so annotations drawn after the text start beyond it.
    func trailingHintExtent(text: String) -> CGFloat {
        let length = text.utf16.count
        guard let hint = inlineHints[text]?.first(where: { trailsLine($0, lineLength: length) }) else { return 0 }
        return Self.hintGap + Self.hintPadding + hintLabel(hint.label).width + 2 * Self.hintPadding
    }

    /// A return type after the last character of its line.
    func trailsLine(_ hint: InlineHint, lineLength: Int) -> Bool {
        hint.returnType && hint.column >= lineLength
    }

    /// The return type label under `point` (view coordinates), if any.
    func returnTypeHint(at point: NSPoint) -> (line: UInt64, column: Int)? {
        guard let document, lineCount > 0, !inlineHints.isEmpty, point.y >= 0 else { return nil }
        let line = self.line(atY: point.y).line
        let text = document.displayLine(line: line)
        guard let hints = inlineHints[text] else { return nil }
        let rows = lineRows(line)
        let top = rowTop(of: line)
        for hint in hints where hint.returnType && hint.column > 0 {
            let at = self.point(column: hint.column, text: text, rows: rows, top: top, originX: textOriginX)
            if hintPill(hint, at: at, lineLength: text.utf16.count).contains(point) { return (line, hint.column) }
        }
        return nil
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
        onAppKitMainThread {
            guard let document, !wrapSuspended else { return "" }
            switch document.foldState(line: line(atY: point.y).line) {
            case .plain: return ""
            case .expanded: return "Click to collapse the range."
            case .folded: return "Click to expand the range."
            }
        }
    }
}

extension EditorView.InlineGitHunk {
    /// Removed rows followed by added rows, numbered from 1.
    init(anchorLine: UInt64, oldLines: [String], newLines: [String]) {
        self.init(
            anchorLine: anchorLine,
            rows: oldLines.enumerated().map { Row(kind: .removed, oldNumber: $0.offset + 1, newNumber: nil, text: $0.element) }
                + newLines.enumerated().map { Row(kind: .added, oldNumber: nil, newNumber: $0.offset + 1, text: $0.element) }
        )
    }
}
