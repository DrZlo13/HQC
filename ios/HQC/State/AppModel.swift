import Foundation
import Observation

/// The app's connection to one hub: who is signed in, the projects, live session summaries and
/// the open requests. Views read it from the environment.
@MainActor
@Observable
final class AppModel {
    enum Phase: Equatable {
        /// No hub address yet, or the user signed out.
        case setup
        /// Checking the stored sign-in with `GET /api/me`.
        case connecting
        /// The hub wants a Cloudflare Access sign-in.
        case signIn
        case ready
    }

    var phase: Phase = .setup
    var hubURL: URL?
    var me: Me?
    var projects: [Project] = []
    var requests: [OpenRequest] = []
    var socketState: HubSocket.State = .closed
    var lastError: String?
    /// Live summaries by session id, from the `summary` frames.
    private(set) var summaries: [String: SessionSummary] = [:]
    /// The selected tab and the Projects tab's navigation, so a notification can open a chat.
    var tab: MainTab = .projects
    var path: [Route] = []
    /// Regular width (the desktop layout): the selected project and session, and what was
    /// pushed over the chat (a room's agent).
    var isRegular = false
    var selectedProjectID: String?
    var selectedSessionID: String?
    var detailPath: [Route] = []
    /// The session whose chat is on screen: its notices show no banner.
    var watching: String?
    /// The device token for Apple push, once the system hands it over.
    var deviceToken: String?
    var pushEnabledOnHub = false
    let artifacts = ArtifactStore()
    /// Whether the user went through onboarding's last step on this device.
    var onboarded: Bool = UserDefaults.standard.bool(forKey: "onboarded")

    func finishOnboarding() {
        onboarded = true
        UserDefaults.standard.set(true, forKey: "onboarded")
    }

    /// The hub's palette for this user, and whether the app follows the device's light or dark
    /// setting with that palette's family (kept on this device).
    var theme: ThemeName = .warm
    var followSystem: Bool = UserDefaults.standard.object(forKey: "followSystem") as? Bool ?? true {
        didSet { UserDefaults.standard.set(followSystem, forKey: "followSystem") }
    }

    func effectiveTheme(systemIsDark: Bool) -> ThemeName {
        followSystem ? theme.variant(light: !systemIsDark) : theme
    }

    /// Saves the palette on the hub, as the web client's Settings does.
    func setTheme(_ t: ThemeName) {
        theme = t
        UserDefaults.standard.set(t.rawValue, forKey: "theme")
        guard let client else { return }
        Task { _ = try? await client.send("PATCH", "/api/me", body: ["theme": t.rawValue]) }
    }
    /// An enrollment code from a link (`hqc-app://enroll?code=…`), shown for confirmation.
    var enrollCode: EnrollCode?

    private(set) var client: HubClient?
    private(set) var socket: HubSocket?

    private static let hubKey = "hubURL"

    init() {
        if let t = UserDefaults.standard.string(forKey: "theme").flatMap(ThemeName.init(rawValue:)) { theme = t }
        if let s = UserDefaults.standard.string(forKey: Self.hubKey), let url = URL(string: s) {
            hubURL = url
            phase = .connecting
        }
    }

    private func tokenAccount(_ url: URL) -> String { "cf-access:\(url.host() ?? "")" }

    /// `https://` is added when the user types no scheme; a path is dropped.
    static func normalize(_ input: String) -> URL? {
        var s = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !s.isEmpty else { return nil }
        if !s.contains("://") { s = "https://" + s }
        guard var c = URLComponents(string: s), let host = c.host, !host.isEmpty,
              c.scheme == "https" || c.scheme == "http" else { return nil }
        c.path = ""
        c.query = nil
        c.fragment = nil
        return c.url
    }

    func setHub(_ url: URL) {
        hubURL = url
        UserDefaults.standard.set(url.absoluteString, forKey: Self.hubKey)
        Task { await connect() }
    }

    /// Called by the login page once Access set its cookie on the hub's host.
    func signedIn(token: String) {
        guard let hubURL else { return }
        Keychain.write(token, account: tokenAccount(hubURL))
        Task { await connect() }
    }

    func signOut() {
        if let token = deviceToken, let client {
            Task { try? await client.unregisterDevice(token: token) }
        }
        if let hubURL { Keychain.delete(tokenAccount(hubURL)) }
        socket?.stop()
        socket = nil
        client = nil
        me = nil
        projects = []
        requests = []
        summaries = [:]
        phase = .signIn
    }

    func forgetHub() {
        signOut()
        hubURL = nil
        UserDefaults.standard.removeObject(forKey: Self.hubKey)
        phase = .setup
    }

    func connect() async {
        guard let hubURL else { phase = .setup; return }
        phase = .connecting
        lastError = nil
        let endpoint = HubEndpoint(baseURL: hubURL, accessToken: Keychain.read(tokenAccount(hubURL)))
        let client = HubClient(endpoint: endpoint)
        do {
            me = try await client.me()
            if let t = me?.theme.flatMap(ThemeName.init(rawValue:)) { theme = t }
        } catch HubError.signInRequired {
            phase = .signIn
            return
        } catch {
            lastError = error.localizedDescription
            // Not reachable: a known hub asks to try again; a new address is asked for again.
            phase = me == nil && !onboarded ? .setup : .signIn
            return
        }
        self.client = client
        socket?.stop()
        let socket = HubSocket(endpoint: endpoint)
        socket.onState = { [weak self] s in self?.socketState = s }
        socket.onSummary = { [weak self] list in
            guard let self else { return }
            for s in list { self.summaries[s.session_id] = s }
        }
        socket.onRequests = { [weak self] list in self?.requests = list }
        socket.onSignInRequired = { [weak self] in self?.signOut() }
        self.socket = socket
        socket.start()
        phase = .ready
        path = []
        await refresh()
        pushEnabledOnHub = await client.apnsEnabled()
        await registerDeviceIfReady()
    }

    /// Sends the device token to the hub once both are there (SPEC §16).
    func registerDeviceIfReady() async {
        guard let token = deviceToken, let client, pushEnabledOnHub else { return }
        #if DEBUG
        let sandbox = true
        #else
        let sandbox = false
        #endif
        do { try await client.registerDevice(token: token, sandbox: sandbox) } catch {
            lastError = error.localizedDescription
        }
    }

    func didRegister(deviceToken data: Data) {
        deviceToken = data.map { String(format: "%02x", $0) }.joined()
        Task { await registerDeviceIfReady() }
    }

    /// `hqc-app://enroll?code=…` and `hqc-app://session/<id>`; the sign-in callback is caught by
    /// its own session and never comes here.
    func handle(url: URL) {
        guard url.scheme == "hqc-app" else { return }
        let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        switch url.host() {
        case "enroll":
            if let code = items.first(where: { $0.name == "code" })?.value { enrollCode = EnrollCode(code: code) }
        case "session":
            let id = url.lastPathComponent
            if !id.isEmpty, id != "/" { open(sessionID: id) }
        default:
            break
        }
    }

    /// Opens a session's chat (a notification, a search hit, a new session): selected in the
    /// desktop layout, pushed on the Projects tab otherwise.
    func open(sessionID: String, title: String = "") {
        if isRegular {
            if let s = summaries[sessionID] { selectedProjectID = s.project_id }
            selectedSessionID = sessionID
            detailPath = []
            return
        }
        tab = .projects
        var p: [Route] = []
        if let s = summaries[sessionID], let project = project(s.project_id) {
            p.append(.project(project))
        }
        p.append(.session(id: sessionID, title: summaries[sessionID]?.title ?? title))
        path = p
    }

    var selectedProject: Project? { selectedProjectID.flatMap { project($0) } }

    /// The personal project first, then favorites, then by name, as the web client's bar.
    var sortedProjects: [Project] {
        projects.sorted { a, b in
            if a.is_personal != b.is_personal { return a.is_personal }
            if (a.favorite_at != nil) != (b.favorite_at != nil) { return a.favorite_at != nil }
            return a.name.localizedCaseInsensitiveCompare(b.name) == .orderedAscending
        }
    }

    func unread(project: String) -> Int {
        summaries.values.filter { $0.project_id == project && $0.unread > 0 }.count
    }

    func refresh() async {
        guard let client else { return }
        do {
            projects = try await client.projects().filter { $0.archived_at == nil }
        } catch HubError.signInRequired {
            signOut()
            return
        } catch {
            lastError = error.localizedDescription
        }
        // The `requests` frame keeps the list current; this load only fills it sooner, so a hub
        // without the endpoint leaves it to the frame.
        if let r = try? await client.requests() { requests = r }
    }

    func foreground() {
        socket?.nudge()
        Task { await refresh() }
    }

    func summary(_ sessionID: String) -> SessionSummary? { summaries[sessionID] }

    func project(_ id: String) -> Project? { projects.first { $0.id == id } }

    func requests(for sessionID: String) -> [OpenRequest] {
        requests.filter { $0.session_id == sessionID }
    }

    /// Answers a request and drops it from the list at once; the next `requests` frame confirms.
    func answer(_ request: OpenRequest, option: ApprovalOption) async throws {
        guard let client else { return }
        try await client.approve(sessionID: request.session_id, requestID: request.request_id, optionID: option.id)
        requests.removeAll { $0.id == request.id }
    }

    /// Signs in through the system browser, where every identity provider works (SPEC §6).
    func signInWithBrowser() async {
        guard let hubURL else { return }
        do {
            let token = try await NativeSignIn.run(hub: hubURL)
            signedIn(token: token)
        } catch NativeSignIn.Failure.cancelled {
            return
        } catch {
            lastError = error.localizedDescription
        }
    }

    /// Unread sessions across projects, for the badge on the projects tab.
    var unreadCount: Int { summaries.values.filter { $0.unread > 0 }.count }
}

enum MainTab: Hashable { case projects, requests, search, settings }

/// A screen of the Projects tab's stack.
enum Route: Hashable {
    case project(Project)
    case session(id: String, title: String)
}

struct EnrollCode: Identifiable, Hashable {
    let code: String
    var id: String { code }
}
