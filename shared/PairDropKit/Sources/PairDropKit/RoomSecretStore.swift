import Foundation
import Security

/// Persists pairing secrets in the keychain.
///
/// A room secret is a bearer credential: anyone holding it joins the paired room and can
/// send to the device. It does not belong in UserDefaults.
public struct RoomSecretStore: Sendable {

    public struct Entry: Codable, Hashable, Sendable {
        public var secret: String
        public var displayName: String
        public var autoAccept: Bool

        public init(secret: String, displayName: String, autoAccept: Bool = false) {
            self.secret = secret
            self.displayName = displayName
            self.autoAccept = autoAccept
        }
    }

    private let service: String
    private let account = "room-secrets"

    public init(service: String = "app.pairdrop.mac.roomsecrets") {
        self.service = service
    }

    public func load() -> [Entry] {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        var result: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data,
              let entries = try? JSONDecoder().decode([Entry].self, from: data) else { return [] }
        return entries
    }

    public func save(_ entries: [Entry]) {
        guard let data = try? JSONEncoder().encode(entries) else { return }
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlock
        ]

        let status = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            SecItemAdd(query.merging(attributes) { _, new in new } as CFDictionary, nil)
        }
    }

    @discardableResult
    public func add(_ entry: Entry) -> [Entry] {
        var entries = load().filter { $0.secret != entry.secret }
        entries.append(entry)
        save(entries)
        return entries
    }

    @discardableResult
    public func remove(secret: String) -> [Entry] {
        let entries = load().filter { $0.secret != secret }
        save(entries)
        return entries
    }

    @discardableResult
    public func replace(secret: String, with newSecret: String) -> [Entry] {
        var entries = load()
        guard let index = entries.firstIndex(where: { $0.secret == secret }) else { return entries }
        entries[index].secret = newSecret
        save(entries)
        return entries
    }

    public func update(secret: String, autoAccept: Bool) {
        var entries = load()
        guard let index = entries.firstIndex(where: { $0.secret == secret }) else { return }
        entries[index].autoAccept = autoAccept
        save(entries)
    }
}
