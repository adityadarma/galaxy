import Foundation
import os

/// Inline code completion (ghost text) through the shared OpenAI-compatible
/// server. Servers behind `AIConnection` usually only offer Chat
/// Completions, not fill-in-the-middle, so the prompt marks the caret and
/// asks for the inserted code only.
@MainActor
final class AICompletionService {
    static let shared = AICompletionService()

    /// Suggestions are cut to this many lines.
    static let maxLines = 3

    enum Keys {
        static let enabled = "AICompletionEnabled"
        static let model = "AICompletionModel"
    }

    /// Short timeouts: a late suggestion is useless, the user has moved on.
    private static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 8
        configuration.timeoutIntervalForResource = 12
        return URLSession(configuration: configuration)
    }()

    /// Needs the checkbox, a reachable provider and a chosen model.
    var isEnabled: Bool {
        UserDefaults.standard.bool(forKey: Keys.enabled) && model != nil && AIConnection.completion.isConfigured
    }

    /// The chosen model; there is no default, the user picks one in Settings.
    var model: String? {
        let saved = AIConnection.trimmed(UserDefaults.standard.string(forKey: Keys.model))
        return saved.isEmpty ? nil : saved
    }

    struct Request {
        var fileName: String
        var language: String?
        /// Text before and after the caret, already limited in size.
        var prefix: String
        var suffix: String
    }

    /// Code to insert at the caret, at most `maxLines` lines, or `nil`.
    func suggest(_ request: Request) async -> String? {
        guard isEnabled, let model else { return nil }
        let language = request.language.map { " (\($0))" } ?? ""
        let system = """
        You are a code completion engine inside a code editor. The user sends a file with a <CURSOR> marker. \
        Reply with ONLY the code to insert at <CURSOR>, at most \(Self.maxLines) lines. \
        Do not repeat code that is before or after the cursor. No explanations, no markdown fences. \
        If nothing useful fits, reply with an empty message.
        """
        let user = "File: \(request.fileName)\(language)\n\n\(request.prefix)<CURSOR>\(request.suffix)"
        do {
            let raw = try await AIConnection.completion.chat(
                [.init(role: "system", content: system), .init(role: "user", content: user)],
                model: model, maxTokens: 160, temperature: 0.1,
                session: Self.session, label: "AI completion")
            guard let raw, !Task.isCancelled else { return nil }
            return Self.clean(raw, prefix: request.prefix, suffix: request.suffix)
        } catch {
            if !Task.isCancelled, (error as? URLError)?.code != .cancelled {
                Log.app.error("AI completion request failed: \(String(describing: error), privacy: .public)")
            }
            return nil
        }
    }

    /// Strips fences and echoed text, keeps at most `maxLines` lines, and
    /// drops a suggestion that only repeats the code after the caret.
    static func clean(_ raw: String, prefix: String, suffix: String) -> String? {
        var lines = raw.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n")
        if lines.first?.trimmingCharacters(in: .whitespaces).hasPrefix("```") == true { lines.removeFirst() }
        if let fence = lines.lastIndex(where: { $0.trimmingCharacters(in: .whitespaces).hasPrefix("```") }) {
            lines.removeSubrange(fence...)
        }
        lines = lines.map { $0.replacingOccurrences(of: "<CURSOR>", with: "") }
        var text = lines.joined(separator: "\n")

        // Some models repeat the caret line before continuing it.
        let currentLine = prefix.components(separatedBy: "\n").last ?? ""
        let typed = currentLine.trimmingCharacters(in: .whitespaces)
        if !typed.isEmpty, text.hasPrefix(currentLine) {
            text = String(text.dropFirst(currentLine.count))
        } else if !typed.isEmpty, text.trimmingCharacters(in: .whitespaces).hasPrefix(typed),
                  !text.hasPrefix("\n") {
            let trimmed = text.drop { $0 == " " || $0 == "\t" }
            text = String(trimmed.dropFirst(typed.count))
        }

        // Code right after the caret must not be suggested again.
        var kept = Array(text.components(separatedBy: "\n").prefix(maxLines))
        let nextLines = suffix.components(separatedBy: "\n")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        if let next = nextLines.first,
           let index = kept.firstIndex(where: { $0.trimmingCharacters(in: .whitespaces) == next }) {
            kept.removeSubrange(index...)
        }
        while let last = kept.last, last.trimmingCharacters(in: .whitespaces).isEmpty { kept.removeLast() }
        // Only trailing whitespace is trimmed; leading space may be wanted.
        if var last = kept.popLast() {
            while last.last == " " || last.last == "\t" { last.removeLast() }
            kept.append(last)
        }
        let result = kept.joined(separator: "\n")
        return result.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : result
    }
}
