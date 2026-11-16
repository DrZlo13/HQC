import AppKit
import AudioToolbox
import UserNotifications

// One event the notification script reports (NotificationScript).
struct PageEvent {
    let sessionID: String
    let tag: String
    // The event's type; only an `approval.request` card stays until it is answered.
    let type: String
    // The project's name; the session's title when the project is not known.
    let title: String
    // The session's title, or empty when it is the title.
    let subtitle: String
    // The whole description, question or message, up to 600 characters.
    let body: String
    // A permission request's tool.
    let tool: String?
    // How many questions a question has after the first.
    let more: Int
    // The project's colour (nil: not known) and the letters of its tile.
    let color: NSColor?
    let glyph: String
    // The page shows this session now.
    let viewing: Bool
    // A permission request with Allow and Deny buttons; an option is nil when the request has
    // none of that kind.
    let requestID: String?
    let allowOption: String?
    let denyOption: String?
}

// The app's own notification cards (NotificationCards) for the events the pages' notification
// script reports, while the app runs. A permission request has Allow and Deny buttons and stays
// until the hub no longer lists it open; every other card goes the time set in Settings after it
// appears, counted again from when the pointer leaves it.
final class Notifier {

    static let shared = Notifier()

    // How long a card other than a permission request stays on screen, in seconds: Settings ▸
    // Notifications, kept in UserDefaults; each new card takes the value of the moment.
    static let lifetimeRange = 1...120
    private static let lifetimeDefault = 5
    private static let lifetimeKey = "notificationLifetime"
    static var lifetime: Int {
        get {
            // 0 when nothing is saved.
            let saved = UserDefaults.standard.integer(forKey: lifetimeKey)
            return lifetimeRange.contains(saved) ? saved : lifetimeDefault
        }
        set {
            let value = min(max(newValue, lifetimeRange.lowerBound), lifetimeRange.upperBound)
            UserDefaults.standard.set(value, forKey: lifetimeKey)
        }
    }

    // Whether the server's page is in front: the app active, the window visible and that
    // server's tab the active one.
    var isShowing: ((_ serverID: UUID) -> Bool)?

    // Opens a session from a clicked card: the window, the server's tab, then `#/s/<session>`
    // in its page.
    var onOpen: ((_ serverID: UUID, _ sessionID: String) -> Void)?

    // Answers a permission request in the server's page; `done` gets whether the hub took it.
    var onAnswer: ((_ serverID: UUID, _ sessionID: String, _ requestID: String, _ optionID: String,
                    _ done: @escaping (Bool) -> Void) -> Void)?

    private let cards = NotificationCards()

    // The sound macOS plays for an app's notification when none is set: Notification Center
    // plays the tone `texttone:Rebound`, this file, as a system sound (its log, macOS 15).
    private static let sound: SystemSoundID? = {
        let url = URL(fileURLWithPath: "/System/Library/PrivateFrameworks/ToneLibrary.framework/Versions/A/Resources/AlertTones/EncoreInfinitum/Rebound-EncoreInfinitum.caf")
        var id: SystemSoundID = 0
        return AudioServicesCreateSystemSoundID(url as CFURL, &id) == noErr ? id : nil
    }()

    // Takes away what the app posted to Notification Center before it drew its own cards, and
    // asks for the permission the Dock badge needs (macOS asks the user once); the answer goes to
    // the system log.
    func start() {
        let center = UNUserNotificationCenter.current()
        center.removeAllDeliveredNotifications()
        center.requestAuthorization(options: [.badge]) { granted, error in
            NSLog("%@", "HQC badge: granted=\(granted) error=\(error.map { "\($0)" } ?? "nil")")
        }
    }

    // Shows one event of a session, with macOS's notification sound. Skipped when the user is
    // looking at that session now. The identifier is `<server id>:<tag>`, so a later event with
    // the same tag replaces the earlier card in its place.
    func notify(server: Server, event: PageEvent) {
        if event.viewing, isShowing?(server.id) == true { return }
        let identifier = "\(server.id.uuidString):\(event.tag)"
        let serverID = server.id
        let session = event.sessionID
        let open: () -> Void = { [weak self] in self?.onOpen?(serverID, session) }

        let card = cards.show(identifier: identifier, event: event,
                              lifetime: event.type == "approval.request" ? nil : TimeInterval(Self.lifetime))
        // A click opens the session.
        card.onOpen = { [weak self] in
            self?.cards.remove(identifier: identifier)
            open()
        }
        // Allow or Deny answers the request without opening the app, and the card goes; when the
        // request has no such option, or the hub does not take the answer, the session opens.
        card.onAnswer = { [weak self, weak card] allow in
            guard let self else { return }
            guard let request = event.requestID,
                  let option = allow ? event.allowOption : event.denyOption,
                  let answer = self.onAnswer else { return open() }
            card?.isAnswering = true
            answer(serverID, session, request, option) { ok in
                card?.isAnswering = false
                if ok {
                    self.cards.remove(identifier: identifier)
                } else {
                    open()
                }
            }
        }
        if let sound = Self.sound { AudioServicesPlaySystemSound(sound) }
    }

    // The requests the hub lists open for a server, as tags: that server's permission request
    // cards that are not among them were answered or went away.
    func keepOpen(server: Server, tags: Set<String>) {
        let prefix = "\(server.id.uuidString):"
        cards.removeAll { card in
            card.identifier.hasPrefix(prefix)
                && card.event.type == "approval.request"
                && !tags.contains(String(card.identifier.dropFirst(prefix.count)))
        }
    }
}
