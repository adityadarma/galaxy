import AppKit

/// Narrow icon strip on the far left, as in VSCode and JetBrains' tool
/// window bar. Clicking the active item hides the sidebar.
@MainActor
final class ActivityBar: NSView {
    enum Item: CaseIterable {
        case explorer, search, sourceControl

        var symbol: String {
            switch self {
            case .explorer: "folder"
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

        var identifier: String {
            "activity-" + title.lowercased().replacingOccurrences(of: " ", with: "-")
        }

        var isAvailable: Bool { true }

        var tooltip: String {
            switch self {
            case .explorer: "Explorer (⇧⌘E)"
            case .search: "Search in Workspace (⇧⌘F)"
            case .sourceControl: "Source Control (⌃⇧G)"
            }
        }
    }

    static let width: CGFloat = 44

    var onSelect: ((Item) -> Void)?
    private(set) var buttons: [Item: ActivityBarButton] = [:]
    private(set) var active: Item?

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityElement(true)
        setAccessibilityRole(.toolbar)
        setAccessibilityLabel("Activity Bar")
        setAccessibilityIdentifier("activity-bar")

        let stack = NSStackView()
        stack.orientation = .vertical
        stack.spacing = 2
        stack.alignment = .centerX
        stack.translatesAutoresizingMaskIntoConstraints = false
        for item in Item.allCases {
            let button = ActivityBarButton(item: item)
            button.target = self
            button.action = #selector(clicked(_:))
            buttons[item] = button
            stack.addArrangedSubview(button)
            button.widthAnchor.constraint(equalToConstant: Self.width).isActive = true
            button.heightAnchor.constraint(equalToConstant: 40).isActive = true
        }
        addSubview(stack)

        NSLayoutConstraint.activate([
            widthAnchor.constraint(equalToConstant: Self.width),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 6),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func draw(_ dirtyRect: NSRect) {
        Theme.chrome.setFill()
        dirtyRect.intersection(bounds).fill()
        // Right edge. Drawn rather than an NSBox: a vertical separator box
        // hugs its intrinsic height hard enough to shrink the window.
        NSColor.separatorColor.setFill()
        NSRect(x: bounds.maxX - 1, y: 0, width: 1, height: bounds.height).fill()
    }

    func isEnabled(_ item: Item) -> Bool { buttons[item]?.isEnabled ?? false }

    /// Highlights `item`, or nothing while the sidebar is hidden.
    func setActive(_ item: Item?) {
        active = item
        for (key, button) in buttons { button.isActive = key == item }
    }

    @objc private func clicked(_ sender: ActivityBarButton) {
        onSelect?(sender.item)
    }
}

@MainActor
final class ActivityBarButton: NSButton {
    let item: ActivityBar.Item
    var isActive = false { didSet { if isActive != oldValue { refresh() } } }

    init(item: ActivityBar.Item) {
        self.item = item
        super.init(frame: .zero)
        isBordered = false
        imagePosition = .imageOnly
        imageScaling = .scaleNone
        image = NSImage(systemSymbolName: item.symbol, accessibilityDescription: item.title)?
            .withSymbolConfiguration(.init(pointSize: 17, weight: .regular))
        toolTip = item.tooltip
        isEnabled = item.isAvailable
        setAccessibilityLabel(item.title)
        setAccessibilityIdentifier(item.identifier)
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    private func refresh() {
        if !item.isAvailable {
            contentTintColor = .tertiaryLabelColor
        } else {
            contentTintColor = isActive ? Theme.activityActive : .secondaryLabelColor
        }
        setAccessibilityValue(isActive ? 1 : 0)
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        if isActive {
            Theme.activityActive.setFill()
            NSRect(x: 0, y: 8, width: 2, height: max(0, bounds.height - 16)).fill()
        }
        super.draw(dirtyRect)
    }
}
