import AppKit
import CoreText

/// Text input (M1-10). Key events go through `interpretKeyEvents`, so the
/// system input method handles composition (Kotoeri, Pinyin, Korean, dead
/// keys, the emoji picker, press-and-hold accents) and calls back into the
/// `NSTextInputClient` methods below.
///
/// All ranges that cross this boundary are **document-wide UTF-16** offsets
/// (`NSRange`); the Rust core does the conversion to chars.
extension EditorView: @preconcurrency NSTextInputClient {
    // MARK: Key events

    override func keyDown(with event: NSEvent) {
        guard document != nil else { return super.keyDown(with: event) }
        LatencyProbe.shared.keyDown(event)
        beginKeyEvent()
        // Routes through the input context, so IMEs see each event once and
        // call back into `insertText`/`setMarkedText`/`doCommand(by:)`.
        interpretKeyEvents([event])
        // Input methods call `didChangeDocument` or `didChangeSelection` for
        // edits and cursor moves. This avoids extra FFI reads solely to find
        // out whether the key did useful work.
        if !endKeyEvent() {
            LatencyProbe.shared.cancelPending()
        } else {
            // Do not wait for the next display link tick to show a typed
            // character. On a 60 Hz display that wait alone costs up to
            // 16.7 ms, although drawing the small viewport is sub-millisecond
            // in the normal case. Expensive whole-document listeners are
            // still coalesced by `scheduleChangeNotification()`.
            drawInputImmediatelyIfDue()
        }
    }

    override func doCommand(by selector: Selector) {
        guard let document else { return }
        switch selector {
        case #selector(NSResponder.insertNewline(_:)): document.edit(command: .newline)
        case #selector(NSResponder.insertTab(_:)): document.edit(command: .indent)
        case #selector(NSResponder.insertBacktab(_:)): document.edit(command: .outdent)
        case #selector(NSResponder.deleteBackward(_:)): document.edit(command: .backspace)
        case #selector(NSResponder.deleteForward(_:)): document.edit(command: .deleteForward)
        default:
            guard performNavigation(selector) else { return }
        }
        didChangeDocument()
    }

    @objc func undoEdit(_ sender: Any?) {
        guard let document, document.undo() else { return }
        didChangeDocument()
    }

    @objc func redoEdit(_ sender: Any?) {
        guard let document, document.redo() else { return }
        didChangeDocument()
    }

    /// Refreshes size and drawing after the core changed, and keeps the
    /// primary caret in view.
    func didChangeDocument() {
        noteDocumentRefreshFromInput()
        requestDocumentRefresh(scrollCaret: true)
        // Whole-document work: once per frame.
        scheduleChangeNotification()
    }

    /// The caret comes from the viewport snapshot that scheduled this
    /// scroll. The editor has no horizontal scroller, so a primary caret
    /// outside that snapshot can safely use column zero until it is painted
    /// after the vertical scroll; this avoids a second FFI query.
    func scrollCaretToVisible(primary cursor: CursorInfo, lineText: String? = nil) {
        let text = lineText ?? ""
        let x = textOriginX + xOffset(in: text, column: cursor.colUtf16)
        let rect = NSRect(x: x, y: CGFloat(cursor.line) * metrics.lineHeight,
                          width: Self.caretWidth, height: metrics.lineHeight)
        scrollToVisible(rect.insetBy(dx: -40, dy: 0))
    }

    // MARK: NSTextInputClient

    func insertText(_ string: Any, replacementRange: NSRange) {
        guard let document else { return }
        let text = Self.plainText(string)
        if document.hasMarkedText() || replacementRange.location != NSNotFound {
            // Composed text or a replaced range: one undo step.
            document.commitText(text: text, replacement: Self.utf16Range(replacementRange))
        } else {
            document.edit(command: .insertText(text: text))
        }
        didChangeDocument()
    }

    func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        guard let document else { return }
        document.setMarkedText(
            text: Self.plainText(string),
            selected: Utf16Range(location: UInt64(max(0, selectedRange.location == NSNotFound ? 0 : selectedRange.location)),
                                 length: UInt64(max(0, selectedRange.length))),
            replacement: Self.utf16Range(replacementRange)
        )
        didChangeDocument()
    }

    func unmarkText() {
        document?.unmarkText()
        didChangeDocument()
    }

    func selectedRange() -> NSRange {
        guard let document else { return NSRange(location: NSNotFound, length: 0) }
        let r = document.selectedRange()
        return NSRange(location: Int(r.location), length: Int(r.length))
    }

    func markedRange() -> NSRange {
        guard let r = document?.markedRange() else { return NSRange(location: NSNotFound, length: 0) }
        return NSRange(location: Int(r.location), length: Int(r.length))
    }

    func hasMarkedText() -> Bool {
        document?.hasMarkedText() ?? false
    }

    func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? {
        guard let document, range.location != NSNotFound else { return nil }
        let total = Int(document.lenUtf16())
        let location = min(range.location, total)
        let length = min(range.length, total - location)
        actualRange?.pointee = NSRange(location: location, length: length)
        let text = document.textInRange(range: Utf16Range(location: UInt64(location), length: UInt64(length)))
        return NSAttributedString(string: text, attributes: [.font: font])
    }

    func validAttributesForMarkedText() -> [NSAttributedString.Key] {
        [.underlineStyle, .underlineColor]
    }

    /// Where the input method should put its candidate window: the rect of
    /// the first line of `range`, in screen coordinates.
    func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        guard let document, let window else { return .zero }
        let location = range.location == NSNotFound ? 0 : range.location
        let pos = document.positionAt(offset: UInt64(location))
        let text = document.displayLine(line: pos.line)
        let x = textOriginX + xOffset(in: text, column: pos.colUtf16)
        let rect = NSRect(x: x, y: CGFloat(pos.line) * metrics.lineHeight,
                          width: 1, height: metrics.lineHeight)
        actualRange?.pointee = NSRange(location: location, length: 0)
        return window.convertToScreen(convert(rect, to: nil))
    }

    func characterIndex(for point: NSPoint) -> Int {
        guard let document, let window, lineCount > 0 else { return NSNotFound }
        let local = convert(window.convertPoint(fromScreen: point), from: nil)
        return Int(document.utf16Offset(pos: textPosition(at: local)))
    }

    // MARK: Helpers

    private static func plainText(_ value: Any) -> String {
        if let attributed = value as? NSAttributedString { return attributed.string }
        return (value as? String) ?? ""
    }

    private static func utf16Range(_ range: NSRange) -> Utf16Range? {
        guard range.location != NSNotFound else { return nil }
        return Utf16Range(location: UInt64(range.location), length: UInt64(max(0, range.length)))
    }
}
