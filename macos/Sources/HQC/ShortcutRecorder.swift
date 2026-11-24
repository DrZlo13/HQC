import AppKit
import Carbon.HIToolbox

// A button that shows the global shortcut and records a new one: click, then press a key
// with at least one of ⌘ ⌃ ⌥. Esc cancels, Delete clears the shortcut.
final class ShortcutRecorder: NSButton {

    // Local monitor that takes every keyDown while recording, so ⌘ combinations reach it
    // before the menu does.
    private var monitor: Any?

    init() {
        super.init(frame: .zero)
        bezelStyle = .rounded
        target = self
        self.action = #selector(startRecording)
        updateTitle()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    @objc private func startRecording() {
        guard monitor == nil else { return }
        // The shortcut itself would run its action instead of being recorded.
        ShortcutMonitor.shared.isPaused = true
        title = "Press a shortcut…"
        // Only the keys of the Settings window: typing in another sheet (a server's address)
        // goes on as usual and is never recorded.
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self, event.window === self.window else { return event }
            self.handle(event)
            return nil
        }
    }

    private func handle(_ event: NSEvent) {
        switch Int(event.keyCode) {
        case kVK_Escape:
            stopRecording()
        case kVK_Delete, kVK_ForwardDelete:
            ShortcutMonitor.shortcut = nil
            stopRecording()
        default:
            // A key without ⌘ ⌃ ⌥ is ignored; recording goes on.
            guard let shortcut = Shortcut(event: event) else { return }
            ShortcutMonitor.shortcut = shortcut
            stopRecording()
        }
    }

    // Ends recording (if it is on) and puts the shortcut back to work.
    func stopRecording() {
        guard let monitor else { return }
        NSEvent.removeMonitor(monitor)
        self.monitor = nil
        ShortcutMonitor.shared.isPaused = false
        updateTitle()
    }

    private func updateTitle() {
        title = ShortcutMonitor.shortcut?.display ?? "Record Shortcut"
    }
}
