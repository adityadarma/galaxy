import AppKit

/// Navigator switcher at the top of the sidebar, in the Xcode style: a row
/// of icons that picks what the sidebar shows. Search and Source Control are
/// present but disabled until those features exist.
@MainActor
final class NavigatorBar: NSView {
    enum Item: Int, CaseIterable {
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

        var isAvailable: Bool { self == .explorer }

        var tooltip: String {
            switch self {
            case .explorer: "Explorer (⇧⌘E)"
            case .search: "Search: planned for M5"
            case .sourceControl: "Source Control: planned for M6"
            }
        }
    }

    static let height: CGFloat = 30

    let control = NSSegmentedControl()
    var onSelect: ((Item) -> Void)?
    var selected: Item? {
        Item(rawValue: control.selectedSegment)
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        control.segmentCount = Item.allCases.count
        control.trackingMode = .selectOne
        control.segmentStyle = .texturedSquare
        control.controlSize = .regular
        control.selectedSegmentBezelColor = Theme.accent
        for item in Item.allCases {
            let i = item.rawValue
            control.setImage(NSImage(systemSymbolName: item.symbol, accessibilityDescription: item.title), forSegment: i)
            control.setImageScaling(.scaleProportionallyDown, forSegment: i)
            control.setToolTip(item.tooltip, forSegment: i)
            control.setEnabled(item.isAvailable, forSegment: i)
            control.setWidth(34, forSegment: i)
        }
        control.selectedSegment = Item.explorer.rawValue
        control.target = self
        control.action = #selector(changed(_:))
        control.setAccessibilityIdentifier("navigator")
        control.setAccessibilityLabel("Navigator")
        control.translatesAutoresizingMaskIntoConstraints = false
        addSubview(control)
        NSLayoutConstraint.activate([
            control.centerXAnchor.constraint(equalTo: centerXAnchor),
            control.centerYAnchor.constraint(equalTo: centerYAnchor),
            heightAnchor.constraint(equalToConstant: Self.height),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func isEnabled(_ item: Item) -> Bool { control.isEnabled(forSegment: item.rawValue) }

    func select(_ item: Item) {
        control.selectedSegment = item.rawValue
        onSelect?(item)
    }

    @objc private func changed(_ sender: NSSegmentedControl) {
        guard let item = selected else { return }
        onSelect?(item)
    }
}
