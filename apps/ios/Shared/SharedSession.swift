import Foundation
import Security
import HolonClient
import CryptoKit

struct SharedSession: Codable, Equatable, Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    let generation: UUID
    let networkID: String
    let connectionName: String
    let apiBaseURL: URL
    let allowInsecureHTTP: Bool
    let runtimeID: String
    let userID: String
    let visibilityScopeID: String
    let credential: String
    let expiresAt: Date?
    var description: String { "SharedSession(<redacted>)" }
    var debugDescription: String { description }

    func validate(now: Date = Date()) throws {
        guard apiBaseURL.scheme?.lowercased() != "http" || allowInsecureHTTP,
              !networkID.isEmpty, !runtimeID.isEmpty, !userID.isEmpty,
              !visibilityScopeID.isEmpty, expiresAt.map({ $0 > now }) ?? true else {
            throw SharedShareError.loginRequired
        }
        _ = try HolonEndpoint(apiBaseURL: apiBaseURL, allowInsecureHTTP: allowInsecureHTTP)
    }
}

enum SharedShareError: Error { case loginRequired, changedConnection, incompatible, missingAgent }

/// Separate service and explicit access group: never searches the host's private vault.
struct SharedSessionVault: Sendable {
    let accessGroup: String
    let service: String
    init(accessGroup: String, service: String = "run.holon.ios.active-share-session") {
        self.accessGroup = accessGroup; self.service = service
    }
    static func configured(bundle: Bundle = .main) throws -> Self {
        guard let group = bundle.object(forInfoDictionaryKey: "HolonSharedAppGroup") as? String,
              !group.isEmpty, !group.contains("$("),
              FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group) != nil else {
            throw SharedImportError.unavailableAppGroup
        }
        return Self(accessGroup: group)
    }
    private var query: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service,
         kSecAttrAccount as String: "active", kSecAttrAccessGroup as String: accessGroup]
    }
    private var authorityURL: URL {
        get throws {
            guard let container = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: accessGroup) else {
                throw SharedImportError.unavailableAppGroup
            }
            let hash = SHA256.hash(data: Data(service.utf8)).map { String(format: "%02x", $0) }.joined()
            return container.appendingPathComponent("ShareAuthority-" + hash + ".json")
        }
    }
    private func authority() throws -> UUID? {
        let url = try authorityURL
        guard FileManager.default.fileExists(atPath: url.path) else { return nil }
        let values = try url.resourceValues(forKeys: [.isSymbolicLinkKey, .fileSizeKey])
        guard values.isSymbolicLink != true, (values.fileSize ?? Int.max) <= 80 else {
            throw SharedImportError.invalidRecord
        }
        return try JSONDecoder().decode(UUID?.self, from: Data(contentsOf: url))
    }
    private func setAuthority(_ generation: UUID?) throws {
        try JSONEncoder().encode(generation).write(to: authorityURL,
            options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }
    func write(_ session: SharedSession) throws {
        try session.validate()
        // Withdraw first: a failed Keychain update cannot expose the old capability.
        try setAuthority(nil)
        let values: [String: Any] = [kSecValueData as String: try JSONEncoder().encode(session),
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly]
        let status = SecItemUpdate(query as CFDictionary, values as CFDictionary)
        if status == errSecItemNotFound {
            try check(SecItemAdd(query.merging(values) { _, new in new } as CFDictionary, nil))
        } else { try check(status) }
        try setAuthority(session.generation)
    }
    func read() throws -> SharedSession? {
        guard let generation = try authority() else { return nil }
        var lookup = query
        lookup[kSecReturnData as String] = true
        lookup[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(lookup as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        try check(status)
        guard let data = result as? Data else { throw SharedShareError.loginRequired }
        let session = try JSONDecoder().decode(SharedSession.self, from: data)
        guard session.generation == generation, try authority() == generation else {
            throw SharedShareError.changedConnection
        }
        try session.validate()
        return session
    }
    func require(_ expected: SharedSession) throws {
        try Task.checkCancellation()
        guard try read() == expected else { throw SharedShareError.changedConnection }
    }
    func clear() throws {
        // This non-secret fence also works when the protected Keychain is locked.
        try setAuthority(nil)
        let status = SecItemDelete(query as CFDictionary)
        if status != errSecItemNotFound { try check(status) }
    }
    private func check(_ status: OSStatus) throws {
        guard status == errSecSuccess else { throw SharedImportError.storageUnavailable }
    }
}
