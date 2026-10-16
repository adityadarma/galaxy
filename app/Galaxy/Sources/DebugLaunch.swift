import Foundation

/// What to debug: the program, how to run it, and an optional build step.
struct DebugTarget: Equatable {
    var program: String
    var args: [String] = []
    var cwd: String
    /// Shell command that produces `program`, run before every launch.
    var build: String?
}

enum DebugTargets {
    /// The first target found in `.galaxy/launch.json`, then
    /// `.vscode/launch.json`, then a Cargo package's debug binary.
    static func detect(in root: URL) -> DebugTarget? {
        for file in [".galaxy/launch.json", ".vscode/launch.json"] {
            let url = root.appendingPathComponent(file)
            if let text = try? String(contentsOf: url, encoding: .utf8), let target = parseLaunch(text, root: root) {
                return target
            }
        }
        return cargoTarget(in: root)
    }

    /// Accepts a single configuration object or VS Code's
    /// `{"configurations": [...]}`; the first one with a `program` wins.
    /// `//` comment lines are ignored. `${workspaceFolder}` is expanded.
    static func parseLaunch(_ text: String, root: URL) -> DebugTarget? {
        let cleaned = text.split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
        guard let data = cleaned.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let configs = (json["configurations"] as? [[String: Any]]) ?? [json]
        let expand = { (s: String) in s.replacingOccurrences(of: "${workspaceFolder}", with: root.path) }
        for config in configs {
            guard let program = config["program"] as? String, !program.isEmpty else { continue }
            let path = expand(program)
            return DebugTarget(
                program: path.hasPrefix("/") ? path : root.appendingPathComponent(path).path,
                args: (config["args"] as? [String] ?? []).map(expand),
                cwd: (config["cwd"] as? String).map(expand) ?? root.path,
                build: config["build"] as? String)
        }
        return nil
    }

    /// `target/debug/<name>` for a `Cargo.toml` that has a `[package]`.
    static func cargoTarget(in root: URL) -> DebugTarget? {
        guard let manifest = try? String(contentsOf: root.appendingPathComponent("Cargo.toml"), encoding: .utf8),
              let name = packageName(inCargoManifest: manifest) else { return nil }
        return DebugTarget(program: root.appendingPathComponent("target/debug/\(name)").path,
                           cwd: root.path, build: "cargo build")
    }

    static func packageName(inCargoManifest text: String) -> String? {
        var inPackage = false
        for raw in text.split(separator: "\n") {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("[") { inPackage = line == "[package]"; continue }
            guard inPackage, line.hasPrefix("name") else { continue }
            let parts = line.split(separator: "=", maxSplits: 1).map { $0.trimmingCharacters(in: .whitespaces) }
            guard parts.count == 2, parts[0] == "name" else { continue }
            let value = parts[1].trimmingCharacters(in: CharacterSet(charactersIn: "\""))
            return value.isEmpty ? nil : value
        }
        return nil
    }
}
