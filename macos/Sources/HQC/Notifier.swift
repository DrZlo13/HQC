import AppKit
import UserNotifications

// macOS notifications for the events the pages' notification script reports
// (NotificationScript), while the app runs.
final class Notifier: NSObject, UNUserNotificationCenterDelegate {

    static let shared = Notifier()

    // Whether the server's page is in front: the app active, the window visible and that
    // server's tab the active one.
    var isShowing: ((_ serverID: UUID) -> Bool)?

    // Opens a session from a clicked notification: the window, the server's tab, then
    // `#/s/<session>` in its page.
    var onOpen: ((_ serverID: UUID, _ sessionID: String) -> Void)?

    // Becomes the notification center's delegate and asks for permission (macOS asks the
    // user once); the answer goes to the system log.
    func start() {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        center.requestAuthorization(options: [.alert, .sound]) { granted, error in
            NSLog("%@", "HQC notifications: granted=\(granted) error=\(error.map { "\($0)" } ?? "nil")")
        }
    }

    // Posts one event of a session. Skipped when the user is looking at that session now.
    // The identifier is `<server id>:<tag>`, so a later notification with the same tag
    // replaces the earlier one.
    func notify(server: Server, sessionID: String, tag: String, title: String, body: String, viewing: Bool) {
        if viewing, isShowing?(server.id) == true { return }
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        content.userInfo = ["server": server.id.uuidString, "session": sessionID]
        let request = UNNotificationRequest(identifier: "\(server.id.uuidString):\(tag)",
                                            content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }

    // MARK: - UNUserNotificationCenterDelegate

    // Shown also while the app is in front.
    func userNotificationCenter(_ center: UNUserNotificationCenter,
                                willPresent notification: UNNotification,
                                withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .list, .sound])
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter,
                                didReceive response: UNNotificationResponse,
                                withCompletionHandler completionHandler: @escaping () -> Void) {
        let info = response.notification.request.content.userInfo
        if let server = (info["server"] as? String).flatMap(UUID.init(uuidString:)),
           let session = info["session"] as? String {
            DispatchQueue.main.async { self.onOpen?(server, session) }
        }
        completionHandler()
    }
}
