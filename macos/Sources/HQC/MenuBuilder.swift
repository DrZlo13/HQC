import AppKit

// Builds and installs the application's main menu programmatically.
// Standard selectors with nil targets are used for Edit/View items so they route through
// the responder chain to the focused view (this is what makes copy/paste work inside the
// hqc web forms).
enum MenuBuilder {

    static func installMainMenu() {
        let mainMenu = NSMenu()

        mainMenu.addItem(appMenuItem())
        mainMenu.addItem(fileMenuItem())
        mainMenu.addItem(editMenuItem())
        mainMenu.addItem(viewMenuItem())
        mainMenu.addItem(goMenuItem())

        let windowItem = windowMenuItem()
        mainMenu.addItem(windowItem)

        NSApp.mainMenu = mainMenu
        // Let AppKit manage the Window menu (Minimize/Zoom + open windows list).
        if let windowSubmenu = windowItem.submenu {
            NSApp.windowsMenu = windowSubmenu
        }
    }

    // MARK: - File menu

    // File ▸ New Session, New Session in Project, Archive Session.
    private static func fileMenuItem() -> NSMenuItem {
        let item = NSMenuItem(title: "File", action: nil, keyEquivalent: "")
        let menu = NSMenu(title: "File")
        menu.addItem(actionItem(.newSession))
        menu.addItem(actionItem(.newSessionInProject))
        menu.addItem(actionItem(.archiveSession))
        item.submenu = menu
        return item
    }

    // MARK: - Go menu

    // Go ▸ back and forward in the page's history, up and down the session column and the
    // project rail.
    private static func goMenuItem() -> NSMenuItem {
        let item = NSMenuItem(title: "Go", action: nil, keyEquivalent: "")
        let menu = NSMenu(title: "Go")
        menu.addItem(actionItem(.back))
        menu.addItem(actionItem(.forward))
        menu.addItem(.separator())
        menu.addItem(actionItem(.previousSession))
        menu.addItem(actionItem(.nextSession))
        menu.addItem(.separator())
        menu.addItem(actionItem(.previousProject))
        menu.addItem(actionItem(.nextProject))
        item.submenu = menu
        return item
    }

    // An action's item: routed to AppDelegate.performMenuAction(_:) through the responder
    // chain. Only the global action's item shows a key, its shortcut, which ShortcutMonitor
    // keeps in step with Settings; the client takes the other actions' keys itself.
    private static func actionItem(_ action: MenuAction) -> NSMenuItem {
        let item = NSMenuItem(title: action.title,
                              action: #selector(AppDelegate.performMenuAction(_:)),
                              keyEquivalent: "")
        item.representedObject = action.rawValue
        if action == ShortcutMonitor.globalAction { ShortcutMonitor.shared.menuItem = item }
        return item
    }

    // MARK: - App menu

    private static func appMenuItem() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu()

        menu.addItem(withTitle: "About HQC",
                     action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)),
                     keyEquivalent: "")
        menu.addItem(.separator())

        // Standard macOS Settings item (⌘,). nil target -> routed through the responder chain to
        // AppDelegate.openSettings(_:).
        let settings = NSMenuItem(title: "Settings…",
                                  action: #selector(AppDelegate.openSettings(_:)),
                                  keyEquivalent: ",")
        settings.target = nil
        menu.addItem(settings)
        menu.addItem(.separator())

        menu.addItem(withTitle: "Hide HQC",
                     action: #selector(NSApplication.hide(_:)),
                     keyEquivalent: "h")
        menu.addItem(.separator())

        menu.addItem(withTitle: "Quit HQC",
                     action: #selector(NSApplication.terminate(_:)),
                     keyEquivalent: "q")

        item.submenu = menu
        return item
    }

    // MARK: - Edit menu

    private static func editMenuItem() -> NSMenuItem {
        let item = NSMenuItem(title: "Edit", action: nil, keyEquivalent: "")
        let menu = NSMenu(title: "Edit")

        menu.addItem(withTitle: "Undo", action: Selector(("undo:")), keyEquivalent: "z")

        let redo = NSMenuItem(title: "Redo", action: Selector(("redo:")), keyEquivalent: "z")
        redo.keyEquivalentModifierMask = [.command, .shift]
        menu.addItem(redo)

        menu.addItem(.separator())

        menu.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        menu.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        menu.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")

        // "Paste and Match Style" (⌥⇧⌘V). nil target -> routed through the responder chain;
        // WKWebView answers pasteAsPlainText: (like paste:), delivering only text/plain.
        let pasteAndMatchStyle = NSMenuItem(title: "Paste and Match Style",
                                            action: #selector(NSTextView.pasteAsPlainText(_:)),
                                            keyEquivalent: "v")
        pasteAndMatchStyle.keyEquivalentModifierMask = [.command, .option, .shift]
        menu.addItem(pasteAndMatchStyle)

        menu.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")

        item.submenu = menu
        return item
    }

    // MARK: - View menu

    private static func viewMenuItem() -> NSMenuItem {
        let item = NSMenuItem(title: "View", action: nil, keyEquivalent: "")
        let menu = NSMenu(title: "View")

        // All nil-targeted so they route to the MainViewController.
        let reload = NSMenuItem(title: "Reload",
                                action: #selector(MainViewController.reloadCurrent(_:)),
                                keyEquivalent: "r")
        reload.target = nil
        menu.addItem(reload)

        menu.addItem(.separator())

        let zoomIn = NSMenuItem(title: "Zoom In",
                                action: #selector(MainViewController.zoomIn(_:)),
                                keyEquivalent: "+")
        zoomIn.target = nil
        menu.addItem(zoomIn)

        let zoomOut = NSMenuItem(title: "Zoom Out",
                                 action: #selector(MainViewController.zoomOut(_:)),
                                 keyEquivalent: "-")
        zoomOut.target = nil
        menu.addItem(zoomOut)

        let actualSize = NSMenuItem(title: "Actual Size",
                                    action: #selector(MainViewController.zoomReset(_:)),
                                    keyEquivalent: "0")
        actualSize.target = nil
        menu.addItem(actualSize)

        item.submenu = menu
        return item
    }

    // MARK: - Window menu

    private static func windowMenuItem() -> NSMenuItem {
        let item = NSMenuItem(title: "Window", action: nil, keyEquivalent: "")
        let menu = NSMenu(title: "Window")

        // ⌘W: the main window's windowShouldClose hides it instead of closing.
        menu.addItem(withTitle: "Close",
                     action: #selector(NSWindow.performClose(_:)),
                     keyEquivalent: "w")
        menu.addItem(withTitle: "Minimize",
                     action: #selector(NSWindow.performMiniaturize(_:)),
                     keyEquivalent: "m")
        menu.addItem(withTitle: "Zoom",
                     action: #selector(NSWindow.performZoom(_:)),
                     keyEquivalent: "")

        item.submenu = menu
        return item
    }
}
