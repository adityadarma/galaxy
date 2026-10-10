import Foundation
import os

/// Unified logging categories. View with:
/// `log stream --predicate 'subsystem == "dev.adityadarma.galaxy"'`
enum Log {
    static let subsystem = "dev.adityadarma.galaxy"
    static let app = Logger(subsystem: subsystem, category: "app")
    static let core = Logger(subsystem: subsystem, category: "core")
    static let ui = Logger(subsystem: subsystem, category: "ui")

    /// `~/Library/Logs/Galaxy`, where the Rust core writes `core.log`.
    static var directory: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/Galaxy", isDirectory: true)
    }
}
