import AppKit

@MainActor
final class SignaturePopup: NSView {
    private let label = NSTextField(wrappingLabelWithString: "")
    private let documentation = NSTextField(wrappingLabelWithString: "")

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 5
        layer?.borderWidth = 1
        layer?.borderColor = NSColor.separatorColor.cgColor
        layer?.shadowOpacity = 0.22
        layer?.shadowRadius = 8
        label.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        documentation.font = .systemFont(ofSize: 11)
        documentation.textColor = .secondaryLabelColor
        for view in [label, documentation] { view.translatesAutoresizingMaskIntoConstraints = false; addSubview(view) }
        NSLayoutConstraint.activate([
            label.topAnchor.constraint(equalTo: topAnchor, constant: 8), label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 10), label.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            documentation.topAnchor.constraint(equalTo: label.bottomAnchor, constant: 4), documentation.leadingAnchor.constraint(equalTo: label.leadingAnchor), documentation.trailingAnchor.constraint(equalTo: label.trailingAnchor), documentation.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -8),
        ])
        setAccessibilityElement(true); setAccessibilityRole(.helpTag); setAccessibilityIdentifier("signature-popup")
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func show(_ signature: SignatureInfo) {
        label.attributedStringValue = highlighted(signature)
        documentation.stringValue = signature.documentation ?? ""
        documentation.isHidden = signature.documentation == nil
    }

    private func highlighted(_ signature: SignatureInfo) -> NSAttributedString {
        let text = signature.label
        let output = NSMutableAttributedString(string: text, attributes: [.foregroundColor: NSColor.labelColor])
        guard signature.activeParameter < UInt64(signature.parameters.count), let parameter = signature.parameters[safe: Int(signature.activeParameter)] else { return output }
        if let range = text.range(of: parameter) { output.addAttribute(.foregroundColor, value: NSColor.controlAccentColor, range: NSRange(range, in: text)); output.addAttribute(.font, value: NSFont.boldSystemFont(ofSize: 12), range: NSRange(range, in: text)) }
        return output
    }

    override func updateLayer() { layer?.backgroundColor = Theme.sidebar.cgColor }
}

private extension Collection {
    subscript(safe index: Index) -> Element? { indices.contains(index) ? self[index] : nil }
}
