import AppKit
import os

extension MainWindowController {
    /// Ctrl+Space: ask for suggestions now, even with nothing typed and
    /// after Escape closed the list.
    @objc func triggerSuggest(_ sender: Any?) {
        guard activeTab?.isDocument == true, !editorView.isReadOnly else { NSSound.beep(); return }
        completionDismissed = false
        requestCompletionIfNeeded(force: true)
        lastSuggestRequest = .now
        suggestRetryUntil = Date().addingTimeInterval(10)
    }

    /// Asks again while a Ctrl+Space list is still empty, at most every
    /// 0.6 s for 10 s, and only while the caret stays where it was.
    private func retrySuggestIfWaiting(_ document: DocumentHandle) {
        guard let until = suggestRetryUntil else { return }
        let now = Date()
        guard now < until else { suggestRetryUntil = nil; return }
        guard now.timeIntervalSince(lastSuggestRequest) > 0.6 else { return }
        lastSuggestRequest = now
        bridge.requestCompletion(document: document, line: completionPosition.line,
                                 character: completionPosition.colUtf16)
    }

    private func isPHPFile(_ path: String) -> Bool {
        let name = (path as NSString).lastPathComponent.lowercased()
        return name.hasSuffix(".php")
    }

    private func isGoFile(_ path: String) -> Bool {
        (path as NSString).lastPathComponent.lowercased().hasSuffix(".go")
    }

    func requestCompletionIfNeeded(force: Bool = false, debounced: Bool = false) {
        completionRequestItem?.cancel()
        if !force, !debounced {
            let item = DispatchWorkItem { [weak self] in
                self?.requestCompletionIfNeeded(debounced: true)
            }
            completionRequestItem = item
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.12, execute: item)
            return
        }
        guard let document = activeTab?.doc else { return }
        // Completion owns the space below the caret. Cancel the old hover so
        // its asynchronous refresh cannot cover the suggestion list.
        dismissHover()
        if !force { suggestRetryUntil = nil }
        let position = document.primaryCursor()
        completionPosition = TextPos(line: position.line, colUtf16: position.colUtf16)
        let line = document.displayLine(line: position.line)
        let column = Int(position.colUtf16)
        let beforePrefix = String(line.prefix(max(0, min(column, line.count))))
        // Only the identifier run directly before the caret: `$o->` has none.
        let prefix = String(beforePrefix.reversed().prefix { $0.isLetter || $0.isNumber || $0 == "_" }.reversed())
        completionQuery = prefix
        var lspTrigger = beforePrefix.hasSuffix(".") || beforePrefix.hasSuffix("::") || beforePrefix.hasSuffix("->")
        // Framework names (Laravel, CodeIgniter, Symfony) live in strings; Blade also has directives and component tags.
        if !lspTrigger, isPHPFile(document.path()) {
            lspTrigger = beforePrefix.contains("'") || beforePrefix.contains("\"")
                || beforePrefix.contains("@") || beforePrefix.contains("<x-") || beforePrefix.contains("<livewire:")
        }
        // Go import paths live in strings: `import "net/ht`.
        if !lspTrigger, isGoFile(document.path()) {
            lspTrigger = beforePrefix.contains("\"")
        }
        guard force || !prefix.isEmpty || lspTrigger else {
            snippetCompletionItems = []
            completionPopup.isHidden = true
            return
        }
        if isSettingsDocument(document) {
            localCompletionItems = GalaxySettings.completionItems()
            refreshCompletion()
            return
        }
        localCompletionItems = nil
        // Snippet metadata crosses the same Rust document handle as native
        // completion. Read it serially on that queue, never on AppKit's main
        // thread while the user is typing.
        snippetCompletionItems = []
        if !prefix.isEmpty {
            let requestedPosition = completionPosition
            bridge.readSnippetCompletions(document: document, prefix: prefix) { [weak self] items in
                guard let self, self.completionPosition == requestedPosition else { return }
                self.snippetCompletionItems = items
                self.refreshCompletion()
            }
        }
        let requestStarted = DispatchTime.now().uptimeNanoseconds
        bridge.requestCompletion(document: document, line: position.line, character: position.colUtf16)
        let requestElapsed = Double(DispatchTime.now().uptimeNanoseconds - requestStarted) / 1_000_000
        Log.ui.debug("completion_dispatch_ms=\(requestElapsed, privacy: .public)")
        refreshCompletion()
    }

    func refreshCompletion() {
        guard let document = activeTab?.doc else { return }
        guard !completionDismissed else { return }
        let position = document.primaryCursor()
        guard position.line == completionPosition.line,
              position.colUtf16 == completionPosition.colUtf16 else {
            completionPopup.isHidden = true
            suggestRetryUntil = nil
            return
        }
        let requestedPosition = completionPosition
        bridge.readCompletions(document: document) { [weak self] lspItems in
            guard let self, self.completionPosition == requestedPosition else { return }
            self.applyCompletion(lspItems, document: document)
        }
    }

    /// Applies results already read on the completion queue.
    private func applyCompletion(_ lspItems: [CompletionInfo], document: DocumentHandle) {
        guard !completionDismissed else { return }
        let position = document.primaryCursor()
        guard position.line == completionPosition.line,
              position.colUtf16 == completionPosition.colUtf16 else {
            completionPopup.isHidden = true
            suggestRetryUntil = nil
            return
        }
        // A name that replaces more than the word before the caret (a dotted
        // config key, a view name): the query is everything it replaces, and
        // snippets do not apply.
        let replaceFrom = lspItems.compactMap { $0.replaceFrom }.first
        if let replaceFrom, replaceFrom <= completionPosition.colUtf16 {
            let line = document.displayLine(line: completionPosition.line)
            let utf16 = Array(line.utf16)
            let end = min(Int(completionPosition.colUtf16), utf16.count)
            let start = min(Int(replaceFrom), end)
            completionQuery = String(utf16CodeUnits: Array(utf16[start..<end]), count: end - start)
            snippetCompletionItems = []
        }
        let items = localCompletionItems ?? (lspItems + snippetCompletionItems)
        guard !items.isEmpty else {
            completionPopup.isHidden = true
            retrySuggestIfWaiting(document)
            return
        }
        suggestRetryUntil = nil
        let rankingStarted = DispatchTime.now().uptimeNanoseconds
        completionPopup.update(items, query: completionQuery)
        let rankingElapsed = Double(DispatchTime.now().uptimeNanoseconds - rankingStarted) / 1_000_000
        Log.ui.debug("completion_popup_ms=\(rankingElapsed, privacy: .public) items=\(items.count, privacy: .public)")
        if completionPopup.isHidden { return }
        let prefixColumn = completionPosition.colUtf16 >= UInt64(completionQuery.utf16.count)
            ? completionPosition.colUtf16 - UInt64(completionQuery.utf16.count) : 0
        completionPopup.frame = popupFrame(size: completionPopup.preferredSize, line: completionPosition.line, column: prefixColumn)
    }

    /// Frame in `detailView` for a popup under the caret line (above it when
    /// there is no room), clamped inside the editor area. The editor is
    /// flipped and `detailView` is not, so the rect must be converted.
    private func popupFrame(size: NSSize, line: UInt64, column: UInt64) -> NSRect {
        let caret = detailView.convert(editorView.rect(forLine: line, column: column), from: editorView)
        let bounds = detailView.bounds
        var y = caret.minY - 4 - size.height
        if y < bounds.minY { y = caret.maxY + 4 }
        let x = min(max(caret.minX, bounds.minX), max(bounds.minX, bounds.maxX - size.width))
        return NSRect(x: x, y: y, width: size.width, height: size.height)
    }

    func closeCompletion() {
        completionRequestItem?.cancel()
        completionRequestItem = nil
        completionPopup.isHidden = true
        completionDismissed = true
        suggestRetryUntil = nil
        snippetCompletionItems = []
        dismissHover()
    }

    func isSettingsDocument(_ document: DocumentHandle) -> Bool {
        let path = Self.fileKey(document.path())
        let global = Self.fileKey(globalSettingsURL.path)
        let workspace = self.workspace.map { Self.fileKey(GalaxySettings.workspaceURL(root: $0.root()).path) }
        return path == global || path == workspace
    }

    func refreshSignatureHelp() {
        // The completion list owns the caret area. Signature help is useful
        // while typing arguments, but must not cover a suggestion selection.
        guard completionPopup.isHidden else {
            signaturePopup.isHidden = true
            lastSignatureRequest = nil
            return
        }
        guard let document = activeTab?.doc else {
            lastSignatureRequest = nil
            signaturePopup.isHidden = true
            return
        }
        // Only after typing in this document (see `signatureHelpDocument`).
        guard signatureHelpDocument == ObjectIdentifier(document) else {
            lastSignatureRequest = nil
            signaturePopup.isHidden = true
            return
        }
        let position = document.primaryCursor()
        let request = (
            path: document.path(),
            revision: document.revision(),
            line: position.line,
            column: position.colUtf16
        )
        guard lastSignatureRequest?.path != request.path
            || lastSignatureRequest?.revision != request.revision
            || lastSignatureRequest?.line != request.line
            || lastSignatureRequest?.column != request.column
        else { return }
        lastSignatureRequest = request
        bridge.requestSignatureHelp(document: document, line: position.line, character: position.colUtf16)
        guard let signature = bridge.signatureHelp(document: document) else {
            // The caret left the call: stay quiet until the next keystroke.
            signaturePopup.isHidden = true
            signatureHelpDocument = nil
            return
        }
        signaturePopup.show(signature)
        signaturePopup.isHidden = false
        signaturePopup.frame = popupFrame(size: NSSize(width: 520, height: signature.documentation == nil ? 48 : 78),
                                           line: position.line, column: position.colUtf16)
    }

    func handleCompletionKey(_ event: NSEvent) -> Bool {
        if event.keyCode == 53, !signaturePopup.isHidden, completionPopup.isHidden, snippetSession == nil {
            // Escape closes the signature help, as in other editors.
            signaturePopup.isHidden = true
            signatureHelpDocument = nil
            return true
        }
        if completionPopup.isHidden, let session = snippetSession {
            switch event.keyCode {
            case 48: // Tab
                return moveSnippetStop(direction: event.modifierFlags.contains(.shift) ? -1 : 1)
            case 53: // Escape
                snippetSession = nil
                return true
            default: break
            }
            _ = session
        }
        guard !completionPopup.isHidden else { return false }
        switch event.keyCode {
        case 125: completionPopup.move(1); return true
        case 126: completionPopup.move(-1); return true
        case 36, 48, 76: completionPopup.accept(); return true
        case 53: completionPopup.cancel(); return true
        default: return false
        }
    }

    func acceptCompletion(_ item: CompletionInfo) {
        guard let document = activeTab?.doc else { return }
        let start = completionPosition.colUtf16 >= UInt64(completionQuery.utf16.count) ? completionPosition.colUtf16 - UInt64(completionQuery.utf16.count) : 0
        var offset = document.utf16Offset(pos: TextPos(line: completionPosition.line, colUtf16: start))
        // Edits elsewhere (a PHP `use` import) go in first, bottom to top, so
        // earlier positions stay valid. Text before the caret shifts it.
        let extraEdits = item.additionalEdits
            .map { (offset: document.utf16Offset(pos: TextPos(line: $0.line, colUtf16: $0.column)), text: $0.text) }
            .sorted { $0.offset > $1.offset }
        for edit in extraEdits {
            document.commitText(text: edit.text, replacement: Utf16Range(location: edit.offset, length: 0))
            if edit.offset <= offset { offset += UInt64(edit.text.utf16.count) }
        }
        let text: String
        var firstStopIndex: UInt32?
        var firstStops: [SnippetStopInfo] = []
        if item.isSnippet {
            let line = document.displayLine(line: completionPosition.line)
            let indent = String(line.prefix { $0 == " " || $0 == "\t" })
            let settings = document.indentSettings()
            let unit = settings.useTabs ? "\t" : String(repeating: " ", count: Int(settings.tabSize))
            let selectedText = document.textInRange(range: document.selectedRange())
            let year = String(Calendar.current.component(.year, from: Date()))
            let expansion = bridge.expandedSnippet(body: item.insertText, baseIndent: indent, indentUnit: unit,
                                                   filename: (document.path() as NSString).lastPathComponent,
                                                   selectedText: selectedText, currentYear: year)
            text = expansion.text
            firstStopIndex = expansion.stops.first(where: { $0.index != 0 })?.index
                ?? expansion.stops.first?.index
            if let firstStopIndex {
                firstStops = expansion.stops.filter { $0.index == firstStopIndex }
            }
            snippetSession = makeSnippetSession(expansion.stops, insertionOffset: offset,
                                                documentLength: document.lenUtf16() + UInt64(text.utf16.count) - UInt64(completionQuery.utf16.count))
            snippetDocumentKey = Self.fileKey(document.path())
        } else {
            text = item.insertText
        }
        document.commitText(text: text, replacement: Utf16Range(location: offset, length: UInt64(completionQuery.utf16.count)))
        if firstStopIndex != nil {
            let selections = firstStops.map { stop in
                let absolute = Utf16Range(location: offset + stop.range.location, length: stop.range.length)
                return TextSelection(anchor: document.positionAt(offset: absolute.location),
                                     head: document.positionAt(offset: absolute.location + absolute.length))
            }
            document.setSelections(selections: selections, primary: 0)
        }
        closeCompletion()
        bridge.pollAnalysis()
        editorView.didChangeDocument()
    }

    private func makeSnippetSession(_ stops: [SnippetStopInfo], insertionOffset: UInt64,
                                    documentLength: UInt64) -> SnippetSession? {
        let groups = Dictionary(grouping: stops, by: \ .index)
            .sorted { lhs, rhs in
                if lhs.key == 0 { return false }
                if rhs.key == 0 { return true }
                return lhs.key < rhs.key
            }
        let ranges = groups.map { index, values in
            values.map { stop in
                SnippetSession.Stop(index: index,
                                    location: insertionOffset + stop.range.location,
                                    length: stop.range.length)
            }
        }
        guard !ranges.isEmpty else { return nil }
        return SnippetSession(stops: ranges, documentLength: documentLength)
    }

    private func moveSnippetStop(direction: Int) -> Bool {
        guard var session = snippetSession,
              let document = activeTab?.doc,
              snippetDocumentKey == Self.fileKey(document.path()) else {
            snippetSession = nil
            snippetDocumentKey = nil
            return false
        }
        session.noteDocumentChange(length: document.lenUtf16())
        guard let stop = session.move(direction) else {
            snippetSession = nil
            return true
        }
        let selections = stop.map { range in
            TextSelection(anchor: document.positionAt(offset: range.location),
                          head: document.positionAt(offset: range.location + range.length))
        }
        document.setSelections(selections: selections, primary: 0)
        snippetSession = session
        editorView.didChangeSelection()
        return true
    }
}
