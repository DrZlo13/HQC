import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {

    // Strong references so the window and the store stay alive for the app lifetime.
    private var windowController: MainWindowController?
    private var store: ServerStore?

    private var mainViewController: MainViewController? {
        windowController?.window?.contentViewController as? MainViewController
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Build the main menu first so standard Edit actions (copy/paste) work everywhere.
        MenuBuilder.installMainMenu()

        let store = ServerStore()
        self.store = store

        let windowController = MainWindowController(store: store)
        self.windowController = windowController
        windowController.showWindow(nil)

        // The shortcuts inside the app (their defaults until the user records others).
        ShortcutMonitor.shared.onPress = { [weak self] action in
            self?.perform(action)
        }
        ShortcutMonitor.shared.start()

        // With no server yet, open Settings so the user can add one.
        if store.servers.isEmpty {
            mainViewController?.presentSettings()
        }

        NSApp.activate(ignoringOtherApps: true)
    }

    // Shows the window (also when it was closed) and runs a shortcut action in the active tab.
    private func perform(_ action: ShortcutAction) {
        if let window = windowController?.window, window.isMiniaturized {
            window.deminiaturize(nil)
        }
        windowController?.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
        mainViewController?.perform(action)
    }

    // The File and Go items of the shortcut actions; the action is the item's represented object.
    @objc func performShortcutAction(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let action = ShortcutAction(rawValue: raw) else { return }
        perform(action)
    }

    // App menu ▸ Settings… (⌘,). Lives here, at the end of the responder chain, so it works
    // while the window is closed: it shows the window and opens Settings over it.
    @objc func openSettings(_ sender: Any?) {
        windowController?.showWindow(nil)
        mainViewController?.presentSettings()
    }

    // Closing the window only hides it: the app and the page keep running.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        return false
    }

    // A click on the Dock icon shows the window again.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        windowController?.showWindow(nil)
        return true
    }
}
