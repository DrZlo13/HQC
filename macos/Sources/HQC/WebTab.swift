import AppKit
import WebKit

// Wraps one persistent WKWebView bound to a single server. Every server's tab is made and
// loaded at launch and kept alive, on screen or not: switching tabs keeps the page state and
// login, and each page reports its server's notifications.
final class WebTab: NSObject, WKNavigationDelegate, WKUIDelegate, WKScriptMessageHandler {

    let server: Server
    let webView: WKWebView

    // Holds the page scripts (notifications, the window's bar) and their message handlers.
    private let contentController = WKUserContentController()

    // What the window's bar shows for this page: its colour, the project bar's (nil until the
    // page reports one), and the breadcrumbs of the open session or project.
    private(set) var barColor: NSColor?
    private(set) var barCrumbs: [String] = []

    // Fired (on the main thread) when the bar's colour, its breadcrumbs or whether the page can
    // go back or forward changes.
    var onChromeChanged: (() -> Void)?

    // KVO tokens for `webView.canGoBack` and `canGoForward`.
    private var historyObservations: [NSKeyValueObservation] = []

    // Guards against reloading the server URL on every tab switch.
    private var hasLoaded = false

    // A shortcut action asked for while the page was still loading; its script then runs
    // once the load finishes.
    private var pendingAction: ShortcutAction?

    // A session a notification click asked for while the page was still loading; it is opened
    // once the load finishes, in the new document.
    private var pendingSession: String?

    // Fired (on the main thread) whenever the document title changes.
    var onTitleChanged: (() -> Void)?

    // KVO token for `webView.title`.
    private var titleObservation: NSKeyValueObservation?

    // The current document title, or nil when the page has no (non-empty) title.
    var pageTitle: String? {
        let t = webView.title
        return (t?.isEmpty == false) ? t : nil
    }

    init(server: Server) {
        self.server = server

        let configuration = WKWebViewConfiguration()
        // Use the default persistent data store so cookies/logins survive restarts.
        configuration.websiteDataStore = .default()
        // A page off screen keeps running (the default suspends it): its socket brings the
        // notifications.
        configuration.preferences.inactiveSchedulingPolicy = .none
        // The notification script, before the page's own scripts open their socket, and the
        // script reporting the bar's colour.
        for source in [NotificationScript.source, ChromeScript.source] {
            contentController.addUserScript(WKUserScript(source: source,
                                                         injectionTime: .atDocumentStart,
                                                         forMainFrameOnly: true,
                                                         in: .page))
        }
        configuration.userContentController = contentController

        self.webView = WKWebView(frame: .zero, configuration: configuration)

        super.init()

        // Through a weak proxy: the controller retains its handlers, and this tab owns the
        // controller.
        for name in [NotificationScript.handlerName, ChromeScript.handlerName] {
            contentController.add(WeakMessageHandler(self), contentWorld: .page, name: name)
        }

        webView.autoresizingMask = [.width, .height]
        webView.allowsBackForwardNavigationGestures = true
        webView.navigationDelegate = self
        webView.uiDelegate = self

        // KVO on `title` fires on the main thread.
        titleObservation = webView.observe(\.title, options: [.new]) { [weak self] _, _ in
            self?.onTitleChanged?()
        }
        historyObservations = [
            webView.observe(\.canGoBack, options: [.new]) { [weak self] _, _ in self?.onChromeChanged?() },
            webView.observe(\.canGoForward, options: [.new]) { [weak self] _, _ in self?.onChromeChanged?() },
        ]
    }

    // MARK: - Loading / navigation

    func loadIfNeeded() {
        guard !hasLoaded else { return }
        hasLoaded = true
        webView.load(URLRequest(url: server.url))
    }

    func reload() {
        // A failed first load (no network at launch) leaves the web view without a URL, and
        // reload() then does nothing: load the server address again instead.
        if hasLoaded, webView.url != nil {
            webView.reload()
        } else {
            hasLoaded = true
            webView.load(URLRequest(url: server.url))
        }
    }

    func goBack() {
        if webView.canGoBack { webView.goBack() }
    }

    func goForward() {
        if webView.canGoForward { webView.goForward() }
    }

    // Applies the page zoom factor to this tab's web content.
    func setPageZoom(_ factor: CGFloat) {
        webView.pageZoom = factor
    }

    // Runs a shortcut action's script in the page; while the page is still loading, once
    // after the load finishes.
    func perform(_ action: ShortcutAction) {
        if webView.isLoading {
            pendingAction = action
        } else {
            run(action)
        }
    }

    private func run(_ action: ShortcutAction) {
        webView.callAsyncJavaScript(action.script, arguments: [:], in: nil, in: .page,
                                    completionHandler: nil)
    }

    // Opens a session in the page, as a click on its row does; while the page is still
    // loading, once after the load finishes.
    func openSession(_ id: String) {
        if webView.isLoading {
            pendingSession = id
        } else {
            setHash(session: id)
        }
    }

    private func setHash(session id: String) {
        webView.callAsyncJavaScript("location.hash = '#/s/' + encodeURIComponent(id);",
                                    arguments: ["id": id], in: nil, in: .page, completionHandler: nil)
    }

    // Answers a permission request with one of its options, by the client's own call
    // (client/src/api.ts approve()); `done` gets whether the hub took it.
    func answer(session: String, request: String, option: String, done: @escaping (Bool) -> Void) {
        let script = """
            const res = await fetch('/api/sessions/' + encodeURIComponent(session) + '/requests/' + encodeURIComponent(request), {
              method: 'POST',
              credentials: 'include',
              headers: { 'content-type': 'application/json' },
              body: JSON.stringify({ option_id: option }),
            });
            return res.ok;
            """
        webView.callAsyncJavaScript(script, arguments: ["session": session, "request": request, "option": option],
                                    in: nil, in: .page) { result in
            if case .success(let ok) = result, ok as? Bool == true {
                done(true)
            } else {
                done(false)
            }
        }
    }

    // Stop any in-flight load and detach delegates before this tab is discarded.
    func tearDown() {
        for name in [NotificationScript.handlerName, ChromeScript.handlerName] {
            contentController.removeScriptMessageHandler(forName: name, contentWorld: .page)
        }
        onChromeChanged = nil
        historyObservations.forEach { $0.invalidate() }
        historyObservations = []
        onTitleChanged = nil
        titleObservation?.invalidate()
        titleObservation = nil
        webView.stopLoading()
        webView.navigationDelegate = nil
        webView.uiDelegate = nil
        webView.removeFromSuperview()
    }

    // MARK: - WKScriptMessageHandler

    // What the page scripts report. To `hqcChrome`: the bar's colour or its breadcrumbs. To
    // `hqcNotify`, handed to the Notifier with this tab's server: an event to post, or
    // `{open: [tag]}`, the tags of every open request, on each `requests` frame.
    func userContentController(_ userContentController: WKUserContentController,
                               didReceive message: WKScriptMessage) {
        guard let m = message.body as? [String: Any] else { return }
        if message.name == ChromeScript.handlerName {
            if let css = m["background"] as? String, let color = Self.color(css: css) {
                barColor = color
            }
            if let crumbs = m["crumbs"] as? [String] {
                barCrumbs = crumbs
            }
            onChromeChanged?()
            return
        }
        if let open = m["open"] as? [String] {
            Notifier.shared.keepOpen(server: server, tags: Set(open))
            return
        }
        guard let session = m["session_id"] as? String,
              let tag = m["tag"] as? String else { return }
        Notifier.shared.notify(server: server, event: PageEvent(
            sessionID: session,
            tag: tag,
            type: m["type"] as? String ?? "",
            title: m["title"] as? String ?? "",
            subtitle: m["subtitle"] as? String ?? "",
            body: m["body"] as? String ?? "",
            viewing: m["viewing"] as? Bool ?? false,
            requestID: m["request_id"] as? String,
            allowOption: m["allow_option"] as? String,
            denyOption: m["deny_option"] as? String))
    }

    // A computed CSS colour, `rgb(r, g, b)` or `rgba(r, g, b, a)`; nil for anything else and
    // for a transparent one (a page without its styles yet).
    private static func color(css: String) -> NSColor? {
        guard let open = css.firstIndex(of: "("), let close = css.lastIndex(of: ")") else { return nil }
        let parts = css[css.index(after: open)..<close]
            .split(separator: ",")
            .compactMap { Double($0.trimmingCharacters(in: .whitespaces)) }
        guard parts.count == 3 || parts.count == 4 else { return nil }
        let alpha = parts.count == 4 ? parts[3] : 1
        guard alpha > 0 else { return nil }
        return NSColor(srgbRed: parts[0] / 255, green: parts[1] / 255, blue: parts[2] / 255, alpha: alpha)
    }

    // MARK: - WKUIDelegate

    func webView(_ webView: WKWebView,
                 createWebViewWith configuration: WKWebViewConfiguration,
                 for navigationAction: WKNavigationAction,
                 windowFeatures: WKWindowFeatures) -> WKWebView? {
        // target=_blank / window.open: the app never opens a second window. A real web
        // address goes to the default browser; an empty or about:blank one is ignored, so the
        // client's window.open('about:blank') gets null and retries with the real address.
        if let url = navigationAction.request.url,
           let scheme = url.scheme?.lowercased(),
           scheme == "http" || scheme == "https" {
            NSWorkspace.shared.open(url)
        }
        return nil
    }

    // WKWebView shows no file chooser for <input type="file"> unless this is
    // implemented. Bridge it to a native NSOpenPanel, honoring the page's
    // directory/multiple-selection hints.
    func webView(_ webView: WKWebView,
                 runOpenPanelWith parameters: WKOpenPanelParameters,
                 initiatedByFrame frame: WKFrameInfo,
                 completionHandler: @escaping ([URL]?) -> Void) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = parameters.allowsDirectories
        panel.allowsMultipleSelection = parameters.allowsMultipleSelection
        panel.resolvesAliases = true

        let finish: (NSApplication.ModalResponse) -> Void = { response in
            completionHandler(response == .OK ? panel.urls : nil)
        }
        // Present as a sheet on the web view's window when possible.
        if let window = webView.window {
            panel.beginSheetModal(for: window, completionHandler: finish)
        } else {
            panel.begin(completionHandler: finish)
        }
    }

    // alert(), confirm() and prompt(): without these WKWebView silently answers
    // undefined / false / null. The client uses prompt() to rename and confirm() to delete.

    func webView(_ webView: WKWebView,
                 runJavaScriptAlertPanelWithMessage message: String,
                 initiatedByFrame frame: WKFrameInfo,
                 completionHandler: @escaping () -> Void) {
        let alert = NSAlert()
        alert.messageText = message
        alert.addButton(withTitle: "OK")
        present(alert, in: webView) { _ in completionHandler() }
    }

    func webView(_ webView: WKWebView,
                 runJavaScriptConfirmPanelWithMessage message: String,
                 initiatedByFrame frame: WKFrameInfo,
                 completionHandler: @escaping (Bool) -> Void) {
        let alert = NSAlert()
        alert.messageText = message
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Cancel")
        present(alert, in: webView) { response in
            completionHandler(response == .alertFirstButtonReturn)
        }
    }

    func webView(_ webView: WKWebView,
                 runJavaScriptTextInputPanelWithPrompt prompt: String,
                 defaultText: String?,
                 initiatedByFrame frame: WKFrameInfo,
                 completionHandler: @escaping (String?) -> Void) {
        let alert = NSAlert()
        alert.messageText = prompt
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Cancel")

        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 320, height: 24))
        field.stringValue = defaultText ?? ""
        alert.accessoryView = field
        alert.window.initialFirstResponder = field

        present(alert, in: webView) { response in
            completionHandler(response == .alertFirstButtonReturn ? field.stringValue : nil)
        }
    }

    // Shows an alert as a sheet on the web view's window when available.
    private func present(_ alert: NSAlert, in webView: WKWebView,
                         completion: @escaping (NSApplication.ModalResponse) -> Void) {
        if let window = webView.window {
            alert.beginSheetModal(for: window, completionHandler: completion)
        } else {
            completion(alert.runModal())
        }
    }

    // MARK: - WKNavigationDelegate

    func webView(_ webView: WKWebView,
                 decidePolicyFor navigationAction: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        // Only intercept explicit link clicks to a different host; let redirects and
        // form submits through so sign-in flows on other hosts keep working.
        if navigationAction.navigationType == .linkActivated,
           let url = navigationAction.request.url,
           let scheme = url.scheme?.lowercased(),
           scheme == "http" || scheme == "https",
           server.isExternalURL(url) {
            NSWorkspace.shared.open(url)
            decisionHandler(.cancel)
            return
        }
        decisionHandler(.allow)
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        if let id = pendingSession {
            pendingSession = nil
            setHash(session: id)
        }
        if let action = pendingAction {
            pendingAction = nil
            run(action)
        }
    }

    // A load that failed drops what waited for it, so a later reload does not run it.
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        pendingAction = nil
        pendingSession = nil
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
                 withError error: Error) {
        pendingAction = nil
        pendingSession = nil
    }

    // The page's web process crashed or was killed: load it again.
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        webView.reload()
    }
}

// Forwards script messages to a handler it does not retain.
private final class WeakMessageHandler: NSObject, WKScriptMessageHandler {
    private weak var target: WKScriptMessageHandler?

    init(_ target: WKScriptMessageHandler) {
        self.target = target
    }

    func userContentController(_ userContentController: WKUserContentController,
                               didReceive message: WKScriptMessage) {
        target?.userContentController(userContentController, didReceive: message)
    }
}
