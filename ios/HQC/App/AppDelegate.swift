import UIKit
import UserNotifications

/// Apple push (SPEC §16): registers for notices, answers an approval from the notification's
/// Allow and Deny buttons without opening the app, and opens the session on a tap.
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    /// Set by the app once its model exists.
    @MainActor static var model: AppModel?

    static let allowAction = "ALLOW"
    static let denyAction = "DENY"

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        let allow = UNNotificationAction(identifier: Self.allowAction, title: "Allow once",
                                         options: [.authenticationRequired])
        let deny = UNNotificationAction(identifier: Self.denyAction, title: "Deny",
                                        options: [.destructive, .authenticationRequired])
        center.setNotificationCategories([
            UNNotificationCategory(identifier: "APPROVAL", actions: [allow, deny], intentIdentifiers: []),
            UNNotificationCategory(identifier: "QUESTION", actions: [], intentIdentifiers: []),
            UNNotificationCategory(identifier: "MESSAGE", actions: [], intentIdentifiers: []),
        ])
        return true
    }

    /// Asks once, after sign-in, then registers with APNs.
    @MainActor
    static func requestPush() async {
        let center = UNUserNotificationCenter.current()
        let granted = (try? await center.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
        if granted { UIApplication.shared.registerForRemoteNotifications() }
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        Task { @MainActor in Self.model?.didRegister(deviceToken: deviceToken) }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        Task { @MainActor in Self.model?.lastError = "Notifications: \(error.localizedDescription)" }
    }

    /// In the foreground: a banner, unless that session's chat is on screen.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        let info = notification.request.content.userInfo
        let session = info["session_id"] as? String
        let watching = await MainActor.run { Self.model?.watching }
        if let session, session == watching { return [] }
        return [.banner, .sound, .list]
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse
    ) async {
        let info = response.notification.request.content.userInfo
        guard let session = info["session_id"] as? String else { return }
        let open = (info["open_session_id"] as? String) ?? session
        switch response.actionIdentifier {
        case Self.allowAction, Self.denyAction:
            let wanted = response.actionIdentifier == Self.allowAction ? ["once", "session"] : ["deny"]
            let options = (info["options"] as? [[String: Any]]) ?? []
            let option = wanted.lazy.compactMap { w in
                options.first { ($0["canonical"] as? String) == w }?["id"] as? String
            }.first
            guard let request = info["request_id"] as? String, let option else { return }
            guard let model = await MainActor.run(body: { Self.model }) else { return }
            // Answered before returning, so the system keeps the app awake until it is sent.
            if model.client == nil { await model.connect() }
            try? await model.client?.approve(sessionID: session, requestID: request, optionID: option)
        default:
            await MainActor.run { Self.model?.open(sessionID: open) }
        }
    }
}
