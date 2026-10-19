import AppKit

/// Shown in place of the editor when no tab is open: a dimmed app icon and
/// the shortcuts that start work (go to file, commands, open a folder).
/// Shortcuts are read from the main menu, so rebinding a key shows here too.
@MainActor
final class EmptyEditorView: NSView {
    private struct Entry {
        let title: String
        let action: Selector
    }

    private static let entries: [Entry] = [
        Entry(title: "Go to File", action: #selector(MainWindowController.showGoToFile(_:))),
        Entry(title: "Show All Commands", action: #selector(MainWindowController.showCommandPalette(_:))),
        Entry(title: "New File", action: #selector(MainWindowController.newDocument(_:))),
        Entry(title: "Open Folder", action: #selector(MainWindowController.openFolder(_:))),
        Entry(title: "Toggle Terminal", action: #selector(MainWindowController.toggleTerminal(_:))),
        Entry(title: "Open Settings", action: #selector(AppDelegate.showPreferences(_:))),
    ]

    /// Below this height the icon is dropped so the shortcuts still fit.
    private static let minimumHeightForIcon: CGFloat = 460

    private let iconView = NSImageView()
    private let stack = NSStackView()
    private var grid = NSGridView()

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityIdentifier("empty-editor")
        isHidden = true

        iconView.image = NSApp.applicationIconImage
        iconView.imageScaling = .scaleProportionallyUpOrDown
        iconView.alphaValue = 0.14
        iconView.setAccessibilityElement(false)
        iconView.translatesAutoresizingMaskIntoConstraints = false

        stack.addArrangedSubview(iconView)
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 44
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)

        NSLayoutConstraint.activate([
            iconView.widthAnchor.constraint(equalToConstant: 180),
            iconView.heightAnchor.constraint(equalToConstant: 180),
            stack.centerXAnchor.constraint(equalTo: centerXAnchor),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor, constant: -16),
        ])
        reload()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isOpaque: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        dirtyRect.intersection(bounds).fill()
    }

    override func layout() {
        super.layout()
        iconView.isHidden = bounds.height < Self.minimumHeightForIcon
    }

    /// Rebuilds the rows from the current menu shortcuts. The old grid is
    /// removed as a view: `NSGridView.removeRow` leaves the cell views in
    /// place, and the next rows would be drawn over them.
    func reload() {
        if grid.superview != nil {
            stack.removeArrangedSubview(grid)
            grid.removeFromSuperview()
        }
        let shortcuts = CommandPaletteView.shortcuts(in: NSApp.mainMenu)
        var rows: [[NSView]] = []
        for entry in Self.entries {
            guard let shortcut = shortcuts[entry.action] else { continue }
            let title = NSTextField(labelWithString: entry.title)
            title.font = .systemFont(ofSize: 13)
            title.textColor = .secondaryLabelColor
            let caps = NSStackView(views: Self.keys(from: shortcut).map { KeyCapView($0) })
            caps.orientation = .horizontal
            caps.spacing = 4
            rows.append([title, caps])
        }
        grid = NSGridView(views: rows)
        grid.rowSpacing = 10
        grid.columnSpacing = 40
        grid.translatesAutoresizingMaskIntoConstraints = false
        if grid.numberOfColumns == 2 {
            grid.column(at: 0).xPlacement = .leading
            grid.column(at: 1).xPlacement = .trailing
        }
        stack.addArrangedSubview(grid)
    }

    /// "⇧⌘P" becomes ["⇧", "⌘", "P"]: one cap per modifier, then the key.
    static func keys(from shortcut: String) -> [String] {
        var parts: [String] = []
        var rest = Substring(shortcut)
        while let first = rest.first, "⌃⌥⇧⌘".contains(first) {
            parts.append(String(first))
            rest = rest.dropFirst()
        }
        if !rest.isEmpty { parts.append(String(rest)) }
        return parts
    }
}

/// One key of a shortcut, drawn as a small rounded cap.
@MainActor
private final class KeyCapView: NSView {
    init(_ text: String) {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = 4
        layer?.borderWidth = 1
        translatesAutoresizingMaskIntoConstraints = false

        let label = NSTextField(labelWithString: text)
        label.font = .systemFont(ofSize: 12, weight: .medium)
        label.textColor = .secondaryLabelColor
        label.alignment = .center
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 22),
            widthAnchor.constraint(greaterThanOrEqualToConstant: 22),
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 6),
            label.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(0.08).cgColor
            layer?.borderColor = NSColor.labelColor.withAlphaComponent(0.14).cgColor
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }
}
