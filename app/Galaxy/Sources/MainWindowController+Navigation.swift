import AppKit
import os

extension MainWindowController {
    // MARK: Go to Line and back/forward (M2-08)

    var isGoToLineVisible: Bool { loadedGoToLine.map { !$0.isHidden } ?? false }

    /// Built on first ⌃G, like the other panels.
    var goToLine: GoToLine {
        if let panel = loadedGoToLine { return panel }
        let panel = GoToLine()
        panel.isHidden = true
        panel.translatesAutoresizingMaskIntoConstraints = false
        panel.onGo = { [weak self] target in self?.goTo(target) }
        panel.onClose = { [weak self] in self?.closeGoToLine() }
        window?.contentView?.addSubview(panel)
        let preferred = panel.widthAnchor.constraint(equalToConstant: GoToLine.width)
        preferred.priority = .defaultHigh
        NSLayoutConstraint.activate([
            panel.topAnchor.constraint(equalTo: detailView.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: detailView.centerXAnchor),
            panel.widthAnchor.constraint(lessThanOrEqualTo: detailView.widthAnchor, constant: -24),
            preferred,
        ])
        loadedGoToLine = panel
        return panel
    }

    /// ⌃G.
    @objc func showGoToLine(_ sender: Any?) {
        guard let doc = editorView.document else {
            statusLabel.stringValue = "Open a file to use Go to Line."
            return NSSound.beep()
        }
        closeOverlays()
        editorView.flushPendingUpdates()
        let panel = goToLine
        panel.reset(lineCount: doc.lineCount(), current: doc.primaryCursor().line)
        panel.isHidden = false
        panel.focus()
    }

    func closeGoToLine() {
        guard isGoToLineVisible else { return }
        goToLine.isHidden = true
        window?.makeFirstResponder(editorView)
    }

    private func goTo(_ target: LineTarget) {
        closeGoToLine()
        guard let tab = activeTab else { return }
        let from = location(of: tab)
        _ = navHistory.recordJump(from: from, to: NavLocation(key: tab.navKey, line: target.line, col: target.col ?? 0))
        refreshNavigationButtons()
        move(tab, toLine: target.line, col: target.col)
    }

    /// ⌃-.
    @objc func navigateBack(_ sender: Any?) { navigate(forward: false) }

    /// ⌃⇧-.
    @objc func navigateForward(_ sender: Any?) { navigate(forward: true) }

    /// Steps through the history, skipping places that can no longer be
    /// shown (a closed Untitled buffer, a deleted file).
    private func navigate(forward: Bool) {
        closeOverlays()
        let current = activeTab.map(location(of:))
        while let loc = forward ? navHistory.forward(current: current) : navHistory.back(current: current) {
            if show(loc) { return refreshNavigationButtons() }
            navHistory.forget(key: loc.key)
        }
        refreshNavigationButtons()
        NSSound.beep()
    }

    /// Shows `loc`, reopening its file if its tab was closed.
    func show(_ loc: NavLocation) -> Bool {
        isNavigating = true
        defer { isNavigating = false }
        let key = Self.fileKey(loc.key) ?? loc.key
        if let found = allTabs.first(where: { $0.navKey == key }) {
            focusTab(found)
        } else {
            guard !loc.key.hasPrefix("untitled:"), FileManager.default.fileExists(atPath: loc.key) else { return false }
            if let workspace, let rel = relativePath(loc.key) {
                let name = (rel as NSString).lastPathComponent
                openWorkspaceFile(workspace, FileEntry(name: name, relPath: rel, isDir: false), preview: false)
            } else {
                try? openFile(at: URL(fileURLWithPath: loc.key))
            }
        }
        guard let tab = activeTab, tab.navKey == key else { return false }
        move(tab, toLine: loc.line, col: loc.col)
        return true
    }

    func move(_ tab: EditorTab, toLine line: UInt64, col: UInt64?) {
        tab.doc.goToLine(line: line, col: col)
        editorView.didChangeSelection()
        editorView.revealLine(Int(min(line, tab.doc.lineCount() - 1)))
        window?.makeFirstResponder(editorView)
    }

    // MARK: Go to Symbol (M3-10)

    var isOutlinePickerVisible: Bool { loadedOutlinePicker.map { !$0.isHidden } ?? false }

    var outlinePicker: OutlinePicker {
        if let panel = loadedOutlinePicker { return panel }
        let panel = OutlinePicker()
        panel.isHidden = true
        panel.translatesAutoresizingMaskIntoConstraints = false
        panel.onGo = { [weak self] symbol in self?.goTo(symbol) }
        panel.onClose = { [weak self] in self?.closeGoToSymbol() }
        window?.contentView?.addSubview(panel)
        let preferred = panel.widthAnchor.constraint(equalToConstant: OutlinePicker.width)
        preferred.priority = .defaultHigh
        NSLayoutConstraint.activate([
            panel.topAnchor.constraint(equalTo: detailView.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: detailView.centerXAnchor),
            panel.widthAnchor.constraint(lessThanOrEqualTo: detailView.widthAnchor, constant: -24),
            preferred,
        ])
        loadedOutlinePicker = panel
        return panel
    }

    /// Shift-Command-O.
    @objc func showGoToSymbol(_ sender: Any?) {
        guard let doc = editorView.document else {
            statusLabel.stringValue = "Open a file to use Go to Symbol."
            return NSSound.beep()
        }
        if isOutlinePickerVisible { return outlinePicker.move(by: 1) }
        closeOverlays()
        let panel = outlinePicker
        let fallback = doc.outline()
        panel.reset(symbols: fallback)
        outlineRequest = doc
        bridge.requestDocumentSymbols(document: doc)
        panel.isHidden = false
        panel.focus()
    }

    func refreshDocumentSymbols() {
        if let document = activeTab?.doc {
            let symbols = bridge.documentSymbols(document: document)
            documentSymbols = symbols.map { OutlineSymbol(uri: $0.uri, name: $0.name, kind: $0.kind, line: $0.line, column: $0.column) }
            let line = editorView.document?.primaryCursor().line ?? 0
            breadcrumbBar.show(symbols: documentSymbols, atLine: line)
        } else {
            documentSymbols = []
            breadcrumbBar.clear()
        }
        guard let document = outlineRequest else { return }
        let symbols = bridge.documentSymbols(document: document)
        guard !symbols.isEmpty else { return }
        outlineRequest = nil
        outlinePicker.reset(symbols: symbols.map { OutlineSymbol(uri: $0.uri, name: $0.name, kind: $0.kind, line: $0.line, column: $0.column) })
    }

    /// Pins the header lines of the blocks around the top of the view.
    ///
    /// With `n` rows pinned, the first line under the bar is the one `n`
    /// rows below the top edge. The bar is right when that line sits in at
    /// least `n` blocks and each of the first `n` headers has reached its
    /// slot: header `i` pins once its own row is at the top plus `i` rows,
    /// so its copy lands on it (nearly) without a jump. A header leaves
    /// when its block ends under the bar. The largest such `n` wins.
    func updateStickyScope() {
        let top = scrollView.contentView.bounds.origin.y
        guard stickyScopeView.isEnabled, activeTab?.isDocument == true, !isDiffPaneShown,
              let document = editorView.document, top > 0 else {
            stickyScopeView.show([])
            return
        }
        let height = editorView.metrics.lineHeight
        let topRow = top / height
        var pinned: [UInt64] = []
        for count in stride(from: Self.stickyLimit, through: 1, by: -1) {
            let under = editorView.line(atY: top + CGFloat(count) * height).line
            let chain = document.stickyScopes(line: under, max: UInt64(Self.stickyLimit))
            guard chain.count >= count else { continue }
            let headers = Array(chain.prefix(count))
            let reached = headers.enumerated().allSatisfy { slot, line in
                editorView.rowTop(of: line) / height <= topRow + CGFloat(slot)
            }
            if reached { pinned = headers; break }
        }
        let rows = pinned.map { line -> StickyScopeView.Row in
            let text = document.displayLine(line: line)
            if let old = stickyScopeView.rows.first(where: { $0.line == line && $0.text == text }) { return old }
            let styles = document.viewport(firstLine: line, count: 1).styles.filter { $0.line == line }
            return StickyScopeView.Row(line: line, text: text, styles: styles)
        }
        stickyScopeView.show(rows)
    }

    /// Most pinned rows at once.
    private static let stickyLimit = 3

    /// A click on a pinned row: caret to its start, with the line scrolled
    /// to just below the pinned rows that still enclose it.
    func jumpToStickyLine(_ line: UInt64, above pinned: Int) {
        guard let document = editorView.document else { return }
        document.setCaret(pos: TextPos(line: line, colUtf16: 0))
        editorView.reloadDocument()
        let clip = scrollView.contentView
        let y = editorView.rowTop(of: line) - CGFloat(pinned) * editorView.metrics.lineHeight
        let target = NSPoint(x: clip.bounds.origin.x, y: y - scrollView.contentInsets.top)
        clip.scroll(to: clip.constrainBoundsRect(NSRect(origin: target, size: clip.bounds.size)).origin)
        scrollView.reflectScrolledClipView(clip)
        window?.makeFirstResponder(editorView)
        updateStickyScope()
    }

    func closeGoToSymbol() {
        guard isOutlinePickerVisible else { return }
        outlinePicker.isHidden = true
        window?.makeFirstResponder(editorView)
    }

    private func goTo(_ symbol: OutlineSymbol) {
        closeGoToSymbol()
        if let uri = symbol.uri, let path = URL(string: uri)?.path,
           let workspace, let rel = relativePath(path) {
            openWorkspaceFile(workspace, FileEntry(name: URL(fileURLWithPath: path).lastPathComponent, relPath: rel, isDir: false), preview: false)
        }
        guard let tab = activeTab else { return }
        let from = location(of: tab)
        _ = navHistory.recordJump(
            from: from,
            to: NavLocation(key: tab.navKey, line: symbol.line, col: symbol.column))
        refreshNavigationButtons()
        move(tab, toLine: symbol.line, col: symbol.column)
    }

    /// Records where the caret was before this find session, once, if find
    /// has since moved it far away.
    func recordFindOrigin() {
        guard let origin = findOrigin, let tab = activeTab else { return }
        findOrigin = nil
        guard origin.key == tab.navKey else { return }
        _ = navHistory.recordJump(from: origin, to: location(of: tab))
        refreshNavigationButtons()
    }

    /// Runs `move` and records where the caret was if it went far.
    func recordingJump(_ move: () -> Void) {
        guard let tab = activeTab else { return move() }
        let from = location(of: tab)
        move()
        _ = navHistory.recordJump(from: from, to: location(of: tab))
        refreshNavigationButtons()
    }

    func location(of tab: EditorTab) -> NavLocation {
        tab.doc.caretLocation(key: tab.navKey)
    }

    /// Records the place being left, unless back/forward is moving.
    func noteLeaving(_ tab: EditorTab) {
        guard !isNavigating else { return }
        navHistory.record(from: location(of: tab))
        refreshNavigationButtons()
    }

    /// Greys the title bar's back and forward buttons when there is nowhere to go.
    func refreshNavigationButtons() {
        titleBar.setNavigation(canGoBack: navHistory.canGoBack(), canGoForward: navHistory.canGoForward())
    }

    /// Top of the document, inset included.
    func scrollToTop() {
        scrollView.contentView.scroll(to: NSPoint(x: 0, y: -scrollView.contentInsets.top))
        scrollView.reflectScrolledClipView(scrollView.contentView)
    }
}
