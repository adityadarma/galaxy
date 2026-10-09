import AppKit

// Programmatic entry point: no storyboard or XIB, which keeps launch lean.
StartupProbe.mark("main")
let app = NSApplication.shared
StartupProbe.mark("nsapp")
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
