import CryptoKit
import Foundation
import Security

/// The key this device makes for its notices (SPEC 0.238): 32 random bytes, kept in the keychain
/// group the app shares with its notification extension, sent to the hub with the device token.
/// The hub seals each notice with it; only this device can open them.
nonisolated enum NoticeKey {
    private static let service = "xyz.drewnja.hqc.notice-key"
    private static let account = "notice-key"
    private static var group: String? {
        (Bundle.main.object(forInfoDictionaryKey: "HQCKeychainGroup") as? String).flatMap { $0.isEmpty ? nil : $0 }
    }

    /// The key, made once and kept from then on.
    static func current() -> Data? {
        #if DEBUG
        // Tests on a simulator seal notices with a key of their own: `-HQCTestNoticeKey <base64>`.
        if let i = CommandLine.arguments.firstIndex(of: "-HQCTestNoticeKey"), i + 1 < CommandLine.arguments.count,
           let k = Data(base64Encoded: CommandLine.arguments[i + 1]), k.count == 32 {
            SecItemDelete(query() as CFDictionary)
            _ = store(k)
            return k
        }
        #endif
        if let k = read() { return k }
        let key = SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }
        return store(key) ? key : nil
    }

    static var base64: String? { current()?.base64EncodedString() }

    private static func query() -> [String: Any] {
        var q: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        if let group { q[kSecAttrAccessGroup as String] = group }
        return q
    }

    private static func read() -> Data? {
        var q = query()
        q[kSecReturnData as String] = true
        var out: CFTypeRef?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess else { return nil }
        return out as? Data
    }

    private static func store(_ key: Data) -> Bool {
        var q = query()
        q[kSecValueData as String] = key
        // The extension runs while the phone is locked, after its first unlock.
        q[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        return SecItemAdd(q as CFDictionary, nil) == errSecSuccess
    }
}
