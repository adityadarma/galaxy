import Foundation
import os
import Security

/// API keys of the AI providers, stored in the login Keychain as generic
/// passwords (service `dev.adityadarma.galaxy.ai`, account = settings key).
/// Values are cached in memory so a keystroke does not hit the Keychain.
enum AIKeychain {
    static let service = "dev.adityadarma.galaxy.ai"

    /// `.some(nil)` caches "no key stored".
    private static let cache = OSAllocatedUnfairLock<[String: String?]>(initialState: [:])

    /// The key for `account`. A key older builds kept in UserDefaults as
    /// plain text is moved into the Keychain and removed from there.
    static func read(_ account: String) -> String? {
        if let cached = cache.withLock({ $0[account] }) { return cached }
        var value = load(account)
        if value == nil {
            let legacy = UserDefaults.standard.string(forKey: account)?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            if !legacy.isEmpty {
                if save(legacy, account: account) { UserDefaults.standard.removeObject(forKey: account) }
                value = legacy
            }
        }
        let found = value
        cache.withLock { $0[account] = .some(found) }
        return found
    }

    /// Stores `value`, or deletes the item when it is empty.
    static func write(_ value: String?, account: String) {
        let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if trimmed.isEmpty {
            SecItemDelete(baseQuery(account) as CFDictionary)
            cache.withLock { $0[account] = .some(nil) }
        } else {
            _ = save(trimmed, account: account)
            cache.withLock { $0[account] = .some(trimmed) }
        }
        // Never leave a plain-text copy behind.
        UserDefaults.standard.removeObject(forKey: account)
    }

    private static func baseQuery(_ account: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service,
         kSecAttrAccount as String: account]
    }

    private static func load(_ account: String) -> String? {
        var query = baseQuery(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data, let text = String(data: data, encoding: .utf8),
              !text.isEmpty else { return nil }
        return text
    }

    @discardableResult
    private static func save(_ value: String, account: String) -> Bool {
        let data = Data(value.utf8)
        let query = baseQuery(account)
        var status = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            var add = query
            add[kSecValueData as String] = data
            add[kSecAttrLabel as String] = "Galaxy AI API key"
            add[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlocked
            status = SecItemAdd(add as CFDictionary, nil)
        }
        if status != errSecSuccess {
            Log.app.error("Keychain save failed for \(account, privacy: .public): \(status, privacy: .public)")
        }
        return status == errSecSuccess
    }
}
