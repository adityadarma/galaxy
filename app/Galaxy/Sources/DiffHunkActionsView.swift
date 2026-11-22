import AppKit

/// Buttons over the split divider. Only the buttons intercept mouse events;
/// the editor and its scrollers remain interactive everywhere else.
@MainActor
final class DiffHunkActionsView: NSView {
    var onRevert: ((Int) -> Void)?
    var onStage: ((Int) -> Void)?
    override var isFlipped: Bool { true }
    private var map = DiffLineMap()
    private var scrollView: NSScrollView?
    private var leftPane: NSScrollView?

    func configure(scrollView: NSScrollView, leftPane: NSScrollView) {
        self.scrollView = scrollView
        self.leftPane = leftPane
    }

    override func layout() {
        super.layout()
        positionButtons()
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        guard !isHidden else { return nil }
        let local = convert(point, from: superview)
        guard bounds.contains(local), let button = subviews.reversed().first(where: { !$0.isHidden && $0.frame.contains(local) }) else { return nil }
        return button.hitTest(local)
    }

    var hunkButtons: [NSButton] { subviews.compactMap { $0 as? NSButton } }

    func show(map: DiffLineMap, lineHeight: CGFloat, actionsEnabled: Bool = true) {
        self.map = map
        self.lineHeight = lineHeight
        subviews.forEach { $0.removeFromSuperview() }
        guard !isHidden, actionsEnabled, lineHeight > 0 else { return updateGutterExclusions() }
        for index in map.hunks.indices {
            addSubview(button("arrow.right", tooltip: "Revert Block", index: index,
                              action: #selector(revertHunk(_:))))
            addSubview(button("plus", tooltip: "Stage Block", index: index,
                              action: #selector(stageHunk(_:))))
        }
        positionButtons()
    }

    private var lineHeight: CGFloat = 1

    func updatePositions() { positionButtons() }

    private func positionButtons() {
        guard let scrollView, let leftPane, lineHeight > 0 else { return }
        // Buttons sit in the gutter left of the resize line, which runs down
        // its right edge.
        let gap = max(0, scrollView.frame.minX - leftPane.frame.maxX)
        let room = gap - EditorSplitView.diffHandleWidth
        let buttonX = (leftPane.frame.maxX + (room - Self.buttonSize) / 2).rounded()
        let scrollY = scrollView.contentView.bounds.minY
        let topInset = scrollView.contentInsets.top
        for (index, hunk) in map.hunks.enumerated() {
            // A zero-length side has a spacer at this position. Anchor to the
            // first changed row on either side, not after that spacer.
            let row = min(map.visualRow(forOld: hunk.oldStart), map.visualRow(forNew: hunk.newStart))
            let y = CGFloat(row) * lineHeight - scrollY + topInset
            let buttons = hunkButtons.filter { $0.tag == index }
            guard buttons.count == 2 else { continue }
            buttons[0].frame.origin = NSPoint(x: buttonX, y: y)
            buttons[1].frame.origin = NSPoint(x: buttonX, y: y + Self.buttonSize + Self.buttonSpacing)
            buttons.forEach { $0.isHidden = y < -49 || y > bounds.height }
        }
        updateGutterExclusions()
    }

    weak var gutter: EditorSplitView?

    private func updateGutterExclusions() {
        guard let gutter else { return }
        gutter.diffGutterExclusions = isHidden ? [] : hunkButtons.filter { !$0.isHidden }.map {
            convert($0.frame, to: gutter)
        }
    }

    static let buttonSize: CGFloat = 20
    static let buttonSpacing: CGFloat = 2

    /// Borderless so the drawn shape matches the frame: a `.rounded` bezel
    /// paints past a 20 pt frame and the stacked buttons overlapped.
    private func button(_ symbol: String, tooltip: String, index: Int, action: Selector) -> NSButton {
        let button = HunkActionButton(frame: NSRect(x: 0, y: 0, width: Self.buttonSize, height: Self.buttonSize))
        button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: tooltip)
        button.imagePosition = .imageOnly
        button.toolTip = tooltip
        button.tag = index
        button.target = self
        button.action = action
        return button
    }

    @objc private func revertHunk(_ sender: NSButton) { onRevert?(sender.tag) }
    @objc private func stageHunk(_ sender: NSButton) { onStage?(sender.tag) }
}

/// Borderless hunk button with a subtle rounded fill that follows the
/// appearance, since layer colours do not update by themselves.
@MainActor
final class HunkActionButton: NSButton {
    override init(frame: NSRect) {
        super.init(frame: frame)
        isBordered = false
        contentTintColor = .secondaryLabelColor
        wantsLayer = true
        layer?.cornerRadius = 4
        layer?.cornerCurve = .continuous
        refreshFill()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        refreshFill()
    }

    private func refreshFill() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(0.08).cgColor
        }
    }
}

/// Editor split view. In a diff the divider widens into a gutter that holds
/// the hunk buttons, so they do not cover the left pane's scrollbar. Only a
/// thin line down the gutter's right edge, beside the buttons, resizes.
@MainActor
final class EditorSplitView: NSSplitView {
    static let diffGutterWidth: CGFloat = 30
    static let diffHandleWidth: CGFloat = 5

    var showsDiffGutter = false {
        didSet {
            guard showsDiffGutter != oldValue else { return }
            adjustSubviews()
            window?.invalidateCursorRects(for: self)
        }
    }

    /// Hunk button frames in this view's coordinates; they never resize.
    var diffGutterExclusions: [NSRect] = [] {
        didSet { if diffGutterExclusions != oldValue { window?.invalidateCursorRects(for: self) } }
    }

    override var dividerThickness: CGFloat {
        showsDiffGutter ? Self.diffGutterWidth : super.dividerThickness
    }

    var diffGutterRect: NSRect? {
        guard showsDiffGutter, isVertical, subviews.count >= 2 else { return nil }
        let left = subviews.min { $0.frame.minX < $1.frame.minX }!
        return NSRect(x: left.frame.maxX, y: bounds.minY, width: dividerThickness, height: bounds.height)
    }

    /// Splits the gutter into the strips that resize and the rest.
    func diffGutterCursorRegions() -> (drag: [NSRect], arrow: [NSRect]) {
        guard let gutter = diffGutterRect else { return ([], []) }
        let handle = NSRect(x: gutter.maxX - Self.diffHandleWidth, y: gutter.minY,
                            width: Self.diffHandleWidth, height: gutter.height)
        var arrow = [
            NSRect(x: gutter.minX, y: gutter.minY, width: handle.minX - gutter.minX, height: gutter.height),
            NSRect(x: handle.maxX, y: gutter.minY, width: gutter.maxX - handle.maxX, height: gutter.height),
        ]
        var drag: [NSRect] = []
        var top = handle.minY
        let blocked = diffGutterExclusions
            .map { $0.intersection(handle) }
            .filter { !$0.isEmpty }
            .sorted { $0.minY < $1.minY }
        for block in blocked {
            if block.minY > top {
                drag.append(NSRect(x: handle.minX, y: top, width: handle.width, height: block.minY - top))
            }
            arrow.append(block)
            top = max(top, block.maxY)
        }
        if handle.maxY > top {
            drag.append(NSRect(x: handle.minX, y: top, width: handle.width, height: handle.maxY - top))
        }
        return (drag, arrow.filter { $0.width > 0 && $0.height > 0 })
    }

    override func resetCursorRects() {
        guard showsDiffGutter else { return super.resetCursorRects() }
        let regions = diffGutterCursorRegions()
        regions.arrow.forEach { addCursorRect($0, cursor: .arrow) }
        regions.drag.forEach { addCursorRect($0, cursor: .resizeLeftRight) }
    }

    override func mouseDown(with event: NSEvent) {
        if let gutter = diffGutterRect {
            let point = convert(event.locationInWindow, from: nil)
            if gutter.contains(point), !diffGutterCursorRegions().drag.contains(where: { $0.contains(point) }) {
                return
            }
        }
        super.mouseDown(with: event)
    }

    override func drawDivider(in rect: NSRect) {
        guard showsDiffGutter else { return super.drawDivider(in: rect) }
        Theme.editorBackground.setFill()
        rect.fill()
        dividerColor.setFill()
        let lineX = (rect.maxX - Self.diffHandleWidth / 2).rounded(.down)
        NSRect(x: lineX, y: rect.minY, width: 1, height: rect.height).fill()
    }
}
