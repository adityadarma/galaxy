import AppKit

/// Path of the open document, at the top of the editor column inside the
/// title bar area: "workspace › src › main.rs". The window title itself is
/// hidden, but still set, for the Window menu and accessibility.
@MainActor
final class PathBar: NSView {
    /// Room for the traffic lights when the sidebar is hidden.
    static let trafficLightInset: CGFloat = 78
    static let normalInset: CGFloat = 16
    /// Used when the window has no title bar layout guide (never in practice).
    static let fallbackHeight: CGFloat = 28

    let label = NSTextField(labelWithString: "")
    /// Copper dot after the file name while there are unsaved changes.
    let editedDot = NSTextField(labelWithString: "●")
    private(set) var components: [String] = []
    var isEdited: Bool { !editedDot.isHidden }
    private var leading: NSLayoutConstraint!

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityElement(false)

        label.font = .systemFont(ofSize: 12)
        label.lineBreakMode = .byTruncatingHead
        label.setAccessibilityIdentifier("path-bar")
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        editedDot.font = .systemFont(ofSize: 8)
        editedDot.textColor = Theme.accent
        editedDot.toolTip = "Unsaved changes"
        editedDot.setAccessibilityLabel("Edited")
        editedDot.setAccessibilityIdentifier("path-edited")
        editedDot.isHidden = true

        let row = NSStackView(views: [label, editedDot])
        row.spacing = 6
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)

        let separator = NSBox()
        separator.boxType = .separator
        separator.translatesAutoresizingMaskIntoConstraints = false
        addSubview(separator)

        leading = row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Self.normalInset)
        NSLayoutConstraint.activate([
            leading,
            row.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -12),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
            separator.bottomAnchor.constraint(equalTo: bottomAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Same color as the editor, so the editor column runs to the top edge.
    override func draw(_ dirtyRect: NSRect) {
        NSColor.textBackgroundColor.setFill()
        dirtyRect.intersection(bounds).fill()
    }

    /// Leaves room for the traffic lights while the sidebar is hidden.
    var clearsTrafficLights: Bool {
        get { leading.constant == Self.trafficLightInset }
        set { leading.constant = newValue ? Self.trafficLightInset : Self.normalInset }
    }

    /// Leading parts are dimmed, the last one (the file) is not.
    func show(_ components: [String]) {
        self.components = components
        let text = NSMutableAttributedString()
        for (i, part) in components.enumerated() {
            let last = i == components.count - 1
            if i > 0 {
                text.append(NSAttributedString(string: "  ›  ", attributes: [
                    .foregroundColor: NSColor.tertiaryLabelColor, .font: label.font!,
                ]))
            }
            text.append(NSAttributedString(string: part, attributes: [
                .foregroundColor: last ? NSColor.labelColor : NSColor.secondaryLabelColor,
                .font: last ? NSFont.systemFont(ofSize: 12, weight: .medium) : label.font!,
            ]))
        }
        label.attributedStringValue = text
        label.setAccessibilityLabel(components.joined(separator: " / "))
    }

    func setEdited(_ edited: Bool) {
        if editedDot.isHidden == edited { editedDot.isHidden = !edited }
    }
}
