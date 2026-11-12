import AppKit
import WebKit

// Wraps one persistent WKWebView bound to a single server. Created lazily on first
// selection and kept alive so switching tabs preserves the page state and login.
final class WebTab: NSObject, WKNavigationDelegate, WKUIDelegate {

    let server: Server
    let webView: WKWebView

    // Guards against reloading the server URL on every tab switch.
    private var hasLoaded = false

    // Set when a new session was asked for while the page was still loading; the click
    // then runs once the load finishes.
    private var pendingNewSession = false

    // Fired (on the main thread) whenever the document title changes.
    var onTitleChanged: (() -> Void)?

    // KVO token for `webView.title`.
    private var titleObservation: NSKeyValueObservation?

    // The current document title, or nil when the page has no (non-empty) title.
    var pageTitle: String? {
        let t = webView.title
        return (t?.isEmpty == false) ? t : nil
    }

    // Clicks the client's own "New session" button as soon as it is there and enabled (it is
    // disabled until the client has loaded the user), checking every 100 ms for up to 5 s.
    // A narrow window shows no session column, so the button is absent: the script then slides
    // the list out ("Sessions" in the chat header) or, where there is no such toggle, follows the
    // page's "Back to sessions" link (the phone layout's chat, Project settings), each at most
    // once, and waits for the list's button. Resolves to whether it clicked.
    private static let newSessionScript = """
        return await new Promise((resolve) => {
          const started = Date.now();
          let slidOut = false, wentBack = false;
          const check = () => {
            const b = document.querySelector('button.btn-new-session');
            if (b && !b.disabled) { b.click(); resolve(true); return; }
            if (!b) {
              const toggle = document.querySelector('button.btn-sessions');
              const back = document.querySelector('a[aria-label="Back to sessions"]:not(.requests-screen *)');
              if (toggle && !slidOut) { toggle.click(); slidOut = true; }
              else if (!toggle && back && !wentBack) { back.click(); wentBack = true; }
            }
            if (Date.now() - started >= 5000) { resolve(false); }
            else { setTimeout(check, 100); }
          };
          check();
        });
        """

    init(server: Server) {
        self.server = server

        let configuration = WKWebViewConfiguration()
        // Use the default persistent data store so cookies/logins survive restarts.
        configuration.websiteDataStore = .default()

        self.webView = WKWebView(frame: .zero, configuration: configuration)

        super.init()

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

    // Opens a new session through the client's own button; while the page is still
    // loading, once after the load finishes.
    func newSession() {
        if webView.isLoading {
            pendingNewSession = true
        } else {
            runNewSessionScript()
        }
    }

    private func runNewSessionScript() {
        webView.callAsyncJavaScript(Self.newSessionScript, arguments: [:], in: nil, in: .page,
                                    completionHandler: nil)
    }

    // Stop any in-flight load and detach delegates before this tab is discarded.
    func tearDown() {
        onTitleChanged = nil
        titleObservation?.invalidate()
        titleObservation = nil
        webView.stopLoading()
        webView.navigationDelegate = nil
        webView.uiDelegate = nil
        webView.removeFromSuperview()
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
        if pendingNewSession {
            pendingNewSession = false
            runNewSessionScript()
        }
    }

    // The page's web process crashed or was killed: load it again.
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        webView.reload()
    }
}
