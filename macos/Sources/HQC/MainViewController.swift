import AppKit

// Hosts the selected server's persistent web view and drives the window's bar (TitleBar): its
// arrows, its breadcrumbs, the server switch, and the window's background behind it.
final class MainViewController: NSViewController {

    private let store: ServerStore

    // The window's bar; MainWindowController puts it into the window's title bar.
    let titleBar = TitleBar()
    private let contentContainer = NSView()

    // One kept-alive web tab per server, made and loaded at launch and when a server is added:
    // every server's page reports its notifications.
    private var webTabs: [UUID: WebTab] = [:]
    // The currently selected server.
    private(set) var selectedServerID: UUID?
    // The web view currently installed in the content container (so we only swap on change).
    private weak var installedContentView: NSView?

    // App-wide page zoom, applied to every tab and persisted across launches.
    private static let zoomDefaultsKey = "pageZoom"
    private static let minZoom: CGFloat = 0.5
    private static let maxZoom: CGFloat = 3.0
    private static let zoomStep: CGFloat = 0.1
    private var currentZoom: CGFloat = 1.0

    // Placeholder shown when there are no servers configured.
    private let placeholderLabel = NSTextField(labelWithString:
        "No servers configured. Open “Settings…” to add one.")

    // Settings window controller is held strongly while presented.
    private var settingsWindowController: SettingsWindowController?

    // The currently visible web tab, if any.
    private var activeTab: WebTab? {
        guard let s = selectedServerID else { return nil }
        return webTabs[s]
    }

    init(store: ServerStore) {
        self.store = store
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    override func loadView() {
        // Build the root view programmatically.
        view = NSView(frame: NSRect(x: 0, y: 0, width: 1200, height: 800))
    }

    // At launch setContent runs before the view has a window: focus the page and dress the
    // bar once it has one.
    override func viewDidAppear() {
        super.viewDidAppear()
        if let webView = activeTab?.webView { view.window?.makeFirstResponder(webView) }
        updateChrome()
    }

    override func viewDidLoad() {
        super.viewDidLoad()

        // Restore the saved zoom; UserDefaults.double returns 0 when unset, so fall back to 1.0.
        let savedZoom = UserDefaults.standard.double(forKey: Self.zoomDefaultsKey)
        currentZoom = savedZoom > 0 ? CGFloat(savedZoom) : 1.0

        contentContainer.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(contentContainer)

        NSLayoutConstraint.activate([
            contentContainer.topAnchor.constraint(equalTo: view.topAnchor),
            contentContainer.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            contentContainer.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            contentContainer.bottomAnchor.constraint(equalTo: view.bottomAnchor)
        ])

        // Centered placeholder for the empty state.
        placeholderLabel.translatesAutoresizingMaskIntoConstraints = false
        placeholderLabel.alignment = .center
        placeholderLabel.textColor = .secondaryLabelColor
        contentContainer.addSubview(placeholderLabel)
        NSLayoutConstraint.activate([
            placeholderLabel.centerXAnchor.constraint(equalTo: contentContainer.centerXAnchor),
            placeholderLabel.centerYAnchor.constraint(equalTo: contentContainer.centerYAnchor)
        ])

        titleBar.onBack = { [weak self] in self?.goBack(nil) }
        titleBar.onForward = { [weak self] in self?.goForward(nil) }
        titleBar.onSelectServer = { [weak self] index in
            guard let self, self.store.servers.indices.contains(index) else { return }
            self.selectServer(id: self.store.servers[index].id)
        }

        NotificationCenter.default.addObserver(self,
                                               selector: #selector(serversDidChange),
                                               name: .serversDidChange,
                                               object: nil)

        // Initial population.
        reloadServerSwitch()
        if let first = store.servers.first {
            selectServer(id: first.id)
        } else {
            updatePlaceholder()
        }
        loadAllTabs()
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
    }

    // MARK: - Selection

    // Select a server. Creates its web tab if it has none yet, then shows it and updates the bar.
    func selectServer(id: UUID) {
        guard let server = store.servers.first(where: { $0.id == id }) else { return }

        let tab = webTabs[id] ?? makeWebTab(server: server)
        showTab(tab)
        selectedServerID = id
        placeholderLabel.isHidden = true
        reloadServerSwitch()
        updateWindowTitle()
        updateChrome()
    }

    // Builds a kept-alive web tab for a server and registers it.
    private func makeWebTab(server: Server) -> WebTab {
        let webTab = WebTab(server: server)
        webTab.onTitleChanged = { [weak self, weak webTab] in
            guard let self, let webTab, self.activeTab === webTab else { return }
            self.updateWindowTitle()
        }
        webTab.onChromeChanged = { [weak self, weak webTab] in
            guard let self, let webTab, self.activeTab === webTab else { return }
            self.updateChrome()
        }
        webTab.onBadgeChanged = { [weak self] in
            self?.updateBadge()
        }
        webTabs[server.id] = webTab
        return webTab
    }

    // Makes and loads the web tab of every server that has none.
    private func loadAllTabs() {
        for server in store.servers where webTabs[server.id] == nil {
            let tab = makeWebTab(server: server)
            tab.setPageZoom(currentZoom)
            tab.loadIfNeeded()
        }
    }

    private func updatePlaceholder() {
        placeholderLabel.isHidden = !store.servers.isEmpty
    }

    // Install a web tab as the visible content.
    private func showTab(_ webTab: WebTab) {
        webTab.loadIfNeeded()
        webTab.setPageZoom(currentZoom)
        setContent(webTab)
    }

    // Installs `webTab`'s web view into the content container, pinned on all edges. No-op when
    // it is already the installed view.
    private func setContent(_ webTab: WebTab) {
        if installedContentView === webTab.webView { return }
        installedContentView?.removeFromSuperview()

        let webView = webTab.webView
        webView.translatesAutoresizingMaskIntoConstraints = false
        contentContainer.addSubview(webView)
        NSLayoutConstraint.activate([
            webView.topAnchor.constraint(equalTo: contentContainer.topAnchor),
            webView.leadingAnchor.constraint(equalTo: contentContainer.leadingAnchor),
            webView.trailingAnchor.constraint(equalTo: contentContainer.trailingAnchor),
            webView.bottomAnchor.constraint(equalTo: contentContainer.bottomAnchor)
        ])
        installedContentView = webView
        // Keyboard input goes to the page shown, not to the window.
        view.window?.makeFirstResponder(webView)
    }

    // The server switch in the bar; it is shown only when there are two or more servers.
    private func reloadServerSwitch() {
        titleBar.setServers(store.servers.map(\.name),
                            selected: store.servers.firstIndex { $0.id == selectedServerID })
    }

    // The Dock icon's badge: the sessions with something unread on every server; none at 0.
    private func updateBadge() {
        let total = webTabs.values.reduce(0) { $0 + $1.unreadSessions }
        NSApp.dockTile.badgeLabel = total > 0 ? String(total) : nil
    }

    // The bar follows the active page: its arrows, its breadcrumbs, and the window's background
    // (so the transparent bar takes the colour the page reports, its project bar's) with a dark
    // or light appearance to match, so the traffic lights and the bar's text stay readable.
    // Before the page reports its colour, the window's own.
    private func updateChrome() {
        let tab = activeTab
        titleBar.setNavigation(canGoBack: tab?.webView.canGoBack ?? false,
                               canGoForward: tab?.webView.canGoForward ?? false)
        titleBar.setCrumbs(tab?.barCrumbs ?? [])
        guard let window = view.window else { return }
        if let color = tab?.barColor, let rgb = color.usingColorSpace(.sRGB) {
            window.backgroundColor = color
            let luminance = 0.2126 * rgb.redComponent + 0.7152 * rgb.greenComponent + 0.0722 * rgb.blueComponent
            window.appearance = NSAppearance(named: luminance < 0.5 ? .darkAqua : .aqua)
        } else {
            window.backgroundColor = .windowBackgroundColor
            window.appearance = nil
        }
    }

    // The window title follows the active page's title, else the (live) server name.
    private func updateWindowTitle() {
        let serverName = selectedServerID.flatMap { id in store.servers.first(where: { $0.id == id })?.name }
        view.window?.title = activeTab?.pageTitle ?? serverName ?? "HQC"
    }

    // MARK: - Server changes

    @objc private func serversDidChange() {
        let currentServers = store.servers
        let validIDs = Set(currentServers.map { $0.id })

        // Tear down the web tabs of servers that were deleted.
        for serverID in Array(webTabs.keys) where !validIDs.contains(serverID) {
            webTabs.removeValue(forKey: serverID)?.tearDown()
        }

        // Tear down web tabs whose server URL changed so they load the new address
        // (made again below).
        for server in currentServers {
            if let tab = webTabs[server.id], tab.server.url != server.url {
                webTabs.removeValue(forKey: server.id)?.tearDown()
            }
        }
        // A torn-down tab's sessions leave the Dock badge.
        updateBadge()

        // Resolve the selection after deletions / URL changes.
        if let selected = selectedServerID, validIDs.contains(selected) {
            if webTabs[selected] == nil {
                // Its web tab was torn down (the URL changed): rebuild now so the new address
                // is shown immediately.
                selectedServerID = nil
                selectServer(id: selected)
            } else {
                // Selected server still exists; refresh the switch (a name may have changed).
                reloadServerSwitch()
                updateWindowTitle()
            }
        } else {
            selectedServerID = nil
            if let first = currentServers.first {
                selectServer(id: first.id)
            } else {
                reloadServerSwitch()
                updatePlaceholder()
                updateWindowTitle()
                updateChrome()
            }
        }
        // An added server, or one with a new address, notifies before it is opened.
        loadAllTabs()
    }

    // MARK: - Settings

    // Presents Settings as a sheet on this window.
    func presentSettings() {
        guard settingsWindowController == nil else { return }

        let controller = SettingsWindowController(store: store)
        settingsWindowController = controller

        guard let window = view.window, let sheet = controller.window else {
            controller.showWindow(nil)
            return
        }

        window.beginSheet(sheet) { [weak self] _ in
            // Release the controller once the sheet dismisses so it rebuilds fresh next time.
            self?.settingsWindowController = nil
        }
    }

    // MARK: - Notifications

    // A clicked notification: the server's tab, then the session in its page.
    func openSession(serverID: UUID, sessionID: String) {
        selectServer(id: serverID)
        guard let tab = activeTab, tab.server.id == serverID else { return }
        view.window?.makeFirstResponder(tab.webView)
        tab.openSession(sessionID)
    }

    // A permission request answered from its notification, in the server's page.
    func answer(serverID: UUID, sessionID: String, requestID: String, optionID: String,
                done: @escaping (Bool) -> Void) {
        guard let tab = webTabs[serverID] else {
            done(false)
            return
        }
        tab.answer(session: sessionID, request: requestID, option: optionID, done: done)
    }

    // MARK: - Shortcut actions

    // Runs a shortcut action in the active tab.
    func perform(_ action: ShortcutAction) {
        guard let tab = activeTab else { return }
        // The new session's message field takes the focus only inside a focused web view.
        view.window?.makeFirstResponder(tab.webView)
        tab.perform(action)
    }

    // MARK: - Menu actions (responder chain targets)

    @objc func reloadCurrent(_ sender: Any?) {
        activeTab?.reload()
    }

    @objc func goBack(_ sender: Any?) {
        activeTab?.goBack()
    }

    @objc func goForward(_ sender: Any?) {
        activeTab?.goForward()
    }

    private func setZoom(_ factor: CGFloat) {
        // Clamp and round to a clean 0.1 step to avoid float drift.
        let clamped = min(max(factor, Self.minZoom), Self.maxZoom)
        currentZoom = (clamped * 10).rounded() / 10
        for tab in webTabs.values { tab.setPageZoom(currentZoom) }
        UserDefaults.standard.set(Double(currentZoom), forKey: Self.zoomDefaultsKey)
    }

    @objc func zoomIn(_ sender: Any?) { setZoom(currentZoom + Self.zoomStep) }
    @objc func zoomOut(_ sender: Any?) { setZoom(currentZoom - Self.zoomStep) }
    @objc func zoomReset(_ sender: Any?) { setZoom(1.0) }
}
