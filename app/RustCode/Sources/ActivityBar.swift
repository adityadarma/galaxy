import AppKit

/// VSCode-style activity bar: a narrow icon strip on the far left. The
/// Explorer icon shows or hides the sidebar; the other icons are present
/// but disabled until their features exist.
@MainActor
final class ActivityBar: NSView {
    enum Item: CaseIterable {
        case explorer, search, sourceControl

        var symbol: String {
            switch self {
            case .explorer: "doc.on.doc"
            case .search: "magnifyingglass"
            case .sourceControl: "arrow.triangle.branch"
            }
        }

        var title: String {
            switch self {
            case .explorer: "Explorer"
            case .search: "Search"
            case .sourceControl: "Source Control"
            }
        }

        var isAvailable: Bool { self == .explorer }

        var tooltip: String {
            switch self {
            case .explorer: "Explorer (⇧⌘E)"
            case .search: "Search: planned for M5"
            case .sourceControl: "Source Control: planned for M6"
            }
        }
    }

    static let width: CGFloat = 48

    var onSelect: ((Item) -> Void)?
    private(set) var buttons: [Item: ActivityBarButton] = [:]

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.spacing = 0
        stack.alignment = .centerX
        stack.translatesAutoresizingMaskIntoConstraints = false
        for item in Item.allCases {
            let button = ActivityBarButton(item: item)
            button.target = self
            button.action = #selector(clicked(_:))
            buttons[item] = button
            stack.addArrangedSubview(button)
            button.widthAnchor.constraint(equalToConstant: Self.width).isActive = true
            button.heightAnchor.constraint(equalToConstant: Self.width).isActive = true
        }
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 4),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
        setAccessibilityRole(.toolbar)
        setAccessibilityLabel("Activity Bar")
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isOpaque: Bool { true }

    override func updateLayer() {
        layer?.backgroundColor = NSColor.underPageBackgroundColor.cgColor
    }

    override var wantsUpdateLayer: Bool { true }

    /// Highlights `item` (or none when the sidebar is collapsed).
    func setActive(_ item: Item?) {
        for (key, button) in buttons { button.isActive = (key == item) }
    }

    @objc private func clicked(_ sender: ActivityBarButton) {
        onSelect?(sender.item)
    }
}

@MainActor
final class ActivityBarButton: NSButton {
    let item: ActivityBar.Item
    private let indicator = NSView()

    var isActive = false {
        didSet { refresh() }
    }

    init(item: ActivityBar.Item) {
        self.item = item
        super.init(frame: .zero)
        isBordered = false
        imagePosition = .imageOnly
        imageScaling = .scaleNone
        image = NSImage(systemSymbolName: item.symbol, accessibilityDescription: item.title)?
            .withSymbolConfiguration(.init(pointSize: 20, weight: .regular))
        toolTip = item.tooltip
        isEnabled = item.isAvailable
        setAccessibilityLabel(item.title)
        setAccessibilityIdentifier("activity-\(item.title.lowercased().replacingOccurrences(of: " ", with: "-"))")
        indicator.wantsLayer = true
        indicator.layer?.backgroundColor = NSColor.controlAccentColor.cgColor
        indicator.isHidden = true
        addSubview(indicator)
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func layout() {
        super.layout()
        indicator.frame = NSRect(x: 0, y: 6, width: 2, height: max(0, bounds.height - 12))
    }

    private func refresh() {
        indicator.isHidden = !isActive
        if !item.isAvailable {
            contentTintColor = .tertiaryLabelColor
        } else {
            contentTintColor = isActive ? .labelColor : .secondaryLabelColor
        }
    }
}
