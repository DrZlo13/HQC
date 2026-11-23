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

/// Where the hub is and how the app proves who it is: the hub's origin and, behind Cloudflare
/// Access, the `CF_Authorization` cookie the login page set (RUNBOOK §1.6). Without Access (a dev
/// hub) the cookie is absent and requests go out bare.
nonisolated struct HubEndpoint: Sendable, Equatable {
    let baseURL: URL
    var accessToken: String?

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
        return c.url!
    }

    func authorize(_ req: inout URLRequest) {
        if let accessToken {
            req.setValue("CF_Authorization=\(accessToken)", forHTTPHeaderField: "Cookie")
        }
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
        if (200..<300).contains(http.statusCode) { return data }
        let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        let code = obj?["error"] as? String
        let message = obj?["message"] as? String ?? String(data: data, encoding: .utf8) ?? ""
        if (http.statusCode == 401 || http.statusCode == 403) && code == nil {
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
