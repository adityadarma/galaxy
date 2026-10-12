import AppKit

/// Plain strip in the (transparent) title bar: holds the traffic lights and
/// drags the window. On the right, like VS Code's layout controls, two
/// buttons show and hide the sidebar (⌘B) and the bottom panel (⌘J).
@MainActor
final class TitleBar: NSView {
    var onToggleSidebar: (() -> Void)?
    var onTogglePanel: (() -> Void)?

    let sidebarButton = TitleBar.toggle(symbol: "sidebar.left", title: "Toggle Primary Side Bar (⌘B)")
    let panelButton = TitleBar.toggle(symbol: "rectangle.bottomthird.inset.filled", title: "Toggle Panel (⌘J)")
    private let projectTitle = NSTextField(labelWithString: "")

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityElement(false)

        let separator = NSBox()
        separator.boxType = .separator
        separator.translatesAutoresizingMaskIntoConstraints = false
        addSubview(separator)

        sidebarButton.target = self
        sidebarButton.action = #selector(toggleSidebar(_:))
        sidebarButton.setAccessibilityIdentifier("toggle-sidebar")
        panelButton.target = self
        panelButton.action = #selector(togglePanel(_:))
        panelButton.setAccessibilityIdentifier("toggle-panel")
        let buttons = NSStackView(views: [sidebarButton, panelButton])
        buttons.orientation = .horizontal
        buttons.spacing = 4
        buttons.translatesAutoresizingMaskIntoConstraints = false
        addSubview(buttons)

        projectTitle.font = .systemFont(ofSize: 13, weight: .medium)
        projectTitle.textColor = .secondaryLabelColor
        projectTitle.lineBreakMode = .byTruncatingMiddle
        projectTitle.alignment = .center
        projectTitle.translatesAutoresizingMaskIntoConstraints = false
        addSubview(projectTitle)

        NSLayoutConstraint.activate([
            separator.bottomAnchor.constraint(equalTo: bottomAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
            buttons.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            buttons.centerYAnchor.constraint(equalTo: centerYAnchor),
            projectTitle.centerXAnchor.constraint(equalTo: centerXAnchor),
            projectTitle.centerYAnchor.constraint(equalTo: centerYAnchor),
            projectTitle.widthAnchor.constraint(lessThanOrEqualTo: widthAnchor, multiplier: 0.45),
        ])
    }

    func setProjectTitle(_ name: String?) { projectTitle.stringValue = name ?? "" }

    /// Filled when the part is shown, as VS Code tints its layout controls.
    func setSidebarVisible(_ visible: Bool) {
        sidebarButton.state = visible ? .on : .off
        sidebarButton.contentTintColor = visible ? .labelColor : .secondaryLabelColor
    }

    func setPanelVisible(_ visible: Bool) {
        panelButton.state = visible ? .on : .off
        panelButton.contentTintColor = visible ? .labelColor : .secondaryLabelColor
    }

    @objc private func toggleSidebar(_ sender: Any?) { onToggleSidebar?() }
    @objc private func togglePanel(_ sender: Any?) { onTogglePanel?() }

    private static func toggle(symbol: String, title: String) -> NSButton {
        let b = NSButton()
        b.setButtonType(.pushOnPushOff)
        b.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)?
            .withSymbolConfiguration(.init(pointSize: 13, weight: .regular))
        b.isBordered = false
        b.imagePosition = .imageOnly
        b.contentTintColor = .secondaryLabelColor
        b.toolTip = title
        b.setAccessibilityLabel(title)
        b.translatesAutoresizingMaskIntoConstraints = false
        b.widthAnchor.constraint(equalToConstant: 24).isActive = true
        b.heightAnchor.constraint(equalToConstant: 22).isActive = true
        return b
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Dragging is done in `mouseDown`, so double-clicks reach this view
    /// instead of the system's zoom handling.
    override var mouseDownCanMoveWindow: Bool { false }

    override func mouseDown(with event: NSEvent) {
        if event.clickCount == 1 { window?.performDrag(with: event) }
    }

    /// Frame to return to after a double-click filled the screen.
    private(set) var restoreFrame: NSRect?

    /// Double-click fills the screen's usable area (menu bar and Dock
    /// excluded), not full screen; again restores the previous frame.
    /// Follows System Settings when it asks for minimize or nothing.
    override func mouseUp(with event: NSEvent) {
        guard event.clickCount == 2 else { return super.mouseUp(with: event) }
        switch UserDefaults.standard.string(forKey: "AppleActionOnDoubleClick") {
        case "Minimize": window?.performMiniaturize(nil)
        case "None": break
        default: toggleFill()
        }
    }

    func toggleFill() {
        guard let window, let screen = window.screen ?? NSScreen.main else { return }
        let target = screen.visibleFrame
        if let restoreFrame, window.frame.equalTo(target) {
            window.setFrame(restoreFrame, display: true, animate: true)
            self.restoreFrame = nil
        } else {
            restoreFrame = window.frame
            window.setFrame(target, display: true, animate: true)
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        Theme.chrome.setFill()
        dirtyRect.intersection(bounds).fill()
    }
}
