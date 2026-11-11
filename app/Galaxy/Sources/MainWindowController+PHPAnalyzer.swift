import AppKit

extension MainWindowController {
    var isIndentationPickerVisible: Bool { loadedIndentationPicker.map { !$0.isHidden } ?? false }

    func showIndentationPicker() {
        guard let document = activeTab?.doc else { return }
        if isIndentationPickerVisible { return closeIndentationPicker() }
        closeOverlays()
        let panel = loadedIndentationPicker ?? IndentationPicker()
        if loadedIndentationPicker == nil {
            panel.isHidden = true
            panel.translatesAutoresizingMaskIntoConstraints = false
            panel.onClose = { [weak self] in self?.closeIndentationPicker() }
            panel.onSelect = { [weak self] action in self?.applyIndentationAction(action) }
            window?.contentView?.addSubview(panel)
            NSLayoutConstraint.activate([
                panel.topAnchor.constraint(equalTo: titleBar.topAnchor, constant: 6),
                panel.centerXAnchor.constraint(equalTo: titleBar.centerXAnchor),
                panel.widthAnchor.constraint(equalToConstant: IndentationPicker.width),
            ])
            loadedIndentationPicker = panel
        }
        let settings = document.indentSettings()
        panel.reset(useTabs: settings.useTabs, tabSize: settings.tabSize)
        panel.isHidden = false
        panel.focus()
    }

    func closeIndentationPicker() {
        guard isIndentationPickerVisible else { return }
        loadedIndentationPicker?.isHidden = true
        window?.makeFirstResponder(editorView)
    }

    private func applyIndentationAction(_ action: IndentationAction) {
        guard let tab = activeTab else { return }
        var settings = tab.doc.indentSettings()
        switch action {
        case let .useTabs(useTabs):
            settings = IndentSettings(useTabs: useTabs, tabSize: settings.tabSize)
        case let .tabSize(size):
            settings = IndentSettings(useTabs: settings.useTabs, tabSize: size)
        case let .convertToTabs(useTabs):
            convertIndentation(document: tab.doc, toTabs: useTabs, tabSize: settings.tabSize)
            settings = IndentSettings(useTabs: useTabs, tabSize: settings.tabSize)
        }
        tab.indentOverride = (settings.useTabs, settings.tabSize)
        bridge.setIndent(document: tab.doc, useTabs: settings.useTabs, tabSize: settings.tabSize)
        editorView.syncTabSize()
        for group in groups { group.pane?.editorView.syncTabSize() }
        secondaryEditorView.syncTabSize()
        statusBar.show(document: tab.doc, path: tab.doc.path())
        closeIndentationPicker()
    }

    private func convertIndentation(document: DocumentHandle, toTabs: Bool, tabSize: UInt32) {
        let text = document.text()
        let size = max(Int(tabSize), 1)
        var edits: [RenameEdit] = []
        for (lineNumber, line) in text.split(separator: "\n", omittingEmptySubsequences: false).enumerated() {
            let prefix = line.prefix { $0 == " " || $0 == "\t" }
            guard !prefix.isEmpty else { continue }
            var column = 0
            for character in prefix {
                column = character == "\t" ? column + size - column % size : column + 1
            }
            let replacement = toTabs
                ? String(repeating: "\t", count: column / size) + String(repeating: " ", count: column % size)
                : String(repeating: " ", count: column)
            guard replacement != String(prefix) else { continue }
            edits.append(RenameEdit(uri: "", startLine: UInt64(lineNumber), startCharacter: 0,
                                    endLine: UInt64(lineNumber), endCharacter: UInt64(prefix.utf16.count),
                                    newText: replacement))
        }
        guard !edits.isEmpty else { return }
        _ = bridge.applyRenameEdits(document: document, edits: edits)
        updateEditedState()
        editorView.needsDisplay = true
    }

    var isPHPVersionPickerVisible: Bool { loadedPHPVersionPicker.map { !$0.isHidden } ?? false }

    private var phpVersionPicker: PHPVersionPicker {
        if let panel = loadedPHPVersionPicker { return panel }
        let panel = PHPVersionPicker()
        panel.isHidden = true
        panel.translatesAutoresizingMaskIntoConstraints = false
        panel.onSelect = { [weak self] version in
            self?.closePHPVersionPicker()
            self?.setPHPAnalyzerTarget(version)
        }
        panel.onClose = { [weak self] in self?.closePHPVersionPicker() }
        window?.contentView?.addSubview(panel)
        let preferred = panel.widthAnchor.constraint(equalToConstant: PHPVersionPicker.width)
        preferred.priority = .defaultHigh
        NSLayoutConstraint.activate([
            panel.topAnchor.constraint(equalTo: titleBar.topAnchor, constant: 6),
            panel.centerXAnchor.constraint(equalTo: titleBar.centerXAnchor),
            panel.widthAnchor.constraint(lessThanOrEqualTo: titleBar.widthAnchor, constant: -24),
            preferred,
        ])
        loadedPHPVersionPicker = panel
        return panel
    }

    func showPHPAnalyzerTargetMenu() {
        guard let workspace, let document = activeTab?.doc,
              (document.syntaxLanguage() ?? StatusBar.language(for: document.path())).lowercased() == "php" else {
            return
        }
        if isPHPVersionPickerVisible { return closePHPVersionPicker() }
        closeOverlays()
        let settingsURL = GalaxySettings.workspaceURL(root: workspace.root())
        let selected = (try? GalaxySettings.load(from: settingsURL).phpVersion)
        let panel = phpVersionPicker
        panel.reset(selected: selected)
        panel.isHidden = false
        panel.focus()
    }

    func closePHPVersionPicker() {
        guard isPHPVersionPickerVisible else { return }
        phpVersionPicker.isHidden = true
        window?.makeFirstResponder(activeTab == nil ? explorer.outlineView : editorView)
    }

    private func setPHPAnalyzerTarget(_ version: String?) {
        guard let workspace else { return }
        let settingsURL = GalaxySettings.workspaceURL(root: workspace.root())
        do {
            try GalaxySettings.write(version ?? "", key: "version", section: "php", to: settingsURL)
            _ = bridge.setPHPAnalyzerTarget(root: workspace.root(), version: version)
            bridge.refreshWorkspaceIndexes(root: workspace.root())
            if let version {
                statusLabel.stringValue = "PHP analyzer target set to PHP \(version)."
            } else {
                statusLabel.stringValue = "PHP analyzer follows Composer/default."
            }
        } catch {
            report(error, while: "set PHP analyzer target")
        }
    }
}
