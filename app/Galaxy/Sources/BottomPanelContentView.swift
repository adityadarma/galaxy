import AppKit

/// Hosts bottom-panel views. Children keep at least `minimumChildHeight`
/// and are clipped, so collapsing the panel to zero height never fights
/// their internal constraints. (When it did, AppKit broke a constraint at
/// random and the panel could cover the editor and its tab bar.)
@MainActor
final class BottomPanelContentView: NSView {
    static let minimumChildHeight: CGFloat = 160

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        clipsToBounds = true
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isFlipped: Bool { true }

    override func layout() {
        super.layout()
        let height = max(bounds.height, Self.minimumChildHeight)
        for child in subviews {
            child.frame = NSRect(x: 0, y: 0, width: bounds.width, height: height)
        }
    }
}
