import AppKit

/// Parameter names before call arguments (`editor.inlineParameterHints`).
/// Core resolves each call like Go to Definition, so requests run off the
/// main thread, cover the visible lines plus a margin, and are dropped if
/// the document moved on before the answer came back.
extension MainWindowController {
    static let parameterHintsDelay: TimeInterval = 0.3
    /// Lines above and below the viewport included in one request.
    static let parameterHintsMargin = 60
    /// An empty answer may mean the engine is still indexing; ask again.
    static let parameterHintsRetry: TimeInterval = 3

    private static let parameterHintsQueue = DispatchQueue(
        label: "dev.adityadarma.galaxy.parameter-hints", qos: .userInitiated)

    func setInlineParameterHints(_ on: Bool) {
        guard on != inlineParameterHintsEnabled else { return }
        inlineParameterHintsEnabled = on
        parameterHintsRequest = nil
        if on {
            scheduleParameterHints(after: 0)
        } else {
            parameterHintsItem?.cancel()
            parameterHintsGeneration += 1
            editorView.setInlineHints([:])
        }
    }

    func scheduleParameterHints(after delay: TimeInterval = parameterHintsDelay) {
        guard inlineParameterHintsEnabled else { return }
        parameterHintsItem?.cancel()
        let item = DispatchWorkItem { [weak self] in
            onAppKitMainThread { self?.refreshParameterHints() }
        }
        parameterHintsItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: item)
    }

    func refreshParameterHints() {
        // With soft wrap on, the core reserves room for the labels in its rows.
        guard inlineParameterHintsEnabled, let tab = activeTab, tab.isDocument, !isDiffPaneShown,
              !tab.doc.path().isEmpty else {
            editorView.setInlineHints([:])
            return
        }
        let doc = tab.doc
        let visible = editorView.lineRange(for: editorView.visibleRect)
        guard !visible.isEmpty else { return }
        guard !parameterHintsInFlight else {
            scheduleParameterHints()
            return
        }
        let lines = max(0, visible.lowerBound - Self.parameterHintsMargin)
            ..< min(editorView.lineCount, visible.upperBound + Self.parameterHintsMargin)
        let revision = doc.revision()
        let id = ObjectIdentifier(doc)
        if let last = parameterHintsRequest, last.doc == id, last.revision == revision,
           last.lines.contains(visible.lowerBound), last.lines.contains(max(visible.lowerBound, visible.upperBound - 1)),
           !(last.empty && Date().timeIntervalSince(last.at) > Self.parameterHintsRetry) {
            return
        }
        parameterHintsRequest = (id, revision, lines, Date(), false)
        parameterHintsGeneration += 1
        parameterHintsInFlight = true
        let generation = parameterHintsGeneration
        let core = bridge.core
        let first = UInt64(lines.lowerBound), count = UInt64(lines.count)
        Self.parameterHintsQueue.async { [weak self, core, doc] in
            let answer = core.parameterHints(document: doc, firstLine: first, lineCount: count)
            var hints: [String: [EditorView.InlineHint]] = [:]
            for line in answer.lines {
                hints[line.text] = line.hints.map { EditorView.InlineHint(column: Int($0.column), label: $0.label, returnType: $0.returnType) }
            }
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.parameterHintsInFlight = false
                guard generation == self.parameterHintsGeneration,
                      self.activeTab?.doc === doc, doc.revision() == answer.revision else {
                    self.scheduleParameterHints(after: 0)
                    return
                }
                if answer.lines.isEmpty {
                    self.parameterHintsRequest?.empty = true
                    self.scheduleParameterHints(after: Self.parameterHintsRetry + 0.1)
                } else {
                    self.editorView.setInlineHints(hints)
                }
            }
        }
    }

    /// Writes the inferred return type of the function whose parameter list
    /// closes at `line`/`column`, with the `use` it needs.
    func insertReturnType(line: UInt64, column: Int) {
        guard let doc = activeTab?.doc else { return }
        let edits = bridge.core.returnTypeEdits(document: doc, line: line, column: UInt64(column))
        guard !edits.isEmpty else { return }
        _ = bridge.applyRenameEdits(document: doc, edits: edits)
        editorView.reloadDocument()
        updateEditedState()
        scheduleParameterHints(after: 0)
    }
}
