import Foundation
import os

/// Generates commit messages through the shared OpenAI-compatible server
/// (`AIConnection`), with its own model choice.
@MainActor
final class AICommitService {
    static let shared = AICommitService()

    enum Keys {
        static let model = "AICommitModel"
    }

    /// Commit requests give up after 45 s in total. The per-request timeout
    /// only counts idle time, so a proxy that keeps the connection alive
    /// would leave the button loading forever.
    private static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 20
        configuration.timeoutIntervalForResource = 45
        return URLSession(configuration: configuration)
    }()

    /// The chosen model; there is no default, the user picks one in Settings.
    var model: String? {
        let saved = AIConnection.trimmed(UserDefaults.standard.string(forKey: Keys.model))
        return saved.isEmpty ? nil : saved
    }

    /// AI generation needs a reachable provider and a chosen model.
    var isConfigured: Bool { model != nil && AIConnection.commit.isConfigured }

    func generateCommitMessage(diff: String) async -> String? {
        guard isConfigured, let model else { return nil }

        // Limit the diff to avoid token overflow.
        let maxDiffLength = 6000
        let truncatedDiff = diff.count > maxDiffLength ? String(diff.prefix(maxDiffLength)) + "\n... (truncated)" : diff

        let prompt = """
        You are an expert developer. Generate a concise, single-line conventional commit message (max 72 characters, e.g. 'feat: add user authentication' or 'fix: resolve layout spacing') summarizing the following git diff.
        Return ONLY the single-line commit message without any quotes, backticks, or explanatory text.

        Git diff:
        \(truncatedDiff)
        """

        do {
            let raw = try await AIConnection.commit.chat(
                [.init(role: "system", content: "You are a concise git commit message generator. Return only the commit message."),
                 .init(role: "user", content: prompt)],
                model: model, maxTokens: 80, temperature: 0.2,
                session: Self.session, label: "AI commit")
            guard let raw else { return nil }
            let message = sanitizeCommitMessage(raw)
            return message.isEmpty ? nil : message
        } catch {
            Log.app.error("AI commit request failed: \(String(describing: error), privacy: .public)")
            return nil
        }
    }

    // MARK: - Sanitization

    private func sanitizeCommitMessage(_ raw: String) -> String {
        var text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.hasPrefix("```") {
            let lines = text.components(separatedBy: "\n").filter { !$0.hasPrefix("```") }
            text = lines.joined(separator: " ").trimmingCharacters(in: .whitespacesAndNewlines)
        }
        if (text.hasPrefix("\"") && text.hasSuffix("\"")) || (text.hasPrefix("'") && text.hasSuffix("'")) {
            text = String(text.dropFirst().dropLast()).trimmingCharacters(in: .whitespacesAndNewlines)
        }
        if text.hasPrefix("`") && text.hasSuffix("`") {
            text = String(text.dropFirst().dropLast()).trimmingCharacters(in: .whitespacesAndNewlines)
        }
        // Take the first line if multiple lines were generated.
        if let firstLine = text.components(separatedBy: "\n").first {
            text = firstLine.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return text
    }
}
