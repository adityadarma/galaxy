import AppKit

extension MainWindowController {
    /// How long after the last edit the file is scanned for tests again.
    static let testScanDelay: TimeInterval = 0.4

    /// Scans the shown file for tests and puts a run icon on each.
    func refreshTestMarkers() {
        testScanItem?.cancel()
        guard let tab = activeTab, tab.isDocument, !isDiffPaneShown,
              TestDetector.mayContainTests(path: tab.doc.path()),
              tab.doc.lenBytes() <= TestDetector.maxBytes else {
            detectedTests = []
            editorView.runMarkers = [:]
            return
        }
        detectedTests = TestDetector.detect(path: tab.doc.path(), text: tab.doc.text(), root: workspace?.root())
        applyTestMarkers()
    }

    /// Shows the state of each detected test, without scanning again.
    func applyTestMarkers() {
        var markers: [UInt64: TestRunState] = [:]
        for test in detectedTests {
            markers[UInt64(test.line)] = testRunner.state(for: test.key) ?? .ready
        }
        editorView.runMarkers = markers
    }

    /// Scans after typing pauses, so each keystroke does not read the file.
    func scheduleTestMarkers() {
        guard activeTab.map({ TestDetector.mayContainTests(path: $0.doc.path()) }) == true || !detectedTests.isEmpty else {
            return
        }
        testScanItem?.cancel()
        let item = DispatchWorkItem { [weak self] in
            onAppKitMainThread { self?.refreshTestMarkers() }
        }
        testScanItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.testScanDelay, execute: item)
    }

    /// Connects the runner to the Test Results panel and the gutter icons.
    func wireTestResults() {
        testResultsPanel.bind(to: testRunner)
        testResultsPanel.onStop = { [weak self] record in self?.testRunner.stop(record) }
        testResultsPanel.onClear = { [weak self] in self?.testRunner.clear() }
        testRunner.onChange = { [weak self] record in
            guard let self else { return }
            self.testResultsPanel.reload(selecting: record)
            self.applyTestMarkers()
        }
        testRunner.onOutput = { [weak self] record, fragment in
            self?.testResultsPanel.append(fragment, to: record)
        }
        testRunner.onFinish = { [weak self] record in
            let verb = switch record.state {
            case .passed: "Passed"
            case .failed: "Failed"
            default: "Stopped"
            }
            self?.statusLabel.stringValue = "\(verb): \(record.test.name)"
        }
    }

    /// Runs the test declared on `line` and shows its output in the Test
    /// Results tab. Unsaved edits are saved first: the test tool reads the
    /// file from disk.
    func runTest(atLine line: UInt64) {
        guard let test = detectedTests.first(where: { UInt64($0.line) == line }) else { return NSSound.beep() }
        if testRunner.isRunning(test.key) { return }
        if let tab = activeTab, tab.doc.isModified(), !save() { return }
        guard testRunner.start(test) != nil else {
            statusLabel.stringValue = "Could not start \(test.name)."
            return NSSound.beep()
        }
        revealBottomPanel(.testResults)
        statusLabel.stringValue = "Running: \(test.name)"
    }
}
