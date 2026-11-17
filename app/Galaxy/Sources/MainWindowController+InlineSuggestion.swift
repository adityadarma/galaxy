import AppKit

/// AI inline suggestions (ghost text). A request goes out once typing
/// pauses; the answer is shown faded at the caret. Typing the same
/// characters keeps it, Tab inserts it, Esc or any other change drops it.
extension MainWindowController {
    static let inlineSuggestionDelay: TimeInterval = 0.35
    /// Context sent around the caret, in UTF-16 units.
    static let inlineSuggestionPrefixLength: UInt64 = 4000
    static let inlineSuggestionSuffixLength: UInt64 = 1500

    /// After an edit: keep a suggestion the edit typed through, otherwise
    /// drop it and ask again once typing pauses.
    func inlineSuggestionDocumentChanged() {
        guard let document = activeTab?.doc else { return clearInlineSuggestion() }
        if editorView.ghostText != nil, inlineSuggestionRevision == document.revision() { return }
        clearInlineSuggestion()
        scheduleInlineSuggestion()
    }

    /// Typed text that matches the start of the suggestion consumes it.
    func inlineSuggestionTextTyped(_ text: String) {
        guard var ghost = editorView.ghostText, let document = activeTab?.doc else { return }
        let caret = document.primaryCursor()
        guard !text.isEmpty, !text.contains("\n"), caret.line == ghost.line,
              caret.colUtf16 == ghost.column + UInt64(text.utf16.count),
              ghost.text.hasPrefix(text) else {
            return clearInlineSuggestion()
        }
        let rest = String(ghost.text.dropFirst(text.count))
        guard !rest.isEmpty else { return clearInlineSuggestion() }
        ghost.text = rest
        ghost.column = caret.colUtf16
        editorView.ghostText = ghost
        inlineSuggestionRevision = document.revision()
    }

    /// A caret move without an edit leaves the suggestion's position.
    func inlineSuggestionCursorMoved(_ primary: CursorInfo) {
        guard let ghost = editorView.ghostText, let document = activeTab?.doc else { return }
        // Edits are handled by `inlineSuggestionDocumentChanged`.
        guard inlineSuggestionRevision == document.revision() else { return }
        if primary.line != ghost.line || primary.colUtf16 != ghost.column { clearInlineSuggestion() }
    }

    func clearInlineSuggestion() {
        inlineSuggestionWork?.cancel()
        inlineSuggestionWork = nil
        inlineSuggestionTask?.cancel()
        inlineSuggestionTask = nil
        inlineSuggestionRevision = nil
        if editorView.ghostText != nil { editorView.ghostText = nil }
    }

    /// Tab accepts and Esc dismisses. Returns `true` if the key was used.
    func handleInlineSuggestionKey(_ event: NSEvent) -> Bool {
        guard editorView.ghostText != nil, completionPopup.isHidden else { return false }
        let modifiers = event.modifierFlags.intersection([.shift, .control, .option, .command])
        switch event.keyCode {
        case 48 where modifiers.isEmpty && snippetSession == nil: // Tab
            return acceptInlineSuggestion()
        case 53: // Escape
            clearInlineSuggestion()
            return true
        default:
            return false
        }
    }

    @discardableResult
    func acceptInlineSuggestion() -> Bool {
        guard let ghost = editorView.ghostText, let document = activeTab?.doc, !editorView.isReadOnly else {
            return false
        }
        let caret = document.primaryCursor()
        guard caret.line == ghost.line, caret.colUtf16 == ghost.column else {
            clearInlineSuggestion()
            return false
        }
        let offset = document.utf16Offset(pos: TextPos(line: caret.line, colUtf16: caret.colUtf16))
        clearInlineSuggestion()
        document.commitText(text: ghost.text, replacement: Utf16Range(location: offset, length: 0))
        snippetSession = nil
        snippetDocumentKey = nil
        editorView.didChangeDocument()
        return true
    }

    private func scheduleInlineSuggestion() {
        guard AICompletionService.shared.isEnabled else { return }
        let work = DispatchWorkItem { [weak self] in self?.requestInlineSuggestion() }
        inlineSuggestionWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.inlineSuggestionDelay, execute: work)
    }

    private func requestInlineSuggestion() {
        inlineSuggestionWork = nil
        guard AICompletionService.shared.isEnabled, let tab = activeTab, tab.isDocument,
              !editorView.isReadOnly, editorView.inlineGitHunk == nil else { return }
        let document = tab.doc
        // One plain caret only: no selection, no IME composition.
        guard document.statusSnapshot().selectionCount == 1, document.selectedRange().length == 0,
              !document.hasMarkedText() else { return }
        let caret = document.primaryCursor()
        // Text after the caret would sit under the ghost text; wait for a line end.
        let line = document.displayLine(line: caret.line)
        let rest = String(utf16CodeUnits: Array(line.utf16.dropFirst(Int(caret.colUtf16))),
                          count: max(0, line.utf16.count - Int(caret.colUtf16)))
        guard rest.allSatisfy(\.isWhitespace) else { return }
        // Nothing typed on an empty file: nothing to continue.
        let offset = document.utf16Offset(pos: TextPos(line: caret.line, colUtf16: caret.colUtf16))
        let length = document.lenUtf16()
        let start = offset > Self.inlineSuggestionPrefixLength ? offset - Self.inlineSuggestionPrefixLength : 0
        let prefix = document.textInRange(range: Utf16Range(location: start, length: offset - start))
        guard !prefix.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        let suffixLength = min(length - offset, Self.inlineSuggestionSuffixLength)
        let suffix = document.textInRange(range: Utf16Range(location: offset, length: suffixLength))

        let path = document.path()
        let request = AICompletionService.Request(
            fileName: path.isEmpty ? "untitled" : (path as NSString).lastPathComponent,
            language: document.syntaxLanguage(),
            prefix: prefix, suffix: suffix)
        let revision = document.revision()
        inlineSuggestionTask?.cancel()
        inlineSuggestionTask = Task { [weak self] in
            let text = await AICompletionService.shared.suggest(request)
            guard let self, !Task.isCancelled, let text else { return }
            // Drop a late answer: the user typed, moved, or switched files.
            let now = document.primaryCursor()
            guard self.activeTab?.doc === document, document.revision() == revision,
                  now.line == caret.line, now.colUtf16 == caret.colUtf16,
                  self.editorView.inlineGitHunk == nil else { return }
            self.inlineSuggestionTask = nil
            self.inlineSuggestionRevision = revision
            self.editorView.ghostText = EditorView.GhostText(line: caret.line, column: caret.colUtf16, text: text)
        }
    }
}
