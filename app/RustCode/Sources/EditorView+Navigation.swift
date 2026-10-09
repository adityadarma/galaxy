import AppKit
import CoreText

/// Cursor movement, selection, mouse and clipboard (M1-11).
///
/// Keys arrive through `interpretKeyEvents` as standard `NSResponder`
/// selectors (`moveLeft:`, `moveWordRightAndModifySelection:` ...), so the
/// user's system key bindings (including Emacs-style Ctrl keys) just work.
extension EditorView {
    /// Pasteboard type that marks text copied as whole lines, so a later
    /// paste goes above the caret's line (VSCode behavior).
    static let linewiseType = NSPasteboard.PasteboardType("dev.rustcode.linewise-copy")

    // MARK: Key bindings

    /// `(motion, extend)`; page motions get their line count at use.
    private static let bindings: [Selector: (CursorMotion, Bool)] = {
        var table: [Selector: (CursorMotion, Bool)] = [:]
        func add(_ plain: Selector, _ shifted: Selector, _ motion: CursorMotion) {
            table[plain] = (motion, false)
            table[shifted] = (motion, true)
        }
        typealias R = NSResponder
        add(#selector(R.moveLeft(_:)), #selector(R.moveLeftAndModifySelection(_:)), .left)
        add(#selector(R.moveBackward(_:)), #selector(R.moveBackwardAndModifySelection(_:)), .left)
        add(#selector(R.moveRight(_:)), #selector(R.moveRightAndModifySelection(_:)), .right)
        add(#selector(R.moveForward(_:)), #selector(R.moveForwardAndModifySelection(_:)), .right)
        add(#selector(R.moveUp(_:)), #selector(R.moveUpAndModifySelection(_:)), .up)
        add(#selector(R.moveDown(_:)), #selector(R.moveDownAndModifySelection(_:)), .down)
        add(#selector(R.moveWordLeft(_:)), #selector(R.moveWordLeftAndModifySelection(_:)), .wordLeft)
        add(#selector(R.moveWordBackward(_:)), #selector(R.moveWordBackwardAndModifySelection(_:)), .wordLeft)
        add(#selector(R.moveWordRight(_:)), #selector(R.moveWordRightAndModifySelection(_:)), .wordRight)
        add(#selector(R.moveWordForward(_:)), #selector(R.moveWordForwardAndModifySelection(_:)), .wordRight)
        add(#selector(R.moveToBeginningOfLine(_:)), #selector(R.moveToBeginningOfLineAndModifySelection(_:)), .lineStart)
        add(#selector(R.moveToLeftEndOfLine(_:)), #selector(R.moveToLeftEndOfLineAndModifySelection(_:)), .lineStart)
        add(#selector(R.moveToBeginningOfParagraph(_:)), #selector(R.moveToBeginningOfParagraphAndModifySelection(_:)), .lineStart)
        add(#selector(R.moveToEndOfLine(_:)), #selector(R.moveToEndOfLineAndModifySelection(_:)), .lineEnd)
        add(#selector(R.moveToRightEndOfLine(_:)), #selector(R.moveToRightEndOfLineAndModifySelection(_:)), .lineEnd)
        add(#selector(R.moveToEndOfParagraph(_:)), #selector(R.moveToEndOfParagraphAndModifySelection(_:)), .lineEnd)
        add(#selector(R.moveToBeginningOfDocument(_:)), #selector(R.moveToBeginningOfDocumentAndModifySelection(_:)), .docStart)
        add(#selector(R.moveToEndOfDocument(_:)), #selector(R.moveToEndOfDocumentAndModifySelection(_:)), .docEnd)
        add(#selector(R.pageUp(_:)), #selector(R.pageUpAndModifySelection(_:)), .pageUp(lines: 0))
        add(#selector(R.pageDown(_:)), #selector(R.pageDownAndModifySelection(_:)), .pageDown(lines: 0))
        // Home/End and Page keys are "scroll" commands by default on macOS;
        // in an editor they move the caret (Home/End act on the line).
        table[#selector(R.scrollToBeginningOfDocument(_:))] = (.lineStart, false)
        table[#selector(R.scrollToEndOfDocument(_:))] = (.lineEnd, false)
        table[#selector(R.scrollPageUp(_:))] = (.pageUp(lines: 0), false)
        table[#selector(R.scrollPageDown(_:))] = (.pageDown(lines: 0), false)
        return table
    }()

    /// Lines in one page, keeping one line of overlap.
    var pageLines: UInt32 {
        UInt32(max(1, Int(visibleRect.height / metrics.lineHeight) - 1))
    }

    /// Runs a navigation selector. Returns `false` if `selector` is not one.
    func performNavigation(_ selector: Selector) -> Bool {
        guard let document else { return false }
        if selector == #selector(NSResponder.selectAll(_:)) {
            document.selectAll()
            return true
        }
        guard let (motion, extend) = Self.bindings[selector] else { return false }
        let resolved: CursorMotion = switch motion {
        case .pageUp: .pageUp(lines: pageLines)
        case .pageDown: .pageDown(lines: pageLines)
        default: motion
        }
        document.moveCursors(motion: resolved, extend: extend)
        return true
    }

    override func selectAll(_ sender: Any?) {
        document?.selectAll()
        reloadDocument()
    }

    // MARK: Mouse

    /// Document position under `point` (view coordinates). Points above,
    /// below or beside the text clamp to the nearest valid position.
    func textPosition(at point: NSPoint) -> TextPos {
        guard let document, lineCount > 0 else { return TextPos(line: 0, colUtf16: 0) }
        let row = point.y / metrics.lineHeight
        let clamped = row.isFinite ? min(max(row, 0), CGFloat(lineCount - 1)) : 0
        let line = Int(floor(clamped))
        let text = document.displayLine(line: UInt64(line))
        let index = text.isEmpty ? 0 : max(0, CTLineGetStringIndexForPosition(
            layout(for: text), CGPoint(x: point.x - textOriginX, y: 0)))
        return TextPos(line: UInt64(line), colUtf16: UInt64(index))
    }

    override func mouseDown(with event: NSEvent) {
        guard let document else { return super.mouseDown(with: event) }
        window?.makeFirstResponder(self)
        if document.hasMarkedText() { inputContext?.discardMarkedText() }
        let local = convert(event.locationInWindow, from: nil)
        let pos = textPosition(at: local)
        let flags = event.modifierFlags
        columnAnchor = nil
        if flags.contains(.option), flags.contains(.shift) {
            // Opt+Shift+drag: column (box) selection.
            let anchor = columnPosition(at: local)
            columnAnchor = anchor
            document.columnSelect(anchor: anchor, head: anchor)
            reloadDocument()
            return
        }
        switch event.clickCount {
        case 2: document.selectWordAt(pos: pos)
        case 3...: document.selectLineAt(pos: pos)
        default:
            if flags.contains(.shift) {
                document.extendSelectionTo(pos: pos)
            } else if flags.contains(.option) {
                document.addCaret(pos: pos)
            } else {
                document.setCaret(pos: pos)
            }
        }
        reloadDocument()
    }

    override func mouseDragged(with event: NSEvent) {
        guard let document else { return }
        autoscroll(with: event)
        let local = convert(event.locationInWindow, from: nil)
        if let anchor = columnAnchor {
            document.columnSelect(anchor: anchor, head: columnPosition(at: local))
        } else {
            document.extendSelectionTo(pos: textPosition(at: local))
        }
        reloadDocument()
    }

    override func mouseUp(with event: NSEvent) {
        columnAnchor = nil
    }

    /// Like `textPosition(at:)`, but a point to the right of a short line
    /// keeps its column (counted in space widths), so a box dragged past
    /// the end of a line still spans the same columns on longer lines.
    func columnPosition(at point: NSPoint) -> TextPos {
        guard let document, lineCount > 0 else { return TextPos(line: 0, colUtf16: 0) }
        var pos = textPosition(at: point)
        let text = document.displayLine(line: pos.line)
        let lineWidth = text.isEmpty ? 0 : CGFloat(CTLineGetTypographicBounds(layout(for: text), nil, nil, nil))
        let beyond = point.x - textOriginX - lineWidth
        if beyond > metrics.spaceWidth / 2, metrics.spaceWidth > 0 {
            pos.colUtf16 = UInt64(text.utf16.count) + UInt64((beyond / metrics.spaceWidth).rounded())
        }
        return pos
    }

    override func resetCursorRects() {
        addCursorRect(visibleRect, cursor: .iBeam)
    }

    // MARK: Multi-cursor

    @objc func selectNextOccurrence(_ sender: Any?) {
        document?.selectNextOccurrence()
        didChangeSelection()
    }

    @objc func selectAllOccurrences(_ sender: Any?) {
        document?.selectAllOccurrences()
        didChangeSelection()
    }

    @objc func addCursorAbove(_ sender: Any?) {
        document?.addCursorVertical(above: true)
        didChangeSelection()
    }

    @objc func addCursorBelow(_ sender: Any?) {
        document?.addCursorVertical(above: false)
        didChangeSelection()
    }

    /// Scrolls `line` to the middle of the view if it is not on screen
    /// (Go to Line, back/forward), as VSCode does. A visible line stays put.
    func revealLine(_ line: Int) {
        guard let scrollView = enclosingScrollView else { return }
        let clip = scrollView.contentView
        let height = metrics.lineHeight
        let top = CGFloat(line) * height
        let visible = clip.bounds
        if top >= visible.minY, top + height <= visible.maxY { return }
        let inset = scrollView.contentInsets.top
        let maxY = max(-inset, frame.height - visible.height)
        let y = min(max(top + height / 2 - visible.height / 2, -inset), maxY)
        clip.scroll(to: NSPoint(x: visible.minX, y: y))
        scrollView.reflectScrolledClipView(clip)
    }

    /// Selection-only change: redraw and keep the primary caret in view.
    func didChangeSelection() {
        noteDocumentRefreshFromInput()
        requestDocumentRefresh(scrollCaret: true)
        // VoiceOver reads the new caret line or selection.
        NSAccessibility.post(element: self, notification: .selectedTextChanged)
    }

    // MARK: Clipboard

    @objc func copy(_ sender: Any?) {
        guard let document else { return }
        write(document.copy())
    }

    @objc func cut(_ sender: Any?) {
        guard let document else { return }
        write(document.cut())
        didChangeDocument()
    }

    @objc func paste(_ sender: Any?) {
        guard let document, let text = pasteboard.string(forType: .string) else { return }
        // The flag only counts if the text is still what we copied.
        let linewise = pasteboard.string(forType: Self.linewiseType) == text
        document.pasteClipboard(text: text, linewise: linewise)
        didChangeDocument()
    }

    private func write(_ copied: ClipboardText) {
        guard !copied.text.isEmpty else { return }
        pasteboard.clearContents()
        pasteboard.setString(copied.text, forType: .string)
        if copied.linewise { pasteboard.setString(copied.text, forType: Self.linewiseType) }
    }
}

extension EditorView: NSMenuItemValidation {
    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        switch item.action {
        case #selector(undoEdit(_:)): document?.canUndo() ?? false
        case #selector(redoEdit(_:)): document?.canRedo() ?? false
        case #selector(paste(_:)): document != nil && pasteboard.string(forType: .string) != nil
        case #selector(copy(_:)), #selector(cut(_:)), #selector(selectAll(_:)),
             #selector(selectNextOccurrence(_:)), #selector(selectAllOccurrences(_:)),
             #selector(addCursorAbove(_:)), #selector(addCursorBelow(_:)): document != nil
        default: true
        }
    }
}

// MARK: Accessibility (the basis of M1-17, and what UI tests read)

extension EditorView {
    /// Text above this size is not exposed, so a huge file cannot stall
    /// an assistive client that asks for the whole value.
    static let accessibilityTextLimit = 1_000_000

    override func accessibilityValue() -> Any? {
        guard let document else { return "" }
        let total = Int(document.lenUtf16())
        let length = min(total, Self.accessibilityTextLimit)
        return document.textInRange(range: Utf16Range(location: 0, length: UInt64(length)))
    }

    /// Matches `accessibilityValue`: clients index into that string, so a
    /// larger count would point past its end on huge files.
    override func accessibilityNumberOfCharacters() -> Int {
        min(Int(document?.lenUtf16() ?? 0), Self.accessibilityTextLimit)
    }

    override func accessibilitySelectedTextRange() -> NSRange {
        selectedRange()
    }

    override func accessibilitySelectedText() -> String? {
        guard let document else { return nil }
        let r = document.selectedRange()
        return document.textInRange(range: r)
    }

    override func accessibilityInsertionPointLineNumber() -> Int {
        guard let document else { return 0 }
        return Int(document.viewport(firstLine: 0, count: 0).primary.line)
    }
}
