import AppKit

/// Turns the ANSI escape codes in a program's output into styled text. Only
/// colors and bold are kept; cursor moves and other codes are dropped. A
/// code cut in two by a chunk boundary is held back until the rest arrives.
@MainActor
struct AnsiRenderer {
    static let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
    static let boldFont = NSFont.monospacedSystemFont(ofSize: 12, weight: .bold)

    /// The 16 basic colors, in SGR order: 30-37, then 90-97.
    static let palette: [NSColor] = [
        NSColor(white: 0.2, alpha: 1), .systemRed, .systemGreen, .systemYellow,
        .systemBlue, .systemPurple, .systemTeal, NSColor(white: 0.85, alpha: 1),
        .systemGray, .systemRed, .systemGreen, .systemYellow,
        .systemBlue, .systemPurple, .systemTeal, .white,
    ]

    /// Color `n` of the 256-color table.
    static func color256(_ n: Int) -> NSColor {
        switch n {
        case 0..<16:
            return palette[n]
        case 16..<232:
            let v = n - 16
            func level(_ c: Int) -> CGFloat { c == 0 ? 0 : CGFloat(55 + 40 * c) / 255 }
            return NSColor(srgbRed: level(v / 36), green: level((v / 6) % 6), blue: level(v % 6), alpha: 1)
        default:
            let gray = CGFloat(8 + 10 * (min(n, 255) - 232)) / 255
            return NSColor(white: gray, alpha: 1)
        }
    }

    /// An escape sequence that did not end in the last chunk.
    private var carry = ""
    private var bold = false
    private var foreground: NSColor?
    private var background: NSColor?

    var attributes: [NSAttributedString.Key: Any] {
        var out: [NSAttributedString.Key: Any] = [
            .font: bold ? Self.boldFont : Self.font,
            .foregroundColor: foreground ?? NSColor.labelColor,
        ]
        if let background { out[.backgroundColor] = background }
        return out
    }

    /// The styled text of `chunk`, continuing from the previous chunks.
    mutating func render(_ chunk: String) -> NSAttributedString {
        let scalars = Array((carry + chunk).unicodeScalars)
        carry = ""
        let result = NSMutableAttributedString()
        var plain = String.UnicodeScalarView()
        func flush(_ renderer: AnsiRenderer) {
            guard !plain.isEmpty else { return }
            result.append(NSAttributedString(string: String(plain), attributes: renderer.attributes))
            plain = String.UnicodeScalarView()
        }
        func keep(from index: Int) {
            let rest = String(String.UnicodeScalarView(scalars[index...]))
            // Garbage that never closes must not pile up.
            carry = rest.unicodeScalars.count > 256 ? "" : rest
        }
        var i = 0
        while i < scalars.count {
            let scalar = scalars[i]
            guard scalar == "\u{1B}" else {
                // Carriage returns only repaint a line; drop them.
                if scalar != "\r" { plain.append(scalar) }
                i += 1
                continue
            }
            guard i + 1 < scalars.count else { keep(from: i); break }
            switch scalars[i + 1] {
            case "[":
                var j = i + 2
                while j < scalars.count, !(0x40...0x7E).contains(scalars[j].value) { j += 1 }
                guard j < scalars.count else { keep(from: i); i = scalars.count; continue }
                if scalars[j] == "m" {
                    flush(self)
                    applySGR(String(String.UnicodeScalarView(scalars[(i + 2)..<j])))
                }
                i = j + 1
            case "]":
                // Operating system command (links, titles): ends at BEL or ESC \.
                var j = i + 2
                var end: Int?
                while j < scalars.count {
                    if scalars[j].value == 7 { end = j + 1; break }
                    if scalars[j] == "\u{1B}", j + 1 < scalars.count, scalars[j + 1] == "\\" { end = j + 2; break }
                    j += 1
                }
                guard let end else { keep(from: i); i = scalars.count; continue }
                i = end
            default:
                i += 2
            }
        }
        flush(self)
        return result
    }

    private mutating func applySGR(_ parameters: String) {
        var codes = parameters.split(separator: ";", omittingEmptySubsequences: false).map { Int($0) ?? 0 }
        if codes.isEmpty { codes = [0] }
        var k = 0
        while k < codes.count {
            let code = codes[k]
            switch code {
            case 0: bold = false; foreground = nil; background = nil
            case 1: bold = true
            case 22: bold = false
            case 30...37: foreground = Self.palette[code - 30]
            case 39: foreground = nil
            case 40...47: background = Self.palette[code - 40]
            case 49: background = nil
            case 90...97: foreground = Self.palette[code - 90 + 8]
            case 100...107: background = Self.palette[code - 100 + 8]
            case 38, 48:
                var color: NSColor?
                if k + 2 < codes.count, codes[k + 1] == 5 {
                    color = Self.color256(codes[k + 2])
                    k += 2
                } else if k + 4 < codes.count, codes[k + 1] == 2 {
                    func level(_ v: Int) -> CGFloat { CGFloat(min(max(v, 0), 255)) / 255 }
                    color = NSColor(srgbRed: level(codes[k + 2]), green: level(codes[k + 3]),
                                    blue: level(codes[k + 4]), alpha: 1)
                    k += 4
                }
                if code == 38 { foreground = color } else { background = color }
            default: break
            }
            k += 1
        }
    }
}

/// One run of one test: its command, what it printed, and how it ended.
@MainActor
final class TestRunRecord {
    /// Counts up from 0 in a window: "Test run #N".
    let id: Int
    let test: TestCase
    let startedAt: Date
    private(set) var finishedAt: Date?
    private(set) var exitCode: Int32?
    /// `.ready` once finished means it was stopped before it ended.
    fileprivate(set) var state: TestRunState = .running
    fileprivate(set) var wasStopped = false
    /// The output so far, styled.
    let output = NSMutableAttributedString()
    fileprivate var process: Process?

    private var renderer = AnsiRenderer()
    /// The start of a character that the next chunk completes.
    private var pendingBytes = Data()
    private var truncated = false

    /// Output past this many characters is dropped.
    static let maxOutputLength = 2_000_000

    init(id: Int, test: TestCase, startedAt: Date = .now) {
        self.id = id
        self.test = test
        self.startedAt = startedAt
        output.append(Self.note("$ \(test.command)\n"))
    }

    var isRunning: Bool { state == .running }

    var duration: TimeInterval? { finishedAt.map { $0.timeIntervalSince(startedAt) } }

    var durationText: String {
        guard let duration else { return "" }
        return duration < 10 ? String(format: "%.2fs", duration) : String(format: "%.1fs", duration)
    }

    private static func note(_ text: String) -> NSAttributedString {
        NSAttributedString(string: text, attributes: [
            .font: AnsiRenderer.font, .foregroundColor: NSColor.secondaryLabelColor,
        ])
    }

    /// Adds a chunk of the program's output. Returns the text that was added.
    func append(_ data: Data) -> NSAttributedString? {
        guard !truncated else { return nil }
        let bytes = pendingBytes + data
        pendingBytes = Data()
        var text = String(data: bytes, encoding: .utf8)
        if text == nil {
            // A character cut in half at the end of the chunk waits for the rest.
            for cut in 1...3 where bytes.count > cut {
                if let head = String(data: bytes.dropLast(cut), encoding: .utf8) {
                    pendingBytes = Data(bytes.suffix(cut))
                    text = head
                    break
                }
            }
        }
        return add(renderer.render(text ?? String(decoding: bytes, as: UTF8.self)))
    }

    private func add(_ fragment: NSAttributedString) -> NSAttributedString {
        if output.length + fragment.length > Self.maxOutputLength {
            truncated = true
            let notice = Self.note("\n[output truncated]\n")
            output.append(notice)
            return notice
        }
        output.append(fragment)
        return fragment
    }

    /// Records the end of the run. Returns the text it added to the output.
    fileprivate func finish(code: Int32) -> NSAttributedString {
        let tail = NSMutableAttributedString()
        if !pendingBytes.isEmpty, !truncated {
            tail.append(add(renderer.render(String(decoding: pendingBytes, as: UTF8.self))))
        }
        pendingBytes = Data()
        finishedAt = .now
        exitCode = code
        state = wasStopped ? .ready : (code == 0 ? .passed : .failed)
        let footer = wasStopped ? "\nStopped.\n" : (code == 0 ? "" : "\nExited with code \(code).\n")
        if !footer.isEmpty {
            let note = Self.note(footer)
            output.append(note)
            tail.append(note)
        }
        return tail
    }
}

/// Runs tests as child processes and keeps what they printed. Commands run
/// through the user's login shell, so `php`, `cargo` and `npx` are found as
/// they are in a terminal.
@MainActor
final class TestRunner {
    static let maxRecords = 50

    /// Oldest first.
    private(set) var records: [TestRunRecord] = []
    private var nextNumber = 0

    /// A run started (the record), or the list changed otherwise (`nil`).
    var onChange: ((TestRunRecord?) -> Void)?
    /// Text was added to a run's output.
    var onOutput: ((TestRunRecord, NSAttributedString) -> Void)?
    var onFinish: ((TestRunRecord) -> Void)?

    /// The shell that runs the commands; tests point it at `/bin/sh`.
    var shell: String = {
        let user = ProcessInfo.processInfo.environment["SHELL"]
        return user.flatMap { FileManager.default.isExecutableFile(atPath: $0) ? $0 : nil } ?? "/bin/sh"
    }()

    var hasRunning: Bool { records.contains(where: \.isRunning) }

    /// How the latest run of the test with `key` went.
    func state(for key: String) -> TestRunState? {
        records.last { $0.test.key == key }?.state
    }

    func isRunning(_ key: String) -> Bool { state(for: key) == .running }

    /// Starts `test`. `nil` if the process could not be launched.
    func start(_ test: TestCase) -> TestRunRecord? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: shell)
        process.arguments = ["-l", "-c", test.command]
        process.currentDirectoryURL = URL(fileURLWithPath: test.directory, isDirectory: true)
        var environment = ProcessInfo.processInfo.environment
        environment["TERM"] = "xterm-256color"
        environment["FORCE_COLOR"] = "1"
        environment["CLICOLOR_FORCE"] = "1"
        environment["CARGO_TERM_COLOR"] = "always"
        environment["NO_COLOR"] = nil
        process.environment = environment
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        process.standardInput = FileHandle.nullDevice
        do { try process.run() } catch { return nil }

        let record = TestRunRecord(id: nextNumber, test: test)
        nextNumber += 1
        record.process = process
        records.append(record)
        trim()
        let handle = pipe.fileHandleForReading
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            while true {
                let data = handle.availableData
                if data.isEmpty { break }
                DispatchQueue.main.async { MainActor.assumeIsolated { self?.received(data, for: record) } }
            }
            process.waitUntilExit()
            let code = process.terminationStatus
            DispatchQueue.main.async { MainActor.assumeIsolated { self?.finished(record, code: code) } }
        }
        onChange?(record)
        return record
    }

    private func received(_ data: Data, for record: TestRunRecord) {
        if let fragment = record.append(data) { onOutput?(record, fragment) }
    }

    private func finished(_ record: TestRunRecord, code: Int32) {
        let tail = record.finish(code: code)
        record.process = nil
        if tail.length > 0 { onOutput?(record, tail) }
        onChange?(nil)
        onFinish?(record)
    }

    /// Asks a run to end; a process that ignores that is killed after two seconds.
    func stop(_ record: TestRunRecord) {
        guard record.isRunning, let process = record.process, process.isRunning else { return }
        record.wasStopped = true
        // The shell's children (the test tool) would otherwise keep running.
        let children = Process()
        children.executableURL = URL(fileURLWithPath: "/usr/bin/pkill")
        children.arguments = ["-TERM", "-P", String(process.processIdentifier)]
        try? children.run()
        process.terminate()
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
            if process.isRunning { kill(process.processIdentifier, SIGKILL) }
        }
    }

    func stopAll() {
        for record in records where record.isRunning { stop(record) }
    }

    /// Removes the finished runs.
    func clear() {
        records.removeAll { !$0.isRunning }
        onChange?(nil)
    }

    private func trim() {
        while records.count > Self.maxRecords, let index = records.firstIndex(where: { !$0.isRunning }) {
            records.remove(at: index)
        }
    }
}
