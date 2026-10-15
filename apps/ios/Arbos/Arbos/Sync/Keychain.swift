// Device token storage (crdt-sync §10.1): the device token lives in the iOS
// Keychain, keyed by relay URL. The relay URL itself is a plain default.

import Foundation
import Security

struct DeviceCredentials: Codable, Equatable {
    var relayUrl: String
    var deviceId: String
    var deviceToken: String
}

enum Keychain {
    private static let service = "sh.arbos.ios.relay"

    static func load(relayUrl: String) -> DeviceCredentials? {
        var query = baseQuery(account: relayUrl)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data else { return nil }
        return try? JSONDecoder().decode(DeviceCredentials.self, from: data)
    }

    static func store(_ creds: DeviceCredentials) throws {
        let data = try JSONEncoder().encode(creds)
        let query = baseQuery(account: creds.relayUrl)
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlock,
        ]
        let status = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            var add = query
            for (k, v) in attributes { add[k] = v }
            let addStatus = SecItemAdd(add as CFDictionary, nil)
            guard addStatus == errSecSuccess else { throw KeychainError.status(addStatus) }
        } else if status != errSecSuccess {
            throw KeychainError.status(status)
        }
    }

    static func delete(relayUrl: String) {
        SecItemDelete(baseQuery(account: relayUrl) as CFDictionary)
    }

    private static func baseQuery(account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    enum KeychainError: Error, LocalizedError {
        case status(OSStatus)

        var errorDescription: String? {
            switch self {
            case .status(let s):
                return "Keychain error \(s)"
            }
        }
    }
}
