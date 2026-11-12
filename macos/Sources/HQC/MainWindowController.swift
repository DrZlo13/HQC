import AppKit

// Owns the only window and installs MainViewController as its content view controller.
final class MainWindowController: NSWindowController, NSWindowDelegate {

    init(store: ServerStore) {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1200, height: 800),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "HQC"
        let main = MainViewController(store: store)
        window.contentViewController = main

        // No bar of its own: the title bar, 28 pt, is transparent over the window's background,
        // which MainViewController sets to the project bar's colour. TitleBar puts its items
        // into it as titlebar accessories, without a toolbar, so it keeps that height.
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.titlebarSeparatorStyle = .none
        main.titleBar.install(in: window)

        super.init(window: window)
        window.delegate = self

        // Restore the saved frame, then enable autosave. setFrameAutosaveName persists
        // size/position changes automatically, but for a programmatically created window
        // it does NOT restore them — so restore explicitly first via setFrameUsingName.
        let autosaveName = NSWindow.FrameAutosaveName("HQCMainWindow")
        if !window.setFrameUsingName(autosaveName) {
            window.center()   // no saved frame yet -> center on first launch
        }
        window.setFrameAutosaveName(autosaveName)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    // Closing hides the window instead, so the pages keep their state; the Dock icon brings it
    // back, and so does any of the app's shortcuts while the app is still active.
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        sender.orderOut(nil)
        return false
    }
}
