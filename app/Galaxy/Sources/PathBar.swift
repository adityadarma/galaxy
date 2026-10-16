import AppKit

/// Path of the open document, under the tabs: "workspace › src › main.rs".
/// The window title itself is hidden, but still set, for the Window menu
/// and accessibility.
@MainActor
final class PathBar: NSView {
    static let height: CGFloat = 26
    static let inset: CGFloat = 16

    let label = NSTextField(labelWithString: "")
    /// Copper dot after the file name while there are unsaved changes.
    let editedDot = NSTextField(labelWithString: "●")
    private(set) var components: [String] = []
    var isEdited: Bool { !editedDot.isHidden }

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityElement(false)

        label.font = .systemFont(ofSize: 13)
        label.lineBreakMode = .byTruncatingHead
        // One line only: a long path truncates at the head instead of
        // wrapping the file name under the bar (and over the editor).
        label.maximumNumberOfLines = 1
        label.cell?.wraps = false
        label.cell?.isScrollable = false
        label.cell?.usesSingleLineMode = true
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

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: Self.height),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Self.inset),
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
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        dirtyRect.intersection(bounds).fill()
    }

    /// Folders, then the file with its type glyph, split by chevrons as in
    /// VSCode's breadcrumbs.
    func show(_ components: [String]) {
        self.components = components
        let font = NSFont.systemFont(ofSize: 13)
        let text = NSMutableAttributedString()
        defer {
            // An attributed string replaces the label's own line break mode;
            // without a paragraph style the cell wraps.
            let style = NSMutableParagraphStyle()
            style.lineBreakMode = .byTruncatingHead
            text.addAttribute(.paragraphStyle, value: style, range: NSRange(location: 0, length: text.length))
            label.attributedStringValue = text
        }
        for (i, part) in components.enumerated() {
            let last = i == components.count - 1
            if i > 0 {
                text.append(NSAttributedString(string: " ", attributes: [.font: font]))
                text.append(Self.symbol("chevron.right", size: 9, weight: .medium,
                                        color: .tertiaryLabelColor, font: font))
                text.append(NSAttributedString(string: " ", attributes: [.font: font]))
            }
            if last {
                let glyph = FileGlyph.for(name: part)
                if let image = glyph.image {
                    text.append(Self.attachment(image, color: glyph.color, font: font))
                    text.append(NSAttributedString(string: " ", attributes: [.font: font]))
                }
            }
            text.append(NSAttributedString(string: part, attributes: [
                .foregroundColor: last ? NSColor.labelColor : NSColor.secondaryLabelColor,
                .font: font,
            ]))
        }
        label.setAccessibilityLabel(components.joined(separator: " / "))
    }

    private static func symbol(_ name: String, size: CGFloat, weight: NSFont.Weight,
                               color: NSColor, font: NSFont) -> NSAttributedString {
        guard let image = NSImage(systemSymbolName: name, accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: size, weight: weight)) else { return NSAttributedString() }
        return attachment(image, color: color, font: font)
    }

    /// Inline image, tinted and centered on the text's x-height.
    private static func attachment(_ image: NSImage, color: NSColor, font: NSFont) -> NSAttributedString {
        let tinted = image.withSymbolConfiguration(.init(paletteColors: [color])) ?? image
        let a = NSTextAttachment()
        a.image = tinted
        // Cap at 14pt so file glyphs sit with 13pt text instead of
        // stretching the row; SF Symbols (chevrons) are already smaller.
        var size = tinted.size
        let maxSide: CGFloat = 14
        if max(size.width, size.height) > maxSide {
            let scale = maxSide / max(size.width, size.height)
            size = NSSize(width: size.width * scale, height: size.height * scale)
        }
        a.bounds = NSRect(x: 0, y: (font.capHeight - size.height) / 2, width: size.width, height: size.height)
        return NSAttributedString(attachment: a)
    }

    func setEdited(_ edited: Bool) {
        if editedDot.isHidden == edited { editedDot.isHidden = !edited }
    }
}
