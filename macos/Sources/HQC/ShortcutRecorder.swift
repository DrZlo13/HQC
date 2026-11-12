import AppKit
import Carbon.HIToolbox

// A button that shows an action's shortcut and records a new one: click, then press a key
// with at least one of ⌘ ⌃ ⌥. Esc cancels, Delete clears the shortcut.
final class ShortcutRecorder: NSButton {

    // The recorder taking the keys now; at most one records at a time.
    private static weak var recording: ShortcutRecorder?

    private let shortcutAction: ShortcutAction

    // Local monitor that takes every keyDown while recording, so ⌘ combinations reach it
    // before the menu does.
    private var monitor: Any?

    private var observer: NSObjectProtocol?

    init(action: ShortcutAction) {
        shortcutAction = action
        super.init(frame: .zero)
        bezelStyle = .rounded
        target = self
        self.action = #selector(startRecording)
        updateTitle()
        // Recording here can clear another action's shortcut: its button follows.
        observer = NotificationCenter.default.addObserver(forName: .shortcutsDidChange, object: nil,
                                                          queue: .main) { [weak self] _ in
            guard let self, self.monitor == nil else { return }
            self.updateTitle()
        }
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    deinit {
        if let observer { NotificationCenter.default.removeObserver(observer) }
    }

    @objc private func startRecording() {
        guard monitor == nil else { return }
        Self.recording?.stopRecording()
        Self.recording = self
        // The shortcut itself would run its action instead of being recorded.
        ShortcutMonitor.shared.isPaused = true
        title = "Press a shortcut…"
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            self?.handle(event)
            return nil
        }
    }

    private func handle(_ event: NSEvent) {
        switch Int(event.keyCode) {
        case kVK_Escape:
            stopRecording()
        case kVK_Delete, kVK_ForwardDelete:
            shortcutAction.shortcut = nil
            stopRecording()
        default:
            // A key without ⌘ ⌃ ⌥ is ignored; recording goes on.
            guard let shortcut = Shortcut(event: event) else { return }
            shortcutAction.record(shortcut)
            stopRecording()
        }
    }

    // Ends recording (if it is on) and puts the shortcuts back to work.
    func stopRecording() {
        guard let monitor else { return }
        NSEvent.removeMonitor(monitor)
        self.monitor = nil
        if Self.recording === self { Self.recording = nil }
        ShortcutMonitor.shared.isPaused = false
        updateTitle()
    }

    private func updateTitle() {
        title = shortcutAction.shortcut?.display ?? "Record Shortcut"
    }
}
