import AppKit

// MARK: - NSImage Tint Extension

private extension NSImage {
    func tinted(with color: NSColor) -> NSImage {
        guard let copy = self.copy() as? NSImage else { return self }
        copy.lockFocus()
        color.set()
        let rect = NSRect(origin: .zero, size: copy.size)
        rect.fill(using: .sourceAtop)
        copy.unlockFocus()
        copy.isTemplate = false
        return copy
    }
}

// MARK: - Search Option Button (Aa, ab, .*, AB, etc.)

@MainActor
final class SearchOptionButton: NSControl {
    var isToggle: Bool = true
    var isOn: Bool = false {
        didSet { needsDisplay = true }
    }
    var titleText: String?
    var symbolImageName: String?
    var symbolPointSize: CGFloat = 10

    var state: NSControl.StateValue {
        get { isOn ? .on : .off }
        set { isOn = (newValue == .on) }
    }

    private var isHovered: Bool = false {
        didSet { needsDisplay = true }
    }
    private var isPressed: Bool = false {
        didSet { needsDisplay = true }
    }
    private var trackingArea: NSTrackingArea?

    init(title: String, tooltip: String, isToggle: Bool = true, target: AnyObject? = nil, action: Selector? = nil) {
        super.init(frame: NSRect(x: 0, y: 0, width: 20, height: 20))
        self.titleText = title
        self.toolTip = tooltip
        self.isToggle = isToggle
        self.target = target
        self.action = action
        setup()
    }

    init(systemSymbol: String, pointSize: CGFloat = 10, tooltip: String, isToggle: Bool = false, target: AnyObject? = nil, action: Selector? = nil) {
        super.init(frame: NSRect(x: 0, y: 0, width: 20, height: 20))
        self.symbolImageName = systemSymbol
        self.symbolPointSize = pointSize
        self.toolTip = tooltip
        self.isToggle = isToggle
        self.target = target
        self.action = action
        setup()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    private func setup() {
        wantsLayer = true
        translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            widthAnchor.constraint(equalToConstant: 20),
            heightAnchor.constraint(equalToConstant: 20),
        ])
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let trackingArea { removeTrackingArea(trackingArea) }
        let area = NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeInActiveApp, .assumeInside], owner: self, userInfo: nil)
        addTrackingArea(area)
        self.trackingArea = area
    }

    override func mouseEntered(with event: NSEvent) { isHovered = true }
    override func mouseExited(with event: NSEvent) { isHovered = false }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .pointingHand)
    }

    override func mouseDown(with event: NSEvent) { isPressed = true }
    override func mouseUp(with event: NSEvent) {
        isPressed = false
        let point = convert(event.locationInWindow, from: nil)
        if bounds.contains(point) {
            if isToggle {
                isOn.toggle()
            }
            if let action, let target {
                NSApp.sendAction(action, to: target, from: self)
            }
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        let bounds = self.bounds
        let path = NSBezierPath(roundedRect: bounds.insetBy(dx: 0.5, dy: 0.5), xRadius: 3, yRadius: 3)

        let accentPurple = NSColor(srgbRed: 0.65, green: 0.40, blue: 0.98, alpha: 0.95)
        let purpleFill = NSColor(srgbRed: 0.45, green: 0.22, blue: 0.85, alpha: 0.45)

        if isOn {
            purpleFill.setFill()
            path.fill()
            accentPurple.setStroke()
            path.lineWidth = 1
            path.stroke()
        } else if isHovered || isPressed {
            NSColor.labelColor.withAlphaComponent(0.08).setFill()
            path.fill()
        }

        let textColor = isOn ? NSColor.white : (isHovered ? NSColor.labelColor : NSColor.secondaryLabelColor)

        if let titleText {
            let font = NSFont.systemFont(ofSize: 11, weight: .semibold)
            let attrs: [NSAttributedString.Key: Any] = [
                .font: font,
                .foregroundColor: textColor
            ]
            let str = NSAttributedString(string: titleText, attributes: attrs)
            let size = str.size()
            let rect = NSRect(
                x: round((bounds.width - size.width) / 2),
                y: round((bounds.height - size.height) / 2) + 0.5,
                width: size.width,
                height: size.height
            )
            str.draw(in: rect)
        } else if let symbol = symbolImageName {
            let config = NSImage.SymbolConfiguration(pointSize: symbolPointSize, weight: .semibold)
            if let img = NSImage(systemSymbolName: symbol, accessibilityDescription: toolTip)?
                .withSymbolConfiguration(config) {
                let tinted = img.tinted(with: textColor)
                let imgSize = tinted.size
                let rect = NSRect(
                    x: round((bounds.width - imgSize.width) / 2),
                    y: round((bounds.height - imgSize.height) / 2),
                    width: imgSize.width,
                    height: imgSize.height
                )
                tinted.draw(in: rect)
            }
        }
    }
}

// MARK: - Search Toggle Replace Button (Pill / Capsule Chevron)

@MainActor
final class SearchToggleReplaceButton: NSControl {
    var isExpanded: Bool = false {
        didSet { needsDisplay = true }
    }

    var isFocusedRing: Bool = false {
        didSet { needsDisplay = true }
    }

    private var isHovered: Bool = false {
        didSet { needsDisplay = true }
    }
    private var isPressed: Bool = false {
        didSet { needsDisplay = true }
    }
    private var trackingArea: NSTrackingArea?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setup()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    private func setup() {
        wantsLayer = true
        translatesAutoresizingMaskIntoConstraints = false
        toolTip = "Toggle Replace"
        setAccessibilityRole(.button)
        setAccessibilityLabel("Toggle Replace")
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let trackingArea { removeTrackingArea(trackingArea) }
        let area = NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeInActiveApp, .assumeInside], owner: self, userInfo: nil)
        addTrackingArea(area)
        self.trackingArea = area
    }

    override func mouseEntered(with event: NSEvent) { isHovered = true }
    override func mouseExited(with event: NSEvent) { isHovered = false }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .pointingHand)
    }

    override func mouseDown(with event: NSEvent) { isPressed = true }
    override func mouseUp(with event: NSEvent) {
        isPressed = false
        let point = convert(event.locationInWindow, from: nil)
        if bounds.contains(point) {
            if let action, let target {
                NSApp.sendAction(action, to: target, from: self)
            }
        }
    }

    override func becomeFirstResponder() -> Bool {
        let ok = super.becomeFirstResponder()
        if ok { isFocusedRing = true }
        return ok
    }

    override func resignFirstResponder() -> Bool {
        let ok = super.resignFirstResponder()
        if ok { isFocusedRing = false }
        return ok
    }

    override func draw(_ dirtyRect: NSRect) {
        let bounds = self.bounds
        let radius: CGFloat = isExpanded ? (bounds.width / 2) : 4
        let path = NSBezierPath(roundedRect: bounds.insetBy(dx: 0.5, dy: 0.5), xRadius: radius, yRadius: radius)

        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let accentPurple = NSColor(srgbRed: 0.65, green: 0.40, blue: 0.98, alpha: 0.95)

        if isFocusedRing {
            NSColor.labelColor.withAlphaComponent(0.08).setFill()
            path.fill()
            accentPurple.setStroke()
            path.lineWidth = 1.5
            path.stroke()
        } else if isHovered || isPressed {
            let hoverBorder = isDark ? NSColor(white: 0.35, alpha: 1.0) : NSColor.separatorColor
            hoverBorder.setStroke()
            path.lineWidth = 1.0
            path.stroke()
            NSColor.labelColor.withAlphaComponent(0.10).setFill()
            path.fill()
        } else {
            // Visible subtle capsule outline and fill so it reads as a tactile pill button
            let subtleBorder = isDark ? NSColor(white: 0.22, alpha: 0.9) : NSColor.separatorColor.withAlphaComponent(0.4)
            subtleBorder.setStroke()
            path.lineWidth = 1.0
            path.stroke()
            let subtleFill = isDark ? NSColor(white: 0.16, alpha: 0.5) : NSColor.labelColor.withAlphaComponent(0.03)
            subtleFill.setFill()
            path.fill()
        }

        let symbolName = isExpanded ? "chevron.down" : "chevron.right"
        let config = NSImage.SymbolConfiguration(pointSize: 9, weight: .bold)
        let iconColor: NSColor = (isHovered || isFocusedRing) ? .labelColor : (isDark ? NSColor(white: 0.70, alpha: 1.0) : .secondaryLabelColor)

        if let img = NSImage(systemSymbolName: symbolName, accessibilityDescription: "Toggle Replace")?
            .withSymbolConfiguration(config) {
            let tinted = img.tinted(with: iconColor)
            let imgSize = tinted.size
            let rect = NSRect(
                x: round((bounds.width - imgSize.width) / 2),
                y: round((bounds.height - imgSize.height) / 2),
                width: imgSize.width,
                height: imgSize.height
            )
            tinted.draw(in: rect)
        }
    }
}

// MARK: - Search Text Field (Focus and Key-command Aware)

@MainActor
final class SearchTextField: NSTextField {
    var onFocusChange: ((Bool) -> Void)?
    var onReturnKey: (() -> Void)?
    var onDownKey: (() -> Void)?
    var onOptionReturnKey: (() -> Void)?

    override func becomeFirstResponder() -> Bool {
        let ok = super.becomeFirstResponder()
        if ok { onFocusChange?(true) }
        return ok
    }

    override func textDidEndEditing(_ notification: Notification) {
        super.textDidEndEditing(notification)
        onFocusChange?(false)
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if event.keyCode == 36 && event.modifierFlags.contains(.option) { // Option+Return
            if let onOptionReturnKey {
                onOptionReturnKey()
                return true
            }
        }
        return super.performKeyEquivalent(with: event)
    }
}

// MARK: - Search Input Field Container (VS Code styled rounded container)

@MainActor
final class SearchInputFieldContainer: NSView {
    let textField: SearchTextField
    let trailingStack: NSStackView

    var isFocused: Bool = false {
        didSet { needsDisplay = true }
    }

    init(textField: SearchTextField, trailingButtons: [NSView] = []) {
        self.textField = textField
        self.trailingStack = NSStackView(views: trailingButtons)
        super.init(frame: .zero)
        setup()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    private func setup() {
        wantsLayer = true
        translatesAutoresizingMaskIntoConstraints = false
        layer?.cornerRadius = 4
        layer?.masksToBounds = true

        textField.focusRingType = .none
        textField.drawsBackground = false
        textField.isBordered = false
        textField.font = .systemFont(ofSize: 12)
        textField.translatesAutoresizingMaskIntoConstraints = false

        trailingStack.orientation = .horizontal
        trailingStack.spacing = 2
        trailingStack.alignment = .centerY
        trailingStack.translatesAutoresizingMaskIntoConstraints = false

        addSubview(textField)
        addSubview(trailingStack)

        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 26),
            textField.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 6),
            textField.centerYAnchor.constraint(equalTo: centerYAnchor),
            trailingStack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -3),
            trailingStack.centerYAnchor.constraint(equalTo: centerYAnchor),
            textField.trailingAnchor.constraint(equalTo: trailingStack.leadingAnchor, constant: -3),
        ])

        textField.onFocusChange = { [weak self] focused in
            self?.isFocused = focused
        }
    }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(textField)
    }

    override func draw(_ dirtyRect: NSRect) {
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let bounds = self.bounds
        let path = NSBezierPath(roundedRect: bounds.insetBy(dx: 0.5, dy: 0.5), xRadius: 4, yRadius: 4)

        let bgColor = Theme.editorBackground.resolved(for: effectiveAppearance)
        bgColor.setFill()
        path.fill()

        let accentPurple = NSColor(srgbRed: 0.65, green: 0.40, blue: 0.98, alpha: 1.0)
        let normalBorder = isDark ? NSColor(white: 0.24, alpha: 1.0) : NSColor.separatorColor.withAlphaComponent(0.6)

        if isFocused {
            accentPurple.setStroke()
            path.lineWidth = 1.5
        } else {
            normalBorder.setStroke()
            path.lineWidth = 1.0
        }
        path.stroke()
    }
}

// MARK: - SearchSidebarController

@MainActor
final class SearchSidebarController: NSObject, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    let view = NSView()

    // Inputs
    private let query = SearchTextField()
    private let replace = SearchTextField()
    private let includeField = SearchTextField()
    private let excludeField = SearchTextField()

    // Containers
    private var queryContainer: SearchInputFieldContainer!
    private var replaceContainer: SearchInputFieldContainer!
    private var includeContainer: SearchInputFieldContainer!
    private var excludeContainer: SearchInputFieldContainer!

    // Toggle Replace Pill Button
    private let replaceToggle = SearchToggleReplaceButton()

    // Search Option Buttons
    private let caseSensitive = SearchOptionButton(title: "Aa", tooltip: "Match Case (⌥⌘C)", isToggle: true)
    private let wholeWord = SearchOptionButton(title: "ab", tooltip: "Match Whole Word (⌥⌘W)", isToggle: true)
    private let regex = SearchOptionButton(title: ".*", tooltip: "Use Regular Expression (⌥⌘R)", isToggle: true)

    // Replace Option Buttons
    private let preserveCase = SearchOptionButton(title: "AB", tooltip: "Preserve Case (⌥⌘P)", isToggle: true)
    private let replaceAllButton = SearchOptionButton(systemSymbol: "arrow.2.squarepath", pointSize: 10, tooltip: "Replace All (⌥⌘↵)", isToggle: false)

    // Summary / Result Count row & More Button
    private let summaryLabel = NSTextField(labelWithString: "Type to search the workspace")
    private let moreButton = SearchOptionButton(systemSymbol: "ellipsis", pointSize: 11, tooltip: "Toggle Search Details", isToggle: false)

    // Include / Exclude Details Buttons
    private let openEditorsOnly = SearchOptionButton(systemSymbol: "doc.text", pointSize: 10, tooltip: "Search only in Open Editors", isToggle: true)
    private let ignoreFiles = SearchOptionButton(systemSymbol: "book", pointSize: 10, tooltip: "Use Exclude Settings and Ignore Files", isToggle: true)
    private let excludeSettings = SearchOptionButton(systemSymbol: "gearshape.fill", pointSize: 10, tooltip: "Use Exclude Settings and Ignore Files", isToggle: true)

    // Details Section UI
    private let detailsView = NSView()
    private let includeLabel = NSTextField(labelWithString: "files to include")
    private let excludeLabel = NSTextField(labelWithString: "files to exclude")

    // Results List
    private let table = NSTableView()
    private let scroll = NSScrollView()
    private var results: [WorkspaceSearchMatch] = []

    // Dynamic Constraints
    private var toggleBottomToQuery: NSLayoutConstraint!
    private var toggleBottomToReplace: NSLayoutConstraint!
    private var replaceContainerHeight: NSLayoutConstraint!
    private var replaceContainerTop: NSLayoutConstraint!

    private var moreButtonTrailing: NSLayoutConstraint!
    private var moreButtonTopCollapsed: NSLayoutConstraint!
    private var moreButtonCenterYExpanded: NSLayoutConstraint!

    private var detailsHeightConstraint: NSLayoutConstraint!
    private var detailsTopConstraint: NSLayoutConstraint!
    private var summaryRowTopConstraint: NSLayoutConstraint!
    private var scrollTopConstraint: NSLayoutConstraint!

    // State
    private(set) var replaceVisible = false {
        didSet { applyReplaceVisibility(animated: true) }
    }

    private(set) var detailsVisible = false {
        didSet { applyDetailsVisibility(animated: true) }
    }

    var onSearch: ((String, WorkspaceSearchOptions) -> Void)?
    var onOpen: ((WorkspaceSearchMatch) -> Void)?
    var onReplaceAll: ((String, String, WorkspaceSearchOptions) -> Void)?

    override init() {
        super.init()
        buildUI()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    // MARK: - Setup

    private func buildUI() {
        view.translatesAutoresizingMaskIntoConstraints = false

        query.placeholderString = "Search"
        replace.placeholderString = "Replace"
        includeField.placeholderString = "e.g. *.ts, src/**/include"
        excludeField.placeholderString = ""

        for field in [query, replace, includeField, excludeField] {
            field.delegate = self
        }

        for btn in [caseSensitive, wholeWord, regex, preserveCase, openEditorsOnly, ignoreFiles, excludeSettings] {
            btn.target = self
            btn.action = #selector(optionsChanged(_:))
        }
        excludeSettings.state = .on // Active by default, matching screenshot

        replaceAllButton.target = self
        replaceAllButton.action = #selector(replaceAllClicked(_:))

        queryContainer = SearchInputFieldContainer(textField: query, trailingButtons: [caseSensitive, wholeWord, regex])
        replaceContainer = SearchInputFieldContainer(textField: replace, trailingButtons: [preserveCase, replaceAllButton])
        includeContainer = SearchInputFieldContainer(textField: includeField, trailingButtons: [openEditorsOnly, ignoreFiles])
        excludeContainer = SearchInputFieldContainer(textField: excludeField, trailingButtons: [excludeSettings])

        replaceToggle.target = self
        replaceToggle.action = #selector(toggleReplace(_:))

        moreButton.target = self
        moreButton.action = #selector(showMore(_:))

        // Summary Label setup
        summaryLabel.font = .systemFont(ofSize: 11, weight: .regular)
        summaryLabel.textColor = .secondaryLabelColor
        summaryLabel.lineBreakMode = .byTruncatingTail
        summaryLabel.translatesAutoresizingMaskIntoConstraints = false

        for label in [includeLabel, excludeLabel] {
            label.font = .systemFont(ofSize: 11, weight: .regular)
            label.textColor = .secondaryLabelColor
            label.translatesAutoresizingMaskIntoConstraints = false
        }

        detailsView.translatesAutoresizingMaskIntoConstraints = false
        detailsView.addSubview(includeLabel)
        detailsView.addSubview(includeContainer)
        detailsView.addSubview(excludeLabel)
        detailsView.addSubview(excludeContainer)

        NSLayoutConstraint.activate([
            includeLabel.topAnchor.constraint(equalTo: detailsView.topAnchor, constant: 4),
            includeLabel.leadingAnchor.constraint(equalTo: detailsView.leadingAnchor),

            includeContainer.topAnchor.constraint(equalTo: includeLabel.bottomAnchor, constant: 3),
            includeContainer.leadingAnchor.constraint(equalTo: detailsView.leadingAnchor),
            includeContainer.trailingAnchor.constraint(equalTo: detailsView.trailingAnchor),

            excludeLabel.topAnchor.constraint(equalTo: includeContainer.bottomAnchor, constant: 6),
            excludeLabel.leadingAnchor.constraint(equalTo: detailsView.leadingAnchor),

            excludeContainer.topAnchor.constraint(equalTo: excludeLabel.bottomAnchor, constant: 3),
            excludeContainer.leadingAnchor.constraint(equalTo: detailsView.leadingAnchor),
            excludeContainer.trailingAnchor.constraint(equalTo: detailsView.trailingAnchor),
            excludeContainer.bottomAnchor.constraint(equalTo: detailsView.bottomAnchor, constant: -2),
        ])

        // Table & ScrollView configuration
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("search-results"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 36
        table.intercellSpacing = NSSize(width: 0, height: 1)
        table.style = .plain
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.doubleAction = #selector(openSelected(_:))
        table.backgroundColor = .clear

        scroll.documentView = table
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.automaticallyAdjustsContentInsets = false
        scroll.contentInsets = NSEdgeInsets(top: 0, left: 0, bottom: 4, right: 0)
        scroll.translatesAutoresizingMaskIntoConstraints = false

        view.addSubview(replaceToggle)
        view.addSubview(queryContainer)
        view.addSubview(replaceContainer)
        view.addSubview(summaryLabel)
        view.addSubview(moreButton)
        view.addSubview(detailsView)
        view.addSubview(scroll)

        replaceContainerHeight = replaceContainer.heightAnchor.constraint(equalToConstant: 0)
        replaceContainerTop = replaceContainer.topAnchor.constraint(equalTo: queryContainer.bottomAnchor, constant: 0)

        toggleBottomToQuery = replaceToggle.bottomAnchor.constraint(equalTo: queryContainer.bottomAnchor)
        toggleBottomToReplace = replaceToggle.bottomAnchor.constraint(equalTo: replaceContainer.bottomAnchor)

        moreButtonTrailing = moreButton.trailingAnchor.constraint(equalTo: queryContainer.trailingAnchor)
        moreButtonTopCollapsed = moreButton.topAnchor.constraint(equalTo: replaceContainer.bottomAnchor, constant: 3)
        moreButtonCenterYExpanded = moreButton.centerYAnchor.constraint(equalTo: includeLabel.centerYAnchor)

        detailsHeightConstraint = detailsView.heightAnchor.constraint(equalToConstant: 0)
        detailsTopConstraint = detailsView.topAnchor.constraint(equalTo: replaceContainer.bottomAnchor, constant: 0)

        summaryRowTopConstraint = summaryLabel.centerYAnchor.constraint(equalTo: moreButton.centerYAnchor)

        let scrollTrailing = scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor)
        scrollTrailing.priority = .init(999)
        let queryTrailing = queryContainer.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -8)
        queryTrailing.priority = .init(999)

        scrollTopConstraint = scroll.topAnchor.constraint(equalTo: moreButton.bottomAnchor, constant: 3)

        NSLayoutConstraint.activate([
            // Replace Toggle Button
            replaceToggle.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 8),
            replaceToggle.topAnchor.constraint(equalTo: queryContainer.topAnchor),
            replaceToggle.widthAnchor.constraint(equalToConstant: 18),
            toggleBottomToQuery,

            // Query Input
            queryContainer.topAnchor.constraint(equalTo: view.topAnchor, constant: 6),
            queryContainer.leadingAnchor.constraint(equalTo: replaceToggle.trailingAnchor, constant: 5),
            queryTrailing,

            // Replace Input
            replaceContainerTop,
            replaceContainer.leadingAnchor.constraint(equalTo: queryContainer.leadingAnchor),
            replaceContainer.trailingAnchor.constraint(equalTo: queryContainer.trailingAnchor),
            replaceContainerHeight,

            // Details View
            detailsTopConstraint,
            detailsView.leadingAnchor.constraint(equalTo: queryContainer.leadingAnchor),
            detailsView.trailingAnchor.constraint(equalTo: queryContainer.trailingAnchor),
            detailsHeightConstraint,

            // More Button (…)
            moreButtonTrailing,
            moreButtonTopCollapsed,

            // Summary Label (positioned on the same row as More Button)
            summaryLabel.leadingAnchor.constraint(equalTo: queryContainer.leadingAnchor, constant: 2),
            summaryLabel.trailingAnchor.constraint(lessThanOrEqualTo: moreButton.leadingAnchor, constant: -6),
            summaryRowTopConstraint,

            // Scroll & Results
            scrollTopConstraint,
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scrollTrailing,
            scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])

        query.onDownKey = { [weak self] in
            guard let self, self.results.count > 0 else { return }
            self.view.window?.makeFirstResponder(self.table)
            self.table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        }
        replace.onOptionReturnKey = { [weak self] in
            self?.replaceAllClicked(nil)
        }

        applyReplaceVisibility(animated: false)
        applyDetailsVisibility(animated: false)
    }

    // MARK: - Visibility Toggles

    private func applyReplaceVisibility(animated: Bool) {
        let updateLayout = {
            self.replaceContainer.isHidden = !self.replaceVisible
            self.replaceContainerHeight.constant = self.replaceVisible ? 26 : 0
            self.replaceContainerTop.constant = self.replaceVisible ? 4 : 0

            self.replaceToggle.isExpanded = self.replaceVisible

            if self.replaceVisible {
                self.toggleBottomToQuery.isActive = false
                self.toggleBottomToReplace.isActive = true
            } else {
                self.toggleBottomToReplace.isActive = false
                self.toggleBottomToQuery.isActive = true
            }

            self.view.layoutSubtreeIfNeeded()
        }

        if animated {
            NSAnimationContext.runAnimationGroup { context in
                context.duration = 0.15
                context.allowsImplicitAnimation = true
                updateLayout()
            }
        } else {
            updateLayout()
        }
    }

    private func applyDetailsVisibility(animated: Bool) {
        let updateLayout = {
            self.detailsView.isHidden = !self.detailsVisible
            self.detailsHeightConstraint.constant = self.detailsVisible ? 104 : 0
            self.detailsTopConstraint.constant = self.detailsVisible ? 4 : 0

            if self.detailsVisible {
                self.moreButtonTopCollapsed.isActive = false
                self.moreButtonCenterYExpanded.isActive = true
                self.summaryRowTopConstraint.isActive = false
                self.summaryLabel.topAnchor.constraint(equalTo: self.detailsView.bottomAnchor, constant: 4).isActive = true
                self.scrollTopConstraint.constant = 104 + 22
            } else {
                self.moreButtonCenterYExpanded.isActive = false
                self.moreButtonTopCollapsed.isActive = true
                self.summaryRowTopConstraint.isActive = true
                self.scrollTopConstraint.constant = 4
            }

            self.view.layoutSubtreeIfNeeded()
        }

        if animated {
            NSAnimationContext.runAnimationGroup { context in
                context.duration = 0.15
                context.allowsImplicitAnimation = true
                updateLayout()
            }
        } else {
            updateLayout()
        }
    }

    // MARK: - Actions

    func toggleReplace() {
        replaceVisible.toggle()
    }

    func toggleDetails() {
        detailsVisible.toggle()
    }

    @objc private func toggleReplace(_ sender: Any?) {
        toggleReplace()
    }

    @objc private func showMore(_ sender: Any?) {
        toggleDetails()
    }

    @objc private func optionsChanged(_ sender: Any?) {
        runSearch()
    }

    @objc private func replaceAllClicked(_ sender: Any?) {
        onReplaceAll?(query.stringValue, replace.stringValue, currentOptions())
    }

    func focus() {
        view.window?.makeFirstResponder(query)
    }

    var queryText: String { query.stringValue }

    /// ⌘⇧F, like VS Code: keeps the previous query unless the editor has a
    /// selection, runs it, and selects the field text for retyping.
    func present(seed: String?) {
        if let seed, !seed.isEmpty, seed != query.stringValue {
            query.stringValue = seed
            runSearch()
        }
        focus()
        query.currentEditor()?.selectAll(nil)
    }

    func reset() {
        query.stringValue = ""
        replace.stringValue = ""
        includeField.stringValue = ""
        excludeField.stringValue = ""
        show([], message: "Type to search the workspace")
    }

    func show(_ matches: [WorkspaceSearchMatch], message text: String? = nil) {
        results = matches
        table.reloadData()

        let fileCount = Set(matches.map { $0.relPath }).count
        if matches.isEmpty {
            summaryLabel.stringValue = text ?? (query.stringValue.isEmpty ? "Type to search the workspace" : "No results found")
        } else {
            summaryLabel.stringValue = "\(matches.count) result\(matches.count == 1 ? "" : "s") in \(fileCount) file\(fileCount == 1 ? "" : "s")"
        }
    }

    func controlTextDidChange(_ obj: Notification) {
        runSearch()
    }

    func refresh() {
        runSearch()
    }

    func clearQuery() {
        query.stringValue = ""
        show([], message: "Type to search the workspace")
        focus()
    }

    func moreMenu() -> NSMenu? {
        let menu = NSMenu()
        menu.addItem(withTitle: detailsVisible ? "Hide Search Details" : "Show Search Details", action: #selector(toggleDetailsFromMenu(_:)), keyEquivalent: "")
        menu.items.last?.target = self
        menu.addItem(withTitle: replaceVisible ? "Hide Replace" : "Show Replace", action: #selector(toggleReplaceFromMenu(_:)), keyEquivalent: "")
        menu.items.last?.target = self
        menu.addItem(.separator())
        menu.addItem(withTitle: "Clear Search Results", action: #selector(clearResults(_:)), keyEquivalent: "")
        menu.items.last?.target = self
        return menu
    }

    @objc private func toggleDetailsFromMenu(_ sender: Any?) { toggleDetails() }
    @objc private func toggleReplaceFromMenu(_ sender: Any?) { toggleReplace() }

    @objc private func clearResults(_ sender: Any?) {
        show([], message: "Type to search the workspace")
    }

    private func currentOptions() -> WorkspaceSearchOptions {
        WorkspaceSearchOptions(
            regex: regex.state == .on,
            caseSensitive: caseSensitive.state == .on,
            wholeWord: wholeWord.state == .on,
            include: includeField.stringValue,
            exclude: excludeField.stringValue
        )
    }

    private func runSearch() {
        if query.stringValue.isEmpty {
            show([], message: "Type to search the workspace")
            return
        }
        summaryLabel.stringValue = "Searching…"
        onSearch?(query.stringValue, currentOptions())
    }

    // MARK: - Table View

    func numberOfRows(in tableView: NSTableView) -> Int {
        results.count
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        openSelected(nil)
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let id = NSUserInterfaceItemIdentifier("search-result-cell")
        let cell = (tableView.makeView(withIdentifier: id, owner: self) as? SearchResultTableCellView) ?? {
            let cell = SearchResultTableCellView()
            cell.identifier = id
            return cell
        }()

        let match = results[row]
        cell.configure(with: match)
        return cell
    }

    @objc private func openSelected(_ sender: Any?) {
        guard table.selectedRow >= 0, table.selectedRow < results.count else { return }
        onOpen?(results[table.selectedRow])
    }
}

// MARK: - Search Result Table Cell View

@MainActor
final class SearchResultTableCellView: NSTableCellView {
    let fileLabel = NSTextField(labelWithString: "")
    let matchLabel = NSTextField(labelWithString: "")

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setup()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    private func setup() {
        fileLabel.font = .systemFont(ofSize: 11.5, weight: .semibold)
        fileLabel.textColor = .labelColor
        fileLabel.lineBreakMode = .byTruncatingTail
        fileLabel.translatesAutoresizingMaskIntoConstraints = false

        matchLabel.font = .monospacedSystemFont(ofSize: 10.5, weight: .regular)
        matchLabel.textColor = .secondaryLabelColor
        matchLabel.lineBreakMode = .byTruncatingTail
        matchLabel.translatesAutoresizingMaskIntoConstraints = false

        addSubview(fileLabel)
        addSubview(matchLabel)

        NSLayoutConstraint.activate([
            fileLabel.topAnchor.constraint(equalTo: topAnchor, constant: 2),
            fileLabel.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            fileLabel.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),

            matchLabel.topAnchor.constraint(equalTo: fileLabel.bottomAnchor, constant: 1),
            matchLabel.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            matchLabel.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            matchLabel.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -2),
        ])
    }

    func configure(with match: WorkspaceSearchMatch) {
        let fileName = (match.relPath as NSString).lastPathComponent
        fileLabel.stringValue = fileName
        matchLabel.stringValue = "\(match.line + 1):\(match.column + 1)  \(match.text.trimmingCharacters(in: .whitespacesAndNewlines))"
    }
}
