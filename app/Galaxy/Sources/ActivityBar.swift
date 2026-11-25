import AppKit

/// Short text for change counts: 0-999 as is, then thousands. An exact
/// multiple of 1000 reads "1k", anything above it "1k+" (2500 is "2k+").
enum CountFormat {
    static func compact(_ count: Int) -> String {
        guard count >= 1000 else { return "\(max(count, 0))" }
        let thousands = count / 1000
        return count % 1000 == 0 ? "\(thousands)k" : "\(thousands)k+"
    }
}

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
            case .sourceControl: "point.topleft.down.curvedto.point.bottomright.up"
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
    }

    func isEnabled(_ item: Item) -> Bool { buttons[item]?.isEnabled ?? false }

    /// Highlights `item`, or nothing while the sidebar is hidden.
    func setActive(_ item: Item?) {
        active = item
        for (key, button) in buttons { button.isActive = key == item }
    }

    func setBadge(_ value: Int, for item: Item) {
        buttons[item]?.badgeValue = value
    }

    @objc private func clicked(_ sender: ActivityBarButton) {
        onSelect?(sender.item)
    }
}

@MainActor
final class ActivityBarButton: NSButton {
    /// Side of the rounded square behind the active icon, as in JetBrains' tool window bar.
    static let activeSquareSide: CGFloat = 32

    let item: ActivityBar.Item
    var isActive = false { didSet { if isActive != oldValue { refresh() } } }
    var badgeValue = 0 { didSet { if badgeValue != oldValue { needsDisplay = true } } }

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
            contentTintColor = isActive ? .labelColor : .secondaryLabelColor
        }
        setAccessibilityValue(isActive ? 1 : 0)
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        if isActive {
            let side = min(Self.activeSquareSide, bounds.width, bounds.height)
            let square = NSRect(x: bounds.midX - side / 2, y: bounds.midY - side / 2, width: side, height: side)
            NSColor.labelColor.withAlphaComponent(0.12).setFill()
            NSBezierPath(roundedRect: square, xRadius: 7, yRadius: 7).fill()
        }
        super.draw(dirtyRect)
        guard badgeValue > 0 else { return }
        let text = CountFormat.compact(badgeValue)
        let font = NSFont.monospacedDigitSystemFont(ofSize: 10, weight: .semibold)
        let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: NSColor.white]
        let size = (text as NSString).size(withAttributes: attributes)
        let badgeWidth = max(20, ceil(size.width) + 10)
        let rect = NSRect(x: bounds.maxX - badgeWidth - 3, y: bounds.maxY - 18, width: badgeWidth, height: 18)
        Theme.accent.setFill()
        NSBezierPath(roundedRect: rect, xRadius: 9, yRadius: 9).fill()
        let origin = NSPoint(x: rect.midX - size.width / 2, y: rect.midY - size.height / 2)
        (text as NSString).draw(at: origin, withAttributes: attributes)
    }
}
