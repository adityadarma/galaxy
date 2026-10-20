import AppKit
import os

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let bridge = CoreBridge()
    private var windowControllers: [MainWindowController] = []
    private var preferencesController: PreferencesWindowController?
    /// A path handed to us before the window existed (`Galaxy <path>`).
    private var pendingURL: URL?
    /// Sessions of the extra windows saved at the last quit.
    private var pendingWindowSessions: [Session] = []
    /// Windows still to put in full screen, one at a time: each transition
    /// has to finish before the next starts, or AppKit drops it.
    private var fullScreenQueue: [NSWindow] = []

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
        // Left in full screen at the last quit: open in full screen again.
        // The project is restored before the window appears, so the
        // transition does not start from an empty window.
        // Older sessions have no per-window flag; they used one global key.
        let savedSession = Self.shouldRestoreSession ? Session.load(from: controller.sessionURL) : nil
        let resumeFullScreen = Self.shouldRestoreSession
            && (savedSession?.fullScreen ?? UserDefaults.standard.bool(forKey: "MainWindowFullScreen"))
        if !resumeFullScreen { showMainWindow(controller) }
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
        if resumeFullScreen { showMainWindow(controller) }
        // Every window is shown on the regular desktop first. Then the ones
        // that were full screen enter it in turn, each in its own Space.
        var fullScreenWindows = resumeFullScreen ? [controller.window].compactMap { $0 } : []
        fullScreenWindows += restorePendingWindows()
        fullScreenQueue = fullScreenWindows
        enterNextFullScreen()

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
        Theme.apply()
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

    private func showMainWindow(_ controller: MainWindowController) {
        controller.showWindow(nil)
        controller.centerTrafficLights()
        StartupProbe.mark("window_shown")
    }

    private func restoreSession(in controller: MainWindowController) -> Bool {
        guard let session = Session.load(from: controller.sessionURL) else { return false }
        pendingWindowSessions = session.otherWindows ?? []
        return controller.restore(session)
    }

    /// Reopens the windows that were open besides the first one at quit.
    /// Returns those that were full screen.
    private func restorePendingWindows() -> [NSWindow] {
        let sessions = pendingWindowSessions
        pendingWindowSessions = []
        var fullScreen: [NSWindow] = []
        for session in sessions {
            let controller = makeWindowController()
            if !controller.restore(session) { controller.newDocument(nil) }
            showMainWindow(controller)
            if session.fullScreen == true, let window = controller.window { fullScreen.append(window) }
        }
        return fullScreen
    }

    /// The window whose full-screen transition is in flight.
    private var fullScreenPending: NSWindow?
    private var fullScreenObserver: NSObjectProtocol?

    private func enterNextFullScreen() {
        guard fullScreenPending == nil, !fullScreenQueue.isEmpty else { return }
        let window = fullScreenQueue.removeFirst()
        fullScreenPending = window
        fullScreenObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.didEnterFullScreenNotification, object: window, queue: .main
        ) { [weak self, weak window] _ in
            MainActor.assumeIsolated {
                if let window { self?.finishFullScreen(of: window) }
            }
        }
        // If the transition never reports, do not lose the other windows.
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self, weak window] in
            MainActor.assumeIsolated {
                if let window { self?.finishFullScreen(of: window) }
            }
        }
        window.toggleFullScreen(nil)
    }

    private func finishFullScreen(of window: NSWindow) {
        guard fullScreenPending === window else { return }
        fullScreenPending = nil
        if let observer = fullScreenObserver {
            NotificationCenter.default.removeObserver(observer)
            fullScreenObserver = nil
        }
        enterNextFullScreen()
    }

    func applicationWillTerminate(_ notification: Notification) {
        // Session is captured in `applicationShouldTerminate` before AppKit
        // starts closing any windows.
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard windowControllers.allSatisfy({ $0.confirmDiscardChanges() }) else { return .terminateCancel }
        let active = activeWindowController
        let others = windowControllers.filter { $0 !== active }.map { controller -> Session in
            var session = controller.currentSession()
            session.fullScreen = controller.window?.styleMask.contains(.fullScreen) == true
            return session
        }.filter { $0.workspace != nil || !$0.tabs.isEmpty }
        active?.saveSessionForQuit(otherWindows: others)
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
        let fromFullScreen = isActiveWindowFullScreen
        let controller = makeWindowController()
        controller.showWindow(sender)
        controller.newDocument(nil)
        NSApp.activate(ignoringOtherApps: true)
        moveToRegularDesktop(controller, fromFullScreen)
    }

    private var isActiveWindowFullScreen: Bool {
        activeWindowController?.window?.styleMask.contains(.fullScreen) == true
    }

    /// A window shown while another one is full screen joins that Space.
    /// AppKit has no switch to refuse that, so the window enters full screen
    /// and leaves it again: on exit, macOS puts it on a regular desktop.
    private func moveToRegularDesktop(_ controller: MainWindowController, _ fromFullScreen: Bool) {
        guard fromFullScreen, let window = controller.window,
              !window.styleMask.contains(.fullScreen) else { return }
        var token: NSObjectProtocol?
        token = NotificationCenter.default.addObserver(
            forName: NSWindow.didEnterFullScreenNotification, object: window, queue: .main
        ) { _ in
            if let token { NotificationCenter.default.removeObserver(token) }
            MainActor.assumeIsolated { window.toggleFullScreen(nil) }
        }
        window.toggleFullScreen(nil)
    }

    private var activeWindowController: MainWindowController? {
        guard let keyWindow = NSApp.keyWindow else { return windowControllers.last }
        return windowControllers.first { $0.window === keyWindow } ?? windowControllers.last
    }

    private func makeWindowController() -> MainWindowController {
        let controller = MainWindowController(bridge: bridge)
        controller.openTerminalInNewWindow = { [weak self] terminal, root in
            guard let self else { return }
            let fromFullScreen = self.isActiveWindowFullScreen
            let window = self.makeWindowController()
            window.showWindow(nil)
            try? window.openWorkspace(at: root)
            window.adoptTerminal(terminal)
            NSApp.activate(ignoringOtherApps: true)
            self.moveToRegularDesktop(window, fromFullScreen)
        }
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
