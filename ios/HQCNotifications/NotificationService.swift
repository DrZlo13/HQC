import CryptoKit
import Foundation
import Security
import UserNotifications
import os

private let log = Logger(subsystem: "xyz.drewnja.hqc.notifications", category: "seal")

/// Opens a sealed notice (SPEC 0.218) on the device: the hub sealed its content with this
/// device's key, so Apple and a relay carried only a generic alert and ciphertext. The real
/// title, text, category, thread and fields (session, request, options) are restored here, before
/// iOS shows it; what cannot be opened shows as the generic alert it came as.
final class NotificationService: UNNotificationServiceExtension {
    private var deliver: ((UNNotificationContent) -> Void)?
    private var fallback: UNNotificationContent?

    override func didReceive(_ request: UNNotificationRequest,
                             withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        deliver = contentHandler
        fallback = request.content
        let opened = Self.open(request.content)
        log.info("notice: sealed=\(request.content.userInfo["enc"] != nil, privacy: .public) opened=\(opened != nil, privacy: .public)")
        contentHandler(opened ?? request.content)
    }

    override func serviceExtensionTimeWillExpire() {
        if let deliver, let fallback { deliver(fallback) }
    }

    static func open(_ content: UNNotificationContent) -> UNNotificationContent? {
        guard let enc = content.userInfo["enc"] as? String,
              let boxed = Data(base64Encoded: enc) else { return nil }
        guard let key = NoticeKeychain.read() else {
            log.error("no notice key in the shared keychain")
            return nil
        }
        guard
              let box = try? AES.GCM.SealedBox(combined: boxed),
              let plain = try? AES.GCM.open(box, using: SymmetricKey(data: key)),
              let fields = (try? JSONSerialization.jsonObject(with: plain)) as? [String: Any],
              let out = content.mutableCopy() as? UNMutableNotificationContent
        else { return nil }
        if let title = fields["title"] as? String { out.title = title }
        if let body = fields["body"] as? String { out.body = body }
        if let category = fields["category"] as? String { out.categoryIdentifier = category }
        if let session = fields["session_id"] as? String { out.threadIdentifier = session }
        var info = content.userInfo
        info.removeValue(forKey: "enc")
        info.removeValue(forKey: "v")
        for (k, v) in fields where !["title", "body", "category"].contains(k) { info[k] = v }
        out.userInfo = info
        return out
    }
}

/// The notice key the app made, read from the keychain group the app and this extension share.
enum NoticeKeychain {
    static func read() -> Data? {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "xyz.drewnja.hqc.notice-key",
            kSecAttrAccount as String: "notice-key",
            kSecReturnData as String: true,
        ]
        if let group = Bundle.main.object(forInfoDictionaryKey: "HQCKeychainGroup") as? String, !group.isEmpty {
            query[kSecAttrAccessGroup as String] = group
        }
        var out: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess else { return nil }
        return out as? Data
    }
}
