import XCTest
@testable import Galaxy

/// The run icon in the gutter: which functions count as tests, the command
/// each one runs, and the passed or failed mark once it is done.
@MainActor
final class TestRunTests: XCTestCase {
    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-testrun-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    @discardableResult
    private func write(_ rel: String, _ text: String) throws -> String {
        let url = root.appendingPathComponent(rel)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try text.write(to: url, atomically: true, encoding: .utf8)
        return url.path
    }

    // MARK: Detection

    func testPHPUnitMethodsAreTestsWhenNamedOrMarked() throws {
        try write("artisan", "<?php\n")
        let path = try write("tests/Feature/ProfileTest.php", """
        <?php
        class ProfileTest extends TestCase
        {
            public function setUp(): void {}

            public function test_update_token(): void {}

            #[Test]
            public function updates_the_name(): void {}

            /**
             * @test
             */
            public function it_deletes(): void {}

            public function helper(): void {}

            #[DataProvider('cases')]
            public function test_with_data($value): void {}
        }
        """)
        let text = try String(contentsOfFile: path, encoding: .utf8)
        let tests = TestDetector.detect(path: path, text: text, root: root.path)
        XCTAssertEqual(tests.map(\.name), ["test_update_token", "updates_the_name", "it_deletes", "test_with_data"])
        XCTAssertEqual(tests.map(\.line), [5, 8, 13, 18])
        XCTAssertEqual(tests[0].directory, root.path)
        XCTAssertEqual(tests[0].command,
                       "php artisan test 'tests/Feature/ProfileTest.php' --filter='/::test_update_token( with data set .*)?$/'")
    }

    func testPlainPHPUnitProjectUsesTheVendorBinary() throws {
        try write("composer.json", "{}")
        let path = try write("tests/UserTest.php", "<?php\nfinal class UserTest {\n    public function testName() {}\n}\n")
        let tests = TestDetector.detect(path: path, text: try String(contentsOfFile: path, encoding: .utf8), root: root.path)
        XCTAssertEqual(tests.map(\.name), ["testName"])
        XCTAssertTrue(tests[0].command.hasPrefix("vendor/bin/phpunit 'tests/UserTest.php' --filter="), tests[0].command)
    }

    func testPestCallsAreTests() throws {
        try write("artisan", "<?php\n")
        try write("vendor/bin/pest", "")
        let path = try write("tests/Feature/LoginTest.php", """
        <?php
        it('logs in (with 2FA)', function () {});
        test("rejects a bad password", function () {});
        """)
        let tests = TestDetector.detect(path: path, text: try String(contentsOfFile: path, encoding: .utf8), root: root.path)
        XCTAssertEqual(tests.map(\.name), ["logs in (with 2FA)", "rejects a bad password"])
        XCTAssertTrue(tests[0].command.hasSuffix("--filter='logs in \\(with 2FA\\)'"), tests[0].command)
    }

    func testRustTestsNeedTheTestAttribute() throws {
        try write("Cargo.toml", "[package]\nname = \"x\"\n")
        let path = try write("src/lib.rs", """
        fn helper() {}

        #[cfg(test)]
        mod tests {
            #[test]
            fn adds_up() {}

            #[test]
            #[should_panic]
            // explained
            pub fn panics() {}

            #[tokio::test]
            async fn waits() {}

            fn not_a_test() {}
        }
        """)
        let tests = TestDetector.detect(path: path, text: try String(contentsOfFile: path, encoding: .utf8), root: root.path)
        XCTAssertEqual(tests.map(\.name), ["adds_up", "panics", "waits"])
        XCTAssertEqual(tests.map(\.line), [5, 10, 13])
        XCTAssertEqual(tests[0].command, "cargo test 'adds_up'")
        XCTAssertEqual(tests[0].directory, root.path)
    }

    func testGoTestsRunByExactName() throws {
        let path = try write("pkg/sum_test.go", """
        package pkg

        func TestSum(t *testing.T) {}
        func helper(t *testing.T) {}
        func TestSumMore(t *testing.T) {}
        func BenchmarkSum(b *testing.B) {}
        """)
        let tests = TestDetector.detect(path: path, text: try String(contentsOfFile: path, encoding: .utf8), root: root.path)
        XCTAssertEqual(tests.map(\.name), ["TestSum", "TestSumMore"])
        XCTAssertEqual(tests[0].command, "go test -v -run '^TestSum$' .")
        XCTAssertEqual(tests[0].directory, root.appendingPathComponent("pkg").path)
    }

    func testJestAndVitestCallsAreTestsWhenThePackageHasTheTool() throws {
        try write("package.json", #"{"devDependencies": {"vitest": "^1"}}"#)
        try write("pnpm-lock.yaml", "")
        let path = try write("src/math.test.ts", """
        describe('math', () => {
          it('adds', () => {})
          test.only("it's subtracting", () => {})
        })
        """)
        let tests = TestDetector.detect(path: path, text: try String(contentsOfFile: path, encoding: .utf8), root: root.path)
        XCTAssertEqual(tests.map(\.name), ["math", "adds", "it's subtracting"])
        XCTAssertEqual(tests[1].command, "pnpm exec vitest run 'src/math.test.ts' -t 'adds'")
        XCTAssertEqual(tests[2].command, "pnpm exec vitest run 'src/math.test.ts' -t 'it'\\''s subtracting'")

        try write("package.json", #"{"name": "no-test-tool"}"#)
        XCTAssertTrue(TestDetector.detect(path: path, text: try String(contentsOfFile: path, encoding: .utf8),
                                          root: root.path).isEmpty, "no jest or vitest, no icon")
    }

    func testOtherFilesHaveNoTests() throws {
        XCTAssertFalse(TestDetector.mayContainTests(path: "/p/app/User.php"))
        XCTAssertFalse(TestDetector.mayContainTests(path: "/p/resources/views/a.blade.php"))
        XCTAssertFalse(TestDetector.mayContainTests(path: "/p/src/index.ts"))
        XCTAssertFalse(TestDetector.mayContainTests(path: "/p/main.go"))
        XCTAssertTrue(TestDetector.mayContainTests(path: "/p/tests/Feature/A.php"))
        XCTAssertTrue(TestDetector.mayContainTests(path: "/p/src/lib.rs"))
        XCTAssertTrue(TestDetector.detect(path: "/p/app/User.php", text: "<?php\nfunction test_x() {}", root: nil).isEmpty)
    }

    // MARK: Gutter

    private func makeView(text: String = "a\nb\nc\n") throws -> EditorView {
        let url = root.appendingPathComponent("view.txt")
        try text.write(to: url, atomically: true, encoding: .utf8)
        let view = EditorView(frame: NSRect(x: 0, y: 0, width: 600, height: 200))
        view.document = try CoreBridge().openDocument(path: url.path)
        return view
    }

    private func click(_ view: EditorView, x: CGFloat, line: Int) throws {
        let point = NSPoint(x: x, y: view.metrics.lineHeight * (CGFloat(line) + 0.5))
        let event = try XCTUnwrap(NSEvent.mouseEvent(
            with: .leftMouseDown, location: view.convert(point, to: nil), modifierFlags: [], timestamp: 0,
            windowNumber: 0, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        view.mouseDown(with: event)
    }

    func testClickingARunIconRunsTheTestAndNotABreakpoint() throws {
        let view = try makeView()
        var runs: [UInt64] = []
        var breakpoints: [UInt64] = []
        view.onRunTest = { runs.append($0) }
        view.onToggleBreakpoint = { breakpoints.append($0) }
        view.runMarkers = [1: .ready]

        try click(view, x: EditorView.runMarkerWidth / 2, line: 1)
        XCTAssertEqual(runs, [1])
        XCTAssertTrue(breakpoints.isEmpty)

        try click(view, x: EditorView.runMarkerWidth / 2, line: 0)
        XCTAssertEqual(runs, [1], "a line without a test has no icon to click")
        XCTAssertEqual(breakpoints, [0], "it is a breakpoint click, as before")

        try click(view, x: EditorView.gutterPadding + 14, line: 1)
        XCTAssertEqual(runs, [1], "the line numbers beside the icon still set a breakpoint")
        XCTAssertEqual(breakpoints, [0, 1])
    }

    /// The icon is drawn in the gutter's left padding, in the color of its state.
    func testRunIconIsGreenAndTurnsRedWhenTheTestFails() throws {
        let view = try makeView()
        func dominantColor(_ state: TestRunState) throws -> String {
            view.runMarkers = [1: state]
            view.layoutSubtreeIfNeeded()
            let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
            view.cacheDisplay(in: view.bounds, to: rep)
            let scale = CGFloat(rep.pixelsWide) / view.bounds.width
            var green = 0, red = 0
            for px in 0..<Int(EditorView.runMarkerWidth * scale) {
                for py in Int(view.metrics.lineHeight * scale)..<Int(view.metrics.lineHeight * 2 * scale) {
                    guard let c = rep.colorAt(x: px, y: py)?.usingColorSpace(.sRGB) else { continue }
                    if c.greenComponent > 0.45, c.greenComponent > c.redComponent + 0.25 { green += 1 }
                    if c.redComponent > 0.6, c.redComponent > c.greenComponent + 0.3 { red += 1 }
                }
            }
            return green > red ? "green" : (red > green ? "red" : "none")
        }
        XCTAssertEqual(try dominantColor(.ready), "green")
        XCTAssertEqual(try dominantColor(.passed), "green")
        XCTAssertEqual(try dominantColor(.failed), "red")
        view.runMarkers = [:]
        XCTAssertEqual(try dominantColor(.ready), "green")
        view.runMarkers = [:]
        view.display()
        let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: rep)
        let scale = CGFloat(rep.pixelsWide) / view.bounds.width
        let c = try XCTUnwrap(rep.colorAt(x: Int(5 * scale), y: Int(view.metrics.lineHeight * 1.5 * scale))?.usingColorSpace(.sRGB))
        XCTAssertFalse(c.greenComponent > c.redComponent + 0.25, "no icon without a marker")
    }

    // MARK: Runner

    private func testCase(_ name: String, command: String, runner: String = "Cargo") -> TestCase {
        TestCase(line: 0, name: name, command: command, directory: root.path, key: "k::" + name, runner: runner)
    }

    private func makeRunner() -> TestRunner {
        let runner = TestRunner()
        runner.shell = "/bin/sh"
        return runner
    }

    private func wait(until condition: () -> Bool, timeout: TimeInterval = 20) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
    }

    func testRunnerReportsExitStatusAndKeepsOutput() throws {
        let runner = makeRunner()
        var finished: [String] = []
        runner.onFinish = { finished.append($0.test.name) }
        let passing = testCase("ok", command: "echo 'it fine'")
        let failing = testCase("bad", command: "echo broken; echo oops >&2; exit 3")
        let passRecord = try XCTUnwrap(runner.start(passing))
        let failRecord = try XCTUnwrap(runner.start(failing))
        XCTAssertEqual(runner.state(for: passing.key), .running)
        XCTAssertTrue(runner.hasRunning)
        wait(until: { !runner.hasRunning })

        XCTAssertEqual(runner.state(for: passing.key), .passed)
        XCTAssertEqual(runner.state(for: failing.key), .failed)
        XCTAssertEqual(passRecord.exitCode, 0)
        XCTAssertEqual(failRecord.exitCode, 3)
        XCTAssertTrue(passRecord.output.string.contains("it fine"))
        XCTAssertTrue(failRecord.output.string.contains("broken"))
        XCTAssertTrue(failRecord.output.string.contains("oops"), "stderr is shown too")
        XCTAssertTrue(failRecord.output.string.contains("Exited with code 3"))
        XCTAssertTrue(passRecord.output.string.hasPrefix("$ echo 'it fine'"), "the command comes first")
        XCTAssertEqual(Set(finished), ["ok", "bad"])
        XCTAssertNotNil(passRecord.duration)
        XCTAssertEqual([passRecord.id, failRecord.id], [0, 1], "runs are numbered in order")
    }

    func testRunnerRunsInTheTestsFolder() throws {
        let runner = makeRunner()
        let record = try XCTUnwrap(runner.start(testCase("pwd", command: "pwd -P")))
        wait(until: { !runner.hasRunning })
        let resolved = root.resolvingSymlinksInPath().path
        XCTAssertTrue(record.output.string.contains(resolved), record.output.string)
    }

    func testRunnerKeepsTheLatestResultPerTest() throws {
        let runner = makeRunner()
        let flaky = testCase("flaky", command: "test -f \(TestDetector.shellQuote(root.path + "/flag"))")
        _ = try XCTUnwrap(runner.start(flaky))
        wait(until: { !runner.hasRunning })
        XCTAssertEqual(runner.state(for: flaky.key), .failed)
        try write("flag", "")
        _ = try XCTUnwrap(runner.start(flaky))
        wait(until: { !runner.hasRunning })
        XCTAssertEqual(runner.state(for: flaky.key), .passed, "the newer run decides")
        XCTAssertEqual(runner.records.count, 2, "the older run stays in the list")
    }

    func testStoppingARunEndsItAsStoppedNotFailed() throws {
        let runner = makeRunner()
        let slow = testCase("slow", command: "sleep 30")
        let record = try XCTUnwrap(runner.start(slow))
        XCTAssertTrue(runner.isRunning(slow.key))
        runner.stop(record)
        wait(until: { !runner.hasRunning }, timeout: 10)
        XCTAssertFalse(record.isRunning)
        XCTAssertEqual(record.state, .ready, "stopped is not a failure")
        XCTAssertTrue(record.output.string.contains("Stopped."))
    }

    func testClearKeepsRunningRuns() throws {
        let runner = makeRunner()
        _ = try XCTUnwrap(runner.start(testCase("done", command: "true")))
        wait(until: { !runner.hasRunning })
        let slow = try XCTUnwrap(runner.start(testCase("slow", command: "sleep 30")))
        runner.clear()
        XCTAssertEqual(runner.records.map(\.test.name), ["slow"])
        runner.stop(slow)
        wait(until: { !runner.hasRunning }, timeout: 10)
    }

    func testOutputKeepsColorsAndDropsOtherEscapes() throws {
        let runner = makeRunner()
        let record = try XCTUnwrap(runner.start(testCase(
            "colors", command: "printf '\\033[31mred\\033[0m plain \\033[1;32mgo\\033[0m\\033[2K\\r\\n'")))
        wait(until: { !runner.hasRunning })
        let text = record.output.string
        XCTAssertTrue(text.contains("red plain go"), text)
        XCTAssertFalse(text.contains("\u{1B}"), "escape codes are not shown")
        // The command line at the top also contains "red"; look at the result.
        let index = (text as NSString).range(of: "red plain go").location
        let color = record.output.attribute(.foregroundColor, at: index, effectiveRange: nil) as? NSColor
        XCTAssertEqual(color, NSColor.systemRed)
        let plain = index + 4
        let plainColor = record.output.attribute(.foregroundColor, at: plain, effectiveRange: nil) as? NSColor
        XCTAssertEqual(plainColor, NSColor.labelColor, "reset returns to the normal color")
    }

    func testAnsiCodeSplitAcrossChunksStillApplies() {
        var renderer = AnsiRenderer()
        let first = renderer.render("a\u{1B}[3")
        let second = renderer.render("1mb\u{1B}[0mc")
        XCTAssertEqual(first.string, "a")
        XCTAssertEqual(second.string, "bc")
        let color = second.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor
        XCTAssertEqual(color, NSColor.systemRed)
    }

    func testAnsiHandlesExtendedColorsAndLinks() {
        var renderer = AnsiRenderer()
        let out = renderer.render("\u{1B}[38;5;196mx\u{1B}[38;2;1;2;3my\u{1B}]8;;https://a.b\u{07}link\u{1B}]8;;\u{07}")
        XCTAssertEqual(out.string, "xylink")
    }

    func testOutputPastTheLimitIsCut() throws {
        let record = TestRunRecord(id: 0, test: testCase("big", command: "true"))
        let chunk = String(repeating: "x", count: 500_000)
        for _ in 0..<5 { _ = record.append(Data(chunk.utf8)) }
        XCTAssertLessThan(record.output.length, TestRunRecord.maxOutputLength + 100)
        XCTAssertTrue(record.output.string.contains("[output truncated]"))
    }

    // MARK: Panel

    func testPanelListsRunsByToolNewestFirstAndShowsTheSelectedOutput() throws {
        let runner = makeRunner()
        let panel = TestResultsPanel(frame: NSRect(x: 0, y: 0, width: 900, height: 300))
        panel.bind(to: runner)
        runner.onChange = { panel.reload(selecting: $0) }
        runner.onOutput = { panel.append($1, to: $0) }
        let first = try XCTUnwrap(runner.start(testCase("one", command: "echo first-output", runner: "Cargo")))
        wait(until: { !runner.hasRunning })
        let other = try XCTUnwrap(runner.start(testCase("two", command: "echo second-output", runner: "Go")))
        wait(until: { !runner.hasRunning })
        let third = try XCTUnwrap(runner.start(testCase("three", command: "echo third-output", runner: "Go")))
        wait(until: { !runner.hasRunning })

        XCTAssertEqual(panel.rows, [.group("Go"), .run(third.id), .run(other.id), .group("Cargo"), .run(first.id)])
        XCTAssertTrue(panel.outputText.contains("third-output"), "the newest run is shown")
        XCTAssertTrue(panel.selectedRecord === third)
        XCTAssertFalse(panel.outputText.contains("first-output"))
    }

    func testPanelStreamsOutputOfTheShownRun() throws {
        let runner = makeRunner()
        let panel = TestResultsPanel(frame: NSRect(x: 0, y: 0, width: 900, height: 300))
        panel.bind(to: runner)
        runner.onChange = { panel.reload(selecting: $0) }
        runner.onOutput = { panel.append($1, to: $0) }
        _ = try XCTUnwrap(runner.start(testCase("stream", command: "echo one; sleep 0.3; echo two")))
        wait(until: { panel.outputText.contains("one") })
        XCTAssertTrue(runner.hasRunning, "the first line arrives before the run ends")
        wait(until: { !runner.hasRunning })
        XCTAssertTrue(panel.outputText.contains("two"))
    }

    func testPanelShowsEmptyHintAfterClear() throws {
        let runner = makeRunner()
        let panel = TestResultsPanel(frame: NSRect(x: 0, y: 0, width: 900, height: 300))
        panel.bind(to: runner)
        runner.onChange = { panel.reload(selecting: $0) }
        _ = try XCTUnwrap(runner.start(testCase("gone", command: "true")))
        wait(until: { !runner.hasRunning })
        XCTAssertFalse(panel.rows.isEmpty)
        runner.clear()
        XCTAssertTrue(panel.rows.isEmpty)
        XCTAssertNil(panel.selectedRecord)
        XCTAssertEqual(panel.outputText, "")
    }

    func testDetectedTestsNameTheirTool() throws {
        try write("artisan", "")
        let php = TestDetector.detect(path: try write("tests/FooTest.php", "<?php\nclass FooTest {\n public function test_a() {}\n}\n"),
                                      text: "<?php\nclass FooTest {\n public function test_a() {}\n}\n", root: root.path)
        XCTAssertEqual(php.first?.runner, "Laravel")
        let rust = TestDetector.detect(path: try write("src/a.rs", "#[test]\nfn t() {}\n"),
                                       text: "#[test]\nfn t() {}\n", root: root.path)
        XCTAssertEqual(rust.first?.runner, "Cargo")
        let go = TestDetector.detect(path: try write("a_test.go", "func TestX(t *testing.T) {}\n"),
                                     text: "func TestX(t *testing.T) {}\n", root: root.path)
        XCTAssertEqual(go.first?.runner, "Go")
    }

    // MARK: Whole flow

    private func makeController() -> MainWindowController {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        c.askSaveChoice = { _ in .discard }
        return c
    }

    func testOpeningATestFileShowsIconsAndClickingRunsInTheTerminal() throws {
        // No Cargo.toml: `cargo test` ends with an error at once, whatever the machine.
        try write("src/lib.rs", "#[test]\nfn first() {}\n\nfn plain() {}\n\n#[test]\nfn second() {}\n")
        try write("src/main.rs", "fn main() {}\n")
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openWorkspace(at: root)
        try c.openFile(at: root.appendingPathComponent("src/lib.rs"))

        XCTAssertEqual(c.detectedTests.map(\.name), ["first", "second"])
        XCTAssertEqual(c.editorView.runMarkers, [1: .ready, 6: .ready])

        try c.openFile(at: root.appendingPathComponent("src/main.rs"))
        XCTAssertTrue(c.editorView.runMarkers.isEmpty, "a file without tests has no icons")
        c.selectTab(0)
        XCTAssertEqual(c.editorView.runMarkers.count, 2)

        c.runTest(atLine: 1)
        XCTAssertEqual(c.editorView.runMarkers[1], .running)
        XCTAssertEqual(c.editorView.runMarkers[6], .ready, "only that test runs")
        XCTAssertEqual(c.bottomPanelTab, .testResults, "the result opens in the Test Results tab")
        XCTAssertFalse(c.testResultsPanel.isHidden)
        XCTAssertTrue(c.terminals.isEmpty, "no terminal is used")

        let deadline = Date().addingTimeInterval(30)
        while c.editorView.runMarkers[1] == .running, Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        }
        XCTAssertEqual(c.editorView.runMarkers[1], .failed, "cargo found no Cargo.toml, so the run failed")
        XCTAssertEqual(c.editorView.runMarkers[6], .ready)
        XCTAssertTrue(c.testResultsPanel.outputText.contains("cargo test"), c.testResultsPanel.outputText)
        XCTAssertEqual(c.testResultsPanel.rows.count, 2, "a tool group and the run")
    }

    func testIconsFollowEditsToTheFile() throws {
        try write("src/lib.rs", "#[test]\nfn first() {}\n")
        let c = makeController()
        defer { c.window?.orderOut(nil) }
        try c.openWorkspace(at: root)
        try c.openFile(at: root.appendingPathComponent("src/lib.rs"))
        XCTAssertEqual(c.editorView.runMarkers.keys.sorted(), [1])

        c.editorView.document?.goToLine(line: 0, col: 0)
        c.editorView.insertText("// header\n", replacementRange: NSRange(location: NSNotFound, length: 0))
        c.editorView.flushPendingUpdates()
        let moved = expectation(description: "icon moves down with the code")
        DispatchQueue.main.asyncAfter(deadline: .now() + MainWindowController.testScanDelay + 0.3) {
            XCTAssertEqual(c.editorView.runMarkers.keys.sorted(), [2])
            moved.fulfill()
        }
        wait(for: [moved], timeout: 3)
    }
}
