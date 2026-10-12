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
        CrashReporter.install()
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
        if pendingURL == nil, let path = Self.launchOpenPath {
            pendingURL = URL(fileURLWithPath: path)
        }
        if let url = pendingURL {
            pendingURL = nil
            controller.open(url: url)
        } else if Self.shouldRestoreSession, restoreSession(in: controller) {
            // The prior workspace and its tabs are restored only after a quit.
        } else {
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

    private static var launchOpenPath: String? {
        let arguments = ProcessInfo.processInfo.arguments
        guard let index = arguments.firstIndex(of: "-OpenPath"), arguments.index(after: index) < arguments.endIndex else {
            return nil
        }
        return arguments[arguments.index(after: index)]
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

    /// Off for benchmarks, tests, and an explicit no-restore launch.
    private static var shouldRestoreSession: Bool {
        let env = ProcessInfo.processInfo.environment
        if env["GALAXY_BENCH"] != nil || env["XCTestConfigurationFilePath"] != nil
            || LatencyBench.requestedCount != nil || ScrollBench.requestedFrames != nil {
            return false
        }
        let arguments = ProcessInfo.processInfo.arguments
        if let index = arguments.firstIndex(of: "-RestoreSession"),
           arguments.index(after: index) < arguments.endIndex,
           arguments[arguments.index(after: index)].uppercased() == "NO" {
            return false
        }
        return true
    }

    private func restoreSession(in controller: MainWindowController) -> Bool {
        guard let session = Session.load(from: controller.sessionURL) else { return false }
        return controller.restore(session)
    }

    func applicationWillTerminate(_ notification: Notification) {
        windowController?.saveSessionForQuit()
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let windowController else { return .terminateNow }
        guard windowController.confirmDiscardChanges() else { return .terminateCancel }
        return .terminateNow
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
        true
    }
}
