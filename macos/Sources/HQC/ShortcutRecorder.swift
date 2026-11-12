import AppKit
import Carbon.HIToolbox

// A button that shows the new-session shortcut and records a new one: click, then press a
// key with at least one of ⌘ ⌃ ⌥. Esc cancels, Delete clears the shortcut.
final class ShortcutRecorder: NSButton {

    // Local monitor that takes every keyDown while recording, so ⌘ combinations reach it
    // before the menu does.
    private var monitor: Any?

    init() {
        super.init(frame: .zero)
        bezelStyle = .rounded
        target = self
        action = #selector(startRecording)
        updateTitle()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    @objc private func startRecording() {
        guard monitor == nil else { return }
        // The shortcut itself would open a new session instead of being recorded.
        NewSessionShortcut.shared.isPaused = true
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
            Shortcut.saved = nil
            stopRecording()
        default:
            // A key without ⌘ ⌃ ⌥ is ignored; recording goes on.
            guard let shortcut = Shortcut(event: event) else { return }
            Shortcut.saved = shortcut
            stopRecording()
        }
    }

    // Ends recording (if it is on) and puts the saved shortcut back to work.
    func stopRecording() {
        guard let monitor else { return }
        NSEvent.removeMonitor(monitor)
        self.monitor = nil
        NewSessionShortcut.shared.isPaused = false
        NewSessionShortcut.shared.updateMenuItem()
        updateTitle()
    }

    private func updateTitle() {
        title = Shortcut.saved?.display ?? "Record Shortcut"
    }
}
