import AppKit

/// One open document (M2-06).
@MainActor
final class EditorTab {
    let doc: DocumentHandle
    /// Set for a terminal moved into the editor area (VS Code "Move
    /// Terminal into Editor Area"). `doc` is then an empty, never-shown
    /// buffer: it has no path and is never modified, so saving, the
    /// session and the unsaved prompts skip the tab.
    let terminal: TerminalHandle?
    /// The Git Graph view as a tab. Like a terminal tab, `doc` is an empty,
    /// never-shown buffer.
    let isGitGraph: Bool
    /// A real text document: not a terminal and not the Git Graph.
    var isDocument: Bool { terminal == nil && !isGitGraph }
    /// File name shown on the tab.
    var title: String
    /// Breadcrumb for the path bar: workspace, folders, file.
    var pathComponents: [String]
    /// Window title (hidden, but used by the Window menu and VoiceOver).
    var windowTitle: String
    /// A preview tab is replaced by the next file opened from the explorer,
    /// until it is edited or double-clicked.
    var isPreview: Bool
    /// Pinned tabs sit first and are skipped by Close Others, Close to the
    /// Right, Close Saved and Close All. Only closing the tab itself closes it.
    var isPinned = false
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
    /// The file as committed at HEAD, shown read-only beside the working copy
    /// (opened from Source Control). `nil` is the plain single-pane view.
    var diffBase: DocumentHandle?
    /// Who last changed each line, for the inline blame; refreshed after edits.
    var blame: BlameInfo?
    /// Document line count when `blame` was computed. Edits that add or remove
    /// lines make the line numbers stale until the next refresh.
    var blameLineCount: UInt64 = 0
    /// Bumped per request, so a slow answer for old text is dropped.
    var blameGeneration = 0
    /// Title with the file's state on disk.
    var displayTitle: String {
        if isDeletedOnDisk { return "\(title) (deleted)" }
        return diffBase == nil ? title : "\(title) (Working Tree)"
    }
    /// Identifies an Untitled buffer in the back/forward history.
    private let untitledKey = "untitled:" + UUID().uuidString
    /// Document key for the back/forward history (M2-08).
    var navKey: String { fileKey ?? untitledKey }

    init(doc: DocumentHandle, title: String, pathComponents: [String], windowTitle: String,
         isPreview: Bool, status: String, terminal: TerminalHandle? = nil, isGitGraph: Bool = false) {
        self.doc = doc
        self.terminal = terminal
        self.isGitGraph = isGitGraph
        self.title = title
        self.pathComponents = pathComponents
        self.windowTitle = windowTitle
        self.isPreview = isPreview
        self.status = status
    }
}

/// Scroll view of the tab strip. No scrollers; the mouse wheel and the
/// trackpad both move it sideways, as in VSCode.
@MainActor
final class TabScrollView: NSScrollView {
    override func scrollWheel(with event: NSEvent) {
        let dx = event.scrollingDeltaX, dy = event.scrollingDeltaY
        var delta = abs(dx) >= abs(dy) ? dx : dy
        if !event.hasPreciseScrollingDeltas { delta *= 12 }
        scroll(toX: contentView.bounds.origin.x - delta)
    }

    /// Moves the strip so `x` is the left edge, within the strip's ends.
    func scroll(toX x: CGFloat) {
        let maxX = max(0, (documentView?.frame.width ?? 0) - contentView.bounds.width)
        let clamped = min(max(0, x), maxX)
        guard abs(clamped - contentView.bounds.origin.x) > 0.01 else { return }
        contentView.scroll(to: NSPoint(x: clamped, y: contentView.bounds.origin.y))
        reflectScrolledClipView(contentView)
    }
}

/// Tab strip in the title bar row. The active tab takes the editor's color
/// and a copper rule on top; preview tabs have an italic title; unsaved
/// tabs show a copper dot where the close button is (the button returns on
/// hover). Drag a tab to reorder it, middle-click to close it. Tabs keep the
/// width of their title; when they do not all fit, the strip scrolls and the
/// active tab is brought into view.
@MainActor
final class TabBar: NSView {
    struct Item: Equatable {
        var title: String
        var isPreview: Bool
        var isEdited: Bool
        var isGitChanged: Bool
        var gitStatus: GitFileStatus
        /// Terminal tab: terminal icon instead of a file glyph.
        var isTerminal = false
        /// Git Graph tab: graph icon instead of a file glyph.
        var isGitGraph = false
        /// File name the glyph is chosen from, when the title has a suffix.
        var glyphName: String?
        /// Pinned: a pin replaces the close button; clicking it unpins.
        var isPinned = false

        init(title: String, isPreview: Bool, isEdited: Bool, isGitChanged: Bool, gitStatus: GitFileStatus = .none,
             isTerminal: Bool = false, glyphName: String? = nil, isGitGraph: Bool = false, isPinned: Bool = false) {
            self.isPinned = isPinned
            self.isTerminal = isTerminal
            self.isGitGraph = isGitGraph
            self.glyphName = glyphName
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
    /// The tab was double-clicked: a preview tab becomes a regular one.
    var onKeep: ((Int) -> Void)?
    /// The pin on a pinned tab was clicked.
    var onUnpin: ((Int) -> Void)?
    /// `(from, to)`: `to` is the tab's final index.
    var onMove: ((Int, Int) -> Void)?
    /// Tab `index` was dropped at a window point away from this bar. Returns
    /// whether another editor group took it; if not, the bar reorders as usual.
    var onDropOutside: ((Int, NSPoint) -> Bool)?
    /// Context menu for tab `index`, if any.
    var menuForTab: ((Int) -> NSMenu?)?
    /// The preview button (shown for Markdown files) was clicked.
    var onTogglePreview: (() -> Void)?

    /// Toggle at the right end of the bar, like VS Code's "Open Preview to the Side".
    let previewButton = NSButton()
    static let previewButtonWidth: CGFloat = 36

    let scrollView = TabScrollView()
    /// Document view of `scrollView`: as wide as all the tabs together.
    private let strip = NSView()
    private let stack = NSStackView()
    /// Selection or tab count changed: scroll the active tab into view on
    /// the next layout, once the bar has a width.
    private var needsReveal = false
    /// Line under the tabs. Hidden while no tab is open.
    private let rule = NSBox()
    /// Width of all the tabs together.
    private(set) var contentWidth: CGFloat = 0

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
        strip.addSubview(stack)

        scrollView.documentView = strip
        scrollView.hasHorizontalScroller = false
        scrollView.hasVerticalScroller = false
        scrollView.horizontalScrollElasticity = .none
        scrollView.verticalScrollElasticity = .none
        scrollView.drawsBackground = false
        scrollView.borderType = .noBorder
        scrollView.contentView.copiesOnScroll = false
        addSubview(scrollView)

        previewButton.setButtonType(.momentaryChange)
        previewButton.isBordered = false
        previewButton.imagePosition = .imageOnly
        previewButton.image = NSImage(systemSymbolName: "eye", accessibilityDescription: "Toggle Markdown Preview")?
            .withSymbolConfiguration(.init(pointSize: 14, weight: .regular))
        previewButton.contentTintColor = .secondaryLabelColor
        previewButton.toolTip = "Toggle Markdown Preview (⇧⌘V)"
        previewButton.setAccessibilityLabel("Toggle Markdown Preview")
        previewButton.setAccessibilityIdentifier("markdown-preview-button")
        previewButton.target = self
        previewButton.action = #selector(previewButtonClicked(_:))
        previewButton.isHidden = true
        previewButton.translatesAutoresizingMaskIntoConstraints = false
        addSubview(previewButton)

        rule.boxType = .separator
        rule.translatesAutoresizingMaskIntoConstraints = false
        addSubview(rule, positioned: .below, relativeTo: scrollView)

        NSLayoutConstraint.activate([
            previewButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            previewButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            previewButton.widthAnchor.constraint(equalToConstant: 26),
            previewButton.heightAnchor.constraint(equalToConstant: 24),
            heightAnchor.constraint(equalToConstant: Theme.tabHeight),
            widthAnchor.constraint(greaterThanOrEqualToConstant: 0),
            stack.leadingAnchor.constraint(equalTo: strip.leadingAnchor),
            stack.topAnchor.constraint(equalTo: strip.topAnchor),
            stack.bottomAnchor.constraint(equalTo: strip.bottomAnchor),
            rule.bottomAnchor.constraint(equalTo: bottomAnchor),
            rule.leadingAnchor.constraint(equalTo: leadingAnchor),
            rule.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
    }

    /// Width a tab takes: its title, within the tab limits.
    private static func width(of tab: TabItemView) -> CGFloat {
        min(max(tab.intrinsicContentSize.width, minTabWidth), maxTabWidth)
    }

    override func layout() {
        super.layout()
        let widths = tabViews.map(Self.width(of:))
        contentWidth = widths.reduce(0, +)
        strip.setFrameSize(NSSize(width: max(contentWidth, 1), height: bounds.height))
        // The scroll view only covers the tabs, so the empty strip after them
        // stays a title bar to drag the window by.
        let reserved = previewButton.isHidden ? 0 : Self.previewButtonWidth
        scrollView.frame = NSRect(x: 0, y: 0, width: min(contentWidth, max(0, bounds.width - reserved)), height: bounds.height)
        guard bounds.width > 0 else { return }
        // Keep the offset valid after the bar grows or tabs close.
        scrollView.scroll(toX: scrollView.contentView.bounds.origin.x)
        if needsReveal, let selectedIndex, widths.indices.contains(selectedIndex) {
            needsReveal = false
            let left = widths[..<selectedIndex].reduce(0, +)
            let right = left + widths[selectedIndex]
            let origin = scrollView.contentView.bounds.origin.x
            let visible = scrollView.contentView.bounds.width
            if left < origin {
                scrollView.scroll(toX: left)
            } else if right > origin + visible {
                scrollView.scroll(toX: right - visible)
            }
        }
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

    /// Shows the preview button for a Markdown tab; `active` while the preview is open.
    func setPreviewButton(visible: Bool, active: Bool) {
        let changed = previewButton.isHidden == visible
        previewButton.isHidden = !visible
        previewButton.contentTintColor = active ? Theme.accent : .secondaryLabelColor
        previewButton.setAccessibilityValue(active ? "on" : "off")
        if changed { needsLayout = true }
    }

    @objc private func previewButtonClicked(_ sender: Any?) { onTogglePreview?() }

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
        if selected != selectedIndex || items.count != self.items.count { needsReveal = true }
        self.items = items
        selectedIndex = selected
        rule.isHidden = items.isEmpty
        needsLayout = true
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
        if event.clickCount == 2 { onKeep?(index) }
        guard let window else { return }
        let start = convert(event.locationInWindow, from: nil)
        var dragging = false
        while let next = window.nextEvent(matching: [.leftMouseDragged, .leftMouseUp]) {
            let point = convert(next.locationInWindow, from: nil)
            if next.type == .leftMouseUp {
                if dragging {
                    // Dropped away from the strip: another group may take it.
                    let outside = point.y < -4 || point.y > bounds.height + 4 || point.x < 0 || point.x > bounds.width
                    if outside || onDropOutside != nil && !bounds.contains(point) {
                        if onDropOutside?(index, next.locationInWindow) == true { return }
                    }
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

    private static let leadingInset: CGFloat = 12 + 16 + 6 // margin, icon, gap
    private static let trailingInset: CGFloat = 6 + 16 + 10 // gap, close button, margin
    /// The title ends before the close button. Flipped off when the tab is too
    /// narrow for both: the title keeps the room and the button overlays it.
    private var closeIsInline = true { didSet { if closeIsInline != oldValue { applyCloseLayout() } } }
    private var titleEndsAtButton: NSLayoutConstraint!
    private var titleEndsAtEdge: NSLayoutConstraint!

    init(item: TabBar.Item) {
        self.item = item
        super.init(frame: .zero)
        setAccessibilityElement(true)
        setAccessibilityRole(.radioButton)
        setAccessibilityIdentifier("tab")

        icon.imageScaling = .scaleProportionallyDown
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
        titleEndsAtButton = label.trailingAnchor.constraint(lessThanOrEqualTo: closeButton.leadingAnchor, constant: -6)
        titleEndsAtEdge = label.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -10)
        closeButton.wantsLayer = true
        closeButton.layer?.cornerRadius = 3
        editedDot.wantsLayer = true
        editedDot.layer?.cornerRadius = 3
        NSLayoutConstraint.activate([
            titleEndsAtButton,
            widthAnchor.constraint(greaterThanOrEqualToConstant: TabBar.minTabWidth),
            widthAnchor.constraint(lessThanOrEqualToConstant: TabBar.maxTabWidth),
            heightAnchor.constraint(equalToConstant: Theme.tabHeight),
            icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 16),
            icon.heightAnchor.constraint(equalToConstant: 16),
            label.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 6),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
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

    /// Width the tab wants: the whole title plus room for the close button.
    override var intrinsicContentSize: NSSize {
        NSSize(width: Self.leadingInset + ceil(label.intrinsicContentSize.width) + Self.trailingInset,
               height: NSView.noIntrinsicMetric)
    }

    override func layout() {
        super.layout()
        let needed = Self.leadingInset + ceil(label.intrinsicContentSize.width) + Self.trailingInset
        closeIsInline = bounds.width >= needed
    }

    private func applyCloseLayout() {
        titleEndsAtButton.isActive = closeIsInline
        titleEndsAtEdge.isActive = !closeIsInline
        refresh()
    }

    private var backgroundColorBehind: NSColor {
        if isSelected { return Theme.editorBackground }
        return isHovered ? Theme.chrome.blended(withFraction: 0.04, of: .labelColor) ?? Theme.chrome : Theme.chrome
    }

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
        if item.isTerminal {
            icon.image = NSImage(systemSymbolName: "apple.terminal", accessibilityDescription: nil)?
                .withSymbolConfiguration(.init(pointSize: 12, weight: .regular))
            icon.contentTintColor = .secondaryLabelColor
        } else if item.isGitGraph {
            icon.image = NSImage(systemSymbolName: "point.3.connected.trianglepath.dotted", accessibilityDescription: nil)?
                .withSymbolConfiguration(.init(pointSize: 12, weight: .regular))
            icon.contentTintColor = .secondaryLabelColor
        } else {
            let glyph = FileGlyph.for(name: item.glyphName ?? item.title)
            icon.image = glyph.image
            icon.contentTintColor = glyph.color
        }
        icon.alphaValue = isSelected ? 1 : 0.8
        editedDot.isHidden = !showsEditedDot
        gitDot.isHidden = true
        if item.isPinned {
            // The pin stays visible, so a pinned tab is recognisable at a glance.
            closeButton.image = NSImage(systemSymbolName: "pin.fill", accessibilityDescription: "Unpin")
            closeButton.isHidden = showsEditedDot
        } else {
            closeButton.image = NSImage(systemSymbolName: "xmark", accessibilityDescription: "Close")
            // Without room, the button only appears on hover so the title is not cut for it.
            closeButton.isHidden = showsEditedDot || !(isHovered || (isSelected && closeIsInline))
        }
        // Over the title, the button and dot carry the tab's own background.
        let behind = closeIsInline ? NSColor.clear : backgroundColorBehind
        effectiveAppearance.performAsCurrentDrawingAppearance {
            closeButton.layer?.backgroundColor = behind.cgColor
            editedDot.layer?.backgroundColor = behind.cgColor
        }
        invalidateIntrinsicContentSize()
        closeButton.setAccessibilityLabel(item.isPinned ? "Unpin \(item.title)" : "Close \(item.title)")
        closeButton.toolTip = item.isPinned ? "Unpin" : nil
        toolTip = item.isPreview ? "\(item.title) (preview: double-click the file to keep it open)" : item.title

        var label = item.title
        if item.isPinned { label += ", pinned" }
        if item.isEdited { label += ", edited" }
        if item.isPreview { label += ", preview" }
        setAccessibilityLabel(label)
        setAccessibilityValue(isSelected ? 1 : 0)
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        if isSelected {
            Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
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

    override func menu(for event: NSEvent) -> NSMenu? {
        bar?.menuForTab?(index) ?? super.menu(for: event)
    }

    override func otherMouseUp(with event: NSEvent) {
        if event.buttonNumber == 2 { bar?.onClose?(index) }
    }

    override func accessibilityPerformPress() -> Bool {
        bar?.onSelect?(index)
        return true
    }

    @objc private func closeClicked(_ sender: Any?) {
        if item.isPinned { bar?.onUnpin?(index) } else { bar?.onClose?(index) }
    }
}
