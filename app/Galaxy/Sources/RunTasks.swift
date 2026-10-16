import Foundation

/// Build and test commands for the project in a folder, chosen from the
/// files that identify it. A kind that is not recognized is `nil`.
enum RunTasks {
    struct Detected: Equatable {
        var build: String?
        var test: String?
    }

    static func detect(in root: URL) -> Detected {
        let fm = FileManager.default
        func has(_ name: String) -> Bool { fm.fileExists(atPath: root.appendingPathComponent(name).path) }
        func read(_ name: String) -> String? { try? String(contentsOf: root.appendingPathComponent(name), encoding: .utf8) }

        if has("Cargo.toml") { return Detected(build: "cargo build", test: "cargo test") }
        if has("Package.swift") { return Detected(build: "swift build", test: "swift test") }
        if has("go.mod") { return Detected(build: "go build ./...", test: "go test ./...") }

        if has("package.json") || has("artisan") {
            var detected = Detected()
            if let data = read("package.json")?.data(using: .utf8),
               let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               let scripts = json["scripts"] as? [String: Any] {
                let runner = has("pnpm-lock.yaml") ? "pnpm" : has("yarn.lock") ? "yarn" : "npm"
                if scripts["build"] != nil { detected.build = "\(runner) run build" }
                if scripts["test"] != nil { detected.test = "\(runner) test" }
            }
            // Laravel keeps its tests in PHP even when it also has a package.json.
            if has("artisan") { detected.test = "php artisan test" }
            return detected
        }

        if let makefile = read("Makefile") {
            let hasTest = makefile.hasPrefix("test:") || makefile.contains("\ntest:")
            return Detected(build: "make", test: hasTest ? "make test" : nil)
        }
        return Detected()
    }
}
