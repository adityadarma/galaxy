import Foundation

/// A test found in a source file: where its gutter icon goes and the shell
/// command that runs it.
struct TestCase: Equatable {
    /// 0-based line of the declaration, where the gutter icon sits.
    var line: Int
    var name: String
    /// Shell command, run in `directory`.
    var command: String
    var directory: String
    /// Identifies the test across edits: its file and name.
    var key: String
    /// Name of the test tool, which groups results in the Test Results panel.
    var runner = "Tests"
}

/// What the gutter icon of a test shows.
enum TestRunState: Equatable {
    case ready
    case running
    case passed
    case failed
}

/// Finds the tests of PHPUnit and Pest, Rust, Go, and Jest or Vitest in a
/// file, and the command for each. Text scanning only: no test is compiled
/// or run here.
enum TestDetector {
    /// Larger files are not scanned.
    static let maxBytes: UInt64 = 400_000

    private enum Language {
        case php
        case rust
        case go
        case script
    }

    private static func language(for path: String) -> Language? {
        let name = (path as NSString).lastPathComponent.lowercased()
        switch (name as NSString).pathExtension {
        case "php":
            if name.hasSuffix(".blade.php") { return nil }
            return name.hasSuffix("test.php") || path.lowercased().contains("/tests/") ? .php : nil
        case "rs":
            return .rust
        case "go":
            return name.hasSuffix("_test.go") ? .go : nil
        case "js", "jsx", "ts", "tsx", "mjs", "cjs", "mts", "cts":
            let stem = (name as NSString).deletingPathExtension
            return stem.hasSuffix(".test") || stem.hasSuffix(".spec") ? .script : nil
        default:
            return nil
        }
    }

    /// A cheap check on the path alone, before the text is read.
    static func mayContainTests(path: String) -> Bool { language(for: path) != nil }

    /// The tests in `text`, the content of the file at `path`. `root` is the
    /// workspace folder, used to find the project and its test tool.
    static func detect(path: String, text: String, root: String?) -> [TestCase] {
        guard let language = language(for: path) else { return [] }
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        switch language {
        case .php: return phpTests(path: path, lines: lines, root: root)
        case .rust: return rustTests(path: path, lines: lines, root: root)
        case .go: return goTests(path: path, lines: lines)
        case .script: return scriptTests(path: path, lines: lines, root: root)
        }
    }

    // MARK: Shell and paths

    /// `text` as one shell word.
    static func shellQuote(_ text: String) -> String {
        "'" + text.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    /// `text` with the characters a regular expression reads specially escaped.
    static func regexEscape(_ text: String) -> String {
        var out = ""
        for character in text {
            if "\\^$.*+?()[]{}|/".contains(character) { out.append("\\") }
            out.append(character)
        }
        return out
    }

    private static func exists(_ directory: String, _ name: String) -> Bool {
        FileManager.default.fileExists(atPath: (directory as NSString).appendingPathComponent(name))
    }

    /// The closest folder above the file that holds one of `markers`, no
    /// higher than `root`. Falls back to `root`, or the file's own folder.
    private static func projectDirectory(of path: String, markers: [String], root: String?) -> String {
        let fileDirectory = (path as NSString).deletingLastPathComponent
        var directory = fileDirectory
        while true {
            if markers.contains(where: { exists(directory, $0) }) { return directory }
            if directory == root || directory == "/" || directory.isEmpty { break }
            let parent = (directory as NSString).deletingLastPathComponent
            if parent == directory { break }
            directory = parent
        }
        return root ?? fileDirectory
    }

    private static func relative(_ path: String, to directory: String) -> String {
        path.hasPrefix(directory + "/") ? String(path.dropFirst(directory.count + 1)) : path
    }

    private static func captures(_ regex: NSRegularExpression, in line: String) -> [String]? {
        let range = NSRange(line.startIndex..., in: line)
        guard let match = regex.firstMatch(in: line, range: range) else { return nil }
        return (1..<match.numberOfRanges).map { index in
            Range(match.range(at: index), in: line).map { String(line[$0]) } ?? ""
        }
    }

    // MARK: PHP

    private static let phpMethod = try! NSRegularExpression(
        pattern: #"^\s*(?:public\s+)?(?:static\s+)?function\s+(\w+)\s*\("#)
    private static let pestCall = try! NSRegularExpression(
        pattern: #"^\s*(?:it|test)\(\s*(['"])((?:(?!\1).)+)\1"#)
    private static let phpAnnotation = try! NSRegularExpression(
        pattern: #"#\[(?:[\w\\]*\\)?test\s*[\],(]|@test\b"#)

    /// `#[Test]` or `@test` in the attributes and doc comment above line `index`.
    private static func hasTestAnnotation(above index: Int, in lines: [String]) -> Bool {
        var i = index - 1
        while i >= 0 {
            let trimmed = lines[i].trimmingCharacters(in: .whitespaces)
            guard trimmed.hasPrefix("#[") || trimmed.hasPrefix("*") || trimmed.hasPrefix("/*")
                    || trimmed.hasPrefix("//") else { return false }
            let lower = trimmed.lowercased()
            if phpAnnotation.firstMatch(in: lower, range: NSRange(lower.startIndex..., in: lower)) != nil {
                return true
            }
            i -= 1
        }
        return false
    }

    private static func phpTests(path: String, lines: [String], root: String?) -> [TestCase] {
        let directory = projectDirectory(of: path, markers: ["artisan", "composer.json"], root: root)
        let file = shellQuote(relative(path, to: directory))
        let runner: String
        if exists(directory, "artisan") {
            runner = "php artisan test \(file)"
        } else if exists(directory, "vendor/bin/pest") {
            runner = "vendor/bin/pest \(file)"
        } else {
            runner = "vendor/bin/phpunit \(file)"
        }
        let runnerName = exists(directory, "artisan") ? "Laravel"
            : exists(directory, "vendor/bin/pest") ? "Pest" : "PHPUnit"
        var tests: [TestCase] = []
        for (index, line) in lines.enumerated() {
            if let groups = captures(phpMethod, in: line) {
                let name = groups[0]
                guard name.hasPrefix("test") || hasTestAnnotation(above: index, in: lines) else { continue }
                // Anchored, so `test_a` does not also run `test_a_more`; data sets add a suffix.
                let filter = shellQuote("/::\(name)( with data set .*)?$/")
                tests.append(TestCase(line: index, name: name, command: "\(runner) --filter=\(filter)",
                                      directory: directory, key: path + "::" + name, runner: runnerName))
            } else if let groups = captures(pestCall, in: line) {
                let description = groups[1]
                let filter = shellQuote(regexEscape(description))
                tests.append(TestCase(line: index, name: description, command: "\(runner) --filter=\(filter)",
                                      directory: directory, key: path + "::" + description, runner: runnerName))
            }
        }
        return tests
    }

    // MARK: Rust

    private static let rustFunction = try! NSRegularExpression(
        pattern: #"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)"#)

    private static func rustTests(path: String, lines: [String], root: String?) -> [TestCase] {
        var tests: [TestCase] = []
        var pending = false
        let directory = projectDirectory(of: path, markers: ["Cargo.toml"], root: root)
        for (index, line) in lines.enumerated() {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("#[") {
                if trimmed == "#[test]" || trimmed.hasPrefix("#[test(") || trimmed.hasSuffix("::test]")
                    || trimmed.contains("::test(") || trimmed.hasPrefix("#[rstest") {
                    pending = true
                }
                continue
            }
            guard pending else { continue }
            if trimmed.isEmpty || trimmed.hasPrefix("//") { continue }
            pending = false
            if let groups = captures(rustFunction, in: line) {
                let name = groups[0]
                tests.append(TestCase(line: index, name: name, command: "cargo test \(shellQuote(name))",
                                      directory: directory, key: path + "::" + name, runner: "Cargo"))
            }
        }
        return tests
    }

    // MARK: Go

    private static let goTest = try! NSRegularExpression(
        pattern: #"^func\s+(Test\w*)\s*\(\s*\w+\s+\*testing\.T\s*\)"#)

    private static func goTests(path: String, lines: [String]) -> [TestCase] {
        let directory = (path as NSString).deletingLastPathComponent
        var tests: [TestCase] = []
        for (index, line) in lines.enumerated() {
            guard let groups = captures(goTest, in: line) else { continue }
            let name = groups[0]
            tests.append(TestCase(line: index, name: name,
                                  command: "go test -v -run \(shellQuote("^\(name)$")) .",
                                  directory: directory, key: path + "::" + name, runner: "Go"))
        }
        return tests
    }

    // MARK: Jest and Vitest

    private static let scriptCall = try! NSRegularExpression(
        pattern: #"^\s*(?:it|test|describe)(?:\.(?:only|concurrent))?\(\s*(['"`])((?:(?!\1).)+)\1"#)

    private static func scriptTests(path: String, lines: [String], root: String?) -> [TestCase] {
        let directory = projectDirectory(of: path, markers: ["package.json"], root: root)
        guard let data = FileManager.default.contents(atPath: (directory as NSString).appendingPathComponent("package.json")),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return [] }
        let packages = Set(((json["dependencies"] as? [String: Any]) ?? [:]).keys)
            .union(((json["devDependencies"] as? [String: Any]) ?? [:]).keys)
        let tool: String
        let runnerName: String
        if packages.contains("vitest") {
            tool = "vitest run"
            runnerName = "Vitest"
        } else if packages.contains("jest") {
            tool = "jest"
            runnerName = "Jest"
        } else {
            return []
        }
        let launcher = exists(directory, "pnpm-lock.yaml") ? "pnpm exec"
            : exists(directory, "yarn.lock") ? "yarn"
            : exists(directory, "bun.lock") || exists(directory, "bun.lockb") ? "bunx" : "npx"
        let file = shellQuote(relative(path, to: directory))
        var tests: [TestCase] = []
        for (index, line) in lines.enumerated() {
            guard let groups = captures(scriptCall, in: line) else { continue }
            let name = groups[1]
            tests.append(TestCase(line: index, name: name,
                                  command: "\(launcher) \(tool) \(file) -t \(shellQuote(regexEscape(name)))",
                                  directory: directory, key: path + "::" + name, runner: runnerName))
        }
        return tests
    }
}
