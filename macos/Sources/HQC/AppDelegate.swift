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

        // The new-session shortcut inside the app (⌘N unless the user records another one).
        NewSessionShortcut.shared.onPress = { [weak self] in
            self?.newSession()
        }
        NewSessionShortcut.shared.start()

        // With no server yet, open Settings so the user can add one.
        if store.servers.isEmpty {
            mainViewController?.presentSettings()
        }

        NSApp.activate(ignoringOtherApps: true)
    }

    // Shows the window (also when it was closed) and starts a new session in the active tab.
    private func newSession() {
        if let window = windowController?.window, window.isMiniaturized {
            window.deminiaturize(nil)
        }
        windowController?.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
        mainViewController?.newSession()
    }

    // File ▸ New Session.
    @objc func newSessionFromMenu(_ sender: Any?) {
        newSession()
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
