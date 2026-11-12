import AppKit
import WebKit

// Wraps one persistent WKWebView bound to a single server. Created lazily on first
// selection and kept alive so switching tabs preserves the page state and login.
final class WebTab: NSObject, WKNavigationDelegate, WKUIDelegate, WKScriptMessageHandler {

    let server: Server
    let webView: WKWebView

    // Holds the notification script and its message handler.
    private let contentController = WKUserContentController()

    // Guards against reloading the server URL on every tab switch.
    private var hasLoaded = false

    // A shortcut action asked for while the page was still loading; its script then runs
    // once the load finishes.
    private var pendingAction: ShortcutAction?

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
        // The notification script, before the page's own scripts open their socket.
        contentController.addUserScript(WKUserScript(source: NotificationScript.source,
                                                     injectionTime: .atDocumentStart,
                                                     forMainFrameOnly: true,
                                                     in: .page))
        configuration.userContentController = contentController

        self.webView = WKWebView(frame: .zero, configuration: configuration)

        super.init()

        // Through a weak proxy: the controller retains its handlers, and this tab owns the
        // controller.
        contentController.add(WeakMessageHandler(self), contentWorld: .page,
                              name: NotificationScript.handlerName)

        webView.autoresizingMask = [.width, .height]
        webView.allowsBackForwardNavigationGestures = true
        webView.navigationDelegate = self
        webView.uiDelegate = self

        // KVO on `title` fires on the main thread.
        titleObservation = webView.observe(\.title, options: [.new]) { [weak self] _, _ in
            self?.onTitleChanged?()
        }
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

    // Opens a session in the page, as a click on its row does.
    func openSession(_ id: String) {
        webView.callAsyncJavaScript("location.hash = '#/s/' + encodeURIComponent(id);",
                                    arguments: ["id": id], in: nil, in: .page, completionHandler: nil)
    }

    // Stop any in-flight load and detach delegates before this tab is discarded.
    func tearDown() {
        contentController.removeScriptMessageHandler(forName: NotificationScript.handlerName,
                                                     contentWorld: .page)
        onTitleChanged = nil
        titleObservation?.invalidate()
        titleObservation = nil
        webView.stopLoading()
        webView.navigationDelegate = nil
        webView.uiDelegate = nil
        webView.removeFromSuperview()
    }

    // MARK: - WKScriptMessageHandler

    // An event the notification script reports: handed to the Notifier with this tab's server.
    func userContentController(_ userContentController: WKUserContentController,
                               didReceive message: WKScriptMessage) {
        guard let m = message.body as? [String: Any],
              let session = m["session_id"] as? String,
              let tag = m["tag"] as? String else { return }
        Notifier.shared.notify(server: server,
                               sessionID: session,
                               tag: tag,
                               title: m["title"] as? String ?? "",
                               body: m["body"] as? String ?? "",
                               viewing: m["viewing"] as? Bool ?? false)
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
        if let action = pendingAction {
            pendingAction = nil
            run(action)
        }
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
