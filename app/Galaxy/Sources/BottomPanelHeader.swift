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
            badgeLabel.stringValue = "\(badge)"
            badgeLabel.isHidden = badge == 0
            setAccessibilityLabel(badge > 0 ? "\(title), \(badge)" : title)
        }
    }

    private let title: String
    private let label: NSTextField
    private let badgeLabel = NSTextField(labelWithString: "")

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

        badgeLabel.font = .monospacedDigitSystemFont(ofSize: 9, weight: .semibold)
        badgeLabel.textColor = .white
        badgeLabel.alignment = .center
        badgeLabel.wantsLayer = true
        badgeLabel.layer?.backgroundColor = Theme.accent.cgColor
        badgeLabel.layer?.cornerRadius = 8
        badgeLabel.isHidden = true
        badgeLabel.translatesAutoresizingMaskIntoConstraints = false

        let stack = NSStackView(views: [label, badgeLabel])
        stack.orientation = .horizontal
        stack.spacing = 5
        stack.alignment = .centerY
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 6),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor),
            heightAnchor.constraint(equalToConstant: BottomPanelHeader.height),
            badgeLabel.heightAnchor.constraint(equalToConstant: 16),
            badgeLabel.widthAnchor.constraint(greaterThanOrEqualToConstant: 16),
        ])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isFlipped: Bool { true }

    override func updateLayer() {
        super.updateLayer()
        badgeLabel.layer?.backgroundColor = Theme.accent.cgColor
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        badgeLabel.layer?.backgroundColor = Theme.accent.resolved(for: effectiveAppearance).cgColor
    }

    override func draw(_ dirtyRect: NSRect) {
        guard isSelected else { return }
        NSColor.labelColor.setFill()
        let inset: CGFloat = 6
        NSRect(x: inset, y: bounds.height - 6, width: bounds.width - inset * 2, height: 1)
            .intersection(dirtyRect).fill()
    }

    override func mouseDown(with event: NSEvent) { onClick?() }

    override func accessibilityPerformPress() -> Bool {
        onClick?()
        return true
    }
}

private extension NSLayoutConstraint {
    func withPriority(_ priority: NSLayoutConstraint.Priority) -> NSLayoutConstraint {
        self.priority = priority
        return self
    }
}
