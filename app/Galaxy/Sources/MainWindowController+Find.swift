import AppKit
import os

extension MainWindowController {
    // MARK: Find and replace (M1-13)

    /// Does not build the bar: this is checked on every edit.
    var isFindVisible: Bool { loadedFindBar != nil && !findBarSlot.isHidden }

    var isCountingFind: Bool { countJob != nil }

    func wireFindBar() {
        findBar.onPatternChange = { [weak self] in self?.applyFind(select: true) }
        findBar.onNext = { [weak self] in self?.findNext(nil) }
        findBar.onPrevious = { [weak self] in self?.findPrevious(nil) }
        findBar.onSelectAll = { [weak self] in self?.selectAllMatches() }
        findBar.onReplace = { [weak self] in self?.replaceCurrent() }
        findBar.onReplaceAll = { [weak self] in self?.replaceAll() }
        findBar.onClose = { [weak self] in self?.closeFind(nil) }
    }

    @objc func showFind(_ sender: Any?) { presentFind(replace: false) }

    @objc func showFindAndReplace(_ sender: Any?) { presentFind(replace: true) }

    /// ⌘E: searches for the selected text without moving focus.
    @objc func useSelectionForFind(_ sender: Any?) {
        guard editorView.document != nil, let seed = selectionSeed() else { return }
        findBar.setPattern(findBar.config.regex ? Self.escapeRegex(seed) : seed)
    }

    private func presentFind(replace: Bool) {
        guard editorView.document != nil else { return }
        if !isFindVisible { findOrigin = activeTab.map(location(of:)) }
        if let seed = selectionSeed() {
            findBar.findField.stringValue = findBar.config.regex ? Self.escapeRegex(seed) : seed
        }
        if replace { findBar.setReplaceVisible(true) }
        setFindVisible(true)
        applyFind(select: true)
        findBar.focusFindField()
    }

    func setFindVisible(_ visible: Bool) {
        findBarSlot.isHidden = !visible
        findBarHeight.constant = visible ? findBar.preferredHeight : 0
    }

    @objc func closeFind(_ sender: Any?) {
        guard isFindVisible else { return }
        recordFindOrigin()
        setFindVisible(false)
        countJob?.cancel()
        countJob = nil
        editorView.document?.clearFind()
        editorView.didChangeSelection()
        window?.makeFirstResponder(editorView)
    }

    @objc func findNext(_ sender: Any?) {
        guard let doc = editorView.document else { return }
        if !isFindVisible || findBar.pattern.isEmpty { return presentFind(replace: false) }
        recordFindOrigin()
        recordingJump { _ = doc.findNext() }
        finishFindMove()
    }

    @objc func findPrevious(_ sender: Any?) {
        guard let doc = editorView.document else { return }
        if !isFindVisible || findBar.pattern.isEmpty { return presentFind(replace: false) }
        recordFindOrigin()
        recordingJump { _ = doc.findPrevious() }
        finishFindMove()
    }

    /// Re-reads pattern and options from the bar. `select` jumps to the
    /// nearest match, as typing in the field does.
    func applyFind(select: Bool) {
        guard let doc = editorView.document else { return }
        _ = doc.setFind(pattern: findBar.pattern, config: findBar.config)
        if select { _ = doc.findNearest() }
        finishFindMove()
    }

    private func finishFindMove() {
        updateFindStatus()
        editorView.didChangeSelection()
    }

    /// Shows the match count. Small documents are counted right here; big
    /// ones on a background thread from a snapshot, and the result is
    /// dropped if the text or the search changed in the meantime.
    func updateFindStatus() {
        countJob?.cancel()
        countJob = nil
        guard let doc = editorView.document else { return }
        let status = doc.findStatus()
        findBar.show(status: status)
        guard !status.counted, let job = doc.startFindCount() else { return }
        countJob = job
        let revision = job.revision()
        Task.detached(priority: .userInitiated) { [weak self] in
            let result = job.run()
            await MainActor.run {
                guard let self, self.countJob === job else { return }
                self.countJob = nil
                guard let result, self.editorView.document === doc,
                      doc.revision() == revision, self.isFindVisible else { return }
                self.findBar.show(status: result)
                self.onFindCountFinished?()
            }
        }
    }

    /// Keeps the "3 of 12" label right while the text changes.
    func refreshFindStatus() {
        guard isFindVisible else { return }
        updateFindStatus()
    }

    private func selectAllMatches() {
        guard let doc = editorView.document, doc.selectAllMatches() else { return }
        updateFindStatus()
        editorView.didChangeSelection()
        window?.makeFirstResponder(editorView)
    }

    private func replaceCurrent() {
        guard let doc = editorView.document else { return }
        _ = doc.replaceCurrent(replacement: findBar.replacement)
        editorView.didChangeDocument()
        finishFindMove()
    }

    private func replaceAll() {
        guard let doc = editorView.document else { return }
        let count = doc.replaceAll(replacement: findBar.replacement)
        editorView.didChangeDocument()
        finishFindMove()
        if count > 0 {
            statusLabel.stringValue = "Replaced \(count) occurrence\(count == 1 ? "" : "s")"
        }
    }

    /// The selected text if it is a sensible search seed: one line, short.
    private func selectionSeed() -> String? {
        guard let doc = editorView.document else { return nil }
        let range = doc.selectedRange()
        guard range.length > 0, range.length <= 200 else { return nil }
        let text = doc.textInRange(range: range)
        return text.contains("\n") ? nil : text
    }

    private static func escapeRegex(_ text: String) -> String {
        let special: Set<Character> = ["\\", ".", "+", "*", "?", "(", ")", "|", "[", "]", "{", "}", "^", "$"]
        return text.reduce(into: "") { out, c in
            if special.contains(c) { out.append("\\") }
            out.append(c)
        }
    }
}
