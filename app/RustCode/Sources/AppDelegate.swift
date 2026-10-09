import AppKit
import os

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let bridge = CoreBridge()
    private var windowController: MainWindowController?
    /// A path handed to us before the window existed (`RustCode <path>`).
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
        StartupProbe.mark("core_started")

        NSApp.mainMenu = MainMenu.build()
        StartupProbe.mark("menu")
        let controller = MainWindowController(bridge: bridge)
        StartupProbe.mark("window_built")
        controller.showWindow(nil)
        StartupProbe.mark("window_shown")
        windowController = controller
        // `RustCode -OpenPath <file or folder>`; UI tests use this instead of
        // driving the open panel.
        if pendingURL == nil, let path = UserDefaults.standard.string(forKey: "OpenPath") {
            pendingURL = URL(fileURLWithPath: path)
        }
        if let url = pendingURL {
            pendingURL = nil
            controller.open(url: url)
        } else {
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
