import AppKit

/// Installs the `galaxy` shell command: a symlink in `~/.local/bin` to the
/// helper bundled at `Galaxy.app/Contents/Resources/galaxy`, like VS Code's
/// "Install 'code' command in PATH".
enum CommandLineTool {
    enum InstallError: LocalizedError {
        case helperMissing(URL)
        case occupied(URL)

        var errorDescription: String? {
            switch self {
            case .helperMissing(let url): "The command helper is missing from the app: \(url.path)"
            case .occupied(let url): "\(url.path) already exists and is not a link. Remove it and try again."
            }
        }
    }

    static var helperURL: URL {
        Bundle.main.bundleURL.appendingPathComponent("Contents/Resources/galaxy")
    }

    static var defaultLinkURL: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".local/bin/galaxy")
    }

    /// A link to this app's helper, not merely a command with the same name
    /// elsewhere on PATH. A stale link to another Galaxy build can be updated.
    static func isInstalled(helper: URL = helperURL, link: URL = defaultLinkURL) -> Bool {
        let files = FileManager.default
        guard files.isExecutableFile(atPath: helper.path),
              let destination = try? files.destinationOfSymbolicLink(atPath: link.path) else { return false }
        let resolved = URL(fileURLWithPath: destination, relativeTo: link.deletingLastPathComponent())
            .standardizedFileURL.resolvingSymlinksInPath()
        return resolved == helper.standardizedFileURL.resolvingSymlinksInPath()
    }

    /// Creates or replaces the symlink. A regular file at `link` is left alone.
    static func install(helper: URL = helperURL, link: URL = defaultLinkURL) throws {
        let files = FileManager.default
        guard files.isExecutableFile(atPath: helper.path) else { throw InstallError.helperMissing(helper) }
        try files.createDirectory(at: link.deletingLastPathComponent(), withIntermediateDirectories: true)
        if let type = try? files.attributesOfItem(atPath: link.path)[.type] as? FileAttributeType {
            guard type == .typeSymbolicLink else { throw InstallError.occupied(link) }
            try files.removeItem(at: link)
        }
        try files.createSymbolicLink(at: link, withDestinationURL: helper)
    }

    /// Whether the login shell's `PATH` contains `directory`. The app's own
    /// environment comes from launchd and rarely has `~/.local/bin`.
    static func shellPathContains(_ directory: URL) -> Bool {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh")
        process.arguments = ["-lc", "printf %s \"$PATH\""]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        guard (try? process.run()) != nil else { return false }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let target = directory.standardizedFileURL.path
        return String(decoding: data, as: UTF8.self).split(separator: ":").contains {
            URL(fileURLWithPath: String($0)).standardizedFileURL.path == target
        }
    }
}
