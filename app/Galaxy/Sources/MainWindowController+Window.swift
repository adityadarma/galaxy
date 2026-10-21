import AppKit
import os

extension MainWindowController {
    static let fullScreenKey = "MainWindowFullScreen"

    /// Remembered so the next launch opens full screen again, as it was left.
    func windowDidEnterFullScreen(_ notification: Notification) {
        UserDefaults.standard.set(true, forKey: Self.fullScreenKey)
    }

    func windowDidExitFullScreen(_ notification: Notification) {
        UserDefaults.standard.set(false, forKey: Self.fullScreenKey)
        centerTrafficLights()
    }

    func windowDidResize(_ notification: Notification) { centerTrafficLights() }

    /// The system title bar is made as tall as `TitleBar`, and the close,
    /// minimise and zoom buttons are centred in it.
    func centerTrafficLights() {
        guard let window, !window.styleMask.contains(.fullScreen),
              let close = window.standardWindowButton(.closeButton),
              let container = close.superview?.superview else { return }
        let height = TitleBar.minimumHeight
        let frame = NSRect(x: 0, y: window.frame.height - height, width: window.frame.width, height: height)
        if container.frame != frame { container.frame = frame }
        for kind in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
            guard let button = window.standardWindowButton(kind), let bar = button.superview else { continue }
            let y = ((bar.bounds.height - button.frame.height) / 2).rounded()
            if button.frame.origin.y != y { button.setFrameOrigin(NSPoint(x: button.frame.origin.x, y: y)) }
        }
    }

    func windowDidBecomeKey(_ notification: Notification) {
        centerTrafficLights()
        // Files outside the workspace are not watched; this catches them,
        // and a conflict that waited for the window.
        checkOpenFiles()
        reloadKeybindings()
    }

    func windowDidResignKey(_ notification: Notification) {
        guard autoSaveMode == .onFocusChange else { return }
        autoSaveModifiedDocuments()
    }
}
