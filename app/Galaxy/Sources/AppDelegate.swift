import AppKit
import os

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation {
    private let bridge = CoreBridge()
    private let updater = AppUpdater()
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
        let controller = makeWindowController(bridge: bridge)
        StartupProbe.mark("window_built")
        let savedSession = Self.shouldRestoreSession ? Session.load(from: controller.sessionURL) : nil
        let resumeFullScreen = Self.shouldRestoreSession
            && (savedSession?.fullScreen ?? UserDefaults.standard.bool(forKey: "MainWindowFullScreen"))
        if pendingURL == nil, let path = Self.launchOpenPath {
            pendingURL = URL(fileURLWithPath: path)
        }
        // Workspace and tab restoration can touch many files. Present the
        // window first so AppKit remains responsive during a large restore.
        let restoredSession = false
        showMainWindow(controller, concealedForFullScreen: resumeFullScreen)
        NSApp.activate()
        Log.app.info("window ready, core \(self.bridge.version, privacy: .public)")

        // Report after the first frame is committed, not just after showWindow.
        DispatchQueue.main.async { [weak self, weak controller] in
            StartupProbe.reportWindowReady()
            // Yield once more after the visible frame before restoring files.
            DispatchQueue.main.async {
                guard let self, let controller else { return }
                self.restoreLaunchSession(
                    in: controller,
                    session: savedSession,
                    resumeFullScreen: resumeFullScreen,
                    restoredSession: restoredSession
                )
            }
        }

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

    private func restoreLaunchSession(
        in controller: MainWindowController,
        session: Session?,
        resumeFullScreen: Bool,
        restoredSession: Bool
    ) {
        // `Galaxy -OpenPath <file or folder>`; UI tests use this instead of
        // driving the open panel.
        if pendingURL == nil, let path = Self.launchOpenPath {
            pendingURL = URL(fileURLWithPath: path)
        }
        if restoredSession {
            // The primary workspace was restored before the window appeared.
        } else if let url = pendingURL {
            pendingURL = nil
            controller.open(url: url)
        } else if Self.shouldRestoreSession, let session, controller.restore(session) {
            pendingWindowSessions = session.otherWindows ?? []
            // The prior workspace and its tabs are restored only after a quit.
        } else {
            controller.newDocument(nil)
        }

        // Restore additional windows one per run-loop turn. Restoring all of
        // them synchronously blocks drawing and makes a multi-window launch
        // look frozen even when the primary editor is ready.
        let fullScreenWindows = resumeFullScreen ? [controller.window].compactMap { $0 } : []
        fullScreenQueue = fullScreenWindows
        enterNextFullScreen()
        restoreNextPendingWindow()

        // `scripts/measure-footprint.sh`: open extra files as tabs after the
        // workspace, so the footprint covers a realistic editing session.
        if let files = ProcessInfo.processInfo.environment["GALAXY_BENCH_OPEN_FILES"] {
            for path in files.split(separator: ":") where !path.isEmpty {
                controller.open(url: URL(fileURLWithPath: String(path)))
            }
        }
        StartupProbe.mark("document_open")
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

    @objc func checkForUpdates(_ sender: Any?) {
        updater.checkForUpdates()
    }

    func canCheckForUpdates() -> Bool {
        updater.isAvailable
    }

    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        if item.action == #selector(checkForUpdates(_:)) {
            return canCheckForUpdates()
        }
        return true
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

    private func showMainWindow(
        _ controller: MainWindowController,
        concealedForFullScreen: Bool = false
    ) {
        if concealedForFullScreen { controller.window?.alphaValue = 0 }
        controller.showWindow(nil)
        controller.centerTrafficLights()
        StartupProbe.mark("window_shown")
    }

    private func restoreSession(in controller: MainWindowController) -> Bool {
        guard let session = Session.load(from: controller.sessionURL) else { return false }
        pendingWindowSessions = session.otherWindows ?? []
        return controller.restore(session)
    }

    /// Reopens one extra window per run-loop turn so AppKit can draw each
    /// restored workspace before the next one starts opening files and tabs.
    private func restoreNextPendingWindow() {
        guard !pendingWindowSessions.isEmpty else {
            enterNextFullScreen()
            return
        }
        // A window shown while another one is full screen joins that Space.
        // Windows that were not full screen wait for the full-screen
        // transitions to end, then leave for a regular desktop.
        if pendingWindowSessions[0].fullScreen != true, fullScreenPending != nil || !fullScreenQueue.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { [weak self] in
                MainActor.assumeIsolated { self?.restoreNextPendingWindow() }
            }
            return
        }
        let session = pendingWindowSessions.removeFirst()
        if let root = session.workspace,
           let existing = windowController(forWorkspaceAt: root) {
            existing.showWindow(nil)
            existing.window?.makeKeyAndOrderFront(nil)
            restoreNextPendingWindow()
            return
        }
        let fromFullScreen = session.fullScreen != true && isAnyWindowFullScreen
        let controller = makeWindowController()
        if !controller.restore(session) { controller.newDocument(nil) }
        showMainWindow(controller, concealedForFullScreen: session.fullScreen == true)
        if session.fullScreen == true, let window = controller.window {
            fullScreenQueue.append(window)
            enterNextFullScreen()
        }
        // The next window is restored once this one has left the full-screen
        // Space, so two transitions never overlap.
        moveToRegularDesktop(controller, fromFullScreen) { [weak self] in
            DispatchQueue.main.async { self?.restoreNextPendingWindow() }
        }
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
        window.alphaValue = 1
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

    private var isAnyWindowFullScreen: Bool {
        windowControllers.contains { $0.window?.styleMask.contains(.fullScreen) == true }
    }

    /// A window shown while another one is full screen joins that Space.
    /// AppKit has no switch to refuse that, so the window enters full screen
    /// and leaves it again: on exit, macOS puts it on a regular desktop.
    private func moveToRegularDesktop(
        _ controller: MainWindowController,
        _ fromFullScreen: Bool,
        completion: (() -> Void)? = nil
    ) {
        guard fromFullScreen, let window = controller.window,
              !window.styleMask.contains(.fullScreen) else {
            completion?()
            return
        }
        let finish = FullScreenExit(completion: completion)
        var enterToken: NSObjectProtocol?
        enterToken = NotificationCenter.default.addObserver(
            forName: NSWindow.didEnterFullScreenNotification, object: window, queue: .main
        ) { _ in
            if let enterToken { NotificationCenter.default.removeObserver(enterToken) }
            var exitToken: NSObjectProtocol?
            exitToken = NotificationCenter.default.addObserver(
                forName: NSWindow.didExitFullScreenNotification, object: window, queue: .main
            ) { _ in
                if let exitToken { NotificationCenter.default.removeObserver(exitToken) }
                MainActor.assumeIsolated { finish.run() }
            }
            MainActor.assumeIsolated { window.toggleFullScreen(nil) }
        }
        // If a transition never reports, do not stall the windows behind it.
        DispatchQueue.main.asyncAfter(deadline: .now() + 5) {
            MainActor.assumeIsolated { finish.run() }
        }
        window.toggleFullScreen(nil)
    }

    private var activeWindowController: MainWindowController? {
        guard let keyWindow = NSApp.keyWindow else { return windowControllers.last }
        return windowControllers.first { $0.window === keyWindow } ?? windowControllers.last
    }

    private func makeWindowController(bridge suppliedBridge: CoreBridge? = nil) -> MainWindowController {
        let windowBridge: CoreBridge
        if let suppliedBridge {
            windowBridge = suppliedBridge
        } else {
            windowBridge = CoreBridge()
            windowBridge.start()
        }
        let controller = MainWindowController(bridge: windowBridge)
        controller.openExistingWorkspace = { [weak self] url in
            guard let self, let existing = self.windowController(forWorkspaceAt: url.path) else {
                return false
            }
            existing.showWindow(nil)
            existing.window?.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return true
        }
        controller.openTerminalInNewWindow = { [weak self] terminal, root in
            guard let self else { return }
            let fromFullScreen = self.isActiveWindowFullScreen
            let window = self.makeWindowController()
            window.showWindow(nil)
            // Keep the terminal's process and working directory, but do not
            // attach the same workspace to a second window.
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

    private func windowController(
        forWorkspaceAt path: String,
        excluding excluded: MainWindowController? = nil
    ) -> MainWindowController? {
        guard let root = MainWindowController.fileKey(path) else { return nil }
        return windowControllers.first {
            $0 !== excluded && MainWindowController.fileKey($0.workspace?.root() ?? "") == root
        }
    }
}

/// Runs a completion handler once, however many signals report the end of a
/// window's trip through full screen.
@MainActor
private final class FullScreenExit {
    private var completion: (() -> Void)?

    init(completion: (() -> Void)?) { self.completion = completion }

    func run() {
        let handler = completion
        completion = nil
        handler?()
    }
}
