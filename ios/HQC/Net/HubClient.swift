import Foundation

nonisolated enum HubError: LocalizedError, Sendable {
    /// Cloudflare Access (or the hub) wants a sign-in: a redirect to the login page, 401 or 403
    /// without a hub error body.
    case signInRequired
    case http(status: Int, code: String?, message: String)
    case badResponse(String)

    var errorDescription: String? {
        switch self {
        case .signInRequired: "Sign in to the hub again."
        case .http(let status, _, let message): message.isEmpty ? "HTTP \(status)" : message
        case .badResponse(let why): "Unexpected answer from the hub: \(why)"
        }
    }
}

/// Credentials that get requests past the hub's front proxy (SPEC §6): either cookies
/// (`CF_Authorization` for Cloudflare Access, `_oauth2_proxy*` or similar for an OIDC proxy)
/// or an `Authorization: Bearer` token, or neither (dev mode).
nonisolated struct HubCredentials: Codable, Equatable, Sendable {
    var cookies: [String: String] = [:]
    var bearer: String?

    var isEmpty: Bool { cookies.isEmpty && (bearer?.isEmpty ?? true) }

    /// Splits a `Cookie` header (`a=1; b=2==`) on the first `=` of each pair.
    static func parseCookieHeader(_ header: String) -> [String: String] {
        var out: [String: String] = [:]
        for part in header.split(separator: ";") {
            let trimmed = part.trimmingCharacters(in: .whitespaces)
            guard let eq = trimmed.firstIndex(of: "=") else { continue }
            let name = String(trimmed[..<eq]).trimmingCharacters(in: .whitespaces)
            let value = String(trimmed[trimmed.index(after: eq)...]).trimmingCharacters(in: .whitespaces)
            guard !name.isEmpty else { continue }
            out[name] = value
        }
        return out
    }

    /// Reads either the JSON format or a legacy raw Access JWT saved by builds 17–18.
    static func decodeStored(_ raw: String?) -> HubCredentials {
        guard let raw, !raw.isEmpty else { return HubCredentials() }
        if raw.hasPrefix("{"),
           let data = raw.data(using: .utf8),
           let decoded = try? JSONDecoder().decode(HubCredentials.self, from: data) {
            return decoded
        }
        return HubCredentials(cookies: ["CF_Authorization": raw])
    }

    func encodeStored() -> String? {
        guard let data = try? JSONEncoder().encode(self) else { return nil }
        return String(data: data, encoding: .utf8)
    }
}

/// Shared, lock-protected credentials for one hub connection. `HubClient` and `HubSocket` hold
/// the same instance so `Set-Cookie` refreshes from an OIDC reverse proxy (e.g. oauth2-proxy's
/// `OAUTH2_PROXY_COOKIE_REFRESH` via Traefik `addAuthCookiesToResponse`, RUNBOOK §1.9) apply to
/// every subsequent HTTP request and WebSocket reconnect, and persist to Keychain.
nonisolated final class HubAuth: @unchecked Sendable {
    private let lock = NSLock()
    private var creds: HubCredentials
    private let onUpdate: (@Sendable (HubCredentials) -> Void)?

    init(_ creds: HubCredentials = HubCredentials(), onUpdate: (@Sendable (HubCredentials) -> Void)? = nil) {
        self.creds = creds
        self.onUpdate = onUpdate
    }

    func authorize(_ req: inout URLRequest) {
        let current = lock.withLock { creds }
        if !current.cookies.isEmpty {
            let header = current.cookies
                .sorted { $0.key < $1.key }
                .map { "\($0.key)=\($0.value)" }
                .joined(separator: "; ")
            req.setValue(header, forHTTPHeaderField: "Cookie")
        }
        if let bearer = current.bearer, !bearer.isEmpty {
            req.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        }
    }

    /// Merges `Set-Cookie` headers returned by the hub's own host into the cookie jar.
    func absorb(response: HTTPURLResponse, url: URL, hubHost: String?) {
        guard let hubHost, url.host()?.lowercased() == hubHost.lowercased() else { return }
        var fields: [String: String] = [:]
        for (k, v) in response.allHeaderFields {
            if let ks = k as? String, let vs = v as? String { fields[ks] = vs }
        }
        let incoming = HTTPCookie.cookies(withResponseHeaderFields: fields, for: url)
        guard !incoming.isEmpty else { return }
        let now = Date()
        let updated: HubCredentials? = lock.withLock {
            var next = creds
            for cookie in incoming {
                if !cookie.path.isEmpty, cookie.path != "/", !url.path.hasPrefix(cookie.path) {
                    continue
                }
                if (cookie.expiresDate.map { $0 <= now } ?? false) || cookie.value.isEmpty {
                    next.cookies.removeValue(forKey: cookie.name)
                } else {
                    next.cookies[cookie.name] = cookie.value
                }
            }
            guard next != creds else { return nil }
            creds = next
            return next
        }
        if let updated { onUpdate?(updated) }
    }
}

/// Where the hub is and how the app proves who it is: the hub's origin and, behind Cloudflare
/// Access or an OIDC reverse proxy (RUNBOOK §1.6, §1.9), the proxy's cookies (or bearer token).
/// Without auth (a dev hub) the credentials are empty and requests go out bare.
nonisolated struct HubEndpoint: Sendable {
    let baseURL: URL
    let auth: HubAuth

    init(baseURL: URL, auth: HubAuth = HubAuth()) {
        self.baseURL = baseURL
        self.auth = auth
    }

    /// `https://host[:port]`, the value the hub compares `Origin` with (SPEC §20).
    var origin: String {
        var s = "\(baseURL.scheme ?? "https")://\(baseURL.host() ?? "")"
        if let port = baseURL.port { s += ":\(port)" }
        return s
    }

    var socketURL: URL {
        var c = URLComponents(url: baseURL, resolvingAgainstBaseURL: false)!
        c.scheme = baseURL.scheme == "http" ? "ws" : "wss"
        c.path = "/api/ws"
        // Names the socket as the iOS app's: the hub then holds back Apple push for a session
        // only while this app shows it, not while a web tab does (SPEC §16).
        c.queryItems = [URLQueryItem(name: "client", value: "ios")]
        return c.url!
    }

    func authorize(_ req: inout URLRequest) { auth.authorize(&req) }

    func absorb(_ response: HTTPURLResponse, url: URL?) {
        guard let url else { return }
        auth.absorb(response: response, url: url, hubHost: baseURL.host())
    }
}

/// Refuses redirects so that Access sending a signed-out request to its login page comes back
/// as a 302 the client can read, not as the login page's HTML.
private nonisolated final class NoRedirect: NSObject, URLSessionTaskDelegate, Sendable {
    func urlSession(
        _ session: URLSession, task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping @Sendable (URLRequest?) -> Void
    ) {
        completionHandler(nil)
    }
}

/// The hub's REST API (SPEC §6), the calls the app makes.
nonisolated final class HubClient: Sendable {
    let endpoint: HubEndpoint
    let session: URLSession

    init(endpoint: HubEndpoint) {
        self.endpoint = endpoint
        let config = URLSessionConfiguration.default
        config.httpShouldSetCookies = false
        config.httpCookieAcceptPolicy = .never
        config.waitsForConnectivity = false
        config.timeoutIntervalForRequest = 30
        session = URLSession(configuration: config, delegate: NoRedirect(), delegateQueue: nil)
    }

    static let decoder = JSONDecoder()

    func send(_ method: String, _ path: String, body: [String: Any]? = nil) async throws -> Data {
        guard let url = URL(string: path, relativeTo: endpoint.baseURL) else {
            throw HubError.badResponse("bad path \(path)")
        }
        var req = URLRequest(url: url)
        req.httpMethod = method
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        endpoint.authorize(&req)
        if let body {
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let (data, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse else { throw HubError.badResponse("not HTTP") }
        if (300..<400).contains(http.statusCode) { throw HubError.signInRequired }
        if (200..<300).contains(http.statusCode) {
            endpoint.absorb(http, url: http.url ?? url)
            return data
        }
        let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        let code = obj?["error"] as? String
        let message = obj?["message"] as? String ?? String(data: data, encoding: .utf8) ?? ""
        if http.statusCode == 401 || (http.statusCode == 403 && code == nil) {
            throw HubError.signInRequired
        }
        throw HubError.http(status: http.statusCode, code: code, message: message)
    }

    func get<T: Decodable>(_ path: String, as: T.Type = T.self) async throws -> T {
        let data = try await send("GET", path)
        do {
            return try Self.decoder.decode(T.self, from: data)
        } catch {
            throw HubError.badResponse("\(path): \(error)")
        }
    }

    static func enc(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(["/"])) ?? s
    }

    // MARK: calls

    func me() async throws -> Me { try await get("/api/me") }

    func projects() async throws -> [Project] {
        try await get("/api/projects", as: ProjectList.self).projects
    }

    func sessions(projectID: String) async throws -> [SessionInfo] {
        try await get("/api/projects/\(Self.enc(projectID))/sessions", as: SessionList.self).sessions
    }

    func session(_ id: String) async throws -> SessionInfo {
        try await get("/api/sessions/\(Self.enc(id))")
    }

    func workspaces(projectID: String) async throws -> [Workspace] {
        try await get("/api/workspaces?project_id=\(Self.enc(projectID))", as: WorkspaceList.self).workspaces
    }

    func requests() async throws -> [OpenRequest] {
        try await get("/api/requests", as: RequestList.self).requests
    }

    /// The newest `limit` events, oldest first.
    func latestEvents(sessionID: String, limit: Int = 200) async throws -> EventPage {
        try await get("/api/sessions/\(Self.enc(sessionID))/events?before_seq=\(Int(Int32.max))&limit=\(limit)")
    }

    func events(sessionID: String, before seq: Int, limit: Int = 200) async throws -> EventPage {
        try await get("/api/sessions/\(Self.enc(sessionID))/events?before_seq=\(seq)&limit=\(limit)")
    }

    func createSession(
        projectID: String, title: String?, workspaceID: String?, permissionMode: PermissionMode?,
        model: String? = nil, providerID: String? = nil
    ) async throws -> SessionInfo {
        var body: [String: Any] = [:]
        if let model, !model.isEmpty { body["model"] = model }
        if let providerID { body["provider_id"] = providerID }
        if let title, !title.isEmpty { body["title"] = title }
        if let workspaceID { body["workspace_id"] = workspaceID }
        if let permissionMode { body["permission_mode"] = permissionMode.rawValue }
        let data = try await send("POST", "/api/projects/\(Self.enc(projectID))/sessions", body: body)
        return try Self.decoder.decode(SessionInfo.self, from: data)
    }

    /// A repeated `clientOpID` returns the same stored message (SPEC §6), so a retry is safe.
    func input(
        sessionID: String, text: String, clientOpID: String, artifactIDs: [String] = []
    ) async throws -> HubEvent {
        let data = try await send(
            "POST", "/api/sessions/\(Self.enc(sessionID))/input",
            body: ["text": text, "artifact_ids": artifactIDs, "client_op_id": clientOpID]
        )
        return try Self.decoder.decode(InputResponse.self, from: data).event
    }

    func stop(sessionID: String) async throws {
        _ = try await send("POST", "/api/sessions/\(Self.enc(sessionID))/stop")
    }

    func retry(sessionID: String) async throws {
        _ = try await send("POST", "/api/sessions/\(Self.enc(sessionID))/retry")
    }

    func archive(sessionID: String) async throws {
        _ = try await send("POST", "/api/sessions/\(Self.enc(sessionID))/archive")
    }

    func setPermissionMode(sessionID: String, _ mode: PermissionMode) async throws {
        _ = try await send("PATCH", "/api/sessions/\(Self.enc(sessionID))", body: ["permission_mode": mode.rawValue])
    }

    /// `message`: a text answer instead of the call, only with a deny option (SPEC §10.1).
    func approve(sessionID: String, requestID: String, optionID: String, message: String? = nil) async throws {
        var body: [String: Any] = ["option_id": optionID]
        if let message { body["message"] = message }
        _ = try await send(
            "POST", "/api/sessions/\(Self.enc(sessionID))/requests/\(Self.enc(requestID))", body: body)
    }

    func clarify(sessionID: String, requestID: String, answers: [String: Any]) async throws {
        _ = try await send(
            "POST", "/api/sessions/\(Self.enc(sessionID))/requests/\(Self.enc(requestID))",
            body: ["answers": answers])
    }

    func markRead(sessionID: String, seq: Int) async throws {
        _ = try await send("PUT", "/api/sessions/\(Self.enc(sessionID))/read_cursor", body: ["last_read_seq": seq])
    }
}
