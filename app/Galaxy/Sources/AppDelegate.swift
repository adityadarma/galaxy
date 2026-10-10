import AppKit
import os

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let bridge = CoreBridge()
    private var windowController: MainWindowController?
    private var preferencesController: PreferencesWindowController?
    /// A path handed to us before the window existed (`Galaxy <path>`).
    private var pendingURL: URL?

    func application(_ application: NSApplication, open urls: [URL]) {
        guard let url = urls.first else { return }
        if let windowController {
            windowController.open(url: url)
        } else {
            pendingURL = url
        }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        StartupProbe.mark("did_finish_launching")
        Log.app.info("launch started")
        bridge.start()
        applyAppearancePreference()
        StartupProbe.mark("core_started")

        NSApp.mainMenu = MainMenu.build()
        StartupProbe.mark("menu")
        let controller = MainWindowController(bridge: bridge)
        StartupProbe.mark("window_built")
        controller.showWindow(nil)
        StartupProbe.mark("window_shown")
        windowController = controller
        // `Galaxy -OpenPath <file or folder>`; UI tests use this instead of
        // driving the open panel.
        if pendingURL == nil, let path = UserDefaults.standard.string(forKey: "OpenPath") {
            pendingURL = URL(fileURLWithPath: path)
        }
        if let url = pendingURL {
            pendingURL = nil
            controller.open(url: url)
        } else if !Self.shouldRestoreSession || !restoreSession(in: controller) {
            // Start in an empty buffer so the window is usable immediately.
            controller.newDocument(nil)
        }

        StartupProbe.mark("document_open")
        NSApp.activate()
        Log.app.info("window ready, core \(self.bridge.version, privacy: .public)")

        // Report after the first frame is committed, not just after showWindow.
        DispatchQueue.main.async { StartupProbe.reportWindowReady() }

        if let count = LatencyBench.requestedCount {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                LatencyBench.start(in: controller, count: count)
            }
        } else if let frames = ScrollBench.requestedFrames {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                ScrollBench.start(in: controller, frames: frames)
            }
        }

        // Verifies the Rust -> Swift callback path end-to-end.
        bridge.ping("startup")
    }

    @objc func showPreferences(_ sender: Any?) {
        let controller = PreferencesWindowController { [weak self] in
            self?.applyAppearancePreference()
            self?.windowController?.applyPreferences()
        }
        preferencesController = controller
        controller.showWindow(sender)
        NSApp.activate(ignoringOtherApps: true)
    }

    private func applyAppearancePreference() {
        switch UserDefaults.standard.string(forKey: "Appearance") {
        case "Light": NSApp.appearance = NSAppearance(named: .aqua)
        case "Dark": NSApp.appearance = NSAppearance(named: .darkAqua)
        default: NSApp.appearance = nil
        }
    }

    /// Off for benchmarks (startup must not depend on what was open last),
    /// for the unit-test host, and with `-RestoreSession NO` (UI tests).
    private static var shouldRestoreSession: Bool {
        let env = ProcessInfo.processInfo.environment
        if env["GALAXY_BENCH"] != nil || env["XCTestConfigurationFilePath"] != nil
            || LatencyBench.requestedCount != nil || ScrollBench.requestedFrames != nil {
            return false
        }
        return UserDefaults.standard.object(forKey: "RestoreSession") as? Bool ?? true
    }

    private func restoreSession(in controller: MainWindowController) -> Bool {
        guard let session = Session.load(from: controller.sessionURL) else { return false }
        return controller.restore(session)
    }

    func applicationWillTerminate(_ notification: Notification) {
        // Quitting with the window open does not close it first.
        windowController?.saveSession()
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        windowController?.confirmDiscardChanges() ?? true ? .terminateNow : .terminateCancel
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
        true
    }
}
