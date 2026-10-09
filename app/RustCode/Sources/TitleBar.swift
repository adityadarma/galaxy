import AppKit

/// Full-width bar in the (transparent) title bar, in the JetBrains "New UI"
/// style: project widget on the left after the traffic lights, the current
/// file centered like VSCode's command center, layout toggles on the right.
@MainActor
final class TitleBar: NSView {
    /// Room for the traffic lights.
    static let trafficLightInset: CGFloat = 80

    /// Project name; clicking it opens a folder.
    let projectButton = NSButton()
    /// "main.rs — workspace", centered.
    let titleLabel = NSTextField(labelWithString: "")
    let sidebarButton = NSButton()

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityElement(false)

        projectButton.isBordered = false
        projectButton.image = NSImage(systemSymbolName: "folder", accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 11, weight: .medium))
        projectButton.imagePosition = .imageLeading
        projectButton.imageHugsTitle = true
        projectButton.font = .systemFont(ofSize: 12, weight: .semibold)
        projectButton.contentTintColor = Theme.accent
        projectButton.toolTip = "Open Folder… (⇧⌘O)"
        projectButton.setAccessibilityIdentifier("project-widget")
        projectButton.translatesAutoresizingMaskIntoConstraints = false
        projectButton.lineBreakMode = .byTruncatingMiddle
        projectButton.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        setProject(nil)

        titleLabel.font = .systemFont(ofSize: 12)
        titleLabel.textColor = .secondaryLabelColor
        titleLabel.alignment = .center
        titleLabel.lineBreakMode = .byTruncatingMiddle
        titleLabel.setAccessibilityIdentifier("window-title")
        titleLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        titleLabel.translatesAutoresizingMaskIntoConstraints = false

        sidebarButton.image = NSImage(systemSymbolName: "sidebar.left", accessibilityDescription: "Toggle Sidebar")
        sidebarButton.isBordered = false
        sidebarButton.imagePosition = .imageOnly
        sidebarButton.contentTintColor = .secondaryLabelColor
        sidebarButton.toolTip = "Toggle Sidebar (⌘B)"
        sidebarButton.setAccessibilityIdentifier("toggle-sidebar")
        sidebarButton.translatesAutoresizingMaskIntoConstraints = false

        for view in [projectButton, titleLabel, sidebarButton] { addSubview(view) }

        let separator = NSBox()
        separator.boxType = .separator
        separator.translatesAutoresizingMaskIntoConstraints = false
        addSubview(separator)

        let centered = titleLabel.centerXAnchor.constraint(equalTo: centerXAnchor)
        centered.priority = .defaultHigh
        NSLayoutConstraint.activate([
            projectButton.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Self.trafficLightInset),
            projectButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            projectButton.widthAnchor.constraint(lessThanOrEqualToConstant: 220),
            centered,
            titleLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
            titleLabel.leadingAnchor.constraint(greaterThanOrEqualTo: projectButton.trailingAnchor, constant: 16),
            titleLabel.trailingAnchor.constraint(lessThanOrEqualTo: sidebarButton.leadingAnchor, constant: -16),
            sidebarButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            sidebarButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            separator.bottomAnchor.constraint(equalTo: bottomAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Empty areas still drag the window, as a title bar should.
    override var mouseDownCanMoveWindow: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        Theme.chrome.setFill()
        dirtyRect.intersection(bounds).fill()
    }

    func setProject(_ name: String?) {
        projectButton.title = name ?? "Open Folder…"
        projectButton.setAccessibilityLabel(name.map { "Project \($0)" } ?? "Open Folder")
    }

    func setTitle(_ title: String) {
        if titleLabel.stringValue != title { titleLabel.stringValue = title }
    }
}
