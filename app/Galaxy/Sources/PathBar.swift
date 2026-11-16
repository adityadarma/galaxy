import AppKit

/// Path of the open document, under the tabs: "workspace › src › main.rs".
/// The window title itself is hidden, but still set, for the Window menu
/// and accessibility.
@MainActor
final class PathBar: NSView {
    static let height: CGFloat = 26
    static let inset: CGFloat = 16
    /// Folders and chevrons stay readable on the editor surface in both
    /// appearances; the file name is the brightest.
    static let folderColor = NSColor.labelColor.withAlphaComponent(0.72)
    static let separatorColor = NSColor.labelColor.withAlphaComponent(0.55)

    let label = NSTextField(labelWithString: "")
    /// Copper dot after the file name while there are unsaved changes.
    let editedDot = NSTextField(labelWithString: "●")
    private(set) var components: [String] = []
    var isEdited: Bool { !editedDot.isHidden }
    var onSelectSymbol: ((OutlineSymbol) -> Void)?
    /// Supplies the menu for a clicked folder component. The final file name
    /// deliberately has no menu because it is not a directory.
    var menuForComponent: ((Int) -> NSMenu?)?
    private let pathRow = NSStackView()
    private var visibleSymbols: [OutlineSymbol] = []
    private var symbolViews: [NSView] = []
    private var heightConstraint: NSLayoutConstraint!
    /// Off: hide the entire breadcrumb row, including the file path.
    var isEnabled = true {
        didSet {
            isHidden = !isEnabled
            heightConstraint.constant = isEnabled ? Self.height : 0
        }
    }

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

        label.isHidden = true
        pathRow.spacing = 2
        pathRow.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let row = NSStackView(views: [pathRow, editedDot])
        row.spacing = 6
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)

        heightConstraint = heightAnchor.constraint(equalToConstant: Self.height)
        NSLayoutConstraint.activate([
            heightConstraint,
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Self.inset),
            row.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -12),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
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
        for view in pathRow.arrangedSubviews {
            pathRow.removeArrangedSubview(view)
            view.removeFromSuperview()
        }
        // The loop above also removed the symbol views; forget them so
        // `show(symbols:atLine:)` does not remove them a second time.
        symbolViews = []
        visibleSymbols = []
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
                text.append(Self.symbol("chevron.right", size: 10, weight: .semibold,
                                        color: Self.separatorColor, font: font))
                text.append(NSAttributedString(string: " ", attributes: [.font: font]))
            }
            let button = NSButton(title: part, target: self, action: #selector(selectComponent(_:)))
            button.tag = i
            button.isBordered = false
            button.bezelStyle = .inline
            button.font = font
            button.contentTintColor = last ? .labelColor : Self.folderColor
            button.setAccessibilityIdentifier("path-component-\(i)")
            button.toolTip = last ? nil : "Show \(part)"
            if last, let image = FileGlyph.for(name: part).image {
                button.image = image
                button.imagePosition = .imageLeading
            }
            pathRow.addArrangedSubview(button)
            if !last {
                let separator = NSTextField(labelWithString: "›")
                separator.font = font
                separator.textColor = Self.separatorColor
                separator.alignment = .center
                separator.setContentHuggingPriority(.required, for: .horizontal)
                separator.widthAnchor.constraint(equalToConstant: 10).isActive = true
                pathRow.addArrangedSubview(separator)
            }
            if last {
                let glyph = FileGlyph.for(name: part)
                if let image = glyph.image {
                    text.append(Self.attachment(image, color: glyph.color, font: font))
                    text.append(NSAttributedString(string: " ", attributes: [.font: font]))
                }
            }
            text.append(NSAttributedString(string: part, attributes: [
                .foregroundColor: last ? NSColor.labelColor : Self.folderColor,
                .font: font,
            ]))
        }
        label.setAccessibilityLabel(components.joined(separator: " / "))
    }

    /// Appends the active symbol scope after the file name, matching editor
    /// breadcrumbs: path, file, namespace/class, then the current member.
    func show(symbols: [OutlineSymbol], atLine line: UInt64) {
        // NSStackView asserts when asked to remove a view it no longer arranges.
        let arranged = Set(pathRow.arrangedSubviews.map(ObjectIdentifier.init))
        for view in symbolViews {
            if arranged.contains(ObjectIdentifier(view)) { pathRow.removeArrangedSubview(view) }
            view.removeFromSuperview()
        }
        var active: [OutlineSymbol] = []
        for symbol in symbols where symbol.line <= line {
            let depth = Int(symbol.depth)
            if active.count > depth { active.removeSubrange(depth...) }
            guard active.count == depth else { continue }
            active.append(symbol)
        }
        visibleSymbols = active
        symbolViews = []
        for (index, symbol) in visibleSymbols.enumerated() {
            let separator = NSTextField(labelWithString: "›")
            separator.font = .systemFont(ofSize: 13)
            separator.textColor = Self.separatorColor
            separator.alignment = .center
            separator.widthAnchor.constraint(equalToConstant: 10).isActive = true
            pathRow.addArrangedSubview(separator)
            symbolViews.append(separator)

            let button = NSButton(title: symbol.name, target: self, action: #selector(selectSymbol(_:)))
            button.tag = index
            button.bezelStyle = .inline
            button.isBordered = false
            button.font = .systemFont(ofSize: 13)
            button.contentTintColor = index == visibleSymbols.count - 1 ? .labelColor : Self.folderColor
            button.setAccessibilityIdentifier("breadcrumb-symbol-\(index)")
            pathRow.addArrangedSubview(button)
            symbolViews.append(button)
        }
    }

    @objc private func selectComponent(_ sender: NSButton) {
        guard sender.tag < components.count - 1, let menu = menuForComponent?(sender.tag) else { return }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: sender.bounds.height), in: sender)
    }

    @objc private func selectSymbol(_ sender: NSButton) {
        guard visibleSymbols.indices.contains(sender.tag) else { return }
        onSelectSymbol?(visibleSymbols[sender.tag])
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
