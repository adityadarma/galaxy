import XCTest
@testable import Galaxy

@MainActor
final class NotificationsTests: XCTestCase {
    func testToastsStackUpToTheLimitAndStayInTheList() {
        let view = NotificationsView()
        for index in 1...5 { view.show(AppNotification("message \(index)", severity: .warning)) }
        XCTAssertEqual(view.toastIDs.count, NotificationsView.maxToasts, "older toasts make room")
        XCTAssertEqual(view.history.count, 5, "hidden toasts stay in the list")
        XCTAssertEqual(view.unreadCount, 5)
    }

    func testTheSameMessageReplacesTheOldOne() {
        let view = NotificationsView()
        view.show(AppNotification("Could not checkout main.", severity: .error))
        view.show(AppNotification("Could not checkout main.", severity: .error))
        XCTAssertEqual(view.history.count, 1)
        XCTAssertEqual(view.toastIDs.count, 1)
    }

    func testOpeningTheListHidesToastsAndMarksThemRead() {
        let view = NotificationsView()
        var changes = 0
        view.onChange = { changes += 1 }
        view.show(AppNotification("one", severity: .error))
        view.toggleCenter()
        XCTAssertTrue(view.isCenterVisible)
        XCTAssertTrue(view.toastIDs.isEmpty)
        XCTAssertEqual(view.unreadCount, 0)
        XCTAssertEqual(view.history.count, 1)

        view.show(AppNotification("two"))
        XCTAssertTrue(view.toastIDs.isEmpty, "the open list shows new ones instead of a toast")
        XCTAssertEqual(view.unreadCount, 0)
        XCTAssertGreaterThan(changes, 0)
    }

    func testDismissAndClearAll() throws {
        let view = NotificationsView()
        view.show(AppNotification("one", severity: .error))
        view.show(AppNotification("two", severity: .error))
        let first = try XCTUnwrap(view.history.first)
        view.dismiss(first.id)
        XCTAssertEqual(view.history.map(\.message), ["two"])
        XCTAssertEqual(view.unreadCount, 1)

        view.hideToast(try XCTUnwrap(view.history.first).id)
        XCTAssertTrue(view.toastIDs.isEmpty)
        XCTAssertEqual(view.history.count, 1, "hiding a toast keeps it in the list")

        view.clearAll()
        XCTAssertTrue(view.history.isEmpty)
        XCTAssertEqual(view.unreadCount, 0)
        XCTAssertFalse(view.isCenterVisible)
    }

    func testGitFailuresBecomeNotificationsAndSuccessesStayInTheStatusBar() {
        let c = MainWindowController(bridge: CoreBridge())
        defer { c.window?.orderOut(nil) }
        c.notifyGitMessage("Committed 1 file")
        XCTAssertTrue(c.notificationsView.history.isEmpty)
        XCTAssertEqual(c.statusLabel.stringValue, "Committed 1 file")

        c.notifyGitMessage("Git: nothing to push")
        XCTAssertEqual(c.notificationsView.history.last?.message, "nothing to push")
        XCTAssertEqual(c.notificationsView.history.last?.source, "Git")
        XCTAssertEqual(c.notificationsView.history.last?.severity, .error)
        XCTAssertEqual(c.statusLabel.stringValue, "Committed 1 file", "a failure is shown once, as a toast")
        XCTAssertEqual(c.statusBar.notifications.accessibilityValue() as? String, "1 new notification")
    }

    func testOpeningABinaryFileOffersTheDefaultAppInsteadOfARawError() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        // Not SQLite (those open in a database tab), just binary.
        try Data("GDBM\0\0\u{1}\u{2}".utf8).write(to: root.appendingPathComponent("app.db"))
        let c = MainWindowController(bridge: CoreBridge())
        defer { c.window?.orderOut(nil) }
        try c.openWorkspace(at: root, rememberRecent: false)
        let ws = try XCTUnwrap(c.workspace)

        c.openWorkspaceFile(ws, FileEntry(name: "app.db", relPath: "app.db", isDir: false),
                            preview: false, rememberRecent: false)

        let note = try XCTUnwrap(c.notificationsView.history.last)
        XCTAssertEqual(note.message, "app.db is a binary file and cannot be shown in the editor.")
        XCTAssertEqual(note.severity, .warning)
        XCTAssertEqual(note.actions.map(\.title), ["Open in Default App", "Reveal in Finder"])
        XCTAssertFalse(note.message.contains("CoreError"))
    }
}
