import AppKit

@MainActor
final class BreadcrumbBar: NSView {
    var onSelect: ((OutlineSymbol) -> Void)?
    private let stack = NSStackView()
    private var heightConstraint: NSLayoutConstraint!
    private var visibleSymbols: [OutlineSymbol] = []
    /// Off: the bar stays hidden whatever the cursor is in.
    var isEnabled = true

    override init(frame: NSRect) {
        super.init(frame: frame)
        stack.orientation = .horizontal
        stack.alignment = .centerY
        stack.spacing = 4
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        heightConstraint = heightAnchor.constraint(equalToConstant: 24)
        NSLayoutConstraint.activate([
            heightConstraint,
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 16),
            stack.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -12),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        setAccessibilityIdentifier("breadcrumbs")
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func show(symbols: [OutlineSymbol], atLine line: UInt64) {
        stack.arrangedSubviews.forEach { $0.removeFromSuperview() }
        let visible = (isEnabled ? symbols : [])
            .filter { $0.line <= line }
            .sorted { $0.line == $1.line ? $0.column < $1.column : $0.line < $1.line }
        visibleSymbols = visible
        heightConstraint.constant = visible.isEmpty ? 0 : 24
        isHidden = visible.isEmpty
        for (index, symbol) in visible.enumerated() {
            if index > 0 { stack.addArrangedSubview(NSTextField(labelWithString: "›")) }
            let button = NSButton(title: symbol.name, target: self, action: #selector(selectSymbol(_:)))
            button.bezelStyle = .inline
            button.isBordered = false
            button.font = .systemFont(ofSize: 12)
            button.contentTintColor = index == visible.count - 1 ? .labelColor : .secondaryLabelColor
            button.identifier = NSUserInterfaceItemIdentifier("breadcrumb-\(index)")
            stack.addArrangedSubview(button)
        }
        setAccessibilityValue(visible.last?.name)
    }

    func clear() { show(symbols: [], atLine: 0) }

    @objc private func selectSymbol(_ sender: NSButton) {
        guard let id = sender.identifier?.rawValue,
              let index = Int(id.split(separator: "-").last ?? ""),
              visibleSymbols.indices.contains(index) else { return }
        onSelect?(visibleSymbols[index])
    }
}
