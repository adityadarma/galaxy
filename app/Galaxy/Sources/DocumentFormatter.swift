import Foundation

/// Runs a file's formatter for `editor.formatOnSave`. A project's own tool
/// (`node_modules/.bin`, `vendor/bin`) wins over one on the login shell's
/// PATH; an app started from Finder does not inherit that PATH.
enum DocumentFormatter {
    struct Command: Equatable, Sendable {
        var name: String
        var executable: URL
        var arguments: [String]
        /// Tools that cannot read stdin format a temporary copy of the file.
        var formatsFile = false
        var directory: URL
    }

    enum Failure: LocalizedError, Equatable {
        case failed(String, String)
        case timedOut(String)

        var errorDescription: String? {
            switch self {
            case .failed(let name, let message): message.isEmpty ? "\(name) failed" : "\(name): \(message)"
            case .timedOut(let name): "\(name) took too long"
            }
        }
    }

    static let prettierExtensions: Set<String> = [
        "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "vue", "css", "scss", "less",
        "json", "jsonc", "html", "md", "markdown", "yaml", "yml", "graphql",
    ]

    /// The formatter for `path`, or nil when none is installed for its language.
    /// `find` returns the first executable named `name` in `localBins`, then on PATH.
    static func command(
        for path: String,
        root: String?,
        find: (_ name: String, _ localBins: [URL]) -> URL? = executable
    ) -> Command? {
        let file = URL(fileURLWithPath: path)
        let directory = file.deletingLastPathComponent()
        let folders = ancestors(of: directory, upTo: root)
        let ext = file.pathExtension.lowercased()
        switch ext {
        case "rs":
            guard let tool = find("rustfmt", []) else { return nil }
            return Command(name: "rustfmt", executable: tool,
                           arguments: ["--emit", "stdout", "--edition", rustEdition(in: folders)], directory: directory)
        case "go":
            guard let tool = find("gofmt", []) else { return nil }
            return Command(name: "gofmt", executable: tool, arguments: [], directory: directory)
        case "dart":
            guard let tool = find("dart", []) else { return nil }
            return Command(name: "dart format", executable: tool,
                           arguments: ["format", "--stdin-name", path], directory: directory)
        case "php":
            let bins = folders.map { $0.appendingPathComponent("vendor/bin") }
            let project = folders.last { FileManager.default.fileExists(atPath: $0.appendingPathComponent("composer.json").path) }
                ?? directory
            if let tool = find("pint", bins) {
                var arguments: [String] = []
                let config = project.appendingPathComponent("pint.json")
                if FileManager.default.fileExists(atPath: config.path) { arguments += ["--config", config.path] }
                return Command(name: "pint", executable: tool, arguments: arguments, formatsFile: true, directory: project)
            }
            if let tool = find("php-cs-fixer", bins) {
                var arguments = ["fix", "--using-cache=no", "--quiet"]
                if let config = [".php-cs-fixer.php", ".php-cs-fixer.dist.php"]
                    .map({ project.appendingPathComponent($0) })
                    .first(where: { FileManager.default.fileExists(atPath: $0.path) }) {
                    arguments.append("--config=\(config.path)")
                }
                return Command(name: "php-cs-fixer", executable: tool, arguments: arguments,
                               formatsFile: true, directory: project)
            }
            return nil
        case _ where prettierExtensions.contains(ext):
            guard let tool = find("prettier", folders.map { $0.appendingPathComponent("node_modules/.bin") }) else {
                return nil
            }
            return Command(name: "prettier", executable: tool, arguments: ["--stdin-filepath", path], directory: directory)
        default:
            return nil
        }
    }

    /// `directory` and its parents, nearest first, stopping at `root` when
    /// `directory` is inside it.
    static func ancestors(of directory: URL, upTo root: String?) -> [URL] {
        let stop = root.map { URL(fileURLWithPath: $0).standardizedFileURL.path }
        var folders: [URL] = []
        var current = directory.standardizedFileURL
        while true {
            folders.append(current)
            let parent = current.deletingLastPathComponent()
            if current.path == stop || parent.path == current.path { break }
            current = parent
        }
        return folders
    }

    /// The nearest `edition = "…"` in a Cargo.toml; workspace members that
    /// inherit their edition find it in the workspace's file.
    static func rustEdition(in folders: [URL]) -> String {
        let pattern = /(?m)^\s*edition\s*=\s*"(\d{4})"/
        for folder in folders {
            guard let text = try? String(contentsOf: folder.appendingPathComponent("Cargo.toml"), encoding: .utf8),
                  let match = text.firstMatch(of: pattern) else { continue }
            return String(match.1)
        }
        return "2021"
    }

    /// The first executable named `name` in `localBins`, then on the login PATH.
    static func executable(_ name: String, localBins: [URL]) -> URL? {
        let path = loginPath.split(separator: ":").map { URL(fileURLWithPath: String($0)) }
        return (localBins + path)
            .map { $0.appendingPathComponent(name) }
            .first { FileManager.default.isExecutableFile(atPath: $0.path) }
    }

    /// The login shell's PATH, read once.
    static let loginPath: String = {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh")
        process.arguments = ["-lc", "printf %s \"$PATH\""]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        let fallback = ProcessInfo.processInfo.environment["PATH"] ?? "/usr/bin:/bin"
        guard (try? process.run()) != nil else { return fallback }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let path = String(decoding: data, as: UTF8.self)
        return process.terminationStatus == 0 && !path.isEmpty ? path : fallback
    }()

    struct Outcome: Sendable {
        var name: String
        var result: Result<String, Failure>
    }

    /// Nil when no formatter is installed for `path`. Blocks; call it off the main thread.
    static func format(path: String, root: String?, text: String) -> Outcome? {
        guard let command = command(for: path, root: root) else { return nil }
        let filename = URL(fileURLWithPath: path).lastPathComponent
        do {
            return Outcome(name: command.name, result: .success(try run(command, text: text, filename: filename)))
        } catch let failure as Failure {
            return Outcome(name: command.name, result: .failure(failure))
        } catch {
            return Outcome(name: command.name, result: .failure(.failed(command.name, error.localizedDescription)))
        }
    }

    /// Formats `text`, named `filename` for tools that read a file. Blocks;
    /// call it off the main thread.
    static func run(_ command: Command, text: String, filename: String, timeout: TimeInterval = 5) throws -> String {
        let files = FileManager.default
        let scratch = files.temporaryDirectory.appendingPathComponent("galaxy-format-\(UUID().uuidString)")
        try files.createDirectory(at: scratch, withIntermediateDirectories: true)
        defer { try? files.removeItem(at: scratch) }
        let input = scratch.appendingPathComponent(filename)
        let output = scratch.appendingPathComponent(".stdout")
        let errors = scratch.appendingPathComponent(".stderr")
        try Data(text.utf8).write(to: input)
        files.createFile(atPath: output.path, contents: nil)
        files.createFile(atPath: errors.path, contents: nil)
        // Files, not pipes: a tool that fills a pipe buffer cannot block.
        let stdin = try FileHandle(forReadingFrom: input)
        let stdout = try FileHandle(forWritingTo: output)
        let stderr = try FileHandle(forWritingTo: errors)
        defer {
            try? stdin.close()
            try? stdout.close()
            try? stderr.close()
        }

        let process = Process()
        process.executableURL = command.executable
        process.arguments = command.formatsFile ? command.arguments + [input.path] : command.arguments
        process.currentDirectoryURL = command.directory
        var environment = ProcessInfo.processInfo.environment
        environment["PATH"] = loginPath
        process.environment = environment
        process.standardInput = command.formatsFile ? FileHandle.nullDevice : stdin
        process.standardOutput = stdout
        process.standardError = stderr
        let done = DispatchSemaphore(value: 0)
        process.terminationHandler = { _ in done.signal() }
        try process.run()
        if done.wait(timeout: .now() + timeout) == .timedOut {
            process.terminate()
            if done.wait(timeout: .now() + 1) == .timedOut { kill(process.processIdentifier, SIGKILL) }
            throw Failure.timedOut(command.name)
        }
        guard process.terminationStatus == 0 else {
            let message = String(decoding: (try? Data(contentsOf: errors)) ?? Data(), as: UTF8.self)
                .split(whereSeparator: \.isNewline)
                .first { !$0.trimmingCharacters(in: .whitespaces).isEmpty }
                .map { $0.trimmingCharacters(in: .whitespaces) } ?? ""
            throw Failure.failed(command.name, message)
        }
        let formatted = String(decoding: try Data(contentsOf: command.formatsFile ? input : output), as: UTF8.self)
        if formatted.isEmpty, !text.isEmpty { throw Failure.failed(command.name, "no output") }
        return formatted.replacingOccurrences(of: "\r\n", with: "\n")
    }

    /// The smallest whole-line replacement that turns `old` into `new`, so
    /// carets outside the changed lines stay where they are. Nil when equal.
    static func edit(from old: String, to new: String) -> RenameEdit? {
        let a = old.components(separatedBy: "\n")
        let b = new.components(separatedBy: "\n")
        guard a != b else { return nil }
        let shared = min(a.count, b.count)
        var prefix = 0
        while prefix < shared, a[prefix] == b[prefix] { prefix += 1 }
        var suffix = 0
        while suffix < shared - prefix, a[a.count - 1 - suffix] == b[b.count - 1 - suffix] { suffix += 1 }
        if suffix > 0 {
            let text = b[prefix..<(b.count - suffix)].map { $0 + "\n" }.joined()
            return RenameEdit(uri: "", startLine: UInt64(prefix), startCharacter: 0,
                              endLine: UInt64(a.count - suffix), endCharacter: 0, newText: text)
        }
        // The last line changed: replace through the end, starting after the
        // last shared line so its newline can go or stay with the new text.
        let end = (line: UInt64(a.count - 1), character: UInt64(a[a.count - 1].utf16.count))
        if prefix == 0 {
            return RenameEdit(uri: "", startLine: 0, startCharacter: 0, endLine: end.line,
                              endCharacter: end.character, newText: b.joined(separator: "\n"))
        }
        return RenameEdit(uri: "", startLine: UInt64(prefix - 1), startCharacter: UInt64(a[prefix - 1].utf16.count),
                          endLine: end.line, endCharacter: end.character,
                          newText: b[prefix...].map { "\n" + $0 }.joined())
    }
}
