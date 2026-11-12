import AppKit
import Carbon.HIToolbox

// A recorded shortcut: the key, its modifiers, the menu key equivalent and how it reads in the UI.
struct Shortcut: Codable, Equatable {
    let keyCode: UInt16
    let modifiers: UInt
    let key: String
    let display: String

    // The modifiers a shortcut is made of.
    static let relevantFlags: NSEvent.ModifierFlags = [.command, .option, .control, .shift]

    private static let defaultsKey = "newSessionShortcut"

    static let standard = Shortcut(keyCode: UInt16(kVK_ANSI_N), modifiers: NSEvent.ModifierFlags.command.rawValue,
                                   key: "n", display: "⌘N")

    var flags: NSEvent.ModifierFlags { NSEvent.ModifierFlags(rawValue: modifiers) }

    // The new-session shortcut kept in UserDefaults: ⌘N until the user records another one;
    // nil when the user cleared it (stored as empty data).
    static var saved: Shortcut? {
        get {
            guard let data = UserDefaults.standard.data(forKey: defaultsKey) else { return standard }
            return try? JSONDecoder().decode(Shortcut.self, from: data)
        }
        set {
            let data = newValue.flatMap { try? JSONEncoder().encode($0) } ?? Data()
            UserDefaults.standard.set(data, forKey: defaultsKey)
        }
    }

    private init(keyCode: UInt16, modifiers: UInt, key: String, display: String) {
        self.keyCode = keyCode
        self.modifiers = modifiers
        self.key = key
        self.display = display
    }

    // The shortcut of a keyDown event; nil unless at least one of ⌘ ⌃ ⌥ is held.
    init?(event: NSEvent) {
        let flags = event.modifierFlags.intersection(Self.relevantFlags)
        guard !flags.isDisjoint(with: [.command, .control, .option]) else { return nil }

        var symbols = ""
        if flags.contains(.control) { symbols += "⌃" }
        if flags.contains(.option) { symbols += "⌥" }
        if flags.contains(.shift) { symbols += "⇧" }
        if flags.contains(.command) { symbols += "⌘" }

        let key = event.charactersIgnoringModifiers ?? ""
        keyCode = event.keyCode
        modifiers = flags.rawValue
        self.key = key.lowercased()
        display = symbols + (key == " " ? "Space" : key.uppercased())
    }

    func matches(_ event: NSEvent) -> Bool {
        event.keyCode == keyCode && event.modifierFlags.intersection(Self.relevantFlags) == flags
    }
}

// The new-session shortcut inside the app: a local key monitor, which sees the keys before the
// web page and the menu do, and the File ▸ New Session menu item showing the same keys.
final class NewSessionShortcut {

    static let shared = NewSessionShortcut()

    // Called when the shortcut is pressed while the app is active.
    var onPress: (() -> Void)?

    // On while the recorder in Settings takes the keys.
    var isPaused = false

    // File ▸ New Session; its key equivalent mirrors the saved shortcut.
    weak var menuItem: NSMenuItem?

    private var monitor: Any?

    func start() {
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self, !self.isPaused, let shortcut = Shortcut.saved, shortcut.matches(event) else {
                return event
            }
            self.onPress?()
            return nil
        }
        updateMenuItem()
    }

    func updateMenuItem() {
        let shortcut = Shortcut.saved
        menuItem?.keyEquivalent = shortcut?.key ?? ""
        menuItem?.keyEquivalentModifierMask = shortcut?.flags ?? []
    }
}
