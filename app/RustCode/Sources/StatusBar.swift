import AppKit

/// Bottom status bar: a message on the left, document facts (cursor,
/// indentation, line ending, encoding) on the right.
@MainActor
final class StatusBar: NSView {
    let message = NSTextField(labelWithString: "")
    let largeFileBadge = NSTextField(labelWithString: "Large file")
    let cursor = StatusBar.label("status-cursor")
    let indent = StatusBar.label("status-indent")
    let lineEnding = StatusBar.label("status-eol")
    let encoding = StatusBar.label("status-encoding")

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Status bar")
        setAccessibilityIdentifier("status-bar")

        message.font = Theme.statusFont
        message.textColor = .secondaryLabelColor
        message.lineBreakMode = .byTruncatingTail
        message.setAccessibilityIdentifier("core-status")
        message.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        message.setContentHuggingPriority(.defaultLow, for: .horizontal)

        largeFileBadge.font = Theme.statusFont
        largeFileBadge.textColor = Theme.accent
        largeFileBadge.toolTip = "Over 20 MB: lines over 10,000 characters are shown cut off."
        largeFileBadge.setAccessibilityIdentifier("large-file-banner")
        largeFileBadge.isHidden = true

        let left = NSStackView(views: [message, largeFileBadge])
        left.spacing = 8
        let right = NSStackView(views: [cursor, indent, lineEnding, encoding])
        right.spacing = 14
        // The spacer takes the slack, so the facts sit on the right edge.
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        spacer.setContentCompressionResistancePriority(.init(1), for: .horizontal)
        let row = NSStackView(views: [left, spacer, right])
        row.distribution = .fill
        row.spacing = 12
        row.translatesAutoresizingMaskIntoConstraints = false
        right.setContentCompressionResistancePriority(.required, for: .horizontal)
        addSubview(row)

        let separator = NSBox()
        separator.boxType = .separator
        separator.translatesAutoresizingMaskIntoConstraints = false
        addSubview(separator)

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: Theme.statusHeight),
            separator.topAnchor.constraint(equalTo: topAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    // Own background, so the bar never shows through to whatever is behind
    // it (a window snapshot, or a transparent title bar later on).
    override func draw(_ dirtyRect: NSRect) {
        Theme.chrome.setFill()
        // Views do not clip to bounds by default since macOS 14, so the
        // dirty rect can reach past the bar and over the editor.
        dirtyRect.intersection(bounds).fill()
    }

    /// Shows the facts of `doc`, or clears them without a document.
    func show(document doc: DocumentHandle?) {
        guard let doc else {
            for label in [cursor, indent, lineEnding, encoding] { label.stringValue = "" }
            largeFileBadge.isHidden = true
            return
        }
        let settings = doc.indentSettings()
        indent.stringValue = settings.useTabs ? "Tab Size: \(settings.tabSize)" : "Spaces: \(settings.tabSize)"
        lineEnding.stringValue = switch doc.lineEnding() {
        case .lf: "LF"
        case .crLf: "CRLF"
        case .cr: "CR"
        }
        let name = switch doc.encoding() {
        case .utf8: "UTF-8"
        case .utf16Le: "UTF-16 LE"
        case .utf16Be: "UTF-16 BE"
        }
        encoding.stringValue = doc.hasBom() ? "\(name) with BOM" : name
        largeFileBadge.isHidden = !doc.isLarge()
        showCursor(of: doc)
    }

    func showCursor(of doc: DocumentHandle) {
        let snap = doc.viewport(firstLine: 0, count: 0)
        showCursor(primary: snap.primary, count: snap.selectionCount)
    }

    /// "Ln 3, Col 7", plus the cursor count when there are several.
    func showCursor(primary: CursorInfo, count: UInt64) {
        var text = "Ln \(primary.line + 1), Col \(primary.col + 1)"
        if count > 1 { text += " (\(count) cursors)" }
        cursor.stringValue = text
    }

    private static func label(_ id: String) -> NSTextField {
        let label = NSTextField(labelWithString: "")
        label.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        label.textColor = .secondaryLabelColor
        label.setAccessibilityIdentifier(id)
        return label
    }
}
