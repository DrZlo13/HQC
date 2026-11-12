import AppKit
import UserNotifications

// One event the notification script reports (NotificationScript).
struct PageEvent {
    let sessionID: String
    let tag: String
    // The event's type; only `approval.request` stays in Notification Center.
    let type: String
    // The project's name; the session's title when the project is not known.
    let title: String
    // The session's title, or empty when it is the title.
    let subtitle: String
    let body: String
    // The page shows this session now.
    let viewing: Bool
    // A permission request with Allow and Deny buttons; an option is nil when the request has
    // none of that kind.
    let requestID: String?
    let allowOption: String?
    let denyOption: String?
}

// macOS notifications for the events the pages' notification script reports, while the app
// runs. A permission request has Allow and Deny buttons and stays until the hub no longer lists
// it open; every other notification is taken away 5 s after it is posted.
final class Notifier: NSObject, UNUserNotificationCenterDelegate {

    static let shared = Notifier()

    private static let approvalCategory = "hqc.approval"
    private static let allowAction = "allow"
    private static let denyAction = "deny"
    // In a permission request's userInfo.
    private static let approvalKey = "approval"

    // How long a notification other than a permission request stays.
    private static let lifetime: TimeInterval = 5

    // Whether the server's page is in front: the app active, the window visible and that
    // server's tab the active one.
    var isShowing: ((_ serverID: UUID) -> Bool)?

    // Opens a session from a clicked notification: the window, the server's tab, then
    // `#/s/<session>` in its page.
    var onOpen: ((_ serverID: UUID, _ sessionID: String) -> Void)?

    // Answers a permission request in the server's page; `done` gets whether the hub took it.
    var onAnswer: ((_ serverID: UUID, _ sessionID: String, _ requestID: String, _ optionID: String,
                    _ done: @escaping (Bool) -> Void) -> Void)?

    // Per notification identifier: the post its removal belongs to, so a later post with the
    // same identifier is not taken away by the earlier one's timer.
    private var removals: [String: UUID] = [:]

    // Becomes the notification center's delegate, sets up the permission request's buttons
    // and asks for permission (macOS asks the user once); the answer goes to the system log.
    func start() {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        let allow = UNNotificationAction(identifier: Self.allowAction, title: "Allow", options: [])
        let deny = UNNotificationAction(identifier: Self.denyAction, title: "Deny", options: [.destructive])
        center.setNotificationCategories([
            UNNotificationCategory(identifier: Self.approvalCategory, actions: [allow, deny],
                                   intentIdentifiers: [], options: []),
        ])
        center.requestAuthorization(options: [.alert, .sound, .badge]) { granted, error in
            NSLog("%@", "HQC notifications: granted=\(granted) error=\(error.map { "\($0)" } ?? "nil")")
        }
    }

    // Posts one event of a session. Skipped when the user is looking at that session now.
    // The identifier is `<server id>:<tag>`, so a later notification with the same tag
    // replaces the earlier one.
    func notify(server: Server, event: PageEvent) {
        if event.viewing, isShowing?(server.id) == true { return }
        let content = UNMutableNotificationContent()
        content.title = event.title
        content.subtitle = event.subtitle
        content.body = event.body
        content.sound = .default
        var info: [String: Any] = ["server": server.id.uuidString, "session": event.sessionID]
        // A permission request, with buttons or not (a publication): keepOpen takes it away.
        if event.type == "approval.request" { info[Self.approvalKey] = true }
        if let request = event.requestID {
            content.categoryIdentifier = Self.approvalCategory
            info["request"] = request
            if let allow = event.allowOption { info["allow"] = allow }
            if let deny = event.denyOption { info["deny"] = deny }
        }
        content.userInfo = info
        let identifier = "\(server.id.uuidString):\(event.tag)"
        let center = UNUserNotificationCenter.current()
        center.add(UNNotificationRequest(identifier: identifier, content: content, trigger: nil))

        // The Alerts style keeps a notification until it is dismissed: only a permission
        // request stays.
        guard event.type != "approval.request" else { return }
        let token = UUID()
        removals[identifier] = token
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.lifetime) { [weak self] in
            guard let self, self.removals[identifier] == token else { return }
            self.removals[identifier] = nil
            center.removeDeliveredNotifications(withIdentifiers: [identifier])
        }
    }

    // The requests the hub lists open for a server, as tags: that server's permission requests
    // in Notification Center that are not among them were answered or went away, also those
    // posted before the page got a new document or before the app last quit.
    func keepOpen(server: Server, tags: Set<String>) {
        let prefix = "\(server.id.uuidString):"
        let center = UNUserNotificationCenter.current()
        center.getDeliveredNotifications { delivered in
            let gone = delivered.map(\.request).filter { request in
                request.identifier.hasPrefix(prefix)
                    && request.content.userInfo[Self.approvalKey] as? Bool == true
                    && !tags.contains(String(request.identifier.dropFirst(prefix.count)))
            }.map(\.identifier)
            if !gone.isEmpty { center.removeDeliveredNotifications(withIdentifiers: gone) }
        }
    }

    // MARK: - UNUserNotificationCenterDelegate

    // Shown also while the app is in front.
    func userNotificationCenter(_ center: UNUserNotificationCenter,
                                willPresent notification: UNNotification,
                                withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .list, .sound])
    }

    // A click opens the session. Allow or Deny answers the request in the background; when the
    // request has no such option, or the hub does not take the answer, the session opens instead.
    func userNotificationCenter(_ center: UNUserNotificationCenter,
                                didReceive response: UNNotificationResponse,
                                withCompletionHandler completionHandler: @escaping () -> Void) {
        let info = response.notification.request.content.userInfo
        let action = response.actionIdentifier
        DispatchQueue.main.async {
            guard let server = (info["server"] as? String).flatMap(UUID.init(uuidString:)),
                  let session = info["session"] as? String else {
                completionHandler()
                return
            }
            let open = { self.onOpen?(server, session) }
            guard action == Self.allowAction || action == Self.denyAction else {
                if action == UNNotificationDefaultActionIdentifier { open() }
                completionHandler()
                return
            }
            guard let request = info["request"] as? String,
                  let option = info[action == Self.allowAction ? "allow" : "deny"] as? String,
                  let answer = self.onAnswer else {
                open()
                completionHandler()
                return
            }
            answer(server, session, request, option) { ok in
                if !ok { open() }
                completionHandler()
            }
        }
    }
}
