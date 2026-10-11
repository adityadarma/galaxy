import AppKit
import Foundation

@MainActor
enum CrashReporter {
    private static var installed = false

    static func install() {
        guard !installed else { return }
        installed = true
        NSSetUncaughtExceptionHandler { exception in
            let directory = Log.directory
            try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let report = "Galaxy Objective-C exception\nname: \(exception.name.rawValue)\nreason: \(exception.reason ?? "unknown")\nstack:\n\(exception.callStackSymbols.joined(separator: "\n"))\n\n"
            let url = directory.appendingPathComponent("crash-report.log")
            if let data = report.data(using: .utf8), let handle = try? FileHandle(forWritingTo: url) {
                handle.seekToEndOfFile()
                try? handle.write(contentsOf: data)
                try? handle.close()
            } else {
                try? report.write(to: url, atomically: true, encoding: .utf8)
            }
        }
    }
}
