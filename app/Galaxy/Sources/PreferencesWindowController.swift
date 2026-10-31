import AppKit

@MainActor
final class PreferencesWindowController: NSWindowController, NSTextFieldDelegate {
    private let onChange: () -> Void
    private let appearance = NSPopUpButton(frame: .zero, pullsDown: false)
    private let font = NSPopUpButton(frame: .zero, pullsDown: false)
    private let fontSize = NSStepper(frame: .zero)
    private let fontSizeLabel = NSTextField(labelWithString: "")

    // AI Commit settings (OpenAI-compatible API)
    private let baseURLField = NSTextField()
    private let modelPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    private let modelRefreshButton = NSButton()
    private let modelStatus = NSTextField(labelWithString: "")
    private let apiKeyField = NSSecureTextField()
    private var modelLoadTask: Task<Void, Never>?
    private var modelReloadWork: DispatchWorkItem?

    // Editor tab: written to settings.json (a workspace's own file still wins).
    private let settingsURL: URL
    private let tabSizePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    private let insertSpacesBox = NSButton(checkboxWithTitle: "Insert spaces when pressing Tab", target: nil, action: nil)
    private let whitespacePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    private let autoClosingTagsBox = NSButton(checkboxWithTitle: "Auto close tags (HTML, PHP, JSX)", target: nil, action: nil)
    private let linkedEditingBox = NSButton(checkboxWithTitle: "Rename matching tag while editing", target: nil, action: nil)
    private let parameterHintsBox = NSButton(checkboxWithTitle: "Show parameter names before call arguments", target: nil, action: nil)
    private let autoSavePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    private let autoSaveDelay = NSStepper(frame: .zero)
    private let autoSaveDelayLabel = NSTextField(labelWithString: "")
    private let settingsStatus = NSTextField(wrappingLabelWithString: "")
    /// One checkbox per default `files.exclude` glob, in `defaultFilesExclude` order.
    private let excludeBoxes: [NSButton] = GalaxySettings.defaultFilesExclude.map {
        NSButton(checkboxWithTitle: $0, target: nil, action: nil)
    }
    private var editorControls: [NSControl] {
        [tabSizePopup, insertSpacesBox, whitespacePopup, parameterHintsBox, autoClosingTagsBox, linkedEditingBox,
         autoSavePopup, autoSaveDelay, gitBlameBox] + excludeBoxes
    }

    // View tab: the same switches as the View menu.
    private let minimapBox = NSButton(checkboxWithTitle: "Minimap", target: nil, action: nil)
    private let wordWrapBox = NSButton(checkboxWithTitle: "Word wrap", target: nil, action: nil)
    private let breadcrumbsBox = NSButton(checkboxWithTitle: "Breadcrumbs", target: nil, action: nil)
    private let stickyScrollBox = NSButton(checkboxWithTitle: "Sticky scroll", target: nil, action: nil)
    private let gitBlameBox = NSButton(checkboxWithTitle: "Inline Git blame on the caret line", target: nil, action: nil)
    private let folderIconsBox = NSButton(checkboxWithTitle: "Show folder icons", target: nil, action: nil)

    private static let tabSizes = [2, 4, 8]
    private static let whitespaceModes: [(title: String, value: WhitespaceRendering)] = [
        ("None", .none), ("Boundary", .boundary), ("All", .all),
    ]
    private static let autoSaveModes: [(title: String, value: AutoSaveMode)] = [
        ("Off", .off), ("After Delay", .afterDelay), ("On Focus Change", .onFocusChange),
    ]
    /// UserDefaults keys of the View toggles, with their defaults.
    private static let viewDefaults: [(key: String, fallback: Bool)] = [
        ("Minimap", true), ("WordWrap", false), ("ShowBreadcrumbs", true), ("StickyScroll", true),
    ]

    init(settingsURL: URL = GalaxySettings.globalURL, onChange: @escaping () -> Void) {
        self.settingsURL = settingsURL
        self.onChange = onChange
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 440, height: 200),
                              styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Settings"
        window.isReleasedWhenClosed = false
        window.toolbarStyle = .preference
        super.init(window: window)
        build()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// One toolbar tab per group, the standard macOS Settings layout.
    private func build() {
        let tabs = SettingsTabViewController()
        tabs.tabStyle = .toolbar
        tabs.addTabViewItem(tab("General", symbol: "gearshape", pane: buildGeneralPane()))
        tabs.addTabViewItem(tab("Editor", symbol: "text.cursor", pane: buildEditorPane()))
        tabs.addTabViewItem(tab("Explorer", symbol: "folder", pane: buildExplorerPane()))
        tabs.addTabViewItem(tab("View", symbol: "rectangle.split.3x1", pane: buildViewPane()))
        tabs.addTabViewItem(tab("AI", symbol: "sparkles", pane: buildAIPane()))
        window?.contentViewController = tabs
        tabs.fitWindow(to: tabs.tabViewItems.first)
        window?.center()
        window?.delegate = self
        syncControls()
        loadModels()
    }

    // MARK: Editor and View tabs

    private func buildEditorPane() -> NSView {
        let stack = NSStackView()
        stack.addArrangedSubview(section("Indentation"))
        tabSizePopup.addItems(withTitles: Self.tabSizes.map(String.init))
        tabSizePopup.setAccessibilityLabel("Tab size")
        tabSizePopup.target = self; tabSizePopup.action = #selector(saveTabSize(_:))
        stack.addArrangedSubview(row("Tab size", tabSizePopup))
        insertSpacesBox.target = self; insertSpacesBox.action = #selector(saveInsertSpaces(_:))
        stack.addArrangedSubview(row("", insertSpacesBox))

        stack.addArrangedSubview(section("Rendering"))
        whitespacePopup.addItems(withTitles: Self.whitespaceModes.map(\.title))
        whitespacePopup.setAccessibilityLabel("Render whitespace")
        whitespacePopup.target = self; whitespacePopup.action = #selector(saveWhitespace(_:))
        stack.addArrangedSubview(row("Whitespace", whitespacePopup))
        parameterHintsBox.target = self; parameterHintsBox.action = #selector(saveParameterHints(_:))
        stack.addArrangedSubview(row("", parameterHintsBox))

        stack.addArrangedSubview(section("Tags"))
        autoClosingTagsBox.target = self; autoClosingTagsBox.action = #selector(saveAutoClosingTags(_:))
        linkedEditingBox.target = self; linkedEditingBox.action = #selector(saveLinkedEditing(_:))
        stack.addArrangedSubview(row("", autoClosingTagsBox))
        stack.addArrangedSubview(row("", linkedEditingBox))

        stack.addArrangedSubview(section("Files"))
        autoSavePopup.addItems(withTitles: Self.autoSaveModes.map(\.title))
        autoSavePopup.setAccessibilityLabel("Auto save")
        autoSavePopup.target = self; autoSavePopup.action = #selector(saveAutoSave(_:))
        stack.addArrangedSubview(row("Auto save", autoSavePopup))
        autoSaveDelay.minValue = 250; autoSaveDelay.maxValue = 10_000; autoSaveDelay.increment = 250
        autoSaveDelay.setAccessibilityLabel("Auto save delay")
        autoSaveDelay.target = self; autoSaveDelay.action = #selector(saveAutoSaveDelay(_:))
        let delayControls = NSStackView(views: [autoSaveDelay, autoSaveDelayLabel])
        delayControls.spacing = 8
        stack.addArrangedSubview(row("Delay", delayControls))

        settingsStatus.font = .systemFont(ofSize: 11)
        settingsStatus.textColor = .secondaryLabelColor
        settingsStatus.preferredMaxLayoutWidth = 370
        settingsStatus.widthAnchor.constraint(equalToConstant: 370).isActive = true
        settingsStatus.stringValue = "Saved to settings.json. A folder's .galaxy/settings.json overrides these."
        stack.addArrangedSubview(settingsStatus)
        return pane(stack)
    }

    private func buildExplorerPane() -> NSView {
        let stack = NSStackView()
        stack.addArrangedSubview(section("Appearance"))
        folderIconsBox.target = self; folderIconsBox.action = #selector(saveFolderIcons(_:))
        stack.addArrangedSubview(folderIconsBox)
        stack.addArrangedSubview(section("Hidden in the Explorer (files.exclude)"))
        for box in excludeBoxes {
            box.target = self; box.action = #selector(saveFilesExclude(_:))
            box.setAccessibilityLabel("Hide \(box.title)")
            stack.addArrangedSubview(box)
        }
        let hint = NSTextField(wrappingLabelWithString: "Uncheck a pattern to show it again. Add other patterns under files.exclude in settings.json, for example {\"**/*.log\": true}.")
        hint.font = .systemFont(ofSize: 11)
        hint.textColor = .secondaryLabelColor
        hint.preferredMaxLayoutWidth = 370
        hint.widthAnchor.constraint(equalToConstant: 370).isActive = true
        stack.addArrangedSubview(hint)
        return pane(stack)
    }

    private func buildViewPane() -> NSView {
        let stack = NSStackView()
        stack.addArrangedSubview(section("Editor"))
        for box in [minimapBox, wordWrapBox, breadcrumbsBox, stickyScrollBox] {
            box.target = self; box.action = #selector(saveViewToggle(_:))
            stack.addArrangedSubview(row("", box))
        }
        stack.addArrangedSubview(section("Git"))
        gitBlameBox.target = self; gitBlameBox.action = #selector(saveGitBlame(_:))
        stack.addArrangedSubview(row("", gitBlameBox))
        let hint = NSTextField(wrappingLabelWithString: "The View menu toggles the same switches.")
        hint.font = .systemFont(ofSize: 11)
        hint.textColor = .secondaryLabelColor
        stack.addArrangedSubview(hint)
        return pane(stack)
    }

    private func section(_ title: String) -> NSTextField {
        let label = NSTextField(labelWithString: title)
        label.font = .systemFont(ofSize: 12, weight: .semibold)
        return label
    }

    /// Shows the saved values; run again when the window comes back, since
    /// the View menu and settings.json can change them meanwhile.
    private func syncControls() {
        let defaults = UserDefaults.standard
        for (box, entry) in zip([minimapBox, wordWrapBox, breadcrumbsBox, stickyScrollBox], Self.viewDefaults) {
            box.state = (defaults.object(forKey: entry.key) as? Bool ?? entry.fallback) ? .on : .off
        }
        folderIconsBox.state = ExplorerIcon.showsFolderIcons ? .on : .off
        let settings: GalaxySettings
        do {
            settings = FileManager.default.fileExists(atPath: settingsURL.path)
                ? try GalaxySettings.load(from: settingsURL) : .empty
        } catch {
            // Leave a broken file for the user to fix rather than overwrite it.
            editorControls.forEach { $0.isEnabled = false }
            showSettingsError("settings.json could not be read, so these are locked. Fix the file to edit them here.")
            return
        }
        editorControls.forEach { $0.isEnabled = true }
        let size = Int(settings.effectiveTabSize)
        if !Self.tabSizes.contains(size), tabSizePopup.item(withTitle: String(size)) == nil {
            tabSizePopup.addItem(withTitle: String(size))
        }
        tabSizePopup.selectItem(withTitle: String(size))
        insertSpacesBox.state = settings.useTabs ? .off : .on
        whitespacePopup.selectItem(at: Self.whitespaceModes.firstIndex { $0.value == settings.effectiveRenderWhitespace } ?? 2)
        autoClosingTagsBox.state = settings.effectiveAutoClosingTags ? .on : .off
        linkedEditingBox.state = settings.effectiveLinkedEditing ? .on : .off
        parameterHintsBox.state = settings.effectiveInlineParameterHints ? .on : .off
        gitBlameBox.state = settings.effectiveGitBlame ? .on : .off
        let hidden = Set(settings.effectiveFilesExclude)
        for box in excludeBoxes { box.state = hidden.contains(box.title) ? .on : .off }
        autoSavePopup.selectItem(at: Self.autoSaveModes.firstIndex { $0.value == settings.effectiveAutoSave } ?? 0)
        autoSaveDelay.doubleValue = settings.effectiveAutoSaveDelay * 1000
        updateAutoSaveDelay()
    }

    private func updateAutoSaveDelay() {
        autoSaveDelayLabel.stringValue = "\(Int(autoSaveDelay.doubleValue)) ms"
        let afterDelay = Self.autoSaveModes[max(autoSavePopup.indexOfSelectedItem, 0)].value == .afterDelay
        autoSaveDelay.isEnabled = afterDelay && autoSavePopup.isEnabled
        autoSaveDelayLabel.textColor = autoSaveDelay.isEnabled ? .labelColor : .tertiaryLabelColor
    }

    private func showSettingsError(_ message: String) {
        settingsStatus.textColor = .systemRed
        settingsStatus.stringValue = message
    }

    /// Writes one key to settings.json and lets open windows pick it up.
    private func store(_ value: Any, _ key: String, in section: String = "editor") {
        do {
            try GalaxySettings.write(value, key: key, section: section, to: settingsURL)
            onChange()
        } catch {
            showSettingsError("Could not save \(section).\(key): \(error.localizedDescription)")
            syncControls()
        }
    }

    @objc private func saveTabSize(_ sender: Any?) {
        guard let size = tabSizePopup.titleOfSelectedItem.flatMap(Int.init) else { return }
        store(size, "tabSize")
    }

    @objc private func saveInsertSpaces(_ sender: Any?) { store(insertSpacesBox.state == .on, "insertSpaces") }
    @objc private func saveAutoClosingTags(_ sender: Any?) { store(autoClosingTagsBox.state == .on, "autoClosingTags") }
    @objc private func saveLinkedEditing(_ sender: Any?) { store(linkedEditingBox.state == .on, "linkedEditing") }
    @objc private func saveGitBlame(_ sender: Any?) { store(gitBlameBox.state == .on, "gitBlame") }
    @objc private func saveParameterHints(_ sender: Any?) {
        store(parameterHintsBox.state == .on, "inlineParameterHints")
    }

    @objc private func saveFilesExclude(_ sender: Any?) {
        guard let box = sender as? NSButton else { return }
        do {
            try GalaxySettings.writeFilesExclude(glob: box.title, enabled: box.state == .on, to: settingsURL)
            onChange()
        } catch {
            showSettingsError("Could not save files.exclude: \(error.localizedDescription)")
            syncControls()
        }
    }

    @objc private func saveWhitespace(_ sender: Any?) {
        store(Self.whitespaceModes[max(whitespacePopup.indexOfSelectedItem, 0)].value.rawValue, "renderWhitespace")
    }

    @objc private func saveAutoSave(_ sender: Any?) {
        updateAutoSaveDelay()
        store(Self.autoSaveModes[max(autoSavePopup.indexOfSelectedItem, 0)].value.rawValue, "autoSave", in: "files")
    }

    @objc private func saveAutoSaveDelay(_ sender: Any?) {
        updateAutoSaveDelay()
        store(Int(autoSaveDelay.doubleValue), "autoSaveDelay", in: "files")
    }

    @objc private func saveFolderIcons(_ sender: Any?) {
        UserDefaults.standard.set(folderIconsBox.state == .on, forKey: ExplorerIcon.showFolderIconsKey)
        onChange()
    }

    @objc private func saveViewToggle(_ sender: Any?) {
        guard let box = sender as? NSButton,
              let index = [minimapBox, wordWrapBox, breadcrumbsBox, stickyScrollBox].firstIndex(of: box) else { return }
        UserDefaults.standard.set(box.state == .on, forKey: Self.viewDefaults[index].key)
        onChange()
    }

    private func tab(_ title: String, symbol: String, pane: NSView) -> NSTabViewItem {
        let controller = NSViewController()
        controller.view = pane
        controller.title = title
        let item = NSTabViewItem(viewController: controller)
        item.label = title
        item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)
        return item
    }

    /// A pane holding `stack` with the usual Settings margins.
    private func pane(_ stack: NSStackView) -> NSView {
        let view = NSView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: view.topAnchor, constant: 20),
            stack.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -20),
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
        ])
        return view
    }

    private func buildGeneralPane() -> NSView {
        let stack = NSStackView()
        appearance.addItems(withTitles: Theme.names)
        appearance.selectItem(withTitle: Theme.names.contains(Theme.selectedName) ? Theme.selectedName : Theme.systemName)
        appearance.target = self; appearance.action = #selector(saveAppearance(_:))
        let installedFonts = Self.installedFontFamilies()
        font.addItems(withTitles: ["System Monospaced"] + installedFonts)
        let savedFont = UserDefaults.standard.string(forKey: "EditorFontName") ?? "System Monospaced"
        font.selectItem(withTitle: installedFonts.contains(savedFont) || savedFont == "System Monospaced" ? savedFont : "System Monospaced")
        font.target = self; font.action = #selector(saveFont(_:))
        let size = UserDefaults.standard.object(forKey: "EditorFontSize") as? Double ?? 13
        fontSize.minValue = 10; fontSize.maxValue = 24; fontSize.increment = 1; fontSize.doubleValue = size
        fontSize.target = self; fontSize.action = #selector(saveFontSize(_:))
        updateFontSizeLabel()

        stack.addArrangedSubview(row("Theme", appearance))
        stack.addArrangedSubview(row("Editor font", font))
        let sizeRow = NSStackView(views: [NSTextField(labelWithString: "Font size"), fontSize, fontSizeLabel])
        sizeRow.spacing = 12
        sizeRow.arrangedSubviews[0].widthAnchor.constraint(equalToConstant: 120).isActive = true
        stack.addArrangedSubview(sizeRow)
        return pane(stack)
    }

    private func buildAIPane() -> NSView {
        let stack = NSStackView()
        // AI commit message: OpenAI-compatible endpoint
        let aiTitle = NSTextField(labelWithString: "AI Commit Message (OpenAI-compatible)")
        aiTitle.font = .systemFont(ofSize: 12, weight: .semibold)
        stack.addArrangedSubview(aiTitle)

        configure(baseURLField, key: AICommitService.Keys.baseURL,
                  placeholder: AICommitService.defaultBaseURL, label: "AI base URL")
        stack.addArrangedSubview(row("Base URL", baseURLField))

        modelPopup.target = self
        modelPopup.action = #selector(saveModel(_:))
        modelPopup.setAccessibilityLabel("AI model")
        modelPopup.translatesAutoresizingMaskIntoConstraints = false
        modelPopup.widthAnchor.constraint(equalToConstant: 208).isActive = true
        modelRefreshButton.bezelStyle = .texturedRounded
        modelRefreshButton.image = NSImage(systemSymbolName: "arrow.clockwise", accessibilityDescription: "Reload models")
        modelRefreshButton.imagePosition = .imageOnly
        modelRefreshButton.toolTip = "Reload models from the server"
        modelRefreshButton.target = self
        modelRefreshButton.action = #selector(reloadModels(_:))
        modelRefreshButton.widthAnchor.constraint(equalToConstant: 26).isActive = true
        let modelControls = NSStackView(views: [modelPopup, modelRefreshButton])
        modelControls.spacing = 4
        stack.addArrangedSubview(row("Model", modelControls))
        showModels([], selecting: AICommitService.shared.model)

        modelStatus.font = .systemFont(ofSize: 11)
        modelStatus.textColor = .secondaryLabelColor
        modelStatus.lineBreakMode = .byTruncatingTail
        modelStatus.translatesAutoresizingMaskIntoConstraints = false
        modelStatus.widthAnchor.constraint(equalToConstant: 238).isActive = true
        stack.addArrangedSubview(row("", modelStatus))

        configure(apiKeyField, key: AICommitService.Keys.apiKey,
                  placeholder: "sk-… (optional for local servers)", label: "AI API key")
        stack.addArrangedSubview(row("API Key", apiKeyField))

        let hint = NSTextField(wrappingLabelWithString: "Uses <Base URL>/chat/completions. Works with OpenAI, OpenRouter, LM Studio, Ollama (http://localhost:11434/v1) and other compatible servers.")
        hint.font = .systemFont(ofSize: 11)
        hint.textColor = .secondaryLabelColor
        hint.preferredMaxLayoutWidth = 370
        hint.widthAnchor.constraint(equalToConstant: 370).isActive = true
        stack.addArrangedSubview(hint)
        return pane(stack)
    }

    // MARK: AI model picker

    /// Fills the popup. The saved model is always present, even when the
    /// server does not list it, so a choice is never silently replaced.
    private func showModels(_ models: [String], selecting selected: String) {
        modelPopup.removeAllItems()
        var titles = models
        if !titles.contains(selected) { titles.insert(selected, at: 0) }
        modelPopup.addItems(withTitles: titles)
        if !models.isEmpty, !models.contains(selected) {
            modelPopup.item(withTitle: selected)?.toolTip = "Not offered by this server"
        }
        modelPopup.selectItem(withTitle: selected)
    }

    private func loadModels() {
        modelLoadTask?.cancel()
        modelRefreshButton.isEnabled = false
        modelStatus.textColor = .secondaryLabelColor
        modelStatus.stringValue = "Loading models…"
        modelLoadTask = Task { [weak self] in
            let result: Result<[String], Error>
            do { result = .success(try await AICommitService.shared.fetchModels()) } catch { result = .failure(error) }
            guard let self, !Task.isCancelled else { return }
            self.modelRefreshButton.isEnabled = true
            let selected = AICommitService.shared.model
            switch result {
            case .success(let models):
                self.showModels(models, selecting: selected)
                if models.isEmpty {
                    self.modelStatus.stringValue = "Server returned no models."
                } else if models.contains(selected) {
                    self.modelStatus.stringValue = "\(models.count) models available."
                } else {
                    self.modelStatus.textColor = .systemOrange
                    self.modelStatus.stringValue = "'\(selected)' is not on this server."
                }
            case .failure(let error):
                self.showModels([], selecting: selected)
                self.modelStatus.textColor = .systemRed
                self.modelStatus.stringValue = "Could not load models: \(error)"
            }
        }
    }

    @objc private func reloadModels(_ sender: Any?) { loadModels() }

    @objc private func saveModel(_ sender: Any?) {
        guard let model = modelPopup.titleOfSelectedItem, !model.isEmpty else { return }
        UserDefaults.standard.set(model, forKey: AICommitService.Keys.model)
        modelStatus.textColor = .secondaryLabelColor
        modelStatus.stringValue = "Using \(model)."
    }

    private func row(_ title: String, _ control: NSView) -> NSStackView {
        let label = NSTextField(labelWithString: title)
        label.translatesAutoresizingMaskIntoConstraints = false
        label.widthAnchor.constraint(equalToConstant: 120).isActive = true
        let row = NSStackView(views: [label, control])
        row.spacing = 12
        return row
    }

    @objc private func saveAppearance(_ sender: Any?) {
        Theme.select(appearance.titleOfSelectedItem ?? Theme.systemName)
        onChange()
    }

    @objc private func saveFont(_ sender: Any?) {
        UserDefaults.standard.set(font.titleOfSelectedItem, forKey: "EditorFontName")
        onChange()
    }

    @objc private func saveFontSize(_ sender: Any?) {
        UserDefaults.standard.set(fontSize.doubleValue, forKey: "EditorFontSize")
        updateFontSizeLabel()
        onChange()
    }

    private func configure(_ field: NSTextField, key: String, placeholder: String, label: String) {
        field.placeholderString = placeholder
        field.stringValue = UserDefaults.standard.string(forKey: key) ?? ""
        field.identifier = NSUserInterfaceItemIdentifier(key)
        field.delegate = self
        field.setAccessibilityLabel(label)
        field.translatesAutoresizingMaskIntoConstraints = false
        field.widthAnchor.constraint(equalToConstant: 238).isActive = true
    }

    /// Saves each AI field as it is typed; an empty field falls back to the default.
    func controlTextDidChange(_ obj: Notification) {
        guard let field = obj.object as? NSTextField, let key = field.identifier?.rawValue,
              [AICommitService.Keys.baseURL, AICommitService.Keys.apiKey].contains(key)
        else { return }
        let value = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        if value.isEmpty {
            UserDefaults.standard.removeObject(forKey: key)
        } else {
            UserDefaults.standard.set(value, forKey: key)
        }
        // Base URL or key changed: reload the list once typing pauses.
        modelReloadWork?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.loadModels() }
        modelReloadWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.8, execute: work)
    }

    private func updateFontSizeLabel() { fontSizeLabel.stringValue = "\(Int(fontSize.doubleValue)) pt" }

    private static func installedFontFamilies() -> [String] {
        NSFontManager.shared.availableFontFamilies
            .filter { $0 != ".AppleSystemUIFont" && $0 != ".SFNSDisplay" && NSFont(name: $0, size: 13) != nil }
            .sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending }
    }
}

extension PreferencesWindowController: NSWindowDelegate {
    func windowDidBecomeKey(_ notification: Notification) { syncControls() }
}

/// Toolbar tabs whose window grows or shrinks to the selected pane, keeping
/// the title bar in place, like the system Settings windows.
@MainActor
final class SettingsTabViewController: NSTabViewController {
    override func tabView(_ tabView: NSTabView, didSelect tabViewItem: NSTabViewItem?) {
        super.tabView(tabView, didSelect: tabViewItem)
        fitWindow(to: tabViewItem, animate: view.window?.isVisible ?? false)
    }

    func fitWindow(to item: NSTabViewItem?, animate: Bool = false) {
        guard let window = view.window, let pane = item?.viewController?.view else { return }
        let size = pane.fittingSize
        let contentRect = window.contentRect(forFrameRect: window.frame)
        // The toolbar sits outside the content view; keep the top edge fixed.
        let chrome = window.frame.height - contentRect.height
        var frame = window.frame
        frame.size = NSSize(width: max(size.width, 440), height: size.height + chrome)
        frame.origin.y = window.frame.maxY - frame.height
        window.setFrame(frame, display: true, animate: animate)
    }
}
