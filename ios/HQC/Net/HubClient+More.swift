import Foundation
import UniformTypeIdentifiers

/// The rest of the hub's REST API (SPEC §6) the app uses.
extension HubClient {
    private func decode<T: Decodable>(_ data: Data, as: T.Type = T.self) throws -> T {
        do { return try Self.decoder.decode(T.self, from: data) } catch {
            throw HubError.badResponse("\(T.self): \(error)")
        }
    }

    // MARK: native sign-in and Apple push

    /// `POST /worker/native/token`: a one-time code and its verifier for the hub credentials.
    func nativeToken(code: String, verifier: String) async throws -> HubCredentials {
        let data = try await send("POST", "/worker/native/token", body: ["code": code, "verifier": verifier])
        let v = try decode(data, as: JSONValue.self)
        if let cookie = v["cookie"]?.string, !cookie.isEmpty {
            return HubCredentials(cookies: HubCredentials.parseCookieHeader(cookie))
        }
        if let bearer = v["bearer"]?.string, !bearer.isEmpty {
            return HubCredentials(bearer: bearer)
        }
        if let token = v["token"]?.string {
            return token.isEmpty ? HubCredentials() : HubCredentials(cookies: ["CF_Authorization": token])
        }
        throw HubError.badResponse("no credentials")
    }

    func apnsEnabled() async -> Bool {
        (try? await get("/api/push/apns", as: JSONValue.self))?["enabled"]?.bool ?? false
    }

    /// With `key`, the hub seals each notice for this device (SPEC 0.218).
    func registerDevice(token: String, sandbox: Bool, key: String? = nil) async throws {
        var body: [String: Any] = ["token": token, "sandbox": sandbox]
        if let key { body["key"] = key }
        _ = try await send("POST", "/api/push/apns", body: body)
    }

    func unregisterDevice(token: String) async throws {
        _ = try await send("DELETE", "/api/push/apns", body: ["token": token])
    }

    // MARK: projects

    func createProject(name: String, description: String) async throws -> Project {
        var body: [String: Any] = ["name": name]
        if !description.isEmpty { body["description"] = description }
        return try decode(try await send("POST", "/api/projects", body: body))
    }

    func patchProject(_ id: String, _ patch: [String: Any]) async throws -> Project {
        try decode(try await send("PATCH", "/api/projects/\(Self.enc(id))", body: patch))
    }

    func setProjectFavorite(_ id: String, _ on: Bool) async throws {
        _ = try await send(on ? "PUT" : "DELETE", "/api/projects/\(Self.enc(id))/favorite")
    }

    func archiveProject(_ id: String) async throws {
        _ = try await send("POST", "/api/projects/\(Self.enc(id))/archive")
    }

    // MARK: sessions

    func patchSession(_ id: String, _ patch: [String: Any]) async throws -> SessionInfo {
        try decode(try await send("PATCH", "/api/sessions/\(Self.enc(id))", body: patch))
    }

    func sessionAction(_ id: String, _ action: String) async throws {
        _ = try await send("POST", "/api/sessions/\(Self.enc(id))/\(action)")
    }

    func deleteSession(_ id: String) async throws {
        _ = try await send("DELETE", "/api/sessions/\(Self.enc(id))")
    }

    func setSessionFavorite(_ id: String, _ on: Bool) async throws {
        _ = try await send(on ? "PUT" : "DELETE", "/api/sessions/\(Self.enc(id))/favorite")
    }

    func stopTask(sessionID: String, taskID: String) async throws {
        _ = try await send("POST", "/api/sessions/\(Self.enc(sessionID))/tasks/\(Self.enc(taskID))/stop")
    }

    // MARK: rooms

    func createRoom(
        projectID: String, title: String, mode: RoomMode, workspaceID: String?, agents: [RoomAgentDraft]
    ) async throws -> SessionInfo {
        var body: [String: Any] = [
            "mode": mode.rawValue,
            "agents": agents.map { a -> [String: Any] in
                var o: [String: Any] = [:]
                if !a.name.trimmingCharacters(in: .whitespaces).isEmpty { o["name"] = a.name }
                if !a.model.trimmingCharacters(in: .whitespaces).isEmpty { o["model"] = a.model }
                if !a.prompt.trimmingCharacters(in: .whitespaces).isEmpty { o["system_prompt"] = a.prompt }
                return o
            },
        ]
        if !title.isEmpty { body["title"] = title }
        if let workspaceID { body["workspace_id"] = workspaceID }
        return try decode(try await send("POST", "/api/projects/\(Self.enc(projectID))/rooms", body: body))
    }

    // MARK: artifacts (SPEC §12)

    /// `POST /api/artifacts`, multipart `file`; the same file uploaded again is the same artifact.
    func upload(data: Data, name: String, mime: String) async throws -> Artifact {
        guard let url = URL(string: "/api/artifacts", relativeTo: endpoint.baseURL) else {
            throw HubError.badResponse("bad path")
        }
        let boundary = "hqc-\(UUID().uuidString)"
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        endpoint.authorize(&req)
        let safeName = name.replacingOccurrences(of: "\"", with: "'")
        var body = Data()
        body.append(Data("--\(boundary)\r\n".utf8))
        body.append(Data("Content-Disposition: form-data; name=\"file\"; filename=\"\(safeName)\"\r\n".utf8))
        body.append(Data("Content-Type: \(mime)\r\n\r\n".utf8))
        body.append(data)
        body.append(Data("\r\n--\(boundary)--\r\n".utf8))
        let (resp, r) = try await session.upload(for: req, from: body)
        guard let http = r as? HTTPURLResponse else { throw HubError.badResponse("not HTTP") }
        if (300..<400).contains(http.statusCode) { throw HubError.signInRequired }
        if (200..<300).contains(http.statusCode) {
            endpoint.absorb(http, url: http.url ?? url)
            return try decode(resp)
        }
        let obj = (try? JSONSerialization.jsonObject(with: resp)) as? [String: Any]
        let code = obj?["error"] as? String
        if http.statusCode == 401 || (http.statusCode == 403 && code == nil) { throw HubError.signInRequired }
        throw HubError.http(status: http.statusCode, code: code,
                            message: obj?["message"] as? String ?? "Upload failed")
    }

    func artifact(_ id: String) async throws -> Artifact {
        try await get("/api/artifacts/\(Self.enc(id))")
    }

    /// The file's bytes, through the signed link the hub gives for ten minutes.
    func artifactData(_ id: String) async throws -> Data {
        let link: ArtifactURL = try await get("/api/artifacts/\(Self.enc(id))/url")
        guard let url = URL(string: link.url, relativeTo: endpoint.baseURL) else {
            throw HubError.badResponse("bad artifact link")
        }
        var req = URLRequest(url: url)
        // The link may be on the hub's own domain, behind the front proxy.
        let sameHost = url.host() == endpoint.baseURL.host()
        if sameHost { endpoint.authorize(&req) }
        let (data, r) = try await session.data(for: req)
        guard let http = r as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw HubError.badResponse("artifact download failed")
        }
        if sameHost { endpoint.absorb(http, url: http.url ?? url) }
        return data
    }

    // MARK: schedules (SPEC §16)

    func schedules(sessionID: String?) async throws -> [Schedule] {
        let q = sessionID.map { "?session_id=\(Self.enc($0))" } ?? ""
        return try await get("/api/schedules\(q)", as: ScheduleList.self).schedules
    }

    func createSchedule(sessionID: String, message: String, cron: String?, at: Date?) async throws -> Schedule {
        var body: [String: Any] = [
            "session_id": sessionID, "message": message, "timezone": TimeZone.current.identifier,
        ]
        if let cron { body["cron"] = cron }
        if let at { body["at"] = ISO8601DateFormatter().string(from: at) }
        return try decode(try await send("POST", "/api/schedules", body: body))
    }

    func setSchedule(_ id: String, enabled: Bool) async throws -> Schedule {
        try decode(try await send("PATCH", "/api/schedules/\(Self.enc(id))", body: ["enabled": enabled]))
    }

    func deleteSchedule(_ id: String) async throws {
        _ = try await send("DELETE", "/api/schedules/\(Self.enc(id))")
    }

    // MARK: search (SPEC §15)

    func search(_ q: String, projectID: String? = nil, cursor: String? = nil) async throws -> SearchPage {
        var c = URLComponents()
        c.queryItems = [URLQueryItem(name: "q", value: q), URLQueryItem(name: "per_session", value: "3")]
        if let projectID { c.queryItems?.append(URLQueryItem(name: "project", value: projectID)) }
        if let cursor { c.queryItems?.append(URLQueryItem(name: "cursor", value: cursor)) }
        return try await get("/api/search?\(c.percentEncodedQuery ?? "")")
    }

    // MARK: workers, enrollment, folders

    func workers() async throws -> [WorkerInfo] {
        try await get("/api/workers?include_revoked=1", as: WorkerList.self).workers
    }

    func revokeWorker(_ id: String) async throws {
        _ = try await send("DELETE", "/api/workers/\(Self.enc(id))")
    }

    func enrollInfo(code: String) async throws -> EnrollInfo {
        try await get("/api/workers/enroll/\(Self.enc(code))")
    }

    func enrollConfirm(code: String, replace: String?) async throws -> EnrollConfirmed {
        var body: [String: Any] = ["code": code]
        if let replace { body["replace_worker_id"] = replace }
        return try decode(try await send("POST", "/api/workers/enroll/confirm", body: body))
    }

    func claudeStatus(workerID: String) async throws -> ClaudeAuthStatus {
        try await get("/api/workers/\(Self.enc(workerID))/claude")
    }

    func claudeLogin(workerID: String) async throws -> ClaudeLogin {
        try decode(try await send("POST", "/api/workers/\(Self.enc(workerID))/claude/login"))
    }

    func claudeLoginCode(workerID: String, loginID: String, code: String) async throws -> ClaudeLoginDone {
        try decode(try await send(
            "POST", "/api/workers/\(Self.enc(workerID))/claude/login/\(Self.enc(loginID))", body: ["code": code]))
    }

    func dirs(workerID: String, path: String?) async throws -> [DirEntry] {
        var q = ""
        if let path, let e = path.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed.subtracting(["&", "=", "+"])) {
            q = "?path=\(e)"
        }
        return try await get("/api/workers/\(Self.enc(workerID))/dirs\(q)", as: DirList.self).entries
    }

    func createWorkspace(projectID: String, workerID: String, path: String, label: String?) async throws -> Workspace {
        var body: [String: Any] = ["project_id": projectID, "worker_id": workerID, "path": path]
        if let label, !label.isEmpty { body["label"] = label }
        return try decode(try await send("POST", "/api/workspaces", body: body))
    }

    func deleteWorkspace(_ id: String) async throws {
        _ = try await send("DELETE", "/api/workspaces/\(Self.enc(id))")
    }
}

extension UTType {
    var mime: String { preferredMIMEType ?? "application/octet-stream" }
}

// MARK: the rest of the web client's session actions

extension HubClient {
    /// A new session in the same project and folder whose conversation is this one's before the
    /// user's message `eventID` (SPEC §6).
    func fork(sessionID: String, eventID: String) async throws -> SessionInfo {
        let data = try await send("POST", "/api/sessions/\(Self.enc(sessionID))/fork", body: ["event_id": eventID])
        return try JSONDecoder().decode(SessionInfo.self, from: data)
    }

    /// Continue a session VS Code drove (SPEC §9.3); 409 within 30 s of its last write.
    func takeover(sessionID: String) async throws {
        _ = try await send("POST", "/api/sessions/\(Self.enc(sessionID))/takeover")
    }

    /// Moves the agent's running call `callID` (one of `foreground_calls`) to the background.
    func background(sessionID: String, callID: String) async throws {
        _ = try await send("POST", "/api/sessions/\(Self.enc(sessionID))/background", body: ["call_id": callID])
    }

    /// A secret or an ssh key for the session's running agent (SPEC 0.147); `requestID`
    /// answers the agent's `hqc_secret`. The value is never kept by the app.
    func giveSecret(sessionID: String, kind: String, name: String, value: String,
                    passphrase: String? = nil, remember: Bool = false, requestID: String? = nil) async throws -> SecretGiven {
        var body: [String: Any] = ["kind": kind, "name": name, "value": value]
        if kind == "ssh_key" {
            if let passphrase, !passphrase.isEmpty { body["passphrase"] = passphrase }
            if remember { body["remember"] = true }
        }
        if let requestID { body["request_id"] = requestID }
        let data = try await send("POST", "/api/sessions/\(Self.enc(sessionID))/secrets", body: body)
        return try JSONDecoder().decode(SecretGiven.self, from: data)
    }

    /// What subagent `taskID` has done, read from its transcript on the worker.
    func subagentHistory(sessionID: String, taskID: String, after: String? = nil) async throws -> SubagentHistory {
        var path = "/api/sessions/\(Self.enc(sessionID))/subagents/\(Self.enc(taskID))/history"
        if let after { path += "?after=\(Self.enc(after))" }
        return try await get(path)
    }

    /// The session continues in folder `path` of its worker, in `project` or a new project
    /// named after the folder.
    func moveSession(_ id: String, path: String, projectID: String?, create: Bool = false) async throws -> SessionInfo {
        var body: [String: Any] = ["path": path]
        if let projectID { body["project"] = projectID }
        if create { body["create"] = true }
        let data = try await send("POST", "/api/sessions/\(Self.enc(id))/move", body: body)
        return try JSONDecoder().decode(SessionInfo.self, from: data)
    }
}

// MARK: projects and their lists

extension HubClient {
    /// The user's order of a project's session list.
    func sessionOrder(projectID: String) async throws -> SessionOrder {
        try await get("/api/projects/\(Self.enc(projectID))/session_order")
    }

    func putSessionOrder(projectID: String, mode: String? = nil, ids: [String]? = nil) async throws -> SessionOrder {
        var body: [String: Any] = [:]
        if let mode { body["mode"] = mode }
        if let ids { body["session_ids"] = ids }
        let data = try await send("PUT", "/api/projects/\(Self.enc(projectID))/session_order", body: body)
        return try JSONDecoder().decode(SessionOrder.self, from: data)
    }

    /// The order of the project bar; the reply is the list in it.
    func putProjectOrder(_ ids: [String]) async throws -> [Project] {
        let data = try await send("PUT", "/api/me/project_order", body: ["project_ids": ids])
        return try JSONDecoder().decode(ProjectList.self, from: data).projects
    }

    /// The archived projects, which `projects` leaves out.
    func archivedProjects() async throws -> [Project] {
        let list: ProjectList = try await get("/api/projects?archived=1")
        return list.projects
    }

    func unarchiveProject(_ id: String) async throws {
        _ = try await send("POST", "/api/projects/\(Self.enc(id))/unarchive")
    }

    func deleteProject(_ id: String) async throws {
        _ = try await send("DELETE", "/api/projects/\(Self.enc(id))")
    }

    /// The project's icon image: PNG or JPEG, square, 128–1024 px, at most 256 KB.
    func uploadProjectIcon(_ id: String, data: Data, mime: String) async throws {
        guard let url = URL(string: "/api/projects/\(Self.enc(id))/icon", relativeTo: endpoint.baseURL) else {
            throw HubError.badResponse("bad path")
        }
        let boundary = "hqc-\(UUID().uuidString)"
        var req = URLRequest(url: url)
        req.httpMethod = "PUT"
        req.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        endpoint.authorize(&req)
        var body = Data()
        body.append(Data("--\(boundary)\r\n".utf8))
        body.append(Data("Content-Disposition: form-data; name=\"file\"; filename=\"icon.\(mime == "image/png" ? "png" : "jpg")\"\r\n".utf8))
        body.append(Data("Content-Type: \(mime)\r\n\r\n".utf8))
        body.append(data)
        body.append(Data("\r\n--\(boundary)--\r\n".utf8))
        let (resp, r) = try await session.upload(for: req, from: body)
        guard let http = r as? HTTPURLResponse else { throw HubError.badResponse("not HTTP") }
        if (300..<400).contains(http.statusCode) { throw HubError.signInRequired }
        if (200..<300).contains(http.statusCode) {
            endpoint.absorb(http, url: http.url ?? url)
            return
        }
        let obj = (try? JSONSerialization.jsonObject(with: resp)) as? [String: Any]
        let code = obj?["error"] as? String
        if http.statusCode == 401 || (http.statusCode == 403 && code == nil) { throw HubError.signInRequired }
        throw HubError.http(status: http.statusCode, code: code,
                            message: obj?["message"] as? String ?? "Upload failed")
    }

    /// Bytes at a path of the hub, signed in (a project's icon image).
    func bytes(at path: String) async throws -> Data {
        guard let url = URL(string: path, relativeTo: endpoint.baseURL) else { throw HubError.badResponse("bad path") }
        var req = URLRequest(url: url)
        endpoint.authorize(&req)
        let (data, r) = try await session.data(for: req)
        guard let http = r as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw HubError.badResponse("icon")
        }
        endpoint.absorb(http, url: http.url ?? url)
        return data
    }
}

// MARK: folders, models, keys and the rest of Settings

extension HubClient {
    private func decodeJSON<T: Decodable>(_ data: Data) throws -> T { try JSONDecoder().decode(T.self, from: data) }

    /// A clone of `url` into a new folder, or a worktree of a folder on `branch`.
    func createWorkspace(_ body: [String: Any]) async throws -> Workspace {
        try decodeJSON(try await send("POST", "/api/workspaces", body: body))
    }

    func patchWorkspace(_ id: String, _ patch: [String: Any]) async throws -> Workspace {
        try decodeJSON(try await send("PATCH", "/api/workspaces/\(Self.enc(id))", body: patch))
    }

    /// Removes the record; with `removeFolder` the worker deletes a folder hqc made too.
    func deleteWorkspace(_ id: String, removeFolder: Bool, force: Bool, archiveSessions: Bool) async throws {
        var q: [String] = []
        if removeFolder { q.append("remove_folder=true") }
        if removeFolder && force { q.append("force=true") }
        if archiveSessions { q.append("archive_sessions=true") }
        _ = try await send("DELETE", "/api/workspaces/\(Self.enc(id))" + (q.isEmpty ? "" : "?" + q.joined(separator: "&")))
    }

    func inspectWorkspace(_ id: String) async throws -> WorkspaceInspection {
        try await get("/api/workspaces/\(Self.enc(id))/inspect")
    }

    func workspaceRules(_ id: String) async throws -> [WorkspaceRule] {
        let list: WorkspaceRuleList = try await get("/api/workspaces/\(Self.enc(id))/rules")
        return list.rules
    }

    func addWorkspaceRules(_ id: String, _ rules: [String]) async throws -> [WorkspaceRule] {
        let list: WorkspaceRuleList = try decodeJSON(try await send("POST", "/api/workspaces/\(Self.enc(id))/rules", body: ["rules": rules]))
        return list.rules
    }

    func deleteWorkspaceRule(_ id: String, ruleID: String) async throws {
        _ = try await send("DELETE", "/api/workspaces/\(Self.enc(id))/rules/\(Self.enc(ruleID))")
    }

    /// The rules the worker reads for the folder and, with `command`, how it decides it.
    func workspacePermissions(_ id: String, command: String? = nil) async throws -> WorkspacePermissions {
        var path = "/api/workspaces/\(Self.enc(id))/permissions"
        if let command, !command.isEmpty { path += "?command=\(Self.enc(command))" }
        return try await get(path)
    }

    func workspaceKeys(_ id: String) async throws -> [WorkspaceKey] {
        let list: WorkspaceKeyList = try await get("/api/workspaces/\(Self.enc(id))/keys")
        return list.keys
    }

    func deleteWorkspaceKey(_ id: String, name: String) async throws {
        _ = try await send("DELETE", "/api/workspaces/\(Self.enc(id))/keys/\(Self.enc(name))")
    }

    func candidates(workerID: String) async throws -> [Candidate] {
        let list: CandidateList = try await get("/api/workers/\(Self.enc(workerID))/candidates")
        return list.candidates
    }

    /// Deletes a revoked worker with its service session and workspaces.
    func purgeWorker(_ id: String) async throws {
        _ = try await send("DELETE", "/api/workers/\(Self.enc(id))?purge=1")
    }

    func providers() async throws -> [ModelProvider] {
        let list: ProviderList = try await get("/api/providers")
        return list.providers
    }

    func saveProvider(id: String?, _ fields: [String: Any]) async throws -> ModelProvider {
        if let id {
            return try decodeJSON(try await send("PATCH", "/api/providers/\(Self.enc(id))", body: fields))
        }
        return try decodeJSON(try await send("POST", "/api/providers", body: fields))
    }

    func deleteProvider(_ id: String) async throws {
        _ = try await send("DELETE", "/api/providers/\(Self.enc(id))")
    }

    /// The owner unpublishes a published page (SPEC 0.209).
    func unpublishArtifact(_ id: String) async throws {
        _ = try await send("DELETE", "/api/artifacts/\(Self.enc(id))/publication")
    }
}
