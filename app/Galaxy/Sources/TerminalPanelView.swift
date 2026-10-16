import AppKit

/// One terminal in the bottom panel, with the per-terminal choices from its
/// context menu (name, color, icon). Terminals with the same `group` were
/// split from each other and share the panel side by side.
@MainActor
final class PanelTerminal {
    static let defaultIcon = "apple.terminal"

    let handle: TerminalHandle
    var customName: String?
    var colorName: String?
    var icon = PanelTerminal.defaultIcon
    var group: Int

    init(handle: TerminalHandle, group: Int) {
        self.handle = handle
        self.group = group
    }

    var displayName: String { customName ?? handle.shellName() }
    var tint: NSColor? { colorName.flatMap { TerminalChoices.color(named: $0) } }
    var summary: TerminalSummary { TerminalSummary(name: displayName, icon: icon, tint: tint, group: group) }
}

/// What the header chip and the terminal list show for one terminal.
struct TerminalSummary {
    var name: String
    var icon: String
    var tint: NSColor?
    var group: Int
}

/// The colors and icons "Change Color…" and "Change Icon…" offer.
enum TerminalChoices {
    static let colors: [(name: String, color: NSColor)] = [
        ("Red", .systemRed), ("Orange", .systemOrange), ("Yellow", .systemYellow),
        ("Green", .systemGreen), ("Cyan", .systemTeal), ("Blue", .systemBlue),
        ("Purple", .systemPurple), ("Pink", .systemPink),
    ]

    static let icons: [(title: String, symbol: String)] = [
        ("Terminal", "apple.terminal"), ("Prompt", "terminal"), ("Code", "chevron.left.forwardslash.chevron.right"),
        ("Build", "hammer"), ("Settings", "gearshape"), ("Server", "server.rack"),
        ("Bolt", "bolt"), ("Flame", "flame"), ("Bug", "ladybug"), ("Package", "shippingbox"),
        ("Network", "network"), ("Cube", "cube"),
    ]

    static func color(named name: String) -> NSColor? {
        colors.first { $0.name == name }?.color
    }

    /// A small filled circle for the color menu.
    static func swatch(_ color: NSColor) -> NSImage {
        NSImage(size: NSSize(width: 12, height: 12), flipped: false) { rect in
            color.setFill()
            NSBezierPath(ovalIn: rect.insetBy(dx: 1, dy: 1)).fill()
            return true
        }
    }

    static func symbol(_ name: String, size: CGFloat = 13) -> NSImage? {
        NSImage(systemSymbolName: name, accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: size, weight: .regular))
    }
}

/// One row of the terminal list: icon, name, and (on hover or when selected)
/// split and kill buttons.
@MainActor
private final class TerminalRowView: NSView {
    static let height: CGFloat = 28
    var onSelect: (() -> Void)?
    var onSplit: (() -> Void)?
    var onKill: (() -> Void)?
    var menuProvider: (() -> NSMenu?)?

    private let iconView = NSImageView()
    private let label = NSTextField(labelWithString: "")
    private let splitButton = TerminalRowView.button("square.split.2x1", "Split Terminal")
    private let killButton = TerminalRowView.button("trash", "Kill Terminal")
    private var indent: CGFloat = 0
    private var isHovered = false { didSet { refresh() } }
    var isSelected = false { didSet { refresh() } }

    override var isFlipped: Bool { true }

    /// Width the row needs to show its name in full.
    var contentWidth: CGFloat { 10 + indent + 16 + 6 + ceil(label.intrinsicContentSize.width) + 10 }

    init() {
        super.init(frame: .zero)
        label.font = .systemFont(ofSize: 12)
        label.lineBreakMode = .byTruncatingTail
        iconView.imageScaling = .scaleProportionallyDown
        splitButton.target = self
        splitButton.action = #selector(split(_:))
        killButton.target = self
        killButton.action = #selector(kill(_:))
        for view in [iconView, label, splitButton, killButton] as [NSView] { addSubview(view) }
        refresh()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func configure(_ item: TerminalSummary, index: Int, indented: Bool) {
        label.stringValue = item.name
        iconView.image = TerminalChoices.symbol(item.icon)
        iconView.contentTintColor = item.tint ?? .secondaryLabelColor
        indent = indented ? 14 : 0
        setAccessibilityElement(true)
        setAccessibilityRole(.button)
        setAccessibilityLabel(item.name)
        setAccessibilityIdentifier("terminal-row-\(index)")
        needsLayout = true
    }

    private static func button(_ symbol: String, _ title: String) -> NSButton {
        let b = NSButton()
        b.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)?
            .withSymbolConfiguration(.init(pointSize: 12, weight: .regular))
        b.isBordered = false
        b.imagePosition = .imageOnly
        b.contentTintColor = .secondaryLabelColor
        b.toolTip = title
        b.setAccessibilityLabel(title)
        return b
    }

    private func refresh() {
        let showButtons = isHovered || isSelected
        splitButton.isHidden = !showButtons
        killButton.isHidden = !showButtons
        label.textColor = isSelected || isHovered ? .labelColor : .secondaryLabelColor
        setAccessibilityValue(isSelected)
        needsLayout = true
        needsDisplay = true
    }

    override func layout() {
        super.layout()
        let buttons: CGFloat = splitButton.isHidden ? 0 : 48
        iconView.frame = NSRect(x: 10 + indent, y: (bounds.height - 16) / 2, width: 16, height: 16)
        let labelX = 10 + indent + 16 + 6
        let labelSize = label.intrinsicContentSize
        label.frame = NSRect(x: labelX, y: ((bounds.height - labelSize.height) / 2).rounded(),
                             width: max(0, bounds.width - labelX - max(buttons, 8)), height: labelSize.height)
        splitButton.frame = NSRect(x: bounds.width - 48, y: (bounds.height - 22) / 2, width: 22, height: 22)
        killButton.frame = NSRect(x: bounds.width - 26, y: (bounds.height - 22) / 2, width: 22, height: 22)
    }

    /// Only the buttons are separate click targets; the rest selects the row
    /// and opens its menu.
    override func hitTest(_ point: NSPoint) -> NSView? {
        guard let hit = super.hitTest(point) else { return nil }
        return hit is NSButton ? hit : self
    }

    override func draw(_ dirtyRect: NSRect) {
        guard isSelected || isHovered else { return }
        NSColor.labelColor.withAlphaComponent(isSelected ? 0.12 : 0.06).setFill()
        NSBezierPath(roundedRect: bounds.insetBy(dx: 4, dy: 1), xRadius: 5, yRadius: 5).fill()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
                                       owner: self))
    }

    override func mouseEntered(with event: NSEvent) { isHovered = true }
    override func mouseExited(with event: NSEvent) { isHovered = false }
    override func mouseDown(with event: NSEvent) { onSelect?() }
    override func menu(for event: NSEvent) -> NSMenu? { menuProvider?() ?? super.menu(for: event) }

    override func accessibilityPerformPress() -> Bool {
        onSelect?()
        return true
    }

    @objc private func split(_ sender: Any?) { onSplit?() }
    @objc private func kill(_ sender: Any?) { onKill?() }
}

/// The list of terminals beside the panel, shown from two terminals on.
@MainActor
final class TerminalListView: NSView {
    static let defaultWidth: CGFloat = 180
    static let minimumWidth: CGFloat = 96
    static let maximumWidth: CGFloat = 280

    var onSelect: ((Int) -> Void)?
    var onSplit: ((Int) -> Void)?
    var onKill: ((Int) -> Void)?
    var menuProvider: ((Int) -> NSMenu?)?
    /// Width follows the longest name instead of the default.
    var fitsContent = false

    private var rows: [TerminalRowView] = []

    override var isFlipped: Bool { true }

    var preferredWidth: CGFloat {
        guard fitsContent else { return Self.defaultWidth }
        let widest = rows.map(\.contentWidth).max() ?? Self.defaultWidth
        return min(max(widest, Self.minimumWidth), Self.maximumWidth)
    }

    func setItems(_ items: [TerminalSummary], selected: Int) {
        while rows.count > items.count { rows.removeLast().removeFromSuperview() }
        while rows.count < items.count {
            let row = TerminalRowView()
            addSubview(row)
            rows.append(row)
        }
        for (index, item) in items.enumerated() {
            let row = rows[index]
            let isSplitChild = index > 0 && items[index - 1].group == item.group
            row.configure(item, index: index, indented: isSplitChild)
            row.isSelected = index == selected
            row.onSelect = { [weak self] in self?.onSelect?(index) }
            row.onSplit = { [weak self] in self?.onSplit?(index) }
            row.onKill = { [weak self] in self?.onKill?(index) }
            row.menuProvider = { [weak self] in self?.menuProvider?(index) }
        }
        needsLayout = true
    }

    override func layout() {
        super.layout()
        for (index, row) in rows.enumerated() {
            row.frame = NSRect(x: 0, y: 4 + CGFloat(index) * TerminalRowView.height,
                               width: bounds.width, height: TerminalRowView.height)
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        dirtyRect.fill()
        NSColor.separatorColor.setFill()
        NSRect(x: 0, y: 0, width: 1, height: bounds.height).intersection(dirtyRect).fill()
    }
}

/// The panel's terminal tab: one or more terminal views side by side (the
/// active group) and, from two terminals on, the list on the right.
@MainActor
final class TerminalPanelView: NSView {
    let list = TerminalListView()
    var onOpenLink: ((String, UInt64, UInt64) -> Void)?
    var onExit: (() -> Void)?
    /// A pane took keyboard focus: it is now the active terminal.
    var onFocusTerminal: ((PanelTerminal) -> Void)?
    var menuProvider: ((PanelTerminal) -> NSMenu?)?

    private static let fitsContentKey = "TerminalListFitsContent"
    private var views: [TerminalView] = []
    private var shown: [PanelTerminal] = []
    private var activePaneIndex = 0

    override var isFlipped: Bool { true }

    /// The pane that gets keyboard input.
    var activePane: TerminalView {
        if views.indices.contains(activePaneIndex), shown.indices.contains(activePaneIndex) {
            return views[activePaneIndex]
        }
        return pane(at: 0)
    }

    /// Panes in use, left to right.
    var panes: [TerminalView] { Array(views.prefix(shown.count)) }

    var listFitsContent: Bool {
        get { list.fitsContent }
        set {
            list.fitsContent = newValue
            UserDefaults.standard.set(newValue, forKey: Self.fitsContentKey)
            needsLayout = true
        }
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        list.fitsContent = UserDefaults.standard.bool(forKey: Self.fitsContentKey)
        list.isHidden = true
        addSubview(list)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func owns(_ responder: NSResponder?) -> Bool {
        guard let responder else { return false }
        return views.contains { $0 === responder }
    }

    private func pane(at index: Int) -> TerminalView {
        while views.count <= index {
            let view = TerminalView()
            view.translatesAutoresizingMaskIntoConstraints = true
            view.autoresizingMask = []
            view.onOpenLink = { [weak self] path, line, col in self?.onOpenLink?(path, line, col) }
            view.onExit = { [weak self] in self?.onExit?() }
            view.onFocus = { [weak self, weak view] in
                guard let self, let view, let index = self.views.firstIndex(where: { $0 === view }),
                      self.shown.indices.contains(index) else { return }
                self.activePaneIndex = index
                self.onFocusTerminal?(self.shown[index])
            }
            view.contextMenu = { [weak self, weak view] in
                guard let self, let view, let index = self.views.firstIndex(where: { $0 === view }),
                      self.shown.indices.contains(index) else { return nil }
                return self.menuProvider?(self.shown[index])
            }
            addSubview(view)
            views.append(view)
        }
        return views[index]
    }

    /// Shows the active terminal's group and the list. An empty `all` clears the panel.
    func show(terminals all: [PanelTerminal], activeIndex: Int) {
        guard all.indices.contains(activeIndex) else {
            shown = []
            views.forEach { $0.stop(); $0.isHidden = true }
            list.isHidden = true
            arrange()
            return
        }
        let active = all[activeIndex]
        shown = all.filter { $0.group == active.group }
        activePaneIndex = shown.firstIndex { $0 === active } ?? 0
        for (index, terminal) in shown.enumerated() {
            let view = pane(at: index)
            view.isHidden = false
            view.showsLeadingDivider = index > 0
            view.start(handle: terminal.handle)
        }
        for view in views.dropFirst(shown.count) {
            view.stop()
            view.isHidden = true
        }
        updateList(all, selected: activeIndex)
        arrange()
    }

    func updateList(_ all: [PanelTerminal], selected: Int) {
        setList(all.map(\.summary), selected: selected)
    }

    func setList(_ items: [TerminalSummary], selected: Int) {
        list.isHidden = items.count < 2
        list.setItems(items, selected: selected)
        arrange()
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        arrange()
    }

    override func layout() {
        super.layout()
        arrange()
    }

    private func arrange() {
        let listWidth = list.isHidden ? 0 : min(list.preferredWidth, bounds.width / 2)
        let panesWidth = bounds.width - listWidth
        list.frame = NSRect(x: panesWidth, y: 0, width: listWidth, height: bounds.height)
        guard !shown.isEmpty else { return }
        let each = (panesWidth / CGFloat(shown.count)).rounded(.down)
        for index in shown.indices {
            let x = CGFloat(index) * each
            let width = index == shown.count - 1 ? panesWidth - x : each
            views[index].frame = NSRect(x: x, y: 0, width: width, height: bounds.height)
        }
    }
}
