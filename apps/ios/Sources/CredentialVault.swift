import Foundation
import Security

/// Callers supply a complete connection/identity scope, never a bare hostname.
struct CredentialVault {
    let service: String

    struct Failure: Error {
        let status: OSStatus
    }

    private func query(account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    func write(_ data: Data, account: String) throws {
        let key = query(account: account)
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
        ]
        let update = SecItemUpdate(key as CFDictionary, attributes as CFDictionary)
        if update == errSecItemNotFound {
            let add = SecItemAdd(key.merging(attributes) { _, new in new } as CFDictionary, nil)
            guard add == errSecSuccess else { throw Failure(status: add) }
        } else if update != errSecSuccess {
            throw Failure(status: update)
        }
    }

    func read(account: String) throws -> Data? {
        var key = query(account: account)
        key[kSecReturnData as String] = true
        key[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(key as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess else { throw Failure(status: status) }
        guard let data = result as? Data else { throw Failure(status: errSecDecode) }
        return data
    }

    func remove(account: String) throws {
        let status = SecItemDelete(query(account: account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw Failure(status: status)
        }
    }
}
