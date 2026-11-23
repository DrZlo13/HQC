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
    /// The launch splash has opened onto the app; onboarding starts its animation then.
    var splashDone = false
    /// The hub's project list has arrived once since connecting; until then lists show skeletons.
    var projectsLoaded = false
    /// The wait before the next connection attempt at launch, doubling while the hub is away.
    private var retryDelay: TimeInterval = 0
    /// Request ids seen so far; nil until the first list arrives.
    private var knownRequests: Set<String>?
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
        projectsLoaded = false
        requests = []
        knownRequests = nil
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
            if onboarded {
                // Not reachable at launch: stay on the main screen with its "Connecting…" bubble
                // and try again, sooner first (2, 4, 8 … 30 s).
                retryDelay = min(max(retryDelay * 2, 2), 30)
                let delay = retryDelay
                Task { [weak self] in
                    try? await Task.sleep(for: .seconds(delay))
                    guard let self, self.phase == .connecting, self.client == nil else { return }
                    await self.connect()
                }
                return
            }
            // A new address that is not reachable is asked for again.
            phase = me == nil ? .setup : .signIn
            return
        }
        retryDelay = 0
        self.client = client
        socket?.stop()
        let socket = HubSocket(endpoint: endpoint)
        socket.onState = { [weak self] s in self?.socketState = s }
        socket.onSummary = { [weak self] list in
            guard let self else { return }
            for s in list { self.summaries[s.session_id] = s }
        }
        socket.onRequests = { [weak self] list in self?.receive(requests: list) }
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
    /// The project bar's order: the user's own, as the hub keeps it (SPEC 0.64), the same on
    /// every device.
    var sortedProjects: [Project] { projects }

    /// Moves projects in the bar and saves the order on the hub.
    func moveProjects(from source: IndexSet, to destination: Int) {
        var list = projects
        list.move(fromOffsets: source, toOffset: destination)
        projects = list
        Feedback.play(.select)
        guard let client else { return }
        Task {
            if let saved = try? await client.putProjectOrder(list.map(\.id)) {
                projects = saved.filter { $0.archived_at == nil }
            }
        }
    }

    /// Moves a project one place up or down the bar.
    func moveProject(_ id: String, by delta: Int) {
        guard let i = projects.firstIndex(where: { $0.id == id }) else { return }
        let j = i + delta
        guard j >= 0, j < projects.count else { return }
        moveProjects(from: [i], to: delta > 0 ? j + 1 : j)
    }

    func unread(project: String) -> Int {
        summaries.values.filter { $0.project_id == project && $0.unread > 0 }.count
    }

    func refresh() async {
        guard let client else { return }
        do {
            projects = try await client.projects().filter { $0.archived_at == nil }
            projectsLoaded = true
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
    /// The open requests from the socket. A new one for a session not on screen knocks or asks
    /// in the app too (the chat on screen cues its own); the first list after connecting is
    /// what was already waiting, and stays quiet.
    func receive(requests list: [OpenRequest]) {
        if let known = knownRequests {
            let fresh = list.filter { !known.contains($0.id) && $0.session_id != watching }
            if fresh.contains(where: { $0.event.type == "approval.request" }) {
                Feedback.play(.approval)
            } else if !fresh.isEmpty {
                Feedback.play(.question)
            }
        }
        knownRequests = Set(list.map(\.id))
        requests = list
    }

    func answer(_ request: OpenRequest, option: ApprovalOption) async throws {
        guard let client else { return }
        try await client.approve(sessionID: request.session_id, requestID: request.request_id, optionID: option.id)
        Feedback.play(.answered)
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
