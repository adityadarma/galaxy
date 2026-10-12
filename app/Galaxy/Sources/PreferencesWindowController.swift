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
    private let modelField = NSTextField()
    private let apiKeyField = NSSecureTextField()

    init(onChange: @escaping () -> Void) {
        self.onChange = onChange
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 420, height: 380),
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

        appearance.addItems(withTitles: ["System", "Light", "Dark"])
        appearance.selectItem(withTitle: UserDefaults.standard.string(forKey: "Appearance") ?? "System")
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

        stack.addArrangedSubview(row("Appearance", appearance))
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

        configure(modelField, key: AICommitService.Keys.model,
                  placeholder: AICommitService.defaultModel, label: "AI model")
        stack.addArrangedSubview(row("Model", modelField))

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
        UserDefaults.standard.set(appearance.titleOfSelectedItem, forKey: "Appearance")
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
              [AICommitService.Keys.baseURL, AICommitService.Keys.model, AICommitService.Keys.apiKey].contains(key)
        else { return }
        let value = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        if value.isEmpty {
            UserDefaults.standard.removeObject(forKey: key)
        } else {
            UserDefaults.standard.set(value, forKey: key)
        }
    }

    private func updateFontSizeLabel() { fontSizeLabel.stringValue = "\(Int(fontSize.doubleValue)) pt" }

    private static func installedFontFamilies() -> [String] {
        NSFontManager.shared.availableFontFamilies
            .filter { $0 != ".AppleSystemUIFont" && $0 != ".SFNSDisplay" && NSFont(name: $0, size: 13) != nil }
            .sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending }
    }
}
