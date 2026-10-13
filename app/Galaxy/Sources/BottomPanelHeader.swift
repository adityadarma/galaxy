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
        case terminal
        case ports

        var title: String {
            switch self {
            case .problems: "PROBLEMS"
            case .output: "OUTPUT"
            case .debugConsole: "DEBUG CONSOLE"
            case .terminal: "TERMINAL"
            case .ports: "PORTS"
            }
        }
    }

    static let height: CGFloat = 34

    var onSelect: ((Tab) -> Void)?
    var onNewTerminal: (() -> Void)?
    var onSelectTerminal: ((Int) -> Void)?
    var onKillTerminal: (() -> Void)?
    var onToggleMaximize: (() -> Void)?
    var onClose: (() -> Void)?
    /// Dragging the top edge (VS Code's sash): called with the drag start
    /// (`true`) and then the vertical distance moved, up positive.
    var onResize: ((_ began: Bool, _ deltaY: CGFloat) -> Void)?

    /// Height of the grab strip along the top edge.
    static let sashHeight: CGFloat = 5

    private func inSash(_ point: NSPoint) -> Bool { point.y < Self.sashHeight }

    override func hitTest(_ point: NSPoint) -> NSView? {
        guard !isHidden, let local = superview.map({ convert(point, from: $0) }),
              bounds.contains(local), inSash(local) else { return super.hitTest(point) }
        return self
    }

    override func resetCursorRects() {
        super.resetCursorRects()
        addCursorRect(NSRect(x: 0, y: 0, width: bounds.width, height: Self.sashHeight), cursor: .resizeUpDown)
    }

    override func mouseDown(with event: NSEvent) {
        guard inSash(convert(event.locationInWindow, from: nil)), let window else {
            return super.mouseDown(with: event)
        }
        let startY = event.locationInWindow.y
        onResize?(true, 0)
        while let next = window.nextEvent(matching: [.leftMouseDragged, .leftMouseUp]) {
            onResize?(false, next.locationInWindow.y - startY)
            if next.type == .leftMouseUp { break }
        }
    }

    private(set) var selectedTab: Tab = .terminal
    private var tabViews: [Tab: PanelTabView] = [:]
    private let tabStack = NSStackView()
    private let actionStack = NSStackView()
    let terminalPicker = NSPopUpButton(frame: .zero, pullsDown: false)
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

        terminalPicker.isBordered = false
        terminalPicker.font = .systemFont(ofSize: 12)
        terminalPicker.controlSize = .small
        terminalPicker.target = self
        terminalPicker.action = #selector(pickTerminal(_:))
        terminalPicker.toolTip = "Select Terminal"
        terminalPicker.setAccessibilityLabel("Terminals")
        terminalPicker.setAccessibilityIdentifier("terminal-picker")
        divider.boxType = .separator

        actionStack.orientation = .horizontal
        actionStack.spacing = 4
        actionStack.alignment = .centerY
        actionStack.translatesAutoresizingMaskIntoConstraints = false
        for view in [terminalPicker, newButton, killButton, divider, maximizeButton, closeButton] as [NSView] {
            actionStack.addArrangedSubview(view)
        }
        actionStack.setCustomSpacing(10, after: killButton)
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
        setTerminals(names: [], selected: 0)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isFlipped: Bool { true }

    func select(_ tab: Tab) {
        selectedTab = tab
        for (key, view) in tabViews { view.isSelected = key == tab }
        let terminal = tab == .terminal
        terminalPicker.isHidden = !terminal || terminalPicker.numberOfItems == 0
        newButton.isHidden = !terminal
        killButton.isHidden = !terminal
        divider.isHidden = !terminal
    }

    func setProblemCount(_ count: Int) {
        tabViews[.problems]?.badge = count
    }

    /// One entry per terminal, VS Code style: just the shell name when there
    /// is one, numbered when there are several.
    func setTerminals(names: [String], selected: Int) {
        terminalPicker.removeAllItems()
        for (index, name) in names.enumerated() {
            terminalPicker.addItem(withTitle: names.count > 1 ? "\(index + 1): \(name)" : name)
            terminalPicker.lastItem?.image = NSImage(systemSymbolName: "apple.terminal",
                                                     accessibilityDescription: nil)
        }
        if names.indices.contains(selected) { terminalPicker.selectItem(at: selected) }
        terminalPicker.isHidden = selectedTab != .terminal || names.isEmpty
        killButton.isEnabled = !names.isEmpty
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
        NSColor.textBackgroundColor.setFill()
        dirtyRect.fill()
        // The line between the editor and the panel.
        NSColor.separatorColor.setFill()
        NSRect(x: 0, y: 0, width: bounds.width, height: 1).intersection(dirtyRect).fill()
    }

    @objc private func pickTerminal(_ sender: NSPopUpButton) { onSelectTerminal?(sender.indexOfSelectedItem) }
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
