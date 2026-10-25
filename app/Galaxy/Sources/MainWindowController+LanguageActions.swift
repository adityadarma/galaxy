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
        let location = bridge.definitionLocations(document: request.document).first
        definitionRequest = nil
        if let location { jump(to: location) }
    }

    /// Opens `location` and records the jump for back/forward.
    private func jump(to location: DefinitionLocation) {
        guard let path = URL(string: location.uri)?.path else { return }
        let target = NavLocation(key: path, line: location.line, col: location.column)
        if let current = activeTab.map(location(of:)) { _ = navHistory.recordJump(from: current, to: target); refreshNavigationButtons() }
        _ = show(target)
    }

    @objc func findReferences(_ sender: Any?) {
        guard let document = activeTab?.doc else { NSSound.beep(); return }
        let position = document.primaryCursor()
        referencesRequest = document
        bridge.requestReferences(document: document, line: position.line, character: position.colUtf16)
    }

    func refreshReferences() {
        guard let document = referencesRequest else { return }
        let locations = bridge.references(document: document)
        guard !locations.isEmpty else { return }
        referencesRequest = nil
        problemsPanel.isHidden = true
        referencesPanel.update(locations)
        if referencesPanel.superview == nil { detailView.addSubview(referencesPanel) }
        referencesPanel.frame = NSRect(x: 0, y: 0, width: detailView.bounds.width, height: 150)
        referencesPanel.isHidden = false
    }

    func showReference(_ location: DefinitionLocation) {
        guard URL(string: location.uri) != nil else { return }
        referencesPanel.removeFromSuperview()
        jump(to: location)
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
