import AppKit

extension MainWindowController {
    // MARK: Notifications

    /// Shows a toast in the bottom-right corner and keeps it in the list
    /// the status bar bell opens.
    func notify(_ message: String, severity: AppNotification.Severity = .info, source: String? = nil,
                actions: [AppNotification.Action] = []) {
        notificationsView.show(AppNotification(message, severity: severity, source: source, actions: actions))
    }

    /// Git panels report successes and failures through one string; failures start with "Git: ".
    func notifyGitMessage(_ message: String) {
        statusLabel.stringValue = message
        if message.hasPrefix("Git: ") {
            notify(String(message.dropFirst(5)), severity: .error, source: "Git")
        }
    }

    func updateNotificationsButton() {
        statusBar.showNotifications(unread: notificationsView.unreadCount, isOpen: notificationsView.isCenterVisible)
    }

    @objc func toggleNotifications(_ sender: Any?) {
        notificationsView.toggleCenter()
    }

    @objc func clearAllNotifications(_ sender: Any?) {
        notificationsView.clearAll()
    }
}
