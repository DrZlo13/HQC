import SwiftUI
import WebKit

/// First run: the hub's address.
struct SetupView: View {
    @Environment(AppModel.self) private var app
    @State private var address = ""
    @FocusState private var focused: Bool

    var body: some View {
        VStack(spacing: 28) {
            Spacer()
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 56, weight: .semibold))
                .foregroundStyle(.tint)
                .symbolEffect(.pulse, options: .repeating)
            VStack(spacing: 8) {
                Text("Connect to your hub")
                    .font(.largeTitle.bold())
                Text("Your agents, on every machine, in one place.")
                    .foregroundStyle(.secondary)
            }
            .multilineTextAlignment(.center)

            TextField("hqc.example.com", text: $address)
                .textContentType(.URL)
                .keyboardType(.URL)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .focused($focused)
                .submitLabel(.go)
                .onSubmit(connect)
                .padding(.horizontal, 20)
                .padding(.vertical, 16)
                .glassEffect(.regular, in: .capsule)

            Button(action: connect) {
                Text("Continue")
                    .font(.headline)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 6)
            }
            .buttonStyle(.glassProminent)
            .controlSize(.large)
            .disabled(AppModel.normalize(address) == nil)
            Spacer()
            Spacer()
        }
        .padding(.horizontal, 28)
        .frame(maxWidth: 520)
        .background(BackdropGradient())
        .onAppear { focused = true }
    }

    private func connect() {
        guard let url = AppModel.normalize(address) else { return }
        app.setHub(url)
    }
}

/// The hub wants a sign-in: Cloudflare Access's login page in a web view, which ends once Access
/// set its `CF_Authorization` cookie on the hub's host.
struct SignInView: View {
    @Environment(AppModel.self) private var app
    @State private var showLogin = false

    var body: some View {
        VStack(spacing: 24) {
            Spacer()
            Image(systemName: "lock.shield")
                .font(.system(size: 56, weight: .semibold))
                .foregroundStyle(.tint)
            VStack(spacing: 8) {
                Text("Sign in")
                    .font(.largeTitle.bold())
                if let host = app.hubURL?.host() {
                    Text(host).font(.headline.monospaced()).foregroundStyle(.secondary)
                }
                if let error = app.lastError {
                    Text(error)
                        .font(.footnote)
                        .foregroundStyle(.red)
                        .multilineTextAlignment(.center)
                }
            }
            GlassEffectContainer(spacing: 12) {
                VStack(spacing: 12) {
                    Button {
                        showLogin = true
                    } label: {
                        Label("Sign in with Cloudflare Access", systemImage: "person.badge.key")
                            .font(.headline)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 6)
                    }
                    .buttonStyle(.glassProminent)
                    .controlSize(.large)

                    HStack(spacing: 12) {
                        Button("Try again") { Task { await app.connect() } }
                            .buttonStyle(.glass)
                        Button("Other hub") { app.forgetHub() }
                            .buttonStyle(.glass)
                    }
                }
            }
            Spacer()
            Spacer()
        }
        .padding(.horizontal, 28)
        .frame(maxWidth: 520)
        .background(BackdropGradient())
        .sheet(isPresented: $showLogin) {
            if let url = app.hubURL {
                NavigationStack {
                    AccessLoginView(hubURL: url) { token in
                        showLogin = false
                        app.signedIn(token: token)
                    }
                    .ignoresSafeArea(edges: .bottom)
                    .navigationTitle(url.host() ?? "Sign in")
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("Cancel", systemImage: "xmark") { showLogin = false }
                        }
                    }
                }
            }
        }
    }
}

struct AccessLoginView: UIViewRepresentable {
    let hubURL: URL
    let onToken: (String) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(hubURL: hubURL, onToken: onToken) }

    func makeUIView(context: Context) -> WKWebView {
        let web = WKWebView(frame: .zero, configuration: WKWebViewConfiguration())
        web.navigationDelegate = context.coordinator
        web.load(URLRequest(url: hubURL))
        return web
    }

    func updateUIView(_ uiView: WKWebView, context: Context) {}

    final class Coordinator: NSObject, WKNavigationDelegate {
        let hubURL: URL
        let onToken: (String) -> Void
        private var done = false

        init(hubURL: URL, onToken: @escaping (String) -> Void) {
            self.hubURL = hubURL
            self.onToken = onToken
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            guard !done, webView.url?.host() == hubURL.host() else { return }
            let host = hubURL.host() ?? ""
            webView.configuration.websiteDataStore.httpCookieStore.getAllCookies { cookies in
                let token = cookies.first {
                    $0.name == "CF_Authorization" && host.hasSuffix($0.domain.trimmingCharacters(in: ["."]))
                }?.value
                Task { @MainActor in
                    guard !self.done, let token else { return }
                    self.done = true
                    self.onToken(token)
                }
            }
        }
    }
}

/// A soft wash of the accent color behind the sign-in screens, for the glass to refract.
struct BackdropGradient: View {
    var body: some View {
        MeshGradient(
            width: 3, height: 3,
            points: [[0, 0], [0.5, 0], [1, 0], [0, 0.5], [0.6, 0.45], [1, 0.5], [0, 1], [0.5, 1], [1, 1]],
            colors: [
                .accentColor.opacity(0.35), .clear, .blue.opacity(0.2),
                .clear, .accentColor.opacity(0.15), .clear,
                .purple.opacity(0.18), .clear, .accentColor.opacity(0.25),
            ]
        )
        .ignoresSafeArea()
        .background(Color(.systemBackground))
    }
}
