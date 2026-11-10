import Foundation
import os

/// Generates commit messages through an OpenAI-compatible Chat Completions
/// API. The base URL and model are configurable, so any compatible server
/// works (OpenAI, OpenRouter, LM Studio, Ollama's `/v1`, a proxy, ...).
@MainActor
final class AICommitService {
    static let shared = AICommitService()

    static let defaultBaseURL = "https://api.openai.com/v1"
    static let defaultModel = "gpt-4o-mini"

    enum Keys {
        static let baseURL = "AICommitBaseURL"
        static let model = "AICommitModel"
        static let apiKey = "AICommitApiKey"
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

    /// Base URL without a trailing slash, e.g. `https://api.openai.com/v1`.
    var baseURL: String {
        let saved = Self.trimmed(UserDefaults.standard.string(forKey: Keys.baseURL))
        let value = saved.isEmpty ? Self.defaultBaseURL : saved
        return value.hasSuffix("/") ? String(value.dropLast()) : value
    }

    var model: String {
        let saved = Self.trimmed(UserDefaults.standard.string(forKey: Keys.model))
        return saved.isEmpty ? Self.defaultModel : saved
    }

    var apiKey: String? {
        let saved = Self.trimmed(UserDefaults.standard.string(forKey: Keys.apiKey))
        if !saved.isEmpty { return saved }
        let env = ProcessInfo.processInfo.environment
        let fromEnv = Self.trimmed(env["OPENAI_API_KEY"] ?? env["LLM_API_KEY"])
        return fromEnv.isEmpty ? nil : fromEnv
    }

    /// `<base>/chat/completions`; a base that already ends with it is kept.
    var endpoint: URL? {
        let base = baseURL
        let full = base.hasSuffix("/chat/completions") ? base : base + "/chat/completions"
        guard let url = URL(string: full), let scheme = url.scheme?.lowercased(),
              scheme == "https" || scheme == "http", url.host != nil else { return nil }
        return url
    }

    /// `<base>/models`, for the model picker in Settings.
    var modelsEndpoint: URL? {
        var base = baseURL
        if base.hasSuffix("/chat/completions") { base = String(base.dropLast("/chat/completions".count)) }
        guard let url = URL(string: base + "/models"), let scheme = url.scheme?.lowercased(),
              scheme == "https" || scheme == "http", url.host != nil else { return nil }
        return url
    }

    enum ModelListError: Error, CustomStringConvertible {
        case invalidURL
        case http(Int)
        case unreadable

        var description: String {
            switch self {
            case .invalidURL: "Invalid base URL"
            case .http(401), .http(403): "API key rejected (HTTP \(self.status))"
            case .http(let code): "Server returned HTTP \(code)"
            case .unreadable: "Unexpected response from /models"
            }
        }

        private var status: Int { if case .http(let code) = self { code } else { 0 } }
    }

    /// Model IDs from `GET <base>/models`, sorted.
    func fetchModels() async throws -> [String] {
        guard let url = modelsEndpoint else { throw ModelListError.invalidURL }
        var request = URLRequest(url: url)
        request.timeoutInterval = 15
        if let apiKey {
            request.setValue("Bearer \(apiKey)", forHTTPHeaderField: "Authorization")
        }
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw ModelListError.unreadable }
        guard http.statusCode == 200 else { throw ModelListError.http(http.statusCode) }
        struct List: Decodable {
            struct Model: Decodable { let id: String }
            let data: [Model]
        }
        guard let list = try? JSONDecoder().decode(List.self, from: data) else { throw ModelListError.unreadable }
        return Array(Set(list.data.map(\.id)))
            .sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending }
    }

    /// The official OpenAI endpoint needs a key; a custom server may not.
    var isConfigured: Bool {
        guard let endpoint else { return false }
        if endpoint.host?.lowercased() == "api.openai.com" { return apiKey != nil }
        return true
    }

    func generateCommitMessage(diff: String) async -> String? {
        guard isConfigured, let endpoint else { return nil }

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
            return try await callChatCompletions(endpoint: endpoint, prompt: prompt)
        } catch {
            Log.app.error("AI commit request failed: \(String(describing: error), privacy: .public)")
            return nil
        }
    }

    private func callChatCompletions(endpoint: URL, prompt: String) async throws -> String? {
        var request = URLRequest(url: endpoint)
        request.httpMethod = "POST"
        request.timeoutInterval = 20
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let apiKey {
            request.setValue("Bearer \(apiKey)", forHTTPHeaderField: "Authorization")
        }

        let body: [String: Any] = [
            "model": model,
            "messages": [
                ["role": "system", "content": "You are a concise git commit message generator. Return only the commit message."],
                ["role": "user", "content": prompt],
            ],
            "max_tokens": 80,
            "temperature": 0.2,
            // Some compatible servers stream by default; ask for one JSON body.
            "stream": false,
        ]
        request.httpBody = try JSONSerialization.data(withJSONObject: body)

        let (data, response) = try await Self.session.data(for: request)
        guard let http = response as? HTTPURLResponse else { return nil }
        guard http.statusCode == 200 else {
            // Log the status and the API's error code only, never the request.
            let code = (try? JSONDecoder().decode(ErrorResponse.self, from: data))?.error.code ?? "-"
            Log.app.error("AI commit HTTP \(http.statusCode) (\(code, privacy: .public)) from \(endpoint.host ?? "?", privacy: .public)")
            return nil
        }

        let decoded = try JSONDecoder().decode(Response.self, from: data)
        guard let raw = decoded.choices.first?.message.content else { return nil }
        let message = sanitizeCommitMessage(raw)
        return message.isEmpty ? nil : message
    }

    private struct Response: Decodable {
        struct Choice: Decodable {
            struct Message: Decodable { let content: String? }
            let message: Message
        }
        let choices: [Choice]
    }

    private struct ErrorResponse: Decodable {
        struct Detail: Decodable { let code: String? }
        let error: Detail
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

    private static func trimmed(_ value: String?) -> String {
        value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }
}
