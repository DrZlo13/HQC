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

        // The app's shortcuts, New Session in Project's from any app (their defaults until the
        // user records others).
        ShortcutMonitor.shared.onPress = { [weak self] action in
            self?.perform(action)
        }
        ShortcutMonitor.shared.start()

        // The app's own notification cards for what the hub would push, while the app runs.
        Notifier.shared.isShowing = { [weak self] serverID in
            guard NSApp.isActive, let window = self?.windowController?.window, window.isVisible else { return false }
            return self?.mainViewController?.selectedServerID == serverID
        }
        Notifier.shared.onOpen = { [weak self] serverID, sessionID in
            self?.openSession(serverID: serverID, sessionID: sessionID)
        }
        Notifier.shared.onAnswer = { [weak self] serverID, sessionID, requestID, optionID, done in
            guard let main = self?.mainViewController else { return done(false) }
            main.answer(serverID: serverID, sessionID: sessionID, requestID: requestID,
                        optionID: optionID, done: done)
        }
        Notifier.shared.start()

        // With no server yet, open Settings so the user can add one.
        if store.servers.isEmpty {
            mainViewController?.presentSettings()
        }

        NSApp.activate(ignoringOtherApps: true)
    }

    // Shows the window (also when it was closed) and hands a shortcut action to
    // MainViewController.perform(_:).
    private func perform(_ action: ShortcutAction) {
        if let window = windowController?.window, window.isMiniaturized {
            window.deminiaturize(nil)
        }
        windowController?.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
        mainViewController?.perform(action)
    }

    // A clicked notification: shows the window (also when it was closed) and opens the session
    // in its server's tab.
    private func openSession(serverID: UUID, sessionID: String) {
        if let window = windowController?.window, window.isMiniaturized {
            window.deminiaturize(nil)
        }
        windowController?.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
        mainViewController?.openSession(serverID: serverID, sessionID: sessionID)
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
