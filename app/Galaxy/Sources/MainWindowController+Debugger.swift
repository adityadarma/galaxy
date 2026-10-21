import AppKit
import os

extension MainWindowController {
    // MARK: Debugger

    /// Start, or continue when the program is paused.
    @objc func startDebugging(_ sender: Any?) {
        switch debugger.state {
        case .paused:
            debugger.resume()
        case .idle:
            guard let workspace else { return NSSound.beep() }
            revealBottomPanel(.debugConsole)
            guard let target = DebugTargets.detect(in: URL(fileURLWithPath: workspace.root())) else {
                debugConsole.append("Nothing to debug. Add .galaxy/launch.json with {\"program\": \"path/to/binary\"}, or open a Cargo project.\n", category: "stderr")
                return
            }
            debugConsole.clear()
            Task { @MainActor in await self.debugger.launch(target) }
        case .building, .running:
            break
        }
    }

    @objc func stopDebugging(_ sender: Any?) { debugger.stop() }
    @objc func debugStepOver(_ sender: Any?) { debugger.stepOver() }
    @objc func debugStepInto(_ sender: Any?) { debugger.stepIn() }
    @objc func debugStepOut(_ sender: Any?) { debugger.stepOut() }
    @objc func removeAllBreakpoints(_ sender: Any?) { debugger.removeAllBreakpoints() }

    @objc func showDebugConsole(_ sender: Any?) {
        revealBottomPanel(.debugConsole)
        debugConsole.focusInput()
    }

    @objc func toggleBreakpoint(_ sender: Any?) {
        guard let doc = activeTab?.doc else { return NSSound.beep() }
        toggleBreakpoint(line: doc.primaryCursor().line)
    }

    /// `line` is 0-based; breakpoints are 1-based like the adapter's.
    func toggleBreakpoint(line: UInt64) {
        guard let tab = activeTab, tab.isDocument, let key = tab.fileKey else { return NSSound.beep() }
        debugger.toggleBreakpoint(path: key, line: UInt32(line + 1))
    }

    func refreshDebugMarkers() {
        guard let tab = activeTab, tab.isDocument, let key = tab.fileKey else {
            editorView.breakpointLines = []
            editorView.debugStopLine = nil
            return
        }
        editorView.breakpointLines = Set(debugger.lines(in: key).map { UInt64($0 - 1) })
        editorView.debugStopLine = debugger.stoppedLocation.flatMap { stop in
            stop.path == key ? Int(stop.line) - 1 : nil
        }
    }

    func debugStateChanged() {
        refreshDebugMarkers()
        debugConsole.showState(frames: debugger.frames, variables: debugger.variables,
                               reason: debugger.state == .paused ? debugger.stopReason : nil)
    }
}
