import AppKit

/// Plain strip in the (transparent) title bar: holds the traffic lights and
/// drags the window. The folder name lives in the sidebar section header,
/// and the sidebar is toggled from the activity bar or ⌘B.
@MainActor
final class TitleBar: NSView {
    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityElement(false)

        let separator = NSBox()
        separator.boxType = .separator
        separator.translatesAutoresizingMaskIntoConstraints = false
        addSubview(separator)
        NSLayoutConstraint.activate([
            separator.bottomAnchor.constraint(equalTo: bottomAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
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
