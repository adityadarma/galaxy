import AppKit

/// What the window had open when it closed (M2-11): the folder, the file
/// tabs in order, each tab's caret and scroll position, and which tab was
/// active. Saved as JSON when the window closes or the app quits, and
/// restored at the next launch unless a file or folder was asked for.
///
/// Untitled buffers are not saved, and unsaved edits are not kept: closing
/// already asks about them, and a restored tab shows the file on disk.
struct Session: Codable, Equatable {
    struct Tab: Codable, Equatable {
        var path: String
        var isPreview: Bool
        var line: UInt64
        var col: UInt64
        var scrollY: Double?
    }

    static let currentVersion = 1

    var version = Session.currentVersion
    var workspace: String?
    var tabs: [Tab] = []
    /// Index into `tabs`.
    var active: Int?
    var sidebarVisible = true

    /// `~/Library/Application Support/Galaxy/session.json`. Under XCTest
    /// a temp file, so tests never read or overwrite the user's session.
    static var defaultURL: URL {
        if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil {
            return FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-test-session.json")
        }
        return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Galaxy/session.json")
    }

    /// A stable, filesystem-safe filename for a workspace-specific tab state.
    static func workspaceURL(for path: String) -> URL {
        var hash: UInt64 = 0xcbf29ce484222325
        for byte in path.utf8 {
            hash ^= UInt64(byte)
            hash &*= 0x100000001b3
        }
        let filename = String(format: "%016llx.json", hash)
        return defaultURL.deletingLastPathComponent()
            .appendingPathComponent("workspaces", isDirectory: true)
            .appendingPathComponent(filename)
    }

    /// `nil` when there is no session, or it cannot be read (then it is
    /// logged and ignored: a bad session must never stop the app opening).
    static func load(from url: URL) -> Session? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        do {
            let session = try JSONDecoder().decode(Session.self, from: data)
            guard session.version == currentVersion else {
                Log.app.info("ignoring session version \(session.version)")
                return nil
            }
            return session
        } catch {
            Log.app.error("ignoring unreadable session: \(String(describing: error), privacy: .public)")
            return nil
        }
    }

    func save(to url: URL) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try encoder.encode(self).write(to: url, options: .atomic)
    }

    static func remove(at url: URL) {
        guard FileManager.default.fileExists(atPath: url.path) else { return }
        do {
            try FileManager.default.removeItem(at: url)
        } catch {
            Log.app.error("removing session failed: \(String(describing: error), privacy: .public)")
        }
    }
}
