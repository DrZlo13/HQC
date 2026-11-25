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
}

// What the File and Go menu items do, each run in the active tab's page; New Session in
// Project is also the app's global shortcut. Every other key is the web client's own, set in
// the client's Settings, so the app does not take them.
enum MenuAction: String {
    case newSession
    case newSessionInProject
    case archiveSession
    case previousSession
    case nextSession
    case previousProject
    case nextProject
    case back
    case forward

    var title: String {
        switch self {
        case .newSession: return "New Session"
        case .newSessionInProject: return "New Session in Project"
        case .archiveSession: return "Archive Session"
        case .previousSession: return "Previous Session"
        case .nextSession: return "Next Session"
        case .previousProject: return "Previous Project"
        case .nextProject: return "Next Project"
        case .back: return "Back"
        case .forward: return "Forward"
        }
    }
}

// The app's one shortcut, New Session in Project's: a system hot key working from any app,
// and the menu item showing the same keys.
final class ShortcutMonitor {

    static let shared = ShortcutMonitor()

    // The action of the global shortcut.
    static let globalAction = MenuAction.newSessionInProject

    private static let defaultsKey = "shortcut.newSessionInProject"

    // The shortcut kept in UserDefaults: ⌃⌥⌘N until the user records another one (not ⇧⌘N: a
    // global one would take it from every other app, Finder's New Folder); nil when the user
    // cleared it (stored as empty data).
    static var shortcut: Shortcut? {
        get {
            guard let data = UserDefaults.standard.data(forKey: defaultsKey) else {
                return Shortcut(keyCode: kVK_ANSI_N, flags: [.control, .option, .command], key: "n")
            }
            return try? JSONDecoder().decode(Shortcut.self, from: data)
        }
        set {
            let data = newValue.flatMap { try? JSONEncoder().encode($0) } ?? Data()
            UserDefaults.standard.set(data, forKey: defaultsKey)
            shared.updateMenuItem()
            shared.updateHotKey()
        }
    }

    // Called when the shortcut is pressed.
    var onPress: (() -> Void)?

    // On while the recorder in Settings takes the keys; the hot key is let go meanwhile, so its
    // keys reach the recorder.
    var isPaused = false {
        didSet { updateHotKey() }
    }

    // The global action's menu item; its key equivalent mirrors the shortcut.
    var menuItem: NSMenuItem?

    private var started = false
    private var hotKey: EventHotKeyRef?

    func start() {
        guard !started else { return }
        started = true
        // The hot key's presses, delivered on the main thread.
        var pressed = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        InstallEventHandler(GetApplicationEventTarget(), { _, _, _ in
            ShortcutMonitor.shared.onPress?()
            return noErr
        }, 1, &pressed, nil, nil)
        updateMenuItem()
        updateHotKey()
    }

    // Registers the shortcut as the system hot key, again whenever it changes; none while
    // paused or with the shortcut cleared.
    private func updateHotKey() {
        if let hotKey {
            UnregisterEventHotKey(hotKey)
            self.hotKey = nil
        }
        guard started, !isPaused, let shortcut = Self.shortcut else { return }
        var modifiers: UInt32 = 0
        if shortcut.flags.contains(.command) { modifiers |= UInt32(cmdKey) }
        if shortcut.flags.contains(.option) { modifiers |= UInt32(optionKey) }
        if shortcut.flags.contains(.control) { modifiers |= UInt32(controlKey) }
        if shortcut.flags.contains(.shift) { modifiers |= UInt32(shiftKey) }
        // Signature "HQC ".
        RegisterEventHotKey(UInt32(shortcut.keyCode), modifiers, EventHotKeyID(signature: 0x4851_4320, id: 1),
                            GetApplicationEventTarget(), 0, &hotKey)
    }

    private func updateMenuItem() {
        let shortcut = Self.shortcut
        menuItem?.keyEquivalent = shortcut?.key ?? ""
        menuItem?.keyEquivalentModifierMask = shortcut?.flags ?? []
    }
}
