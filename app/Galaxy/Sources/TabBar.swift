import AppKit

/// One open document (M2-06).
@MainActor
final class EditorTab {
    let doc: DocumentHandle
    /// File name shown on the tab.
    var title: String
    /// Breadcrumb for the path bar: workspace, folders, file.
    var pathComponents: [String]
    /// Window title (hidden, but used by the Window menu and VoiceOver).
    var windowTitle: String
    /// A preview tab is replaced by the next file opened from the explorer,
    /// until it is edited or double-clicked.
    var isPreview: Bool
    /// Status bar message for this document.
    var status: String
    /// Scroll position to restore when the tab is shown again.
    var scrollY: CGFloat?
    /// Unsaved changes, as of the last check (the active tab is checked
    /// once per frame; others cannot change while hidden).
    var isEdited = false
    /// Resolved path, to find the tab of a file that is already open.
    var fileKey: String?
    /// Path relative to the workspace root, for files inside it.
    var relPath: String?
    /// The file is gone from disk; the tab keeps the text (M2-03).
    var isDeletedOnDisk = false
    /// Changed on disk while it had unsaved edits; asked about once shown.
    var hasDiskConflict = false
    /// Last time this tab was active, used to release background syntax state.
    var lastActiveAt = Date.now
    /// Title with the file's state on disk.
    var displayTitle: String { isDeletedOnDisk ? "\(title) (deleted)" : title }
    /// Identifies an Untitled buffer in the back/forward history.
    private let untitledKey = "untitled:" + UUID().uuidString
    /// Document key for the back/forward history (M2-08).
    var navKey: String { fileKey ?? untitledKey }

    init(doc: DocumentHandle, title: String, pathComponents: [String], windowTitle: String,
         isPreview: Bool, status: String) {
        self.doc = doc
        self.title = title
        self.pathComponents = pathComponents
        self.windowTitle = windowTitle
        self.isPreview = isPreview
        self.status = status
    }
}

/// Tab strip in the title bar row. The active tab takes the editor's color
/// and a copper rule on top; preview tabs have an italic title; unsaved
/// tabs show a copper dot where the close button is (the button returns on
/// hover). Drag a tab to reorder it, middle-click to close it.
@MainActor
final class TabBar: NSView {
    struct Item: Equatable {
        var title: String
        var isPreview: Bool
        var isEdited: Bool
        var isGitChanged: Bool
        var gitStatus: GitFileStatus

        init(title: String, isPreview: Bool, isEdited: Bool, isGitChanged: Bool, gitStatus: GitFileStatus = .none) {
            self.title = title
            self.isPreview = isPreview
            self.isEdited = isEdited
            self.isGitChanged = isGitChanged
            self.gitStatus = gitStatus != .none ? gitStatus : (isGitChanged ? .modified : .none)
        }
    }

    static let minTabWidth: CGFloat = 96
    static let maxTabWidth: CGFloat = 240

    private(set) var items: [Item] = []
    private(set) var selectedIndex: Int?
    private(set) var tabViews: [TabItemView] = []

    var onSelect: ((Int) -> Void)?
    var onClose: ((Int) -> Void)?
    /// `(from, to)`: `to` is the tab's final index.
    var onMove: ((Int, Int) -> Void)?

    private let stack = NSStackView()

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityElement(true)
        setAccessibilityRole(.tabGroup)
        setAccessibilityLabel("Open files")
        setAccessibilityIdentifier("tab-bar")

        stack.orientation = .horizontal
        stack.spacing = 0
        stack.distribution = .fill
        stack.alignment = .centerY
        stack.translatesAutoresizingMaskIntoConstraints = false
        stack.setHuggingPriority(.defaultHigh, for: .horizontal)
        addSubview(stack)

        let rule = NSBox()
        rule.boxType = .separator
        rule.translatesAutoresizingMaskIntoConstraints = false
        addSubview(rule, positioned: .below, relativeTo: stack)

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: Theme.tabHeight),
            widthAnchor.constraint(greaterThanOrEqualToConstant: 0),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            rule.bottomAnchor.constraint(equalTo: bottomAnchor),
            rule.leadingAnchor.constraint(equalTo: leadingAnchor),
            rule.trailingAnchor.constraint(equalTo: trailingAnchor),
            stack.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor),
            stack.topAnchor.constraint(equalTo: topAnchor),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func draw(_ dirtyRect: NSRect) {
        Theme.chrome.setFill()
        dirtyRect.intersection(bounds).fill()
    }

    /// Set by the window: the empty strip after the tabs acts as title bar.
    weak var titleBar: TitleBar?

    override func mouseDown(with event: NSEvent) {
        if event.clickCount == 1 { window?.performDrag(with: event) }
    }

    override func mouseUp(with event: NSEvent) {
        if let titleBar { titleBar.mouseUp(with: event) } else { super.mouseUp(with: event) }
    }

    /// Replaces all tabs. Cheap enough: tab counts are small, and per-key
    /// updates go through `setEdited` instead.
    func set(items: [Item], selected: Int?) {
        if items.count == tabViews.count {
            for (view, item) in zip(tabViews, items) { view.item = item }
        } else {
            tabViews.forEach { $0.removeFromSuperview() }
            tabViews = items.map { item in
                let view = TabItemView(item: item)
                view.bar = self
                stack.addArrangedSubview(view)
                return view
            }
        }
        self.items = items
        selectedIndex = selected
        for (i, view) in tabViews.enumerated() {
            view.index = i
            view.isSelected = i == selected
        }
    }

    /// Called once per frame while typing; touches one view at most.
    func setEdited(_ edited: Bool, at index: Int) {
        guard items.indices.contains(index), items[index].isEdited != edited else { return }
        items[index].isEdited = edited
        tabViews[index].item = items[index]
    }

    // MARK: Dragging

    /// Final index for a tab dragged from `from` and dropped at `x`.
    func dropIndex(forX x: CGFloat, from: Int) -> Int {
        var slot = tabViews.count
        for (i, view) in tabViews.enumerated() {
            let frame = convert(view.bounds, from: view)
            if x < frame.midX { slot = i; break }
        }
        return slot > from ? slot - 1 : slot
    }

    /// Selects on mouse down, then follows the drag until mouse up.
    func track(tab index: Int, from event: NSEvent) {
        onSelect?(index)
        guard let window else { return }
        let start = convert(event.locationInWindow, from: nil)
        var dragging = false
        while let next = window.nextEvent(matching: [.leftMouseDragged, .leftMouseUp]) {
            let point = convert(next.locationInWindow, from: nil)
            if next.type == .leftMouseUp {
                if dragging {
                    let to = dropIndex(forX: point.x, from: index)
                    if to != index { onMove?(index, to) }
                }
                return
            }
            if abs(point.x - start.x) > 4 {
                dragging = true
                NSCursor.closedHand.set()
            }
        }
    }
}

/// One tab: title, and a close button that doubles as the unsaved dot.
@MainActor
final class TabItemView: NSView {
    weak var bar: TabBar?
    var index = 0
    var item: TabBar.Item { didSet { if item != oldValue { refresh() } } }
    var isSelected = false { didSet { if isSelected != oldValue { refresh() } } }

    let label = NSTextField(labelWithString: "")
    /// File-type glyph, as in the explorer.
    let icon = NSImageView()
    let closeButton = NSButton()
    let editedDot = NSTextField(labelWithString: "●")
    let gitDot = NSTextField(labelWithString: "●")
    private var isHovered = false { didSet { if isHovered != oldValue { refresh() } } }

    init(item: TabBar.Item) {
        self.item = item
        super.init(frame: .zero)
        setAccessibilityElement(true)
        setAccessibilityRole(.radioButton)
        setAccessibilityIdentifier("tab")

        icon.imageScaling = .scaleNone
        icon.setAccessibilityElement(false)
        icon.translatesAutoresizingMaskIntoConstraints = false
        label.lineBreakMode = .byTruncatingMiddle
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        label.translatesAutoresizingMaskIntoConstraints = false

        closeButton.image = NSImage(systemSymbolName: "xmark", accessibilityDescription: "Close")
        closeButton.symbolConfiguration = .init(pointSize: 11, weight: .regular)
        closeButton.isBordered = false
        closeButton.imagePosition = .imageOnly
        closeButton.contentTintColor = .secondaryLabelColor
        closeButton.target = self
        closeButton.action = #selector(closeClicked(_:))
        closeButton.setAccessibilityIdentifier("tab-close")
        closeButton.translatesAutoresizingMaskIntoConstraints = false

        editedDot.font = .systemFont(ofSize: 9)
        editedDot.textColor = Theme.accent
        editedDot.alignment = .center
        editedDot.setAccessibilityElement(false)
        editedDot.translatesAutoresizingMaskIntoConstraints = false
        gitDot.font = .systemFont(ofSize: 9)
        gitDot.textColor = .systemYellow
        gitDot.setAccessibilityElement(false)
        gitDot.translatesAutoresizingMaskIntoConstraints = false

        for view in [icon, label, closeButton, editedDot, gitDot] { addSubview(view) }
        NSLayoutConstraint.activate([
            widthAnchor.constraint(greaterThanOrEqualToConstant: TabBar.minTabWidth),
            widthAnchor.constraint(lessThanOrEqualToConstant: TabBar.maxTabWidth),
            heightAnchor.constraint(equalToConstant: Theme.tabHeight),
            icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 16),
            label.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 6),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            closeButton.leadingAnchor.constraint(equalTo: label.trailingAnchor, constant: 6),
            closeButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -10),
            closeButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            closeButton.widthAnchor.constraint(equalToConstant: 16),
            closeButton.heightAnchor.constraint(equalToConstant: 16),
            editedDot.centerXAnchor.constraint(equalTo: closeButton.centerXAnchor),
            editedDot.centerYAnchor.constraint(equalTo: closeButton.centerYAnchor),
            gitDot.trailingAnchor.constraint(equalTo: closeButton.leadingAnchor, constant: -3),
            gitDot.centerYAnchor.constraint(equalTo: closeButton.centerYAnchor),
        ])
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// The dot replaces the close button on unsaved tabs, except on hover.
    var showsEditedDot: Bool { item.isEdited && !isHovered }

    private func refresh() {
        let size: CGFloat = 13
        var font = NSFont.systemFont(ofSize: size, weight: .regular)
        if item.isPreview {
            font = NSFontManager.shared.convert(font, toHaveTrait: .italicFontMask)
        }
        label.font = font
        label.stringValue = item.title
        if let gitColor = item.gitStatus.color {
            label.textColor = gitColor
        } else {
            label.textColor = isSelected ? .labelColor : .secondaryLabelColor
        }
        let glyph = FileGlyph.for(name: item.title)
        icon.image = glyph.image
        icon.contentTintColor = glyph.color
        icon.alphaValue = isSelected ? 1 : 0.8
        editedDot.isHidden = !showsEditedDot
        gitDot.isHidden = true
        closeButton.isHidden = showsEditedDot || !(isSelected || isHovered)
        closeButton.setAccessibilityLabel("Close \(item.title)")
        toolTip = item.isPreview ? "\(item.title) (preview: double-click the file to keep it open)" : item.title

        var label = item.title
        if item.isEdited { label += ", edited" }
        if item.isPreview { label += ", preview" }
        setAccessibilityLabel(label)
        setAccessibilityValue(isSelected ? 1 : 0)
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        if isSelected {
            NSColor.textBackgroundColor.setFill()
            bounds.fill()
            Theme.accent.setFill()
            NSRect(x: 0, y: bounds.maxY - 2, width: bounds.width, height: 2).fill()
        } else if isHovered {
            NSColor.labelColor.withAlphaComponent(0.04).setFill()
            bounds.fill()
        }
        // Full-height divider between tabs, as in VSCode.
        NSColor.separatorColor.setFill()
        NSRect(x: bounds.maxX - 1, y: 0, width: 1, height: bounds.height).fill()
    }

    override var isFlipped: Bool { false }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
                                       owner: self, userInfo: nil))
    }

    override func mouseEntered(with event: NSEvent) { isHovered = true }
    override func mouseExited(with event: NSEvent) { isHovered = false }

    override func mouseDown(with event: NSEvent) {
        bar?.track(tab: index, from: event)
    }

    override func otherMouseUp(with event: NSEvent) {
        if event.buttonNumber == 2 { bar?.onClose?(index) }
    }

    override func accessibilityPerformPress() -> Bool {
        bar?.onSelect?(index)
        return true
    }

    @objc private func closeClicked(_ sender: Any?) {
        bar?.onClose?(index)
    }
}
