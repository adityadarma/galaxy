import AppKit

/// One message for the bottom-right corner, like VS Code's notifications.
struct AppNotification {
    enum Severity {
        case info, warning, error

        var symbol: String {
            switch self {
            case .info: "info.circle.fill"
            case .warning: "exclamationmark.triangle.fill"
            case .error: "xmark.octagon.fill"
            }
        }

        var color: NSColor {
            switch self {
            case .info: Theme.accent
            case .warning: .systemYellow
            case .error: .systemRed
            }
        }

        var name: String {
            switch self {
            case .info: "Info"
            case .warning: "Warning"
            case .error: "Error"
            }
        }
    }

    struct Action {
        let title: String
        let run: @MainActor () -> Void
    }

    let id = UUID()
    let severity: Severity
    let message: String
    let source: String?
    let actions: [Action]

    init(_ message: String, severity: Severity = .info, source: String? = nil, actions: [Action] = []) {
        self.message = message
        self.severity = severity
        self.source = source
        self.actions = actions
    }
}

/// Toasts stacked above the status bar's right end, and the list of recent
/// notifications the status bar bell opens. Info toasts without actions hide
/// after a while but stay in the list; warnings and errors stay until closed.
@MainActor
final class NotificationsView: NSView {
    static let width: CGFloat = 400
    static let maxToasts = 3
    static let historyLimit = 50
    static let infoTimeout: TimeInterval = 10
    private static let centerMaxHeight: CGFloat = 360

    /// Newest last. The list shows them newest first.
    private(set) var history: [AppNotification] = []
    /// Notifications shown as toasts, oldest first.
    private(set) var toastIDs: [UUID] = []
    private(set) var isCenterVisible = false
    /// Notifications that arrived while the list was closed.
    private(set) var unreadCount = 0
    /// Called whenever the count or the list's visibility changes.
    var onChange: (() -> Void)?

    private let toastStack = NSStackView()
    private let column = NSStackView()
    private var toastViews: [UUID: NSView] = [:]
    private var timers: [UUID: DispatchWorkItem] = [:]
    private var hovered: Set<UUID> = []

    private let centerList = NSStackView()
    private let centerScroll = NSScrollView()
    private let centerEmpty = NSTextField(labelWithString: "No new notifications")
    private var centerHeight: NSLayoutConstraint!
    private lazy var center = NotificationCard(content: buildCenterContent())

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        translatesAutoresizingMaskIntoConstraints = false
        setAccessibilityIdentifier("notifications")

        toastStack.orientation = .vertical
        toastStack.alignment = .leading
        toastStack.spacing = 8
        center.isHidden = true
        center.setAccessibilityIdentifier("notification-center")

        column.setViews([toastStack, center], in: .leading)
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = 0
        column.translatesAutoresizingMaskIntoConstraints = false
        addSubview(column)
        NSLayoutConstraint.activate([
            widthAnchor.constraint(equalToConstant: Self.width),
            column.topAnchor.constraint(equalTo: topAnchor),
            column.bottomAnchor.constraint(equalTo: bottomAnchor),
            column.leadingAnchor.constraint(equalTo: leadingAnchor),
            column.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Clicks in the gaps between toasts reach the editor underneath.
    override func hitTest(_ point: NSPoint) -> NSView? {
        let hit = super.hitTest(point)
        return hit === self || hit === column || hit === toastStack ? nil : hit
    }

    // MARK: Model

    func show(_ notification: AppNotification) {
        // The same message again replaces the old one instead of stacking.
        if let old = history.first(where: { $0.message == notification.message && $0.severity == notification.severity }) {
            forget(old.id)
        }
        history.append(notification)
        while history.count > Self.historyLimit { forget(history[0].id) }
        if isCenterVisible {
            reloadCenter()
        } else {
            unreadCount += 1
            toastIDs.append(notification.id)
            while toastIDs.count > Self.maxToasts { dropToast(toastIDs[0]) }
            scheduleTimeout(for: notification)
            reloadToasts()
        }
        announce(notification)
        onChange?()
    }

    /// Hides a toast; the notification stays in the list.
    func hideToast(_ id: UUID) {
        guard toastIDs.contains(id) else { return }
        dropToast(id)
        reloadToasts()
        onChange?()
    }

    /// Clears a notification from the toasts and the list.
    func dismiss(_ id: UUID) {
        forget(id)
        reloadToasts()
        if isCenterVisible { reloadCenter() }
        onChange?()
    }

    func clearAll() {
        for id in history.map(\.id) { forget(id) }
        reloadToasts()
        reloadCenter()
        setCenterVisible(false)
        onChange?()
    }

    func toggleCenter() { setCenterVisible(!isCenterVisible) }

    func setCenterVisible(_ visible: Bool) {
        guard visible != isCenterVisible else { return }
        isCenterVisible = visible
        if visible {
            unreadCount = 0
            for id in toastIDs { dropToast(id) }
            reloadToasts()
            reloadCenter()
        }
        center.isHidden = !visible
        onChange?()
    }

    private func forget(_ id: UUID) {
        history.removeAll { $0.id == id }
        unreadCount = min(unreadCount, history.count)
        dropToast(id)
    }

    private func dropToast(_ id: UUID) {
        toastIDs.removeAll { $0 == id }
        timers.removeValue(forKey: id)?.cancel()
        hovered.remove(id)
    }

    private func scheduleTimeout(for notification: AppNotification) {
        guard notification.severity == .info, notification.actions.isEmpty else { return }
        let id = notification.id
        timers[id]?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.hideToast(id) }
        }
        timers[id] = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.infoTimeout, execute: work)
    }

    private func setHovered(_ id: UUID, _ isHovered: Bool) {
        if isHovered {
            hovered.insert(id)
            timers.removeValue(forKey: id)?.cancel()
        } else if hovered.remove(id) != nil, toastIDs.contains(id),
                  let notification = history.first(where: { $0.id == id }) {
            scheduleTimeout(for: notification)
        }
    }

    private func run(_ action: AppNotification.Action, from id: UUID) {
        dismiss(id)
        action.run()
    }

    private func announce(_ notification: AppNotification) {
        let element: Any
        if let window { element = window } else { element = NSApp as Any }
        NSAccessibility.post(element: element, notification: .announcementRequested, userInfo: [
            .announcement: "\(notification.severity.name): \(notification.message)",
            .priority: NSAccessibilityPriorityLevel.high.rawValue,
        ])
    }

    // MARK: Views

    private func makeRow(_ notification: AppNotification) -> NotificationRowView {
        let id = notification.id
        let row = NotificationRowView(notification, width: Self.width)
        row.onClose = { [weak self] in self?.dismiss(id) }
        row.onAction = { [weak self] action in self?.run(action, from: id) }
        return row
    }

    private func reloadToasts() {
        for (id, view) in toastViews where !toastIDs.contains(id) {
            view.removeFromSuperview()
            toastViews[id] = nil
        }
        for (index, id) in toastIDs.enumerated() {
            let view: NSView
            if let existing = toastViews[id] {
                view = existing
            } else {
                guard let notification = history.first(where: { $0.id == id }) else { continue }
                let row = makeRow(notification)
                row.onHover = { [weak self] isHovered in self?.setHovered(id, isHovered) }
                view = NotificationCard(content: row)
                view.setAccessibilityIdentifier("notification-toast")
                toastViews[id] = view
            }
            if toastStack.arrangedSubviews.firstIndex(of: view) != index {
                view.removeFromSuperview()
                toastStack.insertArrangedSubview(view, at: min(index, toastStack.arrangedSubviews.count))
            }
        }
        toastStack.isHidden = toastIDs.isEmpty
    }

    private func buildCenterContent() -> NSView {
        let title = NSTextField(labelWithString: "NOTIFICATIONS")
        title.font = .systemFont(ofSize: 11, weight: .semibold)
        title.textColor = .secondaryLabelColor
        let clear = Self.iconButton("trash", label: "Clear All Notifications", target: self,
                                    action: #selector(clearAllClicked(_:)))
        let hide = Self.iconButton("chevron.down", label: "Hide Notifications", target: self,
                                   action: #selector(hideClicked(_:)))
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let header = NSStackView(views: [title, spacer, clear, hide])
        header.spacing = 10
        header.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 8, right: 12)

        centerList.orientation = .vertical
        centerList.alignment = .leading
        centerList.spacing = 0
        centerList.translatesAutoresizingMaskIntoConstraints = false
        let document = FlippedView()
        document.translatesAutoresizingMaskIntoConstraints = false
        document.addSubview(centerList)
        centerScroll.documentView = document
        centerScroll.drawsBackground = false
        centerScroll.hasVerticalScroller = true
        centerScroll.autohidesScrollers = true
        centerScroll.scrollerStyle = .overlay
        centerScroll.translatesAutoresizingMaskIntoConstraints = false
        centerHeight = centerScroll.heightAnchor.constraint(equalToConstant: 0)
        NSLayoutConstraint.activate([
            centerList.topAnchor.constraint(equalTo: document.topAnchor),
            centerList.bottomAnchor.constraint(equalTo: document.bottomAnchor),
            centerList.leadingAnchor.constraint(equalTo: document.leadingAnchor),
            centerList.trailingAnchor.constraint(equalTo: document.trailingAnchor),
            document.widthAnchor.constraint(equalTo: centerScroll.contentView.widthAnchor),
            document.topAnchor.constraint(equalTo: centerScroll.contentView.topAnchor),
            centerScroll.widthAnchor.constraint(equalToConstant: Self.width),
            centerHeight,
        ])

        centerEmpty.font = .systemFont(ofSize: 12)
        centerEmpty.textColor = .secondaryLabelColor
        let empty = NSStackView(views: [centerEmpty])
        empty.edgeInsets = NSEdgeInsets(top: 4, left: 12, bottom: 14, right: 12)

        let content = NSStackView(views: [header, Self.separator(), centerScroll, empty])
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = 0
        header.widthAnchor.constraint(equalToConstant: Self.width).isActive = true
        return content
    }

    private func reloadCenter() {
        centerList.arrangedSubviews.forEach { $0.removeFromSuperview() }
        for (index, notification) in history.reversed().enumerated() {
            if index > 0 { centerList.addArrangedSubview(Self.separator()) }
            centerList.addArrangedSubview(makeRow(notification))
        }
        centerEmpty.superview?.isHidden = !history.isEmpty
        centerScroll.isHidden = history.isEmpty
        centerHeight.constant = min(centerList.fittingSize.height, Self.centerMaxHeight)
    }

    @objc private func clearAllClicked(_ sender: Any?) { clearAll() }
    @objc private func hideClicked(_ sender: Any?) { setCenterVisible(false) }

    private static func separator() -> NSView {
        let line = NSBox()
        line.boxType = .separator
        line.translatesAutoresizingMaskIntoConstraints = false
        line.widthAnchor.constraint(equalToConstant: width).isActive = true
        return line
    }

    static func iconButton(_ symbol: String, label: String, target: AnyObject?, action: Selector) -> NSButton {
        let image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)?
            .withSymbolConfiguration(.init(pointSize: 11, weight: .medium))
        let button = NSButton(image: image ?? NSImage(), target: target, action: action)
        button.isBordered = false
        button.contentTintColor = .secondaryLabelColor
        button.toolTip = label
        button.setAccessibilityLabel(label)
        button.translatesAutoresizingMaskIntoConstraints = false
        button.widthAnchor.constraint(equalToConstant: 16).isActive = true
        button.heightAnchor.constraint(equalToConstant: 16).isActive = true
        return button
    }
}

/// Severity icon, message, source and actions, with a close button.
@MainActor
final class NotificationRowView: NSView {
    let notification: AppNotification
    var onClose: (() -> Void)?
    var onAction: ((AppNotification.Action) -> Void)?
    var onHover: ((Bool) -> Void)?

    private static let inset: CGFloat = 12
    private static let iconSize: CGFloat = 16
    private static let spacing: CGFloat = 10

    init(_ notification: AppNotification, width: CGFloat) {
        self.notification = notification
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        setAccessibilityElement(false)
        let textWidth = width - Self.inset * 2 - Self.iconSize * 2 - Self.spacing * 2

        let icon = NSImageView()
        icon.image = NSImage(systemSymbolName: notification.severity.symbol,
                             accessibilityDescription: notification.severity.name)
        icon.symbolConfiguration = .init(pointSize: 13, weight: .regular)
        icon.contentTintColor = notification.severity.color
        icon.translatesAutoresizingMaskIntoConstraints = false

        let message = NSTextField(wrappingLabelWithString: notification.message)
        message.font = .systemFont(ofSize: 12)
        message.textColor = .labelColor
        message.isSelectable = true
        message.maximumNumberOfLines = 6
        message.cell?.truncatesLastVisibleLine = true
        message.preferredMaxLayoutWidth = textWidth
        message.setAccessibilityIdentifier("notification-message")
        message.translatesAutoresizingMaskIntoConstraints = false

        let text = NSStackView(views: [message])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 8
        if let footer = Self.footer(notification, width: textWidth, target: self) { text.addArrangedSubview(footer) }

        let close = NotificationsView.iconButton("xmark", label: "Clear Notification", target: self,
                                                 action: #selector(closeClicked(_:)))
        let row = NSStackView(views: [icon, text, close])
        row.orientation = .horizontal
        row.alignment = .top
        row.spacing = Self.spacing
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            icon.widthAnchor.constraint(equalToConstant: Self.iconSize),
            icon.heightAnchor.constraint(equalToConstant: Self.iconSize),
            message.widthAnchor.constraint(equalToConstant: textWidth),
            widthAnchor.constraint(equalToConstant: width),
            row.topAnchor.constraint(equalTo: topAnchor, constant: 11),
            row.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -11),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Self.inset),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Self.inset),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// Source on the left, action buttons on the right; nil when neither exists.
    private static func footer(_ notification: AppNotification, width: CGFloat, target: NotificationRowView) -> NSView? {
        guard notification.source != nil || !notification.actions.isEmpty else { return nil }
        var views: [NSView] = []
        if let source = notification.source {
            let label = NSTextField(labelWithString: "Source: \(source)")
            label.font = .systemFont(ofSize: 11)
            label.textColor = .secondaryLabelColor
            label.lineBreakMode = .byTruncatingTail
            label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
            views.append(label)
        }
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        views.append(spacer)
        for (index, action) in notification.actions.enumerated() {
            let button = NSButton(title: action.title, target: target, action: #selector(actionClicked(_:)))
            button.tag = index
            button.bezelStyle = .rounded
            button.controlSize = .small
            button.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            if index == 0 { button.bezelColor = .controlAccentColor }
            views.append(button)
        }
        let footer = NSStackView(views: views)
        footer.spacing = 8
        footer.translatesAutoresizingMaskIntoConstraints = false
        footer.widthAnchor.constraint(equalToConstant: width).isActive = true
        return footer
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect],
                                       owner: self, userInfo: nil))
    }

    override func mouseEntered(with event: NSEvent) { onHover?(true) }
    override func mouseExited(with event: NSEvent) { onHover?(false) }

    @objc private func closeClicked(_ sender: Any?) { onClose?() }

    @objc private func actionClicked(_ sender: NSButton) {
        guard notification.actions.indices.contains(sender.tag) else { return }
        onAction?(notification.actions[sender.tag])
    }
}

/// Rounded panel with a hairline border and a soft shadow, floating over
/// the editor.
@MainActor
final class NotificationCard: NSView {
    init(content: NSView) {
        super.init(frame: .zero)
        wantsLayer = true
        translatesAutoresizingMaskIntoConstraints = false
        layer?.cornerRadius = 8
        layer?.cornerCurve = .continuous
        layer?.borderWidth = 1
        layer?.shadowOpacity = 0.22
        layer?.shadowRadius = 10
        layer?.shadowOffset = CGSize(width: 0, height: -3)
        needsDisplay = true
        setAccessibilityRole(.group)
        content.translatesAutoresizingMaskIntoConstraints = false
        addSubview(content)
        NSLayoutConstraint.activate([
            content.topAnchor.constraint(equalTo: topAnchor),
            content.bottomAnchor.constraint(equalTo: bottomAnchor),
            content.leadingAnchor.constraint(equalTo: leadingAnchor),
            content.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var wantsUpdateLayer: Bool { true }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    /// Layer colours do not follow appearance changes by themselves.
    override func updateLayer() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = Theme.sidebar.cgColor
            layer?.borderColor = NSColor.separatorColor.cgColor
            layer?.shadowColor = NSColor.black.cgColor
        }
    }
}

private final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}
