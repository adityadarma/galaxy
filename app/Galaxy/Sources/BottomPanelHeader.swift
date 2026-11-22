import AppKit

/// The bottom panel's title row, laid out like VS Code's: flat uppercase
/// tabs on the left (the active one underlined), and on the right the
/// terminal picker and actions, then maximize and close.
@MainActor
final class BottomPanelHeader: NSView {
    enum Tab: Int, CaseIterable {
        case problems
        case output
        case debugConsole
        case testResults
        case terminal

        var title: String {
            switch self {
            case .problems: "PROBLEMS"
            case .output: "OUTPUT"
            case .debugConsole: "DEBUG CONSOLE"
            case .testResults: "TEST RESULTS"
            case .terminal: "TERMINAL"
            }
        }
    }

    static let height: CGFloat = 34

    var onSelect: ((Tab) -> Void)?
    var onNewTerminal: (() -> Void)?
    var onSplitTerminal: (() -> Void)?
    var onKillTerminal: (() -> Void)?
    /// Menu for the lone terminal's chip (the same one as a right-click).
    var chipMenuProvider: (() -> NSMenu?)? {
        get { terminalChip.menuProvider }
        set { terminalChip.menuProvider = newValue }
    }
    var onToggleMaximize: (() -> Void)?
    var onClose: (() -> Void)?
    private(set) var selectedTab: Tab = .terminal
    private var tabViews: [Tab: PanelTabView] = [:]
    private let tabStack = NSStackView()
    private let actionStack = NSStackView()
    /// Shown while there is a single terminal; from two on, the list at the
    /// panel's right edge takes over.
    let terminalChip = TerminalChipView()
    private lazy var splitButton = Self.tool("square.split.2x1", "Split Terminal", #selector(splitTerminal(_:)), self)
    private lazy var newButton = Self.tool("plus", "New Terminal", #selector(newTerminal(_:)), self)
    private lazy var killButton = Self.tool("trash", "Kill Terminal", #selector(killTerminal(_:)), self)
    private lazy var maximizeButton = Self.tool("chevron.up", "Maximize Panel Size", #selector(toggleMaximize(_:)), self)
    private lazy var closeButton = Self.tool("xmark", "Hide Panel", #selector(closePanel(_:)), self)
    private let divider = NSBox()

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setAccessibilityRole(.tabGroup)
        setAccessibilityLabel("Bottom panel")
        setAccessibilityIdentifier("bottom-panel-header")

        tabStack.orientation = .horizontal
        tabStack.spacing = 2
        tabStack.alignment = .centerY
        tabStack.translatesAutoresizingMaskIntoConstraints = false
        for tab in Tab.allCases {
            let view = PanelTabView(title: tab.title)
            view.setAccessibilityIdentifier("panel-tab-\(tab)")
            view.onClick = { [weak self] in
                guard let self else { return }
                self.select(tab)
                self.onSelect?(tab)
            }
            tabViews[tab] = view
            tabStack.addArrangedSubview(view)
        }

        terminalChip.setAccessibilityLabel("Terminal")
        terminalChip.setAccessibilityIdentifier("terminal-chip")
        divider.boxType = .separator

        actionStack.orientation = .horizontal
        actionStack.spacing = 4
        actionStack.alignment = .centerY
        actionStack.translatesAutoresizingMaskIntoConstraints = false
        for view in [terminalChip, newButton, splitButton, killButton, divider, maximizeButton, closeButton] as [NSView] {
            actionStack.addArrangedSubview(view)
        }
        actionStack.setCustomSpacing(10, after: killButton)
        actionStack.setCustomSpacing(8, after: terminalChip)
        actionStack.setCustomSpacing(10, after: divider)

        addSubview(tabStack)
        addSubview(actionStack)
        NSLayoutConstraint.activate([
            tabStack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            // Top-aligned, not tied to the header's height: the header
            // collapses to 0 when the panel is closed, and clips.
            tabStack.topAnchor.constraint(equalTo: topAnchor),
            actionStack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            actionStack.centerYAnchor.constraint(equalTo: tabStack.centerYAnchor),
            actionStack.leadingAnchor.constraint(greaterThanOrEqualTo: tabStack.trailingAnchor, constant: 12)
                .withPriority(.defaultHigh),
            divider.heightAnchor.constraint(equalToConstant: 16),
        ])
        select(.terminal)
        setTerminals([])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isFlipped: Bool { true }

    func select(_ tab: Tab) {
        selectedTab = tab
        for (key, view) in tabViews { view.isSelected = key == tab }
        updateTerminalActions()
    }

    /// The chip names a lone terminal and the header carries its split and
    /// kill buttons; from two terminals on, the list's rows carry them.
    private func updateTerminalActions() {
        let terminal = selectedTab == .terminal
        let single = terminal && terminalCount <= 1
        terminalChip.isHidden = !terminal || terminalCount != 1
        newButton.isHidden = !terminal
        splitButton.isHidden = !single
        killButton.isHidden = !single
        divider.isHidden = !terminal
    }

    func setProblemCount(_ count: Int) {
        tabViews[.problems]?.badge = count
    }

    private var terminalCount = 0

    /// The chip names the only terminal; with more, the panel's list does.
    func setTerminals(_ terminals: [TerminalSummary]) {
        terminalCount = terminals.count
        if let only = terminals.first, terminals.count == 1 {
            terminalChip.configure(name: only.name, icon: only.icon, tint: only.tint)
        }
        updateTerminalActions()
        killButton.isEnabled = !terminals.isEmpty
        splitButton.isEnabled = !terminals.isEmpty
    }

    func setMaximized(_ maximized: Bool) {
        let symbol = maximized ? "chevron.down" : "chevron.up"
        let title = maximized ? "Restore Panel Size" : "Maximize Panel Size"
        maximizeButton.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)?
            .withSymbolConfiguration(.init(pointSize: 12, weight: .regular))
        maximizeButton.toolTip = title
        maximizeButton.setAccessibilityLabel(title)
    }

    override func draw(_ dirtyRect: NSRect) {
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        dirtyRect.fill()
    }

    @objc private func splitTerminal(_ sender: Any?) { onSplitTerminal?() }
    @objc private func newTerminal(_ sender: Any?) { onNewTerminal?() }
    @objc private func killTerminal(_ sender: Any?) { onKillTerminal?() }
    @objc private func toggleMaximize(_ sender: Any?) { onToggleMaximize?() }
    @objc private func closePanel(_ sender: Any?) { onClose?() }

    private static func tool(_ symbol: String, _ title: String, _ action: Selector, _ target: AnyObject) -> NSButton {
        let b = NSButton()
        b.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)?
            .withSymbolConfiguration(.init(pointSize: 12, weight: .regular))
        b.isBordered = false
        b.imagePosition = .imageOnly
        b.contentTintColor = .secondaryLabelColor
        b.toolTip = title
        b.target = target
        b.action = action
        b.setAccessibilityLabel(title)
        b.translatesAutoresizingMaskIntoConstraints = false
        b.widthAnchor.constraint(equalToConstant: 22).isActive = true
        b.heightAnchor.constraint(equalToConstant: 22).isActive = true
        return b
    }
}

/// Transparent resize target centered in the gap between the editor and
/// bottom-panel cards, rather than inside the panel's tab header.
@MainActor
final class BottomPanelResizeHandle: NSView {
    var onResize: ((_ began: Bool, _ deltaY: CGFloat) -> Void)?

    override func resetCursorRects() {
        super.resetCursorRects()
        addCursorRect(bounds, cursor: .resizeUpDown)
    }

    override func mouseDown(with event: NSEvent) {
        guard let window else { return }
        let startY = event.locationInWindow.y
        onResize?(true, 0)
        while let next = window.nextEvent(matching: [.leftMouseDragged, .leftMouseUp]) {
            onResize?(false, next.locationInWindow.y - startY)
            if next.type == .leftMouseUp { break }
        }
    }
}

/// The lone terminal's name in the header: icon and shell name on a
/// rounded background. Clicking it opens the terminal menu.
@MainActor
final class TerminalChipView: NSView {
    var menuProvider: (() -> NSMenu?)?
    private let iconView = NSImageView()
    private let label = NSTextField(labelWithString: "")
    private var isHovered = false { didSet { needsDisplay = true } }

    override var isFlipped: Bool { true }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        translatesAutoresizingMaskIntoConstraints = false
        label.font = .systemFont(ofSize: 12)
        label.textColor = .labelColor
        label.translatesAutoresizingMaskIntoConstraints = false
        iconView.translatesAutoresizingMaskIntoConstraints = false
        addSubview(iconView)
        addSubview(label)
        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 26),
            iconView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            iconView.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: 16),
            iconView.heightAnchor.constraint(equalToConstant: 16),
            label.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 5),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            trailingAnchor.constraint(equalTo: label.trailingAnchor, constant: 10),
        ])
        setAccessibilityElement(true)
        setAccessibilityRole(.menuButton)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func configure(name: String, icon: String, tint: NSColor?) {
        label.stringValue = name
        iconView.image = TerminalChoices.symbol(icon)
        iconView.contentTintColor = tint ?? .labelColor
        setAccessibilityValue(name)
        toolTip = name
    }

    /// The icon and label are not separate click targets.
    override func hitTest(_ point: NSPoint) -> NSView? {
        super.hitTest(point) == nil ? nil : self
    }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.labelColor.withAlphaComponent(isHovered ? 0.14 : 0.08).setFill()
        NSBezierPath(roundedRect: bounds, xRadius: 7, yRadius: 7).fill()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
                                       owner: self))
    }

    override func mouseEntered(with event: NSEvent) { isHovered = true }
    override func mouseExited(with event: NSEvent) { isHovered = false }

    private func showMenu() {
        guard let menu = menuProvider?() else { return }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: bounds.height + 4), in: self)
    }

    override func mouseDown(with event: NSEvent) { showMenu() }
    override func menu(for event: NSEvent) -> NSMenu? { menuProvider?() }

    override func accessibilityPerformPress() -> Bool {
        showMenu()
        return true
    }
}

/// One flat panel tab: uppercase title, an optional count badge, and an
/// underline when selected.
@MainActor
final class PanelTabView: NSView {
    var onClick: (() -> Void)?
    var isSelected = false {
        didSet {
            label.textColor = isSelected ? .labelColor : .secondaryLabelColor
            setAccessibilityValue(isSelected)
            needsDisplay = true
        }
    }
    var badge = 0 {
        didSet {
            badgeView.count = badge
            badgeView.isHidden = badge == 0
            setAccessibilityLabel(badge > 0 ? "\(title), \(badge)" : title)
        }
    }

    private let title: String
    private let label: NSTextField
    let badgeView = CountBadge()

    init(title: String) {
        self.title = title
        label = NSTextField(labelWithString: title)
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        setAccessibilityElement(true)
        setAccessibilityRole(.radioButton)
        setAccessibilityLabel(title)

        label.font = .systemFont(ofSize: 11, weight: .medium)
        label.textColor = .secondaryLabelColor
        label.translatesAutoresizingMaskIntoConstraints = false

        badgeView.isHidden = true
        badgeView.translatesAutoresizingMaskIntoConstraints = false

        let stack = NSStackView(views: [label, badgeView])
        stack.orientation = .horizontal
        stack.spacing = 6
        stack.alignment = .centerY
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 6),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor),
            heightAnchor.constraint(equalToConstant: BottomPanelHeader.height),
        ])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isFlipped: Bool { true }

    /// VS Code's active panel tab: a 1pt rule a few points under the title,
    /// as wide as the title and badge.
    override func draw(_ dirtyRect: NSRect) {
        guard isSelected else { return }
        NSColor.labelColor.setFill()
        let inset: CGFloat = 6
        let y = round(bounds.midY + 11)
        NSRect(x: inset, y: y, width: bounds.width - inset * 2, height: 1)
            .intersection(dirtyRect).fill()
    }

    private var isHovered = false {
        didSet { label.textColor = isSelected || isHovered ? .labelColor : .secondaryLabelColor }
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
                                       owner: self, userInfo: nil))
    }

    override func mouseEntered(with event: NSEvent) { isHovered = true }
    override func mouseExited(with event: NSEvent) { isHovered = false }

    override func mouseDown(with event: NSEvent) { onClick?() }

    override func accessibilityPerformPress() -> Bool {
        onClick?()
        return true
    }
}

/// Count pill like VS Code's panel badge: 18pt tall, fully rounded, the
/// number centered both ways (an `NSTextField` sat it high in the pill).
@MainActor
final class CountBadge: NSView {
    static let height: CGFloat = 18
    var count = 0 {
        didSet {
            guard count != oldValue else { return }
            invalidateIntrinsicContentSize()
            needsDisplay = true
        }
    }

    var text: String { count > 999 ? "999+" : "\(count)" }

    private var attributes: [NSAttributedString.Key: Any] {
        [.font: NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .semibold), .foregroundColor: NSColor.white]
    }

    override var intrinsicContentSize: NSSize {
        // 5pt each side: one digit stays a circle, more digits widen it.
        let width = ceil((text as NSString).size(withAttributes: attributes).width) + 10
        return NSSize(width: max(Self.height, width), height: Self.height)
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setContentHuggingPriority(.required, for: .horizontal)
        setContentCompressionResistancePriority(.required, for: .horizontal)
        setAccessibilityElement(false)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func draw(_ dirtyRect: NSRect) {
        Theme.accent.resolved(for: effectiveAppearance).setFill()
        NSBezierPath(roundedRect: bounds, xRadius: bounds.height / 2, yRadius: bounds.height / 2).fill()
        let string = NSAttributedString(string: text, attributes: attributes)
        let size = string.size()
        // Center the digits (cap height) rather than the whole line box.
        // Not flipped: `draw(at:)` takes the line's bottom, a descender
        // below the baseline.
        let font = NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .semibold)
        let baseline = (bounds.height - font.capHeight) / 2
        string.draw(at: NSPoint(x: ((bounds.width - size.width) / 2).rounded(),
                                y: (baseline + font.descender).rounded()))
    }
}

private extension NSLayoutConstraint {
    func withPriority(_ priority: NSLayoutConstraint.Priority) -> NSLayoutConstraint {
        self.priority = priority
        return self
    }
}
