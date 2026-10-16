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

    init(onChange: @escaping () -> Void) {
        self.onChange = onChange
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 420, height: 420),
                              styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Settings"
        window.isReleasedWhenClosed = false
        super.init(window: window)
        build()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    private func build() {
        guard let content = window?.contentView else { return }
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        stack.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(stack)

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

        // Divider
        let separator = NSBox()
        separator.boxType = .separator
        separator.translatesAutoresizingMaskIntoConstraints = false
        stack.addArrangedSubview(separator)
        separator.widthAnchor.constraint(equalToConstant: 370).isActive = true

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

        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: content.topAnchor, constant: 20),
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(lessThanOrEqualTo: content.trailingAnchor, constant: -24),
        ])
        loadModels()
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
