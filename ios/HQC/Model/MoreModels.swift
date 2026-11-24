import Foundation

// The rest of the hub's objects the app reads (SPEC §3, §6, §12, §15, §16): artifacts,
// schedules, search, workers, enrollment, folders, rooms.

nonisolated struct Artifact: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let kind: String
    let name: String
    let mime: String?
    let size: Int?
    let public_url: String?

    var isImage: Bool { kind == "image" || (mime ?? "").hasPrefix("image/") }
}

nonisolated struct ArtifactURL: Codable, Sendable {
    let url: String
    let expires_at: String?
}

nonisolated struct Schedule: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let session_id: String
    let cron: String?
    let at: String?
    let timezone: String
    let message: String
    var enabled: Bool
    let catch_up: Bool?
    let next_run: String?
    let last_run: String?
}

nonisolated struct ScheduleList: Codable, Sendable { let schedules: [Schedule] }

nonisolated struct SearchHit: Codable, Identifiable, Hashable, Sendable {
    let session_id: String
    let address: String
    let title: String
    let archived: Bool
    let seq: Int?
    let type: String?
    let ts: String
    let snippet: String
    let session_hits: Int

    var id: String { "\(session_id)/\(seq ?? -1)/\(type ?? "title")" }
}

nonisolated struct SearchPage: Codable, Sendable {
    let results: [SearchHit]
    let next_cursor: String?
}

nonisolated struct WorkerInfo: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let os: String?
    let status: String?
    let revoked_at: String?
    let last_seen: String?
    /// The usage limits of the worker's Claude account as it last reported them.
    let limits: WorkerLimits?
    /// `session_secrets = true` in its worker.toml: it takes secrets for its owner's sessions.
    let session_secrets: Bool?

    var isOnline: Bool { status == "online" }
    var isRevoked: Bool { status == "revoked" || revoked_at != nil }
}

nonisolated struct WorkerList: Codable, Sendable { let workers: [WorkerInfo] }

nonisolated struct ReplaceableWorker: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let os: String
    let status: String
    let sessions_count: Int
    let same_machine: Bool?
}

nonisolated struct EnrollInfo: Codable, Sendable {
    let name: String
    let os: String
    let created_at: String
    let country: String?
    let replaceable: [ReplaceableWorker]?
}

nonisolated struct EnrollConfirmed: Codable, Sendable {
    let worker_id: String
    let name: String
    let replaced: String?
}

nonisolated struct DirEntry: Codable, Identifiable, Hashable, Sendable {
    let name: String
    let path: String
    var id: String { path }
}

nonisolated struct DirList: Codable, Sendable { let entries: [DirEntry] }

nonisolated struct ClaudeAuthStatus: Codable, Sendable {
    let logged_in: Bool
    let email: String?
    let subscription_type: String?
    let error: String?
}

nonisolated struct ClaudeLogin: Codable, Sendable {
    let login_id: String
    let url: String
}

nonisolated struct ClaudeLoginDone: Codable, Sendable {
    let ok: Bool
    let message: String?
}

nonisolated enum RoomMode: String, CaseIterable, Identifiable, Sendable {
    case conversation, council
    var id: String { rawValue }
    var label: String { self == .conversation ? "Conversation" : "Council" }
    var explanation: String {
        switch self {
        case .conversation: "Agents speak one at a time, in order, and answer each other."
        case .council: "Every agent answers your message on its own."
        }
    }
}

/// One agent of a new room.
struct RoomAgentDraft: Identifiable, Hashable {
    let id = UUID()
    var name = ""
    var model = ""
    var prompt = ""
}

/// What the agent runs now: a subagent, a background command, a Monitor, a wakeup or a cron
/// job (`subagents.live`, SPEC §4).
nonisolated struct LiveTask: Identifiable, Hashable, Sendable {
    let id: String
    let description: String
    let activity: String?
    let kind: String
    let tokens: Int?
    let toolUses: Int?
    let durationMs: Double
    let stoppable: Bool

    static func list(from payload: JSONValue) -> [LiveTask] {
        (payload["agents"]?.array ?? []).compactMap { a in
            guard let id = a["task_id"]?.string else { return nil }
            return LiveTask(
                id: id,
                description: a["description"]?.string ?? "",
                activity: a["activity"]?.string,
                kind: a["kind"]?.string ?? "agent",
                tokens: a["total_tokens"]?.int,
                toolUses: a["tool_uses"]?.int,
                durationMs: a["duration_ms"]?.double ?? 0,
                stoppable: a["stoppable"]?.bool ?? false
            )
        }
    }

    var symbol: String {
        switch kind {
        case "command": "terminal"
        case "monitor": "eye"
        case "wakeup": "alarm"
        case "cron": "calendar.badge.clock"
        default: "person.crop.circle.badge.clock"
        }
    }
}

/// A usage window of a worker's Claude account: how much is used, in percent, and when it
/// resets (Unix seconds).
nonisolated struct LimitWindow: Codable, Hashable, Sendable {
    let utilization: Double
    let resets_at: Double
    var resetsAt: Date { Date(timeIntervalSince1970: resets_at) }
    /// The reset time has passed since the worker reported it.
    var hasReset: Bool { resetsAt <= Date() }
}

nonisolated struct WorkerLimits: Codable, Hashable, Sendable {
    /// The 5-hour window ("Current session" in Claude Code's /usage).
    let five_hour: LimitWindow?
    /// The 7-day window of all models.
    let seven_day: LimitWindow?
    /// The nearest limit's state: allowed, allowed_warning or rejected.
    let status: String?
    let rate_limit_type: String?
    let resets_at: Double?

    /// The limits as they stand now. The worker reports them only when a turn runs, so a
    /// report can outlive its windows: a window whose reset time has passed is empty again,
    /// and a limit hit before that reset no longer holds.
    func current(_ now: Date = Date()) -> WorkerLimits {
        func fresh(_ w: LimitWindow?) -> LimitWindow? {
            guard let w else { return nil }
            return w.resetsAt <= now ? LimitWindow(utilization: 0, resets_at: w.resets_at) : w
        }
        let rejectedStill = status == "rejected"
            && (resets_at.map { Date(timeIntervalSince1970: $0) > now } ?? true)
        return WorkerLimits(five_hour: fresh(five_hour), seven_day: fresh(seven_day),
                            status: rejectedStill ? status : (status == "rejected" ? "allowed" : status),
                            rate_limit_type: rate_limit_type, resets_at: resets_at)
    }
}

/// What a subagent has done, from its transcript on the worker.
nonisolated struct SubagentHistory: Codable, Sendable {
    let events: [HubEvent]
    let truncated: Bool
    let continues: Bool?
}

/// The hub's answer to a secret: what the agent was told, never the value.
nonisolated struct SecretGiven: Codable, Sendable {
    let note: String
    let path: String?
    let fingerprint: String?
    let public_key: String?
    let remembered: Bool?
}

/// How a project's session list is ordered: by state, by recency, or the user's own.
nonisolated struct SessionOrder: Codable, Sendable {
    let mode: String
    let session_ids: [String]
}

// MARK: folders, models and keys

/// One of hqc's allow rules of a folder.
nonisolated struct WorkspaceRule: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let rule: String
    let created_at: String
}

nonisolated struct WorkspaceRuleList: Codable, Sendable { let rules: [WorkspaceRule] }

/// The rules of one file the worker reads for the folder (worker.toml, Claude Code's settings).
nonisolated struct RuleSource: Codable, Hashable, Sendable {
    let source: String
    let allow: [String]
    let deny: [String]
    let ask: [String]
    let error: String?
}

nonisolated struct CheckResult: Codable, Sendable {
    let verdict: String
    let rule: String?
    let source: String?
}

/// The worker's view of a folder's rules, and how it decides a command.
nonisolated struct WorkspacePermissions: Codable, Sendable {
    let mode: String?
    let sources: [RuleSource]
    let hqc: [String]
    let check: CheckResult?
}

nonisolated struct WorkspaceKey: Codable, Identifiable, Hashable, Sendable {
    let name: String
    let fingerprint: String
    var id: String { name }
}

nonisolated struct WorkspaceKeyList: Codable, Sendable { let keys: [WorkspaceKey] }

/// Uncommitted changes and unpushed commits of a folder hqc made.
nonisolated struct WorkspaceInspection: Codable, Sendable {
    struct Unpushed: Codable, Hashable, Sendable {
        let branch: String
        let commits: Int
    }
    let git: Bool
    let dirty: [String]
    let dirty_count: Int
    let unpushed: [Unpushed]
}

/// A folder with Claude Code sessions on a worker, to import.
nonisolated struct Candidate: Codable, Identifiable, Hashable, Sendable {
    let path: String
    let sessions_count: Int
    let last_active: String
    var id: String { path }
}

nonisolated struct CandidateList: Codable, Sendable { let candidates: [Candidate] }

/// An Anthropic-compatible endpoint with one model, which a session can pick (SPEC 0.144).
nonisolated struct ModelProvider: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let label: String
    let base_url: String
    let model: String
    let context_tokens: Int?
    /// The workers whose worker.toml allows it.
    let workers: [String]
}

nonisolated struct ProviderList: Codable, Sendable { let providers: [ModelProvider] }
