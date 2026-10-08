import Foundation
import os

/// Unified logging categories. View with:
/// `log stream --predicate 'subsystem == "dev.rustcode.RustCode"'`
enum Log {
    static let subsystem = "dev.rustcode.RustCode"
    static let app = Logger(subsystem: subsystem, category: "app")
    static let core = Logger(subsystem: subsystem, category: "core")
    static let ui = Logger(subsystem: subsystem, category: "ui")

    /// `~/Library/Logs/RustCode`, where the Rust core writes `core.log`.
    static var directory: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/RustCode", isDirectory: true)
    }
}
