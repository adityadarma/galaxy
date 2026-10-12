import Foundation

@MainActor
final class AICommitService {
    static let shared = AICommitService()

    enum Provider: String, CaseIterable {
        case openAI = "OpenAI"
        case gemini = "Gemini"
        case ollama = "Ollama"

        var defaultModel: String {
            switch self {
            case .openAI: return "gpt-4o-mini"
            case .gemini: return "gemini-1.5-flash"
            case .ollama: return "llama3"
            }
        }
    }

    var provider: Provider {
        let saved = UserDefaults.standard.string(forKey: "AICommitProvider") ?? "OpenAI"
        return Provider(rawValue: saved) ?? .openAI
    }

    var apiKey: String? {
        if let key = UserDefaults.standard.string(forKey: "AICommitApiKey")?.trimmingCharacters(in: .whitespacesAndNewlines), !key.isEmpty {
            return key
        }
        let env = ProcessInfo.processInfo.environment
        switch provider {
        case .openAI:
            return env["OPENAI_API_KEY"] ?? env["LLM_API_KEY"]
        case .gemini:
            return env["GEMINI_API_KEY"] ?? env["LLM_API_KEY"]
        case .ollama:
            return "ollama" // Ollama does not require a secret key
        }
    }

    var isConfigured: Bool {
        if provider == .ollama {
            return true
        }
        guard let key = apiKey else { return false }
        return !key.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    func generateCommitMessage(diff: String) async -> String? {
        guard isConfigured else { return nil }

        // Limit diff to avoid token overflow
        let maxDiffLength = 6000
        let truncatedDiff = diff.count > maxDiffLength ? String(diff.prefix(maxDiffLength)) + "\n... (truncated)" : diff

        let prompt = """
        You are an expert developer. Generate a concise, single-line conventional commit message (max 72 characters, e.g. 'feat: add user authentication' or 'fix: resolve layout spacing') summarizing the following git diff.
        Return ONLY the single-line commit message without any quotes, backticks, or explanatory text.

        Git diff:
        \(truncatedDiff)
        """

        do {
            switch provider {
            case .openAI, .ollama:
                return try await callOpenAICompatible(prompt: prompt)
            case .gemini:
                return try await callGemini(prompt: prompt)
            }
        } catch {
            return nil
        }
    }

    // MARK: - OpenAI / Ollama Provider

    private func callOpenAICompatible(prompt: String) async throws -> String? {
        let endpointString: String
        let model = UserDefaults.standard.string(forKey: "AICommitModel") ?? provider.defaultModel

        if provider == .ollama {
            endpointString = "http://localhost:11434/v1/chat/completions"
        } else {
            endpointString = "https://api.openai.com/v1/chat/completions"
        }

        guard let url = URL(string: endpointString) else { return nil }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 12
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        if let apiKey, !apiKey.isEmpty {
            request.setValue("Bearer \(apiKey)", forHTTPHeaderField: "Authorization")
        }

        let body: [String: Any] = [
            "model": model,
            "messages": [
                ["role": "system", "content": "You are a concise git commit message generator. Return only the commit message."],
                ["role": "user", "content": prompt]
            ],
            "max_tokens": 80,
            "temperature": 0.2
        ]

        request.httpBody = try JSONSerialization.data(withJSONObject: body)

        let (data, response) = try await URLSession.shared.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse, httpResponse.statusCode == 200 else {
            return nil
        }

        struct Response: Decodable {
            struct Choice: Decodable {
                struct Message: Decodable {
                    let content: String?
                }
                let message: Message
            }
            let choices: [Choice]
        }

        let decoded = try JSONDecoder().decode(Response.self, from: data)
        if let raw = decoded.choices.first?.message.content {
            return sanitizeCommitMessage(raw)
        }
        return nil
    }

    // MARK: - Gemini Provider

    private func callGemini(prompt: String) async throws -> String? {
        guard let key = apiKey, !key.isEmpty else { return nil }
        let model = UserDefaults.standard.string(forKey: "AICommitModel") ?? provider.defaultModel
        let endpoint = "https://generativelanguage.googleapis.com/v1beta/models/\(model):generateContent?key=\(key)"

        guard let url = URL(string: endpoint) else { return nil }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 12
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        let body: [String: Any] = [
            "contents": [
                [
                    "parts": [
                        ["text": prompt]
                    ]
                ]
            ],
            "generationConfig": [
                "maxOutputTokens": 80,
                "temperature": 0.2
            ]
        ]

        request.httpBody = try JSONSerialization.data(withJSONObject: body)

        let (data, response) = try await URLSession.shared.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse, httpResponse.statusCode == 200 else {
            return nil
        }

        struct Response: Decodable {
            struct Candidate: Decodable {
                struct Content: Decodable {
                    struct Part: Decodable {
                        let text: String?
                    }
                    let parts: [Part]
                }
                let content: Content
            }
            let candidates: [Candidate]?
        }

        let decoded = try JSONDecoder().decode(Response.self, from: data)
        if let raw = decoded.candidates?.first?.content.parts.first?.text {
            return sanitizeCommitMessage(raw)
        }
        return nil
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
        if (text.hasPrefix("`") && text.hasSuffix("`")) {
            text = String(text.dropFirst().dropLast()).trimmingCharacters(in: .whitespacesAndNewlines)
        }
        // Take the first line if multiple lines were generated
        if let firstLine = text.components(separatedBy: "\n").first {
            text = firstLine.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return text
    }
}
