import AppKit

/// Plain strip in the (transparent) title bar: holds the traffic lights and
/// drags the window. In the centre, back and forward buttons sit beside a
/// search pill that opens the command palette. On the right, like VS Code's
/// layout controls, buttons show and hide the sidebar (⌘B) and the bottom
/// panel (⌘J).
@MainActor
final class TitleBar: NSView {
    /// Height of the bar. The system title bar is stretched to match in a
    /// window; full screen has none, so the bar keeps this height itself.
    static let minimumHeight: CGFloat = 38

    var onToggleSidebar: (() -> Void)?
    var onTogglePanel: (() -> Void)?
    var onBack: (() -> Void)?
    var onForward: (() -> Void)?
    var onSearch: (() -> Void)?

    let sidebarButton = TitleBar.toggle(symbol: "sidebar.left", title: "Toggle Primary Side Bar (⌘B)")
    let panelButton = TitleBar.toggle(symbol: "rectangle.bottomthird.inset.filled", title: "Toggle Panel (⌘J)")
    let backButton = TitleBar.toggle(symbol: "arrow.left", title: "Go Back")
    let forwardButton = TitleBar.toggle(symbol: "arrow.right", title: "Go Forward")
    let searchPill = SearchPill()
    private static let searchPlaceholder = "Search"

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityElement(false)

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

        backButton.target = self
        backButton.action = #selector(goBack(_:))
        backButton.setAccessibilityIdentifier("navigate-back")
        forwardButton.target = self
        forwardButton.action = #selector(goForward(_:))
        forwardButton.setAccessibilityIdentifier("navigate-forward")
        for b in [backButton, forwardButton] {
            b.setButtonType(.momentaryChange)
            b.isEnabled = false
        }

        searchPill.text = Self.searchPlaceholder
        searchPill.target = self
        searchPill.action = #selector(openSearch(_:))
        searchPill.setAccessibilityIdentifier("title-search")
        searchPill.setAccessibilityLabel("Search commands")
        searchPill.toolTip = "Command Palette (⇧⌘P)"

        let navigation = NSStackView(views: [backButton, forwardButton, searchPill])
        navigation.orientation = .horizontal
        navigation.spacing = 8
        navigation.alignment = .centerY
        navigation.translatesAutoresizingMaskIntoConstraints = false
        addSubview(navigation)

        NSLayoutConstraint.activate([
            buttons.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            buttons.centerYAnchor.constraint(equalTo: centerYAnchor),
            navigation.centerXAnchor.constraint(equalTo: centerXAnchor),
            navigation.centerYAnchor.constraint(equalTo: centerYAnchor),
            searchPill.widthAnchor.constraint(equalToConstant: 360).prioritized(.defaultHigh),
            searchPill.widthAnchor.constraint(lessThanOrEqualTo: widthAnchor, multiplier: 0.4),
            searchPill.heightAnchor.constraint(equalToConstant: 26),
        ])
    }

    func setProjectTitle(_ name: String?) {
        searchPill.text = (name?.isEmpty == false) ? name! : Self.searchPlaceholder
    }

    func setNavigation(canGoBack: Bool, canGoForward: Bool) {
        if backButton.isEnabled != canGoBack { backButton.isEnabled = canGoBack }
        if forwardButton.isEnabled != canGoForward { forwardButton.isEnabled = canGoForward }
    }

    /// Filled when the part is shown, as VS Code tints its layout controls.
    func setSidebarVisible(_ visible: Bool) {
        sidebarButton.state = visible ? .on : .off
        sidebarButton.contentTintColor = visible ? .labelColor : .secondaryLabelColor
    }

    func setPanelVisible(_ visible: Bool) {
        panelButton.state = visible ? .on : .off
        panelButton.contentTintColor = visible ? .labelColor : .secondaryLabelColor
    }

    @objc private func goBack(_ sender: Any?) { onBack?() }
    @objc private func goForward(_ sender: Any?) { onForward?() }
    @objc private func openSearch(_ sender: Any?) { onSearch?() }
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

private extension NSLayoutConstraint {
    func prioritized(_ p: NSLayoutConstraint.Priority) -> NSLayoutConstraint {
        priority = p
        return self
    }
}

/// Borderless rounded field in the title bar. Looks like a search box but
/// is a button: a click opens the command palette instead of taking text.
@MainActor
final class SearchPill: NSButton {
    override init(frame: NSRect) {
        super.init(frame: frame)
        isBordered = false
        setButtonType(.momentaryChange)
        alignment = .left
        font = .systemFont(ofSize: 13)
        lineBreakMode = .byTruncatingTail
        wantsLayer = true
        layer?.cornerRadius = 6
        translatesAutoresizingMaskIntoConstraints = false
        refreshColors()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    var text = "" {
        didSet { refreshColors() }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        refreshColors()
    }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .arrow)
    }

    /// Dynamic colours do not follow appearance changes on a layer by themselves.
    private func refreshColors() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(0.07).cgColor
        }
        let style = NSMutableParagraphStyle()
        style.alignment = .left
        style.lineBreakMode = .byTruncatingTail
        style.firstLineHeadIndent = 10
        style.headIndent = 10
        style.tailIndent = -10
        attributedTitle = NSAttributedString(string: text, attributes: [
            .font: NSFont.systemFont(ofSize: 13),
            .foregroundColor: NSColor.secondaryLabelColor,
            .paragraphStyle: style,
        ])
    }
}
