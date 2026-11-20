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
        guard let button = subviews.reversed().first(where: { !$0.isHidden && $0.frame.contains(local) }) else { return nil }
        return button.hitTest(local)
    }

    var hunkButtons: [NSButton] { subviews.compactMap { $0 as? NSButton } }

    func show(map: DiffLineMap, lineHeight: CGFloat) {
        self.map = map
        self.lineHeight = lineHeight
        subviews.forEach { $0.removeFromSuperview() }
        guard !isHidden, lineHeight > 0 else { return }
        for index in map.hunks.indices {
            let revert = button("arrow.right", tooltip: "Restore this change from the left", index: index,
                                action: #selector(revertHunk(_:)))
            let stage = button("arrow.left", tooltip: "Stage this change (right to index)", index: index,
                               action: #selector(stageHunk(_:)))
            addSubview(revert)
            addSubview(stage)
        }
        positionButtons()
    }

    private var lineHeight: CGFloat = 1

    func updatePositions() { positionButtons() }

    private func positionButtons() {
        guard let scrollView, let leftPane, lineHeight > 0 else { return }
        let dividerX = leftPane.frame.maxX
        let scrollY = scrollView.contentView.bounds.minY
        let topInset = scrollView.contentInsets.top
        for (index, hunk) in map.hunks.enumerated() {
            // A zero-length side has a spacer at this position. Anchor to the
            // first changed row on either side, not after that spacer.
            let row = min(map.visualRow(forOld: hunk.oldStart), map.visualRow(forNew: hunk.newStart))
            let y = CGFloat(row) * lineHeight - scrollY + topInset
            let buttons = hunkButtons.filter { $0.tag == index }
            guard buttons.count == 2 else { continue }
            buttons[0].frame.origin = NSPoint(x: dividerX - 43, y: y)
            buttons[1].frame.origin = NSPoint(x: dividerX - 22, y: y)
            buttons.forEach { $0.isHidden = y < -28 || y > bounds.height }
        }
    }

    private func button(_ symbol: String, tooltip: String, index: Int, action: Selector) -> NSButton {
        let button = NSButton(frame: NSRect(x: 0, y: 0, width: 20, height: 20))
        button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: tooltip)
        button.imagePosition = .imageOnly
        button.bezelStyle = .rounded
        button.toolTip = tooltip
        button.tag = index
        button.target = self
        button.action = action
        return button
    }

    @objc private func revertHunk(_ sender: NSButton) { onRevert?(sender.tag) }
    @objc private func stageHunk(_ sender: NSButton) { onStage?(sender.tag) }
}
