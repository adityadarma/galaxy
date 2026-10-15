import AppKit

/// Text of the inline git blame and the card shown when hovering it.
enum BlameFormat {
    /// "Putu Adi, 4 years ago • update sweetalert", or "You • Uncommitted changes".
    static func annotation(for commit: BlameCommitInfo, now: Date = .now) -> String {
        if commit.uncommitted { return "You • Uncommitted changes" }
        let who = commit.author.isEmpty ? "Unknown" : commit.author
        let summary = commit.summary.isEmpty ? "" : " • \(commit.summary)"
        return "\(who), \(relative(commit.time, now: now))\(summary)"
    }

    static func relative(_ time: Int64, now: Date = .now) -> String {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        let date = Date(timeIntervalSince1970: TimeInterval(time))
        // Seconds-old commits read "in 0 seconds" when clocks differ slightly.
        if abs(date.timeIntervalSince(now)) < 60 { return "just now" }
        return formatter.localizedString(for: date, relativeTo: now)
    }

    static func absolute(_ time: Int64) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(identifier: "GMT")
        formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss 'GMT'"
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(time)))
    }
}

/// Card with the author, message, short SHA and date of a commit, and a
/// button that copies the full SHA.
@MainActor
final class BlamePopover: NSView {
    static let width: CGFloat = 420
    private static let padding: CGFloat = 14

    private let nameLabel = NSTextField(labelWithString: "")
    private let emailLabel = NSTextField(labelWithString: "")
    private let summaryLabel = NSTextField(wrappingLabelWithString: "")
    private let shaLabel = NSTextField(labelWithString: "")
    private let dateLabel = NSTextField(labelWithString: "")
    private let copyButton = NSButton()
    private let stack = NSStackView()
    private(set) var sha = ""

    /// The pointer entered or left the card, so the owner can keep it open
    /// while the user reaches for `Copy SHA`.
    var onPointerEntered: (() -> Void)?
    var onPointerExited: (() -> Void)?

    /// The pasteboard `Copy SHA` writes to. Tests use a private one.
    var pasteboard: NSPasteboard = .general

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.cornerRadius = 10
        layer?.borderWidth = 1
        layer?.borderColor = NSColor.separatorColor.cgColor
        layer?.shadowOpacity = 0.25
        layer?.shadowRadius = 8
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Commit details")

        nameLabel.font = .systemFont(ofSize: 14, weight: .semibold)
        emailLabel.font = .systemFont(ofSize: 13)
        emailLabel.textColor = .linkColor
        summaryLabel.font = .systemFont(ofSize: 13, weight: .semibold)
        summaryLabel.maximumNumberOfLines = 4
        summaryLabel.preferredMaxLayoutWidth = Self.width - Self.padding * 2
        shaLabel.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        shaLabel.wantsLayer = true
        shaLabel.layer?.cornerRadius = 4
        shaLabel.layer?.backgroundColor = NSColor.quaternaryLabelColor.cgColor
        dateLabel.font = .systemFont(ofSize: 12)
        dateLabel.textColor = .secondaryLabelColor
        dateLabel.lineBreakMode = .byTruncatingTail

        copyButton.attributedTitle = NSAttributedString(string: " Copy SHA", attributes: [
            .foregroundColor: NSColor.linkColor,
            .font: NSFont.systemFont(ofSize: 13),
        ])
        copyButton.image = NSImage(systemSymbolName: "doc.on.doc", accessibilityDescription: nil)
        copyButton.imagePosition = .imageLeading
        copyButton.isBordered = false
        copyButton.contentTintColor = .linkColor
        copyButton.target = self
        copyButton.action = #selector(copySha(_:))
        // Pinned to the title plus icon: the stack view otherwise shrinks a
        // borderless button to its image.
        copyButton.translatesAutoresizingMaskIntoConstraints = false
        let titleWidth = ceil(copyButton.attributedTitle.size().width)
        copyButton.widthAnchor.constraint(equalToConstant: titleWidth + 24).isActive = true
        copyButton.heightAnchor.constraint(equalToConstant: 18).isActive = true
        copyButton.setAccessibilityLabel("Copy SHA")
        copyButton.setAccessibilityIdentifier("blame-copy-sha")

        let meta = NSStackView(views: [shaLabel, dateLabel])
        meta.orientation = .horizontal
        meta.spacing = 8
        meta.alignment = .centerY

        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        for view in [nameLabel, emailLabel, summaryLabel, meta, copyButton] { stack.addArrangedSubview(view) }
        stack.setCustomSpacing(12, after: emailLabel)
        stack.setCustomSpacing(12, after: summaryLabel)
        stack.setCustomSpacing(12, after: meta)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Self.padding),
            stack.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -Self.padding),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: Self.padding),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func show(_ commit: BlameCommitInfo) {
        sha = commit.sha
        if commit.uncommitted {
            nameLabel.stringValue = "You"
            emailLabel.stringValue = ""
            summaryLabel.stringValue = "Uncommitted changes"
            shaLabel.stringValue = " 0000000 "
            dateLabel.stringValue = ""
            copyButton.isHidden = true
        } else {
            nameLabel.stringValue = commit.author
            emailLabel.stringValue = commit.email
            summaryLabel.stringValue = commit.summary
            shaLabel.stringValue = " \(commit.sha.prefix(7)) "
            dateLabel.stringValue = "\(BlameFormat.absolute(commit.time)) (\(BlameFormat.relative(commit.time)))"
            copyButton.isHidden = false
        }
        emailLabel.isHidden = emailLabel.stringValue.isEmpty
        setAccessibilityValue("\(nameLabel.stringValue), \(summaryLabel.stringValue)")
        stack.layoutSubtreeIfNeeded()
    }

    /// Fixed width, height from the content.
    func fittingSize() -> NSSize {
        stack.layoutSubtreeIfNeeded()
        return NSSize(width: Self.width, height: ceil(stack.fittingSize.height) + Self.padding * 2)
    }

    @objc func copySha(_ sender: Any?) {
        guard !sha.isEmpty else { return }
        pasteboard.clearContents()
        pasteboard.setString(sha, forType: .string)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: bounds, options: [.activeInKeyWindow, .mouseEnteredAndExited, .inVisibleRect], owner: self))
    }

    override func mouseEntered(with event: NSEvent) { onPointerEntered?() }
    override func mouseExited(with event: NSEvent) { onPointerExited?() }

    override func updateLayer() { layer?.backgroundColor = Theme.sidebar.cgColor }
}
