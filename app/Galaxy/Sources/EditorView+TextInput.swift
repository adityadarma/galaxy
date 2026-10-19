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
        if onCompletionKey?(event) == true { return }
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
        if isReadOnly {
            guard performNavigation(selector) else { return }
            return didChangeSelection()
        }
        switch selector {
        case #selector(NSResponder.insertNewline(_:)): document.edit(command: .newline)
        case #selector(NSResponder.insertTab(_:)): document.edit(command: .indent)
        case #selector(NSResponder.insertBacktab(_:)): document.edit(command: .outdent)
        case #selector(NSResponder.deleteBackward(_:)): document.edit(command: .backspace)
        case #selector(NSResponder.deleteForward(_:)): document.edit(command: .deleteForward)
        default:
            // Cursor moves change no text, so skip the whole-document
            // listeners (modified flag, find count) that edits trigger.
            guard performNavigation(selector) else { return }
            return didChangeSelection()
        }
        didChangeDocument()
    }

    @objc func undoEdit(_ sender: Any?) {
        guard !isReadOnly, let document, document.undo() else { return }
        onSnippetInvalidated?()
        didChangeDocument()
    }

    @objc func redoEdit(_ sender: Any?) {
        guard !isReadOnly, let document, document.redo() else { return }
        onSnippetInvalidated?()
        didChangeDocument()
    }

    @objc func toggleComment(_ sender: Any?) {
        guard !isReadOnly, let document else { return }
        document.edit(command: .toggleComment)
        didChangeDocument()
    }

    // MARK: Line operations (Edit and Selection menus)

    private func runEdit(_ command: EditCommand) {
        guard !isReadOnly, let document else { return }
        document.edit(command: command)
        onSnippetInvalidated?()
        didChangeDocument()
    }

    @objc func moveLinesUp(_ sender: Any?) { runEdit(.moveLinesUp) }
    @objc func moveLinesDown(_ sender: Any?) { runEdit(.moveLinesDown) }
    @objc func copyLinesUp(_ sender: Any?) { runEdit(.copyLinesUp) }
    @objc func copyLinesDown(_ sender: Any?) { runEdit(.copyLinesDown) }
    @objc func deleteLines(_ sender: Any?) { runEdit(.deleteLines) }
    @objc func joinLines(_ sender: Any?) { runEdit(.joinLines) }
    @objc func transformToUppercase(_ sender: Any?) { runEdit(.upperCase) }
    @objc func transformToLowercase(_ sender: Any?) { runEdit(.lowerCase) }
    @objc func trimTrailingWhitespace(_ sender: Any?) { runEdit(.trimTrailingWhitespace) }
    @objc func sortLinesAscending(_ sender: Any?) { runEdit(.sortLinesAscending) }
    @objc func sortLinesDescending(_ sender: Any?) { runEdit(.sortLinesDescending) }

    /// Refreshes size and drawing after the core changed, and keeps the
    /// primary caret in view.
    func didChangeDocument() {
        noteDocumentRefreshFromInput()
        requestDocumentRefresh(scrollCaret: true)
        // Whole-document work: once per frame.
        scheduleChangeNotification()
    }

    /// The caret comes from the viewport snapshot that scheduled this
    /// scroll, with its line's text. Without it the line is read, but only
    /// when the text can scroll sideways and the caret is not at column 0.
    func scrollCaretToVisible(primary cursor: CursorInfo, lineText: String? = nil) {
        var text = lineText ?? ""
        if lineText == nil, cursor.colUtf16 > 0, !wrapActive {
            text = document?.displayLine(line: cursor.line) ?? ""
        }
        let p = point(column: Int(cursor.colUtf16), text: text, rows: lineRows(cursor.line),
                      top: rowTop(of: cursor.line), originX: textOriginX)
        let rect = NSRect(x: p.x, y: p.y, width: Self.caretWidth, height: metrics.lineHeight)
        // Keep the caret clear of the line numbers on the left (they stay at
        // the edge) and of the minimap on the right.
        let margin: CGFloat = 40
        let left = gutterWidth + margin
        let right = margin + rightInset
        scrollToVisible(NSRect(x: rect.minX - left, y: rect.minY,
                               width: rect.width + left + right, height: rect.height))
    }

    // MARK: NSTextInputClient

    func insertText(_ string: Any, replacementRange: NSRange) {
        guard !isReadOnly, let document else { return }
        let text = Self.plainText(string)
        if document.hasMarkedText() || replacementRange.location != NSNotFound {
            // Composed text or a replaced range: one undo step.
            document.commitText(text: text, replacement: Self.utf16Range(replacementRange))
        } else {
            document.edit(command: .insertText(text: text))
        }
        didChangeDocument()
        onTextInput?(text)
    }

    func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        guard !isReadOnly, let document else { return }
        document.setMarkedText(
            text: Self.plainText(string),
            selected: Utf16Range(location: UInt64(max(0, selectedRange.location == NSNotFound ? 0 : selectedRange.location)),
                                 length: UInt64(max(0, selectedRange.length))),
            replacement: Self.utf16Range(replacementRange)
        )
        didChangeDocument()
    }

    func unmarkText() {
        guard !isReadOnly else { return }
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
        let rect = self.rect(forLine: pos.line, column: pos.colUtf16)
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
