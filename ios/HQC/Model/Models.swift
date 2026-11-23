import Foundation
import SwiftUI

// The hub objects the app reads (SPEC §3, §6). Optional fields stay optional so an older or
// newer hub decodes; enums with an `unknown` case take any value this build does not know.

nonisolated enum SessionState: String, Codable, Sendable {
    case archived, locked_vscode, worker_offline, waiting, error, running, paused, queued, idle
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = SessionState(rawValue: raw) ?? .unknown
    }

    var label: String {
        switch self {
        case .archived: "Archived"
        case .locked_vscode: "In VS Code"
        case .worker_offline: "Worker offline"
        case .waiting: "Waiting for you"
        case .error: "Error"
        case .running: "Running"
        case .paused: "Paused"
        case .queued: "Queued"
        case .idle: "Idle"
        case .unknown: "Unknown"
        }
    }

    var color: Color {
        switch self {
        case .running, .queued: .accentColor
        case .waiting: .orange
        case .error: .red
        case .worker_offline, .locked_vscode, .paused: .secondary
        case .idle, .archived, .unknown: .gray
        }
    }

    var isBusy: Bool { self == .running || self == .queued }
}

nonisolated enum PermissionMode: String, Codable, Sendable, CaseIterable, Identifiable {
    case ask, auto, allow_all
    var id: String { rawValue }
    var label: String {
        switch self {
        case .ask: "Ask"
        case .auto: "Auto"
        case .allow_all: "Allow all"
        }
    }
}

nonisolated struct Me: Codable, Sendable {
    let id: String
    let email: String
    let display_name: String?
    let personal_project_id: String
    /// The web client's base palette (T49), the same on every device.
    let theme: String?
}

nonisolated struct ProjectIcon: Codable, Hashable, Sendable {
    let kind: String
    let value: String?
    let url: String?
}

nonisolated struct Project: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let slug: String
    let description: String?
    let default_workspace_id: String?
    let is_personal: Bool
    let color: String?
    let icon: ProjectIcon?
    let favorite_at: String?
    let archived_at: String?

    /// The project's color as the web client's `theme.ts` maps it.
    var tint: Color {
        switch color {
        case "plum": Color(hex: 0xB57BD6)
        case "blue": Color(hex: 0x6E9BFF)
        case "rose": Color(hex: 0xE77A9A)
        case "moss": Color(hex: 0x8DBA5B)
        case "coral": Color(hex: 0xF08A5D)
        case "cyan": Color(hex: 0x4FC3E8)
        case "slate": Color(hex: 0x8A93A6)
        default: Color(hex: 0x3FB8A8)
        }
    }

    /// The tile's text: an emoji or letters icon, else the name's first letters.
    var monogram: String {
        if let icon, icon.kind == "emoji" || icon.kind == "letters", let v = icon.value, !v.isEmpty {
            return v
        }
        let words = name.split(whereSeparator: { $0 == " " || $0 == "-" || $0 == "_" })
        let letters = words.prefix(2).compactMap(\.first).map(String.init).joined()
        return letters.isEmpty ? "?" : letters.uppercased()
    }

    var displayName: String { is_personal ? "Personal" : name }
}

nonisolated struct HeldBy: Codable, Hashable, Sendable {
    let entrypoint: String
    let status: String
}

/// `GET /api/sessions/{id}` and the lists (SPEC §6), the fields the app uses.
nonisolated struct SessionInfo: Decodable, Identifiable, Hashable, Sendable {
    let id: String
    let project_id: String
    let workspace_id: String?
    let worker_id: String?
    let title: String
    let slug: String?
    var state: SessionState
    var unread: Int
    var last_seq: Int
    var last_read_seq: Int
    let archived: Bool
    let updated_at: String
    let system: Bool
    let held_by: HeldBy?
    let permission_mode: PermissionMode?
    let effective_permission_mode: PermissionMode?
    /// `agent.model`: the model hqc starts the agent with; absent: the runtime's default.
    let model: String?
    let room_mode: String?
    let room_id: String?
    let favorite_at: String?
    let paused_at: String?
    let archived_at: String?
    let commands: [String]

    var isRoom: Bool { room_mode != nil }

    private enum Keys: String, CodingKey {
        case id, project_id, workspace_id, worker_id, title, slug, state, unread, last_seq, last_read_seq
        case archived, updated_at, system, held_by, permission_mode, effective_permission_mode
        case agent, model, room_mode, room_id, favorite_at, paused_at, archived_at, commands
    }

    /// As the web client's `parseSessionInfo`: what may be absent has a default, and a mode this
    /// build does not know reads as none.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        id = try c.decode(String.self, forKey: .id)
        project_id = try c.decode(String.self, forKey: .project_id)
        workspace_id = try c.decodeIfPresent(String.self, forKey: .workspace_id)
        worker_id = try c.decodeIfPresent(String.self, forKey: .worker_id)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? "Session"
        slug = try c.decodeIfPresent(String.self, forKey: .slug)
        state = try c.decodeIfPresent(SessionState.self, forKey: .state) ?? .unknown
        unread = try c.decodeIfPresent(Int.self, forKey: .unread) ?? 0
        last_seq = try c.decodeIfPresent(Int.self, forKey: .last_seq) ?? 0
        last_read_seq = try c.decodeIfPresent(Int.self, forKey: .last_read_seq) ?? 0
        archived = try c.decodeIfPresent(Bool.self, forKey: .archived) ?? false
        updated_at = try c.decodeIfPresent(String.self, forKey: .updated_at) ?? ""
        system = try c.decodeIfPresent(Bool.self, forKey: .system) ?? false
        held_by = try? c.decodeIfPresent(HeldBy.self, forKey: .held_by)
        permission_mode = (try? c.decodeIfPresent(String.self, forKey: .permission_mode)).flatMap { $0 }
            .flatMap(PermissionMode.init(rawValue:))
        effective_permission_mode = (try? c.decodeIfPresent(String.self, forKey: .effective_permission_mode))
            .flatMap { $0 }.flatMap(PermissionMode.init(rawValue:))
        let agent = (try? c.decodeIfPresent(JSONValue.self, forKey: .agent)) ?? nil
        model = agent?["model"]?.string
        room_mode = try c.decodeIfPresent(String.self, forKey: .room_mode)
        room_id = try c.decodeIfPresent(String.self, forKey: .room_id)
        favorite_at = try c.decodeIfPresent(String.self, forKey: .favorite_at)
        paused_at = try c.decodeIfPresent(String.self, forKey: .paused_at)
        archived_at = try c.decodeIfPresent(String.self, forKey: .archived_at)
        commands = (try? c.decodeIfPresent([String].self, forKey: .commands)) ?? []
    }
}

/// A line of the `summary` / `session.summary` frames (SPEC §6, §17.1).
nonisolated struct SessionSummary: Codable, Hashable, Sendable {
    let session_id: String
    let project_id: String
    let title: String
    let state: SessionState
    let unread: Int
    let last_seq: Int
    let last_read_seq: Int
    let subagents: Int?
    let output_per_s: Double?
}

nonisolated struct Workspace: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let project_id: String
    let worker_id: String
    let path: String
    let label: String?

    var displayName: String {
        if let label, !label.isEmpty { return label }
        return (path as NSString).lastPathComponent
    }
}

/// An open request of the user (SPEC §6 `GET /api/requests`, the `requests` frame).
nonisolated struct OpenRequest: Codable, Identifiable, Hashable, Sendable {
    let session_id: String
    let request_id: String
    let kind: String
    let address: String
    let title: String
    let worker_name: String?
    let event: HubEvent

    var id: String { "\(session_id)/\(request_id)" }
}

nonisolated struct ProjectList: Codable, Sendable { let projects: [Project] }
nonisolated struct SessionList: Decodable, Sendable { let sessions: [SessionInfo] }
nonisolated struct WorkspaceList: Codable, Sendable { let workspaces: [Workspace] }
nonisolated struct RequestList: Codable, Sendable { let requests: [OpenRequest] }
nonisolated struct EventPage: Codable, Sendable { let events: [HubEvent]; let last_seq: Int }
nonisolated struct InputResponse: Codable, Sendable { let event: HubEvent }

nonisolated extension Color {
    init(hex: UInt32) {
        self.init(
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255
        )
    }
}
