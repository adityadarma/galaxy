import AppKit
import os

extension MainWindowController {
    func requestDefinition(at position: TextPos) -> Bool {
        guard let document = activeTab?.doc else { return false }
        // The core resolves log channels from config/logging.php.
        if let channel = editorView.logChannelName(at: position) {
            if let found = laravelLogChannelLocation(file: document.path(), channel: channel) {
                jump(to: found)
            } else {
                NSSound.beep()
            }
            return true
        }
        // A relative import is a file on disk: open it without the server.
        // Packages (`node:fs`, `react`) go to the server below.
        if let specifier = editorView.importSpecifier(at: position),
           let target = bridge.resolveJavaScriptRelativeImport(specifier: specifier.path, fromPath: document.path()) {
            jump(to: target)
            return true
        }
        definitionRequest = (document, position)
        bridge.requestDefinition(document: document, line: position.line, character: position.colUtf16)
        return true
    }

    func refreshDefinition() {
        guard let request = definitionRequest else { return }
        let locations = bridge.definitionLocations(document: request.document)
        definitionRequest = nil
        showDefinitionLocations(locations, at: request.position)
    }

    func showDefinitionLocations(_ locations: [DefinitionLocation], at position: TextPos) {
        guard locations.count > 1 else {
            guard let location = locations.first else { return }
            // Like VS Code: going to the definition from the definition itself
            // shows its references instead of a no-op jump.
            if let document = activeTab?.doc, isDefinition(location, at: position, in: document) {
                // The request is cancelled when the caret moves, so pin it to the click.
                document.goToLine(line: position.line, col: position.colUtf16)
                startReferences(document: document, position: position)
            } else {
                jump(to: location)
            }
            return
        }
        presentReferencesPeek(at: position)
        referencesPanel.update(
            locations,
            workspaceRoot: workspace?.root(),
            title: "Definitions",
            itemName: "definition"
        )
        window?.makeFirstResponder(referencesPanel)
    }

    /// Opens `location` and records the jump for back/forward.
    private func jump(to location: DefinitionLocation) {
        guard let path = URL(string: location.uri)?.path else { return }
        let target = NavLocation(key: path, line: location.line, col: location.column)
        if let current = activeTab.map(location(of:)) { _ = navHistory.recordJump(from: current, to: target); refreshNavigationButtons() }
        _ = show(target)
    }

    /// True when `location` starts on the identifier under `position` in `document`.
    func isDefinition(_ location: DefinitionLocation, at position: TextPos, in document: DocumentHandle) -> Bool {
        guard let url = URL(string: location.uri), url.isFileURL,
              location.line == position.line,
              location.column <= position.colUtf16 else { return false }
        let target = url.resolvingSymlinksInPath().path
        let current = URL(fileURLWithPath: document.path()).resolvingSymlinksInPath().path
        guard target == current else { return false }
        // Identifiers are short; a distant start on the same line is another symbol.
        return position.colUtf16 - location.column <= 128
    }

    @objc func findReferences(_ sender: Any?) {
        guard let document = activeTab?.doc else { NSSound.beep(); return }
        let cursor = document.primaryCursor()
        startReferences(document: document, position: TextPos(line: cursor.line, colUtf16: cursor.colUtf16))
    }

    func startReferences(document: DocumentHandle, position: TextPos) {
        closeOverlays()
        presentReferencesPeek(at: position)
        let indexingDetail = workspaceIndexState.flatMap { state in
            document.path().hasPrefix(state.root + "/")
                ? "Scanning workspace \(state.completed) of \(state.total) files. References will appear when the language index is ready."
                : nil
        }
        referencesPanel.showStatus(
            "Finding references...",
            detail: indexingDetail
                ?? "Searching the workspace. Language indexing may still be in progress."
        )
        referencesRequest = (
            document,
            position,
            false,
            true,
            Date().addingTimeInterval(5),
            Date()
        )
        requestReferences(document: document, position: position)
    }

    func refreshReferences() {
        guard var request = referencesRequest else { return }
        let cursor = request.document.primaryCursor()
        guard activeTab?.doc === request.document,
              cursor.line == request.position.line,
              cursor.colUtf16 == request.position.colUtf16 else {
            referencesRequest = nil
            return
        }
        let indexing = workspaceIndexState.flatMap { state in
            request.document.path().hasPrefix(state.root + "/") ? state : nil
        }
        if !request.answered {
            if let indexing {
                let progress = indexing.total == 0
                    ? "Scanning workspace..."
                    : "Scanning workspace \(indexing.completed) of \(indexing.total) files"
                if bridge.references(document: request.document).isEmpty {
                    referencesPanel.showStatus(
                        "Finding references...",
                        detail: "\(progress). References will appear when the language index is ready."
                    )
                }
                request.deadline = Date().addingTimeInterval(5)
            }
            // While indexing, a shown partial result waits for the final
            // request after indexing ends instead of rescanning every second.
            let waitingForIndex = indexing != nil && !bridge.references(document: request.document).isEmpty
            if !request.inFlight, !waitingForIndex, Date().timeIntervalSince(request.lastAttempt) >= 1 {
                request.inFlight = true
                request.lastAttempt = Date()
                referencesRequest = request
                requestReferences(document: request.document, position: request.position)
                return
            }
        }
        let locations = bridge.references(document: request.document)
        guard !locations.isEmpty else {
            if indexing != nil {
                request.answered = false
                referencesRequest = request
            } else if !request.inFlight, Date() >= request.deadline {
                referencesRequest = nil
                referencesPanel.showStatus(
                    "No references found",
                    detail: "Place the cursor on a symbol in a supported language and try again. If the workspace was just opened, wait for indexing to finish."
                )
            } else {
                request.answered = false
                referencesRequest = request
            }
            return
        }
        presentReferencesPeek(at: request.position)
        let requestURI = URL(fileURLWithPath: request.document.path()).absoluteString
        let preferred = locations
            .filter { $0.uri == requestURI }
            .min { lhs, rhs in
                let lhsDistance = abs(Int64(lhs.line) - Int64(request.position.line)) * 1_000_000
                    + abs(Int64(lhs.column) - Int64(request.position.colUtf16))
                let rhsDistance = abs(Int64(rhs.line) - Int64(request.position.line)) * 1_000_000
                    + abs(Int64(rhs.column) - Int64(request.position.colUtf16))
                return lhsDistance < rhsDistance
            }
        referencesPanel.update(
            locations,
            workspaceRoot: workspace?.root(),
            preferredLocation: preferred
        )
        preloadReferenceSnippets(locations)
        window?.makeFirstResponder(referencesPanel)
        if indexing != nil {
            // Local occurrences can be available before the workspace importer
            // snapshot. Keep the visible partial result and require one final
            // request after indexing finishes.
            request.answered = false
            referencesRequest = request
        } else if request.answered {
            referencesRequest = nil
        } else {
            referencesRequest = request
        }
    }

    private func requestReferences(document: DocumentHandle, position: TextPos) {
        bridge.requestReferences(
            document: document,
            line: position.line,
            character: position.colUtf16
        ) { [weak self, weak document] answered in
            guard let self, let document, var request = self.referencesRequest,
                  request.document === document,
                  request.position == position else { return }
            request.answered = answered
            request.inFlight = false
            request.lastAttempt = Date()
            self.referencesRequest = request
            self.refreshReferences()
        }
    }

    private func presentReferencesPeek(at position: TextPos) {
        if referencesPanel.superview == nil {
            groupChrome.addSubview(referencesPanel, positioned: .above, relativeTo: stickyScopeView)
        }
        layoutReferencesPeek(at: position)
        referencesPanel.isHidden = false
    }

    func showReference(_ location: DefinitionLocation) {
        guard URL(string: location.uri) != nil else { return }
        closeReferencesPeek()
        jump(to: location)
    }

    func closeReferencesPeek() {
        referencePreviewGeneration += 1
        referenceSnippetGeneration += 1
        referencesRequest = nil
        loadedReferencesPanel?.removeFromSuperview()
        window?.makeFirstResponder(editorView)
    }

    func previewReference(_ location: DefinitionLocation) {
        guard let url = URL(string: location.uri), url.isFileURL else { return }
        referencePreviewGeneration += 1
        let generation = referencePreviewGeneration
        if let document = allTabs.first(where: { $0.doc.path() == url.path })?.doc {
            let snapshot = document.viewport(firstLine: 0, count: document.lineCount())
            referencesPanel.showPreview(
                path: url.path,
                text: document.text(),
                location: location,
                styles: snapshot.styles,
                gutterWidth: editorView.textOriginX,
                stickyScopes: Self.stickyScopeProvider(for: document)
            )
            return
        }
        let core = bridge.core
        DispatchQueue.global(qos: .utility).async { [weak self, url, location, core] in
            // The peek is not a file viewer. Refuse an unexpectedly large
            // dependency instead of copying it into NSTextView.
            let values = try? url.resourceValues(forKeys: [.fileSizeKey])
            var text: String?
            var styles: [StyleRun] = []
            var scopes: (@Sendable (Int) -> [Int])?
            if (values?.fileSize ?? 0) <= 2 * 1_024 * 1_024,
               let document = try? core.openDocument(path: url.path) {
                text = document.text()
                let deadline = Date().addingTimeInterval(0.5)
                while document.syntaxLanguage() != nil, Date() < deadline {
                    if document.pollSyntax() { break }
                    Thread.sleep(forTimeInterval: 0.005)
                }
                styles = document.viewport(firstLine: 0, count: document.lineCount()).styles
                scopes = Self.stickyScopeProvider(for: document)
            }
            Task { @MainActor [weak self] in
                guard let self, generation == self.referencePreviewGeneration,
                      self.referencesPanel.superview != nil else { return }
                self.referencesPanel.showPreview(
                    path: url.path,
                    text: text ?? "Preview unavailable for this file.",
                    location: location,
                    styles: styles,
                    gutterWidth: self.editorView.textOriginX,
                    stickyScopes: scopes
                )
            }
        }
    }

    /// Scope headers of `document` for the peek's sticky rows.
    private nonisolated static func stickyScopeProvider(for document: DocumentHandle) -> @Sendable (Int) -> [Int] {
        { line in document.stickyScopes(line: UInt64(line), max: 3).map(Int.init) }
    }

    private func preloadReferenceSnippets(_ locations: [DefinitionLocation]) {
        referenceSnippetGeneration += 1
        let generation = referenceSnippetGeneration
        let grouped = Dictionary(grouping: locations, by: \.uri)
        DispatchQueue.global(qos: .utility).async { [weak self] in
            var values: [(DefinitionLocation, String)] = []
            for (uri, fileLocations) in grouped {
                guard let url = URL(string: uri), url.isFileURL,
                      let text = try? String(contentsOf: url, encoding: .utf8) else { continue }
                for location in fileLocations {
                    let range = ReferencesPanel.lineRange(in: text, line: Int(location.line))
                    guard range.length > 0 else { continue }
                    values.append((location, (text as NSString).substring(with: range)))
                }
            }
            Task { @MainActor [weak self] in
                guard let self, generation == self.referenceSnippetGeneration,
                      self.referencesPanel.superview != nil else { return }
                self.referencesPanel.setSnippets(values)
            }
        }
    }

    func layoutReferencesPeek(at position: TextPos) {
        let anchor = editorView.convert(
            editorView.rect(forLine: position.line, column: position.colUtf16),
            to: groupChrome
        )
        let inset: CGFloat = 0
        let minimapInset: CGFloat = minimapEnabled && !isDiffPaneShown ? MinimapView.width : 0
        let width = max(0, groupChrome.bounds.width - minimapInset)
        let height = min(320, max(210, groupChrome.bounds.height * 0.42))
        let x: CGFloat = 0
        // `groupChrome` is not flipped: smaller Y values are visually below.
        let below = anchor.minY - height - 2
        let y = below >= inset ? below : min(groupChrome.bounds.height - height - inset, anchor.maxY + 2)
        referencesPanel.frame = NSRect(x: x, y: max(inset, y), width: width, height: height)
    }

    @objc func renameSymbol(_ sender: Any?) {
        guard let document = activeTab?.doc else { NSSound.beep(); return }
        let current = document.primaryCursor()
        let line = document.displayLine(line: current.line)
        let column = min(Int(current.colUtf16), line.utf16.count)
        let prefix = String(line.prefix(column))
        let suffix = String(line.dropFirst(column))
        let left = prefix.lastIndex(where: { !$0.isLetter && !$0.isNumber && $0 != "_" }).map { prefix.index(after: $0) } ?? prefix.startIndex
        let right = suffix.firstIndex(where: { !$0.isLetter && !$0.isNumber && $0 != "_" }) ?? suffix.endIndex
        let field = NSTextField(string: String(prefix[left...]) + String(suffix[..<right]))
        field.frame = NSRect(x: 0, y: 0, width: 280, height: 24)
        let alert = NSAlert(); alert.messageText = "Rename Symbol"; alert.informativeText = "Enter the new symbol name."; alert.accessoryView = field
        alert.addButton(withTitle: "Rename"); alert.addButton(withTitle: "Cancel"); alert.window.initialFirstResponder = field
        guard alert.runModal() == .alertFirstButtonReturn, !field.stringValue.isEmpty else { return }
        renameRequest = document
        bridge.requestRename(document: document, line: current.line, character: current.colUtf16, newName: field.stringValue)
    }

    func refreshRename() {
        guard let document = renameRequest else { return }
        let edits = bridge.renameEdits(document: document)
        guard !edits.isEmpty else { return }
        renameRequest = nil
        snippetSession = nil
        snippetDocumentKey = nil
        var grouped: [String: [RenameEdit]] = [:]
        for edit in edits { grouped[edit.uri, default: []].append(edit) }
        for (uri, fileEdits) in grouped {
            guard let path = URL(string: uri)?.path else { continue }
            let target: DocumentHandle
            let wasOpen = allTabs.contains { $0.doc.path() == path }
            if let tab = allTabs.first(where: { $0.doc.path() == path }) { target = tab.doc }
            else { guard let opened = try? bridge.openDocument(path: path) else { continue }; target = opened }
            _ = bridge.applyRenameEdits(document: target, edits: fileEdits)
            if !wasOpen { try? bridge.save(document: target) }
        }
        updateEditedState()
    }

    @objc func showCodeActions(_ sender: Any?) {
        guard let document = activeTab?.doc else { NSSound.beep(); return }
        let position = document.primaryCursor()
        codeActionRequest = document
        bridge.requestCodeActions(document: document, line: position.line, character: position.colUtf16)
    }

    func refreshCodeActions() {
        guard let document = codeActionRequest else { return }
        let actions = bridge.codeActions(document: document)
        guard !actions.isEmpty else { return }
        codeActionRequest = nil
        codeActionPopup.update(actions)
        codeActionPopup.isHidden = false
        codeActionPopup.frame = NSRect(x: editorView.textOriginX + 8, y: editorView.metrics.lineHeight + 8, width: 420, height: 156)
        window?.makeFirstResponder(editorView)
    }

    private func applyCodeAction(_ action: CodeActionInfo) {
        codeActionPopup.isHidden = true
        snippetSession = nil
        snippetDocumentKey = nil
        var grouped: [String: [RenameEdit]] = [:]
        for edit in action.edits { grouped[edit.uri, default: []].append(edit) }
        for (uri, edits) in grouped {
            guard let path = URL(string: uri)?.path else { continue }
            // A fix that creates a view or a page: the file starts out empty.
            if !FileManager.default.fileExists(atPath: path) {
                let folder = (path as NSString).deletingLastPathComponent
                try? FileManager.default.createDirectory(atPath: folder, withIntermediateDirectories: true)
                guard FileManager.default.createFile(atPath: path, contents: Data()) else { continue }
            }
            let wasOpen = allTabs.contains { $0.doc.path() == path }
            let target: DocumentHandle
            if let tab = allTabs.first(where: { $0.doc.path() == path }) {
                target = tab.doc
            } else {
                guard let opened = try? bridge.openDocument(path: path) else { continue }
                target = opened
            }
            _ = bridge.applyRenameEdits(document: target, edits: edits)
            if !wasOpen { try? bridge.save(document: target) }
        }
        updateEditedState()
        editorView.needsDisplay = true
    }

    @objc func goToDefinition(_ sender: Any?) {
        guard let document = activeTab?.doc else { NSSound.beep(); return }
        let position = document.primaryCursor()
        _ = requestDefinition(at: TextPos(line: position.line, colUtf16: position.colUtf16))
    }
}
