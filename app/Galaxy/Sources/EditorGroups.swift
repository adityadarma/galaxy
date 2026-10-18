import AppKit

/// One editor group (VSCode "editor group"): its own tabs and active tab.
///
/// The window has one live editor stack (tab bar, path bar, editor, find
/// bar, terminal) that sits over the slot of the active group. Every other
/// group shows a light `EditorGroupPane` in its slot; clicking it makes
/// that group the active one.
@MainActor
final class EditorGroup {
    var tabs: [EditorTab] = []
    var activeIndex: Int?
    var activeTab: EditorTab? {
        guard let activeIndex, tabs.indices.contains(activeIndex) else { return nil }
        return tabs[activeIndex]
    }
    /// Cell of the groups split view.
    let slot = NSView()
    /// What the slot shows while the group is not the active one.
    var pane: EditorGroupPane?
}

/// Lays the groups side by side (or stacked). It reports every size change
/// so the live editor stack can follow the active group's slot.
@MainActor
final class EditorGroupsSplitView: NSSplitView, NSSplitViewDelegate {
    static let minGroupSize: CGFloat = 160
    var onResize: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        isVertical = true
        dividerStyle = .thin
        delegate = self
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func layout() {
        super.layout()
        onResize?()
    }

    func splitView(_ splitView: NSSplitView, constrainMinCoordinate proposedMinimumPosition: CGFloat,
                   ofSubviewAt dividerIndex: Int) -> CGFloat {
        proposedMinimumPosition + Self.minGroupSize
    }

    func splitView(_ splitView: NSSplitView, constrainMaxCoordinate proposedMaximumPosition: CGFloat,
                   ofSubviewAt dividerIndex: Int) -> CGFloat {
        proposedMaximumPosition - Self.minGroupSize
    }

    func splitView(_ splitView: NSSplitView, canCollapseSubview subview: NSView) -> Bool { false }

    func splitViewDidResizeSubviews(_ notification: Notification) { onResize?() }

    /// Gives every group the same share of the space.
    func equalize() {
        layoutSubtreeIfNeeded()
        let count = subviews.count
        guard count > 1 else { return }
        let total = isVertical ? bounds.width : bounds.height
        let size = (total - dividerThickness * CGFloat(count - 1)) / CGFloat(count)
        for i in 1..<count {
            setPosition(size * CGFloat(i) + dividerThickness * CGFloat(i - 1), ofDividerAt: i - 1)
        }
        adjustSubviews()
        onResize?()
    }
}

/// Covers a pane under its tab bar. A click selects the group; the wheel
/// still scrolls the pane's editor.
@MainActor
private final class GroupClickCatcher: NSView {
    var onMouseDown: ((NSEvent) -> Void)?
    weak var scrollTarget: NSScrollView?

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func mouseDown(with event: NSEvent) { onMouseDown?(event) }
    override func rightMouseDown(with event: NSEvent) { onMouseDown?(event) }
    override func scrollWheel(with event: NSEvent) { scrollTarget?.scrollWheel(with: event) }
}

/// A group that is not the active one: its tab bar, path bar and a
/// read-only view of its active tab, so the whole layout stays visible.
@MainActor
final class EditorGroupPane: NSView {
    let tabBar = TabBar()
    let pathBar = PathBar()
    let scrollView = DiagnosticScrollView()
    let editorView = EditorView(frame: .zero)
    private let breadcrumbSpacer = NSView()
    private let placeholder = NSTextField(labelWithString: "")
    private let catcher = GroupClickCatcher()
    private var spacerHeight: NSLayoutConstraint!
    /// The tab whose document the editor shows.
    private(set) weak var shownTab: EditorTab?

    /// A click below the tab bar.
    var onActivate: ((NSEvent) -> Void)? {
        get { catcher.onMouseDown }
        set { catcher.onMouseDown = newValue }
    }

    var scrollY: CGFloat { scrollView.contentView.bounds.origin.y }

    override init(frame: NSRect) {
        super.init(frame: frame)
        tabBar.setAccessibilityIdentifier("group-tab-bar")
        pathBar.setAccessibilityElement(false)

        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.scrollerStyle = .overlay
        scrollView.autohidesScrollers = true
        scrollView.automaticallyAdjustsContentInsets = false
        scrollView.contentInsets = NSEdgeInsets(top: Theme.editorTopInset, left: 0, bottom: 0, right: 0)
        scrollView.drawsBackground = true
        scrollView.backgroundColor = Theme.editorBackground
        scrollView.borderType = .noBorder
        editorView.autoresizingMask = [.width]
        editorView.isReadOnly = true
        editorView.wrapSuspended = true
        scrollView.documentView = editorView
        catcher.scrollTarget = scrollView

        placeholder.font = .systemFont(ofSize: 13)
        placeholder.textColor = .tertiaryLabelColor
        placeholder.alignment = .center
        placeholder.isHidden = true

        spacerHeight = breadcrumbSpacer.heightAnchor.constraint(equalToConstant: 0)
        for view in [tabBar, pathBar, breadcrumbSpacer, scrollView, placeholder, catcher] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            tabBar.topAnchor.constraint(equalTo: topAnchor),
            tabBar.leadingAnchor.constraint(equalTo: leadingAnchor),
            tabBar.trailingAnchor.constraint(equalTo: trailingAnchor),
            pathBar.topAnchor.constraint(equalTo: tabBar.bottomAnchor),
            pathBar.leadingAnchor.constraint(equalTo: leadingAnchor),
            pathBar.trailingAnchor.constraint(equalTo: trailingAnchor),
            breadcrumbSpacer.topAnchor.constraint(equalTo: pathBar.bottomAnchor),
            breadcrumbSpacer.leadingAnchor.constraint(equalTo: leadingAnchor),
            breadcrumbSpacer.trailingAnchor.constraint(equalTo: trailingAnchor),
            spacerHeight,
            scrollView.topAnchor.constraint(equalTo: breadcrumbSpacer.bottomAnchor),
            scrollView.leadingAnchor.constraint(equalTo: leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: trailingAnchor),
            scrollView.bottomAnchor.constraint(equalTo: bottomAnchor),
            placeholder.centerXAnchor.constraint(equalTo: centerXAnchor),
            placeholder.centerYAnchor.constraint(equalTo: centerYAnchor),
            catcher.topAnchor.constraint(equalTo: pathBar.topAnchor),
            catcher.leadingAnchor.constraint(equalTo: leadingAnchor),
            catcher.trailingAnchor.constraint(equalTo: trailingAnchor),
            catcher.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override func draw(_ dirtyRect: NSRect) {
        Theme.editorBackground.resolved(for: effectiveAppearance).setFill()
        dirtyRect.intersection(bounds).fill()
    }

    /// Brings the pane in line with its group.
    func show(items: [TabBar.Item], selected: Int?, tab: EditorTab?, breadcrumbHeight: CGFloat) {
        tabBar.set(items: items, selected: selected)
        pathBar.show(tab?.pathComponents ?? [])
        pathBar.setEdited(tab?.isEdited ?? false)
        spacerHeight.constant = breadcrumbHeight
        if let tab, tab.isDocument {
            scrollView.isHidden = false
            placeholder.isHidden = true
            if shownTab !== tab {
                shownTab = tab
                editorView.document = tab.doc
                layoutSubtreeIfNeeded()
                let y = tab.scrollY ?? -scrollView.contentInsets.top
                scrollView.contentView.scroll(to: NSPoint(x: 0, y: y))
                scrollView.reflectScrolledClipView(scrollView.contentView)
            }
        } else {
            scrollView.isHidden = true
            clear()
            placeholder.stringValue = tab == nil ? "" : (tab?.isGitGraph == true ? "Git Graph" : "Terminal")
            placeholder.isHidden = tab == nil
        }
    }

    /// Drops the document, e.g. while the group is the active one.
    func clear() {
        shownTab = nil
        if editorView.document != nil { editorView.document = nil }
    }
}
