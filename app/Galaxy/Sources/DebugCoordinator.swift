import Foundation

/// Owns one debug session: breakpoints, the polling timer, and what the UI
/// shows (state, stop location, call stack, locals). It has no views, so it
/// can be tested against a real adapter.
@MainActor
final class DebugCoordinator {
    enum State: Equatable { case idle, building, running, paused }

    private(set) var state: State = .idle
    /// 1-based lines per file, keyed by `key(for:)`.
    private(set) var breakpoints: [String: Set<UInt32>] = [:]
    private(set) var frames: [DebugFrameInfo] = []
    private(set) var variables: [DebugVariableInfo] = []
    private(set) var stopReason: String?
    /// Where the program is stopped, when it is.
    private(set) var stoppedLocation: (path: String, line: UInt32)?

    /// Text for the Debug Console: category ("stdout", "stderr", "console", ...).
    var onOutput: ((String, String) -> Void)?
    /// Anything the UI shows changed.
    var onChange: (() -> Void)?
    /// The program stopped at a file and line; open it.
    var onStop: ((String, UInt32) -> Void)?

    private var handle: DebugHandle?
    private var timer: Timer?
    private var localsReference: Int64?
    private var cancelled = false

    nonisolated static func key(for path: String) -> String {
        URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL.path
    }

    var isActive: Bool { state != .idle }

    // MARK: Breakpoints

    func lines(in path: String) -> Set<UInt32> { breakpoints[Self.key(for: path)] ?? [] }

    /// Adds or removes a breakpoint; returns `true` if it is now set.
    @discardableResult
    func toggleBreakpoint(path: String, line: UInt32) -> Bool {
        let key = Self.key(for: path)
        var set = breakpoints[key] ?? []
        let added = set.insert(line).inserted
        if !added { set.remove(line) }
        breakpoints[key] = set.isEmpty ? nil : set
        handle?.setBreakpoints(path: key, lines: set.sorted())
        onChange?()
        return added
    }

    func removeAllBreakpoints() {
        let keys = Array(breakpoints.keys)
        breakpoints = [:]
        for key in keys { handle?.setBreakpoints(path: key, lines: []) }
        onChange?()
    }

    // MARK: Session

    /// Builds if the target asks for it, then starts the adapter. Progress and
    /// errors go to `onOutput`.
    func launch(_ target: DebugTarget) async {
        guard state == .idle else { return }
        cancelled = false
        if let build = target.build {
            state = .building
            onChange?()
            onOutput?("console", "> \(build)\n")
            let result = await Self.runShell(build, cwd: target.cwd)
            onOutput?(result.ok ? "stdout" : "stderr", result.output)
            guard result.ok, !cancelled else {
                if !cancelled { onOutput?("stderr", "Build failed; not starting the debugger.\n") }
                return finish()
            }
        }
        guard FileManager.default.isExecutableFile(atPath: target.program) else {
            onOutput?("stderr", "Program not found: \(target.program)\n")
            return finish()
        }
        do {
            let sets = breakpoints.map { DebugBreakpointSet(path: $0.key, lines: $0.value.sorted()) }
            handle = try startDebug(adapter: [], program: target.program, args: target.args, cwd: target.cwd,
                                    stopOnEntry: false, breakpoints: sets)
        } catch {
            onOutput?("stderr", "\(Self.describe(error))\n")
            return finish()
        }
        state = .running
        onOutput?("console", "Debugging \(target.program)\n")
        timer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in
            onAppKitMainThread { self?.poll() }
        }
        onChange?()
    }

    private static func describe(_ error: Error) -> String {
        if case CoreError.Io(let message) = error { return message }
        return "\(error)"
    }

    func resume() { send(paused: true) { $0.resume() } }
    func stepOver() { send(paused: true) { $0.stepOver() } }
    func stepIn() { send(paused: true) { $0.stepIn() } }
    func stepOut() { send(paused: true) { $0.stepOut() } }
    func pause() { send(paused: false) { $0.pause() } }

    func evaluate(_ expression: String) {
        guard let handle, state == .paused || state == .running else {
            onOutput?("stderr", "Not debugging.\n")
            return
        }
        handle.evaluate(expression: expression, frameId: frames.first?.id)
    }

    func stop() {
        cancelled = true
        handle?.stop()
        finish()
    }

    private func send(paused: Bool, _ action: (DebugHandle) -> Void) {
        guard let handle, !paused || state == .paused else { return }
        action(handle)
    }

    private func finish() {
        timer?.invalidate()
        timer = nil
        handle = nil
        state = .idle
        clearStop()
        onChange?()
    }

    private func clearStop() {
        frames = []
        variables = []
        localsReference = nil
        stopReason = nil
        stoppedLocation = nil
    }

    // MARK: Events

    /// Applies whatever the adapter has sent. The timer calls it; tests call
    /// it directly.
    func poll() {
        guard let handle else { return }
        for event in handle.poll() { apply(event, handle) }
    }

    private func apply(_ event: DebugEventInfo, _ handle: DebugHandle) {
        switch event {
        case .initialized, .continued:
            if case .continued = event { state = .running; clearStop(); onChange?() }
        case .output(let category, let text):
            onOutput?(category, text)
        case .stopped(_, let reason, let description):
            state = .paused
            stopReason = description ?? reason
            onOutput?("console", "Paused: \(stopReason ?? reason)\n")
            onChange?()
        case .frames(_, let frames):
            self.frames = frames
            if let top = frames.first {
                handle.requestScopes(frameId: top.id)
                if let path = top.path {
                    stoppedLocation = (Self.key(for: path), top.line)
                    onStop?(Self.key(for: path), top.line)
                }
            }
            onChange?()
        case .scopes(_, let scopes):
            if let first = scopes.first {
                localsReference = first.variablesReference
                handle.requestVariables(reference: first.variablesReference)
            }
        case .variables(let reference, let variables):
            if reference == localsReference { self.variables = variables; onChange?() }
        case .evaluated(let expression, let result, let ok):
            onOutput?(ok ? "console" : "stderr", ok ? "\(expression) = \(result)\n" : "\(result)\n")
        case .terminated, .adapterClosed:
            onOutput?("console", "Debug session ended.\n")
            finish()
        case .exited(let code):
            onOutput?("console", "Program exited with code \(code).\n")
        case .error(let message):
            onOutput?("stderr", "\(message)\n")
        }
    }

    // MARK: Build

    /// Runs `command` in a login shell so `cargo`, `swift`, `npm` are on PATH
    /// the way they are in Terminal.
    nonisolated static func runShell(_ command: String, cwd: String) async -> (ok: Bool, output: String) {
        await withCheckedContinuation { continuation in
            DispatchQueue.global().async {
                let log = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-build-\(UUID().uuidString).log")
                FileManager.default.createFile(atPath: log.path, contents: nil)
                defer { try? FileManager.default.removeItem(at: log) }
                let process = Process()
                process.executableURL = URL(fileURLWithPath: "/bin/zsh")
                process.arguments = ["-lc", command]
                process.currentDirectoryURL = URL(fileURLWithPath: cwd)
                guard let out = try? FileHandle(forWritingTo: log) else {
                    return continuation.resume(returning: (false, "Cannot create a build log.\n"))
                }
                process.standardOutput = out
                process.standardError = out
                do { try process.run() } catch {
                    return continuation.resume(returning: (false, "Cannot run \(command): \(error)\n"))
                }
                process.waitUntilExit()
                try? out.close()
                let text = (try? String(contentsOf: log, encoding: .utf8)) ?? ""
                continuation.resume(returning: (process.terminationStatus == 0, text))
            }
        }
    }
}
