import AppKit
import Carbon.HIToolbox

extension Notification.Name {
    // Posted when any action's shortcut is recorded or cleared.
    static let shortcutsDidChange = Notification.Name("ShortcutsDidChange")
}

// A recorded shortcut: the key, its modifiers, the menu key equivalent and how it reads in the UI.
struct Shortcut: Codable, Equatable {
    let keyCode: UInt16
    let modifiers: UInt
    let key: String
    let display: String

    // The modifiers a shortcut is made of.
    static let relevantFlags: NSEvent.ModifierFlags = [.command, .option, .control, .shift]

    var flags: NSEvent.ModifierFlags { NSEvent.ModifierFlags(rawValue: modifiers) }

    // `key` is the menu key equivalent: the character the key types without modifiers (an
    // arrow's or a function key's is its function-key character).
    init(keyCode: Int, flags: NSEvent.ModifierFlags, key: String) {
        self.keyCode = UInt16(keyCode)
        modifiers = flags.rawValue
        self.key = key
        display = Self.display(flags: flags, key: key)
    }

    // The shortcut of a keyDown event; nil unless at least one of ⌘ ⌃ ⌥ is held. The event's
    // other flags (an arrow's .numericPad and .function) are not part of it.
    init?(event: NSEvent) {
        let flags = event.modifierFlags.intersection(Self.relevantFlags)
        guard !flags.isDisjoint(with: [.command, .control, .option]) else { return nil }
        self.init(keyCode: Int(event.keyCode), flags: flags,
                  key: (event.charactersIgnoringModifiers ?? "").lowercased())
    }

    // Modifier symbols in the order ⌃⌥⇧⌘, then the key: arrows as ↑ ↓ ← →, F1–F20 by name,
    // the space bar as "Space", any other key as its character in upper case.
    private static func display(flags: NSEvent.ModifierFlags, key: String) -> String {
        var symbols = ""
        if flags.contains(.control) { symbols += "⌃" }
        if flags.contains(.option) { symbols += "⌥" }
        if flags.contains(.shift) { symbols += "⇧" }
        if flags.contains(.command) { symbols += "⌘" }

        let name: String
        switch key.unicodeScalars.first.map({ Int($0.value) }) ?? 0 {
        case NSUpArrowFunctionKey: name = "↑"
        case NSDownArrowFunctionKey: name = "↓"
        case NSLeftArrowFunctionKey: name = "←"
        case NSRightArrowFunctionKey: name = "→"
        case let c where (NSF1FunctionKey...NSF20FunctionKey).contains(c): name = "F\(c - NSF1FunctionKey + 1)"
        default: name = key == " " ? "Space" : key.uppercased()
        }
        return symbols + name
    }

    func matches(_ event: NSEvent) -> Bool {
        event.keyCode == keyCode && event.modifierFlags.intersection(Self.relevantFlags) == flags
    }

    // The same keys, whatever the stored character and display.
    func sameKeys(as other: Shortcut) -> Bool {
        keyCode == other.keyCode && modifiers == other.modifiers
    }
}

// What the app's shortcuts do, each run in the active tab's page.
enum ShortcutAction: String, CaseIterable {
    case newSession
    case archiveSession
    case previousSession
    case nextSession
    case previousProject
    case nextProject

    var title: String {
        switch self {
        case .newSession: return "New Session"
        case .archiveSession: return "Archive Session"
        case .previousSession: return "Previous Session"
        case .nextSession: return "Next Session"
        case .previousProject: return "Previous Project"
        case .nextProject: return "Next Project"
        }
    }

    private var defaultsKey: String { "shortcut.\(rawValue)" }

    private static let up = String(UnicodeScalar(UInt16(NSUpArrowFunctionKey))!)
    private static let down = String(UnicodeScalar(UInt16(NSDownArrowFunctionKey))!)

    var defaultShortcut: Shortcut {
        switch self {
        case .newSession: return Shortcut(keyCode: kVK_ANSI_N, flags: [.command], key: "n")
        case .archiveSession: return Shortcut(keyCode: kVK_ANSI_A, flags: [.control, .command], key: "a")
        case .previousSession: return Shortcut(keyCode: kVK_UpArrow, flags: [.option, .command], key: Self.up)
        case .nextSession: return Shortcut(keyCode: kVK_DownArrow, flags: [.option, .command], key: Self.down)
        case .previousProject: return Shortcut(keyCode: kVK_UpArrow, flags: [.control, .command], key: Self.up)
        case .nextProject: return Shortcut(keyCode: kVK_DownArrow, flags: [.control, .command], key: Self.down)
        }
    }

    // The action's shortcut kept in UserDefaults: the default until the user records another
    // one; nil when the user cleared it (stored as empty data).
    var shortcut: Shortcut? {
        get {
            guard let data = UserDefaults.standard.data(forKey: defaultsKey) else { return defaultShortcut }
            return try? JSONDecoder().decode(Shortcut.self, from: data)
        }
        nonmutating set {
            let data = newValue.flatMap { try? JSONEncoder().encode($0) } ?? Data()
            UserDefaults.standard.set(data, forKey: defaultsKey)
            NotificationCenter.default.post(name: .shortcutsDidChange, object: nil)
        }
    }

    // Records `shortcut` for this action and clears it from any other action that had the
    // same keys, which would otherwise never fire.
    func record(_ shortcut: Shortcut) {
        for other in Self.allCases where other != self {
            if let theirs = other.shortcut, theirs.sameKeys(as: shortcut) {
                other.shortcut = nil
            }
        }
        self.shortcut = shortcut
    }
}

// The shortcuts inside the app: one local key monitor, which sees the keys before the web
// page and the menu do, and the menu items showing the same keys.
final class ShortcutMonitor {

    static let shared = ShortcutMonitor()

    // Called with the action whose shortcut was pressed while the app is active.
    var onPress: ((ShortcutAction) -> Void)?

    // On while a recorder in Settings takes the keys.
    var isPaused = false

    // The menu item of each action; its key equivalent mirrors the action's shortcut.
    var menuItems: [ShortcutAction: NSMenuItem] = [:]

    private var monitor: Any?

    func start() {
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self, !self.isPaused,
                  let action = ShortcutAction.allCases.first(where: { $0.shortcut?.matches(event) == true }) else {
                return event
            }
            self.onPress?(action)
            return nil
        }
        NotificationCenter.default.addObserver(forName: .shortcutsDidChange, object: nil, queue: .main) { [weak self] _ in
            self?.updateMenuItems()
        }
        updateMenuItems()
    }

    private func updateMenuItems() {
        for (action, item) in menuItems {
            let shortcut = action.shortcut
            item.keyEquivalent = shortcut?.key ?? ""
            item.keyEquivalentModifierMask = shortcut?.flags ?? []
        }
    }
}
