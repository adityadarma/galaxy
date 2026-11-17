import Foundation
import os

/// One OpenAI-compatible server: base URL and API key. The general
/// provider is shared by every AI feature; a feature may switch to its own
/// provider (see `AIConnection.completion`).
struct AIProvider {
    static let defaultBaseURL = "https://api.openai.com/v1"

    /// UserDefaults key of the base URL, and Keychain account of the API key.
    let baseURLKey: String
    let apiKeyKey: String
    /// Whether `OPENAI_API_KEY` / `LLM_API_KEY` fill an empty key field.
    let usesEnvironmentKey: Bool

    /// Base URL without a trailing slash, e.g. `https://api.openai.com/v1`.
    var baseURL: String {
        let saved = AIConnection.trimmed(UserDefaults.standard.string(forKey: baseURLKey))
        let value = saved.isEmpty ? Self.defaultBaseURL : saved
        return value.hasSuffix("/") ? String(value.dropLast()) : value
    }

    var apiKey: String? {
        let saved = AIConnection.trimmed(AIKeychain.read(apiKeyKey))
        if !saved.isEmpty { return saved }
        guard usesEnvironmentKey else { return nil }
        let env = ProcessInfo.processInfo.environment
        let fromEnv = AIConnection.trimmed(env["OPENAI_API_KEY"] ?? env["LLM_API_KEY"])
        return fromEnv.isEmpty ? nil : fromEnv
    }

    /// `<base>/chat/completions`; a base that already ends with it is kept.
    var chatEndpoint: URL? {
        let base = baseURL
        return Self.httpURL(base.hasSuffix("/chat/completions") ? base : base + "/chat/completions")
    }

    /// `<base>/models`, for the model pickers in Settings.
    var modelsEndpoint: URL? {
        var base = baseURL
        if base.hasSuffix("/chat/completions") { base = String(base.dropLast("/chat/completions".count)) }
        return Self.httpURL(base + "/models")
    }

    /// The official OpenAI endpoint needs a key; a custom server may not.
    var isConfigured: Bool {
        guard let endpoint = chatEndpoint else { return false }
        if endpoint.host?.lowercased() == "api.openai.com" { return apiKey != nil }
        return true
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

    struct Message {
        let role: String
        let content: String
    }

    /// One non-streaming Chat Completions call. Returns the first choice's
    /// content, or `nil` on an HTTP error (logged without the request body).
    func chat(_ messages: [Message], model: String, maxTokens: Int, temperature: Double,
              session: URLSession, label: String) async throws -> String? {
        guard let endpoint = chatEndpoint else { return nil }
        var request = URLRequest(url: endpoint)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let apiKey {
            request.setValue("Bearer \(apiKey)", forHTTPHeaderField: "Authorization")
        }
        let body: [String: Any] = [
            "model": model,
            "messages": messages.map { ["role": $0.role, "content": $0.content] },
            "max_tokens": maxTokens,
            "temperature": temperature,
            // Some compatible servers stream by default; ask for one JSON body.
            "stream": false,
        ]
        request.httpBody = try JSONSerialization.data(withJSONObject: body)

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { return nil }
        guard http.statusCode == 200 else {
            // Log the status and the API's error code only, never the request.
            let code = (try? JSONDecoder().decode(ErrorResponse.self, from: data))?.error.code ?? "-"
            Log.app.error("\(label, privacy: .public) HTTP \(http.statusCode) (\(code, privacy: .public)) from \(endpoint.host ?? "?", privacy: .public)")
            return nil
        }
        return try JSONDecoder().decode(Response.self, from: data).choices.first?.message.content
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

    private static func httpURL(_ string: String) -> URL? {
        guard let url = URL(string: string), let scheme = url.scheme?.lowercased(),
              scheme == "https" || scheme == "http", url.host != nil else { return nil }
        return url
    }
}

/// Which provider each AI feature uses.
enum AIConnection {
    enum Keys {
        // The general key names predate the split, so saved settings keep working.
        static let baseURL = "AICommitBaseURL"
        static let apiKey = "AICommitApiKey"
        static let completionCustomProvider = "AICompletionCustomProvider"
        static let completionBaseURL = "AICompletionBaseURL"
        static let completionApiKey = "AICompletionApiKey"
    }

    static let general = AIProvider(baseURLKey: Keys.baseURL, apiKeyKey: Keys.apiKey, usesEnvironmentKey: true)
    /// Code completion's own server, used only when its checkbox is on.
    static let completionOverride = AIProvider(baseURLKey: Keys.completionBaseURL,
                                               apiKeyKey: Keys.completionApiKey, usesEnvironmentKey: false)

    static var completionUsesCustomProvider: Bool {
        UserDefaults.standard.bool(forKey: Keys.completionCustomProvider)
    }

    static var commit: AIProvider { general }
    static var completion: AIProvider { completionUsesCustomProvider ? completionOverride : general }

    static func trimmed(_ value: String?) -> String {
        value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }
}
