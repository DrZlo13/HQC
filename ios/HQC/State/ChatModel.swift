import Foundation
import Observation

/// What one chat shows: the stored events by `seq`, the live `agent.delta` stream, and the
/// messages being sent. A port of the essentials of the web client's `timeline.ts`.
@MainActor
@Observable
final class ChatModel {
    let sessionID: String
    private(set) var info: SessionInfo?
    private(set) var items: [FeedItem] = []
    /// The answer being written, from `agent.delta` (not the thinking's).
    private(set) var streaming = ""
    /// The thinking being written.
    private(set) var thinking = ""
    /// What the agent runs now (`subagents.live`): subagents, background commands, wakeups.
    private(set) var live: [LiveTask] = []
    private(set) var loading = true
    private(set) var hasOlder = false
    var error: String?

    private var bySeq: [Int: HubEvent] = [:]
    /// Inputs sent but not yet echoed with a `seq`, by `client_op_id`.
    private var pending: [String: HubEvent] = [:]
    private var lastSeq = 0
    private var oldestSeq = Int.max
    private weak var app: AppModel?

    init(sessionID: String) {
        self.sessionID = sessionID
    }

    var state: SessionState { app?.summary(sessionID)?.state ?? info?.state ?? .unknown }

    func start(app: AppModel) async {
        self.app = app
        guard let client = app.client else { return }
        do {
            async let s = client.session(sessionID)
            async let page = client.latestEvents(sessionID: sessionID)
            info = try await s
            let p = try await page
            for ev in p.events { apply(ev, rebuild: false) }
            hasOlder = p.events.count >= 200
            rebuild()
        } catch {
            self.error = error.localizedDescription
        }
        loading = false
        app.socket?.subscribe(sessionID, .init(
            afterSeq: { [weak self] in self?.lastSeq ?? 0 },
            onEvent: { [weak self] ev in self?.apply(ev) },
            onDelivery: { [weak self] id, d in self?.delivery(id, d) },
            onReset: { [weak self] in self?.resetStream() }
        ))
        markRead()
    }

    func stop() {
        app?.socket?.unsubscribe(sessionID)
    }

    func loadOlder() async {
        guard hasOlder, let client = app?.client, oldestSeq != .max else { return }
        do {
            let p = try await client.events(sessionID: sessionID, before: oldestSeq)
            for ev in p.events { apply(ev, rebuild: false) }
            hasOlder = p.events.count >= 200
            rebuild()
        } catch {
            self.error = error.localizedDescription
        }
    }

    func send(_ text: String, artifacts: [Artifact] = []) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty || !artifacts.isEmpty, let client = app?.client else { return }
        let ids = artifacts.map(\.id)
        let op = UUID().uuidString
        // Shown at once, marked as sending, until the hub's copy with a `seq` replaces it.
        pending[op] = HubEvent(
            session_id: sessionID, event_id: "local-\(op)", seq: nil,
            ts: ISO8601DateFormatter().string(from: .now), type: "user.message",
            author: .init(kind: "user", id: app?.me?.id),
            payload: .object(["text": .string(trimmed), "artifact_ids": .array(ids.map(JSONValue.string))]),
            client_op_id: op, delivery: "sending")
        rebuild()
        do {
            let ev = try await client.input(sessionID: sessionID, text: trimmed, clientOpID: op, artifactIDs: ids)
            pending[op] = nil
            apply(ev)
        } catch {
            pending[op]?.delivery = "failed"
            self.error = error.localizedDescription
            rebuild()
        }
    }

    func stopAgent() async {
        do { try await app?.client?.stop(sessionID: sessionID) } catch { self.error = error.localizedDescription }
    }

    func retry() async {
        do { try await app?.client?.retry(sessionID: sessionID) } catch { self.error = error.localizedDescription }
    }

    func setMode(_ mode: PermissionMode) async {
        do {
            try await app?.client?.setPermissionMode(sessionID: sessionID, mode)
            info = try await app?.client?.session(sessionID)
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// Pause, resume, restart, archive, unarchive (SPEC §17.3, §17.4).
    func act(_ action: String) async {
        do {
            try await app?.client?.sessionAction(sessionID, action)
            info = try await app?.client?.session(sessionID)
        } catch {
            self.error = error.localizedDescription
        }
    }

    func patch(_ fields: [String: Any]) async {
        do {
            info = try await app?.client?.patchSession(sessionID, fields)
        } catch {
            self.error = error.localizedDescription
        }
    }

    func setFavorite(_ on: Bool) async {
        do {
            try await app?.client?.setSessionFavorite(sessionID, on)
            info = try await app?.client?.session(sessionID)
        } catch {
            self.error = error.localizedDescription
        }
    }

    func stopTask(_ task: LiveTask) async {
        do { try await app?.client?.stopTask(sessionID: sessionID, taskID: task.id) } catch {
            self.error = error.localizedDescription
        }
    }

    func approve(requestID: String, option: ApprovalOption) async {
        do {
            try await app?.client?.approve(sessionID: sessionID, requestID: requestID, optionID: option.id)
            Feedback.play(.answered)
            app?.requests.removeAll { $0.session_id == sessionID && $0.request_id == requestID }
        } catch {
            self.error = error.localizedDescription
        }
    }

    func clarify(requestID: String, answers: [String: Any]) async {
        do {
            try await app?.client?.clarify(sessionID: sessionID, requestID: requestID, answers: answers)
            Feedback.play(.answered)
            app?.requests.removeAll { $0.session_id == sessionID && $0.request_id == requestID }
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func markRead() {
        guard lastSeq > 0, let client = app?.client else { return }
        let seq = lastSeq
        Task { try? await client.markRead(sessionID: sessionID, seq: seq) }
    }

    // MARK: events

    private func apply(_ ev: HubEvent, rebuild again: Bool = true) {
        if ev.type == "agent.delta" {
            if ev.payload["reset"]?.bool == true {
                streaming = ""
                thinking = ""
            } else if ev.payload["thinking"]?.bool == true {
                thinking += ev.text
            } else {
                streaming += ev.text
            }
            return
        }
        if ev.type == "subagents.live" {
            live = LiveTask.list(from: ev.payload)
            return
        }
        // Only stored events are drawn (SPEC §5.2); other transport-only frames are dropped.
        guard let seq = ev.seq else { return }
        if let op = ev.client_op_id { pending[op] = nil }
        if let seen = bySeq[seq] {
            if seen.event_id == ev.event_id, seen.delivery != ev.delivery {
                bySeq[seq] = ev
                if again { rebuild() }
            }
            return
        }
        bySeq[seq] = ev
        oldestSeq = min(oldestSeq, seq)
        let fresh = seq > lastSeq
        lastSeq = max(lastSeq, seq)
        if fresh {
            switch ev.type {
            case "agent.message":
                streaming = ""
                thinking = ""
            case "agent.thinking":
                thinking = ""
            default:
                break
            }
        }
        if again {
            rebuild()
            if fresh {
                markRead()
                cue(for: ev)
            }
        }
    }

    /// What the agent just did, felt and heard while this chat is open. The finished turn and
    /// a failure come from the session's state (ChatView); the user's own message from Send.
    private func cue(for ev: HubEvent) {
        switch ev.type {
        case "tool.call": Feedback.play(.tool)
        case "tool.result" where ev.payload["error"]?.bool == true: Feedback.play(.toolFailed)
        case "approval.request": Feedback.play(.approval)
        case "clarify.request": Feedback.play(.question)
        case "peer.message": Feedback.play(.peer)
        default: break
        }
    }

    private func delivery(_ eventID: String, _ delivery: String) {
        guard let (seq, ev) = bySeq.first(where: { $0.value.event_id == eventID }) else { return }
        var copy = ev
        copy.delivery = delivery
        bySeq[seq] = copy
        rebuild()
    }

    private func resetStream() {
        streaming = ""
        thinking = ""
        live = []
    }

    /// Folds the events into what the feed draws: a tool call with its result, a request with
    /// whether it is still open, hidden what a `retraction` took back.
    private func rebuild() {
        let events = bySeq.keys.sorted().compactMap { bySeq[$0] }
        var hidden = Set<String>()
        var results: [String: HubEvent] = [:]
        var closed = Set<String>()
        for ev in events {
            switch ev.type {
            case "retraction": hidden.formUnion(ev.payload["event_ids"]?.stringArray ?? [])
            case "tool.result": if let id = ev.callID { results[id] = ev }
            case "approval.response", "approval.cancel", "clarify.response", "clarify.cancel":
                if let id = ev.requestID { closed.insert(id) }
            default: break
            }
        }
        var out: [FeedItem] = []
        for ev in events where !hidden.contains(ev.event_id) {
            switch ev.type {
            case "user.message":
                out.append(.user(ev))
            case "peer.message":
                out.append(.peer(ev))
            case "agent.message":
                if ev.payload["runtime"]?.bool == true {
                    out.append(.note(ev.event_id, ev.text, .warning))
                } else if !ev.text.isEmpty {
                    out.append(.agent(ev))
                }
            case "agent.thinking":
                if !ev.text.isEmpty { out.append(.thinking(ev)) }
            case "tool.call":
                // Calls one after another fold into one group, as the web client's feed does.
                let call = ToolCallItem(call: ev, result: ev.callID.flatMap { results[$0] })
                if case .tools(let calls)? = out.last {
                    out[out.count - 1] = .tools(calls + [call])
                } else {
                    out.append(.tools([call]))
                }
            case "usage":
                // Under the answer it ended: tokens in and out, cache, cost.
                if ev.payload["input_tokens"]?.double != nil || ev.payload["output_tokens"]?.double != nil {
                    out.append(.usage(ev))
                }
            case "approval.request":
                let open = !(ev.requestID.map(closed.contains) ?? true)
                out.append(.approval(ev, open: open))
            case "clarify.request":
                let open = !(ev.requestID.map(closed.contains) ?? true)
                out.append(.clarify(ev, open: open))
            case "attachment":
                out.append(.attachment(ev))
            case "system":
                let level: FeedItem.NoteLevel = ev.payload["level"]?.string == "error" ? .error : .info
                if !ev.text.isEmpty { out.append(.note(ev.event_id, ev.text, level)) }
            case "status":
                if ev.payload["state"]?.string == "error" {
                    let detail = ev.payload["detail"]?.string ?? "The agent stopped with an error"
                    out.append(.note(ev.event_id, detail, .error))
                }
            default:
                break // usage, responses, cancels and types this build does not draw
            }
        }
        for ev in pending.values.sorted(by: { $0.ts < $1.ts }) {
            out.append(.user(ev))
        }
        items = out
    }
}

/// One row of the chat.
enum FeedItem: Identifiable {
    enum NoteLevel { case info, warning, error }

    case user(HubEvent)
    case peer(HubEvent)
    case agent(HubEvent)
    case thinking(HubEvent)
    case tools([ToolCallItem])
    case usage(HubEvent)
    case approval(HubEvent, open: Bool)
    case clarify(HubEvent, open: Bool)
    case attachment(HubEvent)
    case note(String, String, NoteLevel)

    var id: String {
        switch self {
        case .user(let e), .peer(let e), .agent(let e), .thinking(let e), .attachment(let e), .usage(let e):
            e.event_id
        case .tools(let calls): calls.first?.call.event_id ?? "tools"
        case .approval(let e, _), .clarify(let e, _): e.event_id
        case .note(let id, _, _): id
        }
    }
}

/// A tool call and its result, once it came.
struct ToolCallItem: Identifiable {
    let call: HubEvent
    let result: HubEvent?
    var id: String { call.event_id }

    var tool: String { call.payload["tool"]?.string ?? "Tool" }
    var argsPreview: String { call.payload["args_preview"]?.string ?? "" }
    var failed: Bool { result?.payload["error"]?.bool == true }
    var durationMs: Double? { result?.payload["duration_ms"]?.double }
}
