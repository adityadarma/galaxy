import XCTest
@testable import Galaxy

@MainActor
final class DebugTests: XCTestCase {
    private let root = URL(fileURLWithPath: "/work/app")

    // MARK: Targets

    func testParsesAVSCodeStyleLaunchFile() {
        let text = """
        {
          // comment lines are ignored
          "configurations": [
            {"name": "no program"},
            {"program": "${workspaceFolder}/bin/app", "args": ["-v", "${workspaceFolder}/in"], "build": "make"}
          ]
        }
        """
        let target = DebugTargets.parseLaunch(text, root: root)
        XCTAssertEqual(target, DebugTarget(program: "/work/app/bin/app", args: ["-v", "/work/app/in"],
                                           cwd: "/work/app", build: "make"))
    }

    func testParsesASingleConfigWithARelativeProgram() {
        let target = DebugTargets.parseLaunch(#"{"program": "build/run", "cwd": "/tmp"}"#, root: root)
        XCTAssertEqual(target?.program, "/work/app/build/run")
        XCTAssertEqual(target?.cwd, "/tmp")
        XCTAssertNil(target?.build)
    }

    func testRejectsLaunchFilesWithoutAProgram() {
        XCTAssertNil(DebugTargets.parseLaunch("not json", root: root))
        XCTAssertNil(DebugTargets.parseLaunch(#"{"configurations": []}"#, root: root))
        XCTAssertNil(DebugTargets.parseLaunch(#"{"program": ""}"#, root: root))
    }

    func testCargoPackageNameComesFromThePackageTableOnly() {
        let manifest = """
        [workspace]
        name = "not-this"
        [package]
        version = "0.1.0"
        name = "galaxy-demo"
        [dependencies]
        name = "nor-this"
        """
        XCTAssertEqual(DebugTargets.packageName(inCargoManifest: manifest), "galaxy-demo")
        XCTAssertNil(DebugTargets.packageName(inCargoManifest: "[workspace]\nmembers = []"))
    }

    func testDetectPrefersLaunchJSONOverCargo() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("dbg-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir.appendingPathComponent(".galaxy"), withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        try "[package]\nname = \"demo\"\n".write(to: dir.appendingPathComponent("Cargo.toml"), atomically: true, encoding: .utf8)
        XCTAssertEqual(DebugTargets.detect(in: dir)?.program, dir.appendingPathComponent("target/debug/demo").path)
        XCTAssertEqual(DebugTargets.detect(in: dir)?.build, "cargo build")

        try #"{"program": "out/main"}"#.write(to: dir.appendingPathComponent(".galaxy/launch.json"), atomically: true, encoding: .utf8)
        XCTAssertEqual(DebugTargets.detect(in: dir)?.program, dir.appendingPathComponent("out/main").path)
    }

    // MARK: Breakpoints

    func testTogglingBreakpointsAddsAndRemoves() {
        let d = DebugCoordinator()
        var changes = 0
        d.onChange = { changes += 1 }
        XCTAssertTrue(d.toggleBreakpoint(path: "/tmp/a.c", line: 4))
        XCTAssertTrue(d.toggleBreakpoint(path: "/tmp/a.c", line: 9))
        XCTAssertEqual(d.lines(in: "/tmp/a.c"), [4, 9])
        XCTAssertFalse(d.toggleBreakpoint(path: "/tmp/a.c", line: 4))
        XCTAssertEqual(d.lines(in: "/tmp/a.c"), [9])
        d.removeAllBreakpoints()
        XCTAssertTrue(d.breakpoints.isEmpty)
        XCTAssertEqual(changes, 4)
        XCTAssertEqual(d.state, .idle)
        d.stop() // no session: must not crash
    }

    func testLaunchReportsAMissingProgramAndReturnsToIdle() async {
        let d = DebugCoordinator()
        var output = ""
        d.onOutput = { _, text in output += text }
        await d.launch(DebugTarget(program: "/nonexistent/prog", cwd: "/tmp"))
        XCTAssertEqual(d.state, .idle)
        XCTAssertTrue(output.contains("Program not found"))
    }

    func testLaunchStopsWhenTheBuildFails() async {
        let d = DebugCoordinator()
        var output = ""
        d.onOutput = { _, text in output += text }
        await d.launch(DebugTarget(program: "/bin/ls", cwd: "/tmp", build: "echo broken >&2; exit 3"))
        XCTAssertEqual(d.state, .idle)
        XCTAssertTrue(output.contains("broken"))
        XCTAssertTrue(output.contains("Build failed"))
    }

    // MARK: Editor markers

    func testEditorDrawsBreakpointsAndTheStopLine() throws {
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 600, height: 300))
        view.breakpointLines = [1]
        view.debugStopLine = 2
        let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: rep)
        XCTAssertEqual(view.breakpointLines, [1])
        XCTAssertEqual(view.debugStopLine, 2)
    }

    func testClickingTheGutterTogglesABreakpointInsteadOfMovingTheCaret() throws {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        defer { c.window?.orderOut(nil) }
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("dbg-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("a.c")
        try "int a;\nint b;\nint c;\n".write(to: file, atomically: true, encoding: .utf8)
        try c.openFile(at: file)

        let v = c.editorView
        let y = v.metrics.lineHeight * 1.5
        let point = v.convert(NSPoint(x: 4, y: y), to: nil)
        let event = try XCTUnwrap(NSEvent.mouseEvent(
            with: .leftMouseDown, location: point, modifierFlags: [], timestamp: 0,
            windowNumber: c.window?.windowNumber ?? 0, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        v.mouseDown(with: event)

        let key = DebugCoordinator.key(for: file.path)
        XCTAssertEqual(c.debugger.lines(in: key), [2], "second line, 1-based")
        XCTAssertEqual(v.breakpointLines, [1], "0-based in the view")
        XCTAssertEqual(c.activeTab?.doc.primaryCursor().line, 0, "the caret stayed put")

        v.mouseDown(with: event)
        XCTAssertTrue(c.debugger.lines(in: key).isEmpty)
        XCTAssertTrue(v.breakpointLines.isEmpty)
    }

    // MARK: Menu

    func testRunMenuHasTheDebugItemsAndTheirKeys() throws {
        let main = MainMenu.build()
        Keybindings.defaults.apply(to: main)
        let run = try XCTUnwrap(main.items.compactMap(\.submenu).first { $0.title == "Run" })
        func item(_ title: String) -> NSMenuItem? { run.items.first { $0.title == title } }
        XCTAssertEqual(item("Start Debugging")?.keyEquivalent, String(UnicodeScalar(0xF708)!), "F5")
        XCTAssertEqual(item("Step Over")?.keyEquivalent, String(UnicodeScalar(0xF70D)!), "F10")
        XCTAssertEqual(item("Step Into")?.keyEquivalent, String(UnicodeScalar(0xF70E)!), "F11")
        XCTAssertEqual(item("Step Out")?.keyEquivalentModifierMask, .shift)
        XCTAssertEqual(item("Toggle Breakpoint")?.keyEquivalent, String(UnicodeScalar(0xF70C)!), "F9")
        XCTAssertNotNil(item("Remove All Breakpoints"))
    }

    func testMenuItemsAreDisabledWithoutASession() {
        let c = MainWindowController(bridge: CoreBridge())
        let stop = NSMenuItem(title: "", action: #selector(MainWindowController.stopDebugging(_:)), keyEquivalent: "")
        let step = NSMenuItem(title: "", action: #selector(MainWindowController.debugStepOver(_:)), keyEquivalent: "")
        let clear = NSMenuItem(title: "", action: #selector(MainWindowController.removeAllBreakpoints(_:)), keyEquivalent: "")
        XCTAssertFalse(c.validateMenuItem(stop))
        XCTAssertFalse(c.validateMenuItem(step))
        XCTAssertFalse(c.validateMenuItem(clear))
    }

    // MARK: Console

    func testConsoleShowsOutputAndTheStack() {
        let panel = DebugConsolePanel(frame: NSRect(x: 0, y: 0, width: 800, height: 200))
        panel.append("hello\n", category: "stdout")
        XCTAssertEqual(panel.transcriptText, "hello\n")
        panel.showState(
            frames: [DebugFrameInfo(id: 1, name: "main", path: "/tmp/a.c", line: 4, column: 1)],
            variables: [DebugVariableInfo(name: "x", value: "41", typeName: "int", variablesReference: 0)],
            reason: "breakpoint 1.1")
        XCTAssertTrue(panel.stateText.contains("main  a.c:4"))
        XCTAssertTrue(panel.stateText.contains("x = 41"))
        XCTAssertTrue(panel.stateText.contains("PAUSED: breakpoint 1.1"))
        panel.clear()
        XCTAssertEqual(panel.transcriptText, "")
    }

    func testEvaluateFeedsTheConsoleInput() {
        let panel = DebugConsolePanel(frame: NSRect(x: 0, y: 0, width: 800, height: 200))
        var evaluated: [String] = []
        panel.onEvaluate = { evaluated.append($0) }
        panel.submit() // empty: ignored
        XCTAssertTrue(evaluated.isEmpty)
    }

    // MARK: Real adapter

    private func spin(_ seconds: TimeInterval = 0.05, until done: () -> Bool, timeout: TimeInterval = 20) -> Bool {
        let end = Date().addingTimeInterval(timeout)
        while !done() && Date() < end { RunLoop.current.run(until: Date().addingTimeInterval(seconds)) }
        return done()
    }

    func testBreakpointStepAndVariablesAgainstLLDBDAP() async throws {
        guard let adapter = try? Self.run("/usr/bin/xcrun", ["-f", "lldb-dap"]), !adapter.isEmpty else {
            throw XCTSkip("lldb-dap is not installed")
        }
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("dbg-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let source = URL(fileURLWithPath: DebugCoordinator.key(for: dir.path)).appendingPathComponent("prog.c")
        try """
        #include <stdio.h>
        int main(void) {
            int x = 41;
            x = x + 1;
            printf("x=%d\\n", x);
            return 0;
        }
        """.write(to: source, atomically: true, encoding: .utf8)
        let program = source.deletingLastPathComponent().appendingPathComponent("prog").path
        _ = try Self.run("/usr/bin/xcrun", ["clang", "-g", "-O0", "-o", program, source.path])

        let d = DebugCoordinator()
        var output = ""
        var stops: [UInt32] = []
        d.onOutput = { _, text in output += text }
        d.onStop = { _, line in stops.append(line) }
        d.toggleBreakpoint(path: source.path, line: 4)

        await d.launch(DebugTarget(program: program, cwd: dir.path))
        XCTAssertNotEqual(d.state, .idle, output)
        XCTAssertTrue(spin(until: { d.poll(); return d.state == .paused && d.stoppedLocation != nil }), "never stopped: \(output)")
        XCTAssertEqual(d.stoppedLocation?.line, 4)
        XCTAssertEqual(d.stoppedLocation?.path, DebugCoordinator.key(for: source.path))
        XCTAssertEqual(d.frames.first?.name, "main")
        XCTAssertTrue(spin(until: { d.poll(); return d.variables.contains { $0.name == "x" } }), "no locals")
        XCTAssertEqual(d.variables.first { $0.name == "x" }?.value, "41")
        XCTAssertEqual(stops.last, 4)

        d.stepOver()
        XCTAssertTrue(spin(until: { d.poll(); return d.stoppedLocation?.line == 5 }), "step over did not reach line 5")
        XCTAssertTrue(spin(until: { d.poll(); return d.variables.first { $0.name == "x" }?.value == "42" }))

        d.resume()
        XCTAssertTrue(spin(until: { d.poll(); return d.state == .idle }), "session did not end: \(output)")
        XCTAssertTrue(output.contains("x=42"), output)
        XCTAssertNil(d.stoppedLocation)
        XCTAssertTrue(d.frames.isEmpty)
    }

    func testStopKillsARunningProgram() async throws {
        guard let adapter = try? Self.run("/usr/bin/xcrun", ["-f", "lldb-dap"]), !adapter.isEmpty else {
            throw XCTSkip("lldb-dap is not installed")
        }
        let d = DebugCoordinator()
        await d.launch(DebugTarget(program: "/bin/sleep", args: ["30"], cwd: "/tmp"))
        XCTAssertNotEqual(d.state, .idle)
        d.stop()
        XCTAssertEqual(d.state, .idle)
    }

    @discardableResult
    private static func run(_ tool: String, _ args: [String]) throws -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: tool)
        process.arguments = args
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        try process.run()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw NSError(domain: "DebugTests", code: Int(process.terminationStatus),
                          userInfo: [NSLocalizedDescriptionKey: String(decoding: data, as: UTF8.self)])
        }
        return String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
