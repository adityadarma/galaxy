import AppKit

@MainActor
final class PreferencesWindowController: NSWindowController {
    private let onChange: () -> Void
    private let appearance = NSPopUpButton(frame: .zero, pullsDown: false)
    private let font = NSPopUpButton(frame: .zero, pullsDown: false)
    private let fontSize = NSStepper(frame: .zero)
    private let fontSizeLabel = NSTextField(labelWithString: "")

    init(onChange: @escaping () -> Void) {
        self.onChange = onChange
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 390, height: 190),
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
        stack.spacing = 16
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
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: content.topAnchor, constant: 24),
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(lessThanOrEqualTo: content.trailingAnchor, constant: -24),
        ])
    }

    private func row(_ title: String, _ control: NSView) -> NSStackView {
        let row = NSStackView(views: [NSTextField(labelWithString: title), control])
        row.spacing = 12
        row.arrangedSubviews[0].widthAnchor.constraint(equalToConstant: 120).isActive = true
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

    private func updateFontSizeLabel() { fontSizeLabel.stringValue = "\(Int(fontSize.doubleValue)) pt" }

    private static func installedFontFamilies() -> [String] {
        NSFontManager.shared.availableFontFamilies
            .filter { $0 != ".AppleSystemUIFont" && $0 != ".SFNSDisplay" && NSFont(name: $0, size: 13) != nil }
            .sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending }
    }
}
