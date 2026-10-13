import AppKit
import os

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let bridge = CoreBridge()
    private var windowControllers: [MainWindowController] = []
    private var preferencesController: PreferencesWindowController?
    /// A path handed to us before the window existed (`Galaxy <path>`).
    private var pendingURL: URL?

    func application(_ application: NSApplication, open urls: [URL]) {
        guard let url = urls.first else { return }
        if let windowController = activeWindowController {
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
        let controller = makeWindowController()
        StartupProbe.mark("window_built")
        controller.showWindow(nil)
        StartupProbe.mark("window_shown")
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

        // `scripts/measure-footprint.sh`: open extra files as tabs after the
        // workspace, so the footprint covers a realistic editing session.
        if let files = ProcessInfo.processInfo.environment["GALAXY_BENCH_OPEN_FILES"] {
            for path in files.split(separator: ":") where !path.isEmpty {
                controller.open(url: URL(fileURLWithPath: String(path)))
            }
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
            self?.windowControllers.forEach { $0.applyPreferences() }
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
        // Session is captured in `applicationShouldTerminate` before AppKit
        // starts closing any windows.
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard windowControllers.allSatisfy({ $0.confirmDiscardChanges() }) else { return .terminateCancel }
        activeWindowController?.saveSessionForQuit()
        windowControllers.forEach {
            $0.saveWorkspaceSessionForQuit()
            $0.prepareForApplicationQuit()
        }
        return .terminateNow
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
        true
    }

    @objc func newWindow(_ sender: Any?) {
        let controller = makeWindowController()
        controller.showWindow(sender)
        controller.newDocument(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    private var activeWindowController: MainWindowController? {
        guard let keyWindow = NSApp.keyWindow else { return windowControllers.last }
        return windowControllers.first { $0.window === keyWindow } ?? windowControllers.last
    }

    private func makeWindowController() -> MainWindowController {
        let controller = MainWindowController(bridge: bridge)
        controller.onWindowClosed = { [weak self, weak controller] in
            guard let self, let controller else { return }
            self.windowControllers.removeAll { $0 === controller }
            if self.windowControllers.isEmpty && !controller.isClosingForApplicationTermination {
                Session.remove(at: Session.defaultURL)
            }
        }
        windowControllers.append(controller)
        return controller
    }
}
