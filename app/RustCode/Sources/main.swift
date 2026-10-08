import AppKit

// Programmatic entry point: no storyboard or XIB, which keeps launch lean.
let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
