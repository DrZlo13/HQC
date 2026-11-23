import SwiftUI

/// One row of the chat, drawn as the web client's feed draws it (client/src/ui/Chat.tsx,
/// feed.ts): the user's message as a card on the right, the agent's answer as plain text with
/// its time and token line, thinking and tool calls as quiet one-line rows that open.
struct FeedRow: View {
    @Environment(\.palette) private var palette
    let item: FeedItem
    let model: ChatModel

    var body: some View {
        switch item {
        case .user(let ev):
            UserBubble(event: ev)
        case .peer(let ev):
            PeerCard(event: ev)
        case .agent(let ev):
            AgentMessage(event: ev)
        case .thinking(let ev):
            ThinkingView(text: ev.text, live: false)
        case .tools(let calls):
            ToolGroup(calls: calls)
        case .usage(let ev):
            UsageLine(event: ev)
        case .approval(let ev, let open):
            ApprovalCard(event: ev, open: open) { option in
                if let id = ev.requestID { await model.approve(requestID: id, option: option) }
            }
        case .clarify(let ev, let open):
            ClarifyCard(event: ev, open: open) { answers in
                if let id = ev.requestID { await model.clarify(requestID: id, answers: answers) }
            }
        case .attachment(let ev):
            if let id = ev.payload["artifact_id"]?.string {
                VStack(alignment: .leading, spacing: 4) {
                    ArtifactView(id: id)
                    if let caption = ev.payload["caption"]?.string, !caption.isEmpty {
                        Text(caption).font(.caption).foregroundStyle(palette.muted)
                    }
                }
            }
        case .note(_, let text, let level):
            NoteRow(text: text, level: level)
        case .refusal(let ev, let answered):
            RefusalRow(event: ev, answered: answered)
        }
    }
}

/// Hours and minutes of an event, as the web client's rows show them.
struct EventTime: View {
    @Environment(\.palette) private var palette
    let event: HubEvent

    var body: some View {
        if let d = event.date {
            Text(d, format: .dateTime.hour().minute())
                .font(.caption2.monospacedDigit())
                .foregroundStyle(palette.muted)
        }
    }
}

struct UserBubble: View {
    @Environment(\.palette) private var palette
    @Environment(\.chatActions) private var actions
    let event: HubEvent

    var body: some View {
        HStack {
            Spacer(minLength: 60)
            VStack(alignment: .trailing, spacing: 6) {
                let ids = event.payload["artifact_ids"]?.stringArray ?? []
                if !event.text.isEmpty {
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        Text(event.text)
                            .foregroundStyle(palette.text)
                            .textSelection(.enabled)
                            .fixedSize(horizontal: false, vertical: true)
                        EventTime(event: event)
                    }
                }
                if !ids.isEmpty { ArtifactStrip(ids: ids, alignment: .trailing) }
                delivery
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(palette.surface, in: .rect(cornerRadius: 14))
            .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(palette.border.opacity(0.7)))
            .fixedSize(horizontal: false, vertical: true)
            .contextMenu {
                if let quote = actions.quote, !event.text.isEmpty {
                    Button("Quote", systemImage: "text.quote") { quote(Quoting.firstSentences(event.text)) }
                }
                Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = event.text }
                if let fork = actions.fork, event.delivery == nil {
                    Button("Fork from here", systemImage: "arrow.triangle.branch") { fork(event) }
                }
                if let add = actions.addQuick, !event.text.isEmpty {
                    Button("Add to quick messages", systemImage: "bolt") { add(event.text) }
                }
            }
        }
    }

    @ViewBuilder private var delivery: some View {
        switch event.delivery {
        case "sending":
            Label("Sending", systemImage: "clock").font(.caption2).foregroundStyle(palette.muted)
        case "pending":
            Text("Awaiting delivery").font(.caption2).foregroundStyle(palette.muted)
        case "failed":
            Label("Not delivered", systemImage: "exclamationmark.circle").font(.caption2).foregroundStyle(palette.danger)
        default:
            EmptyView()
        }
    }
}

/// A message from another session: the relay color, as the web client marks it.
struct PeerCard: View {
    @Environment(\.palette) private var palette
    @Environment(AppModel.self) private var app
    let event: HubEvent

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: event.payload["mode"]?.string == "ask" ? "arrow.down.left.circle.fill" : "arrow.triangle.branch")
                    .foregroundStyle(palette.relay)
                Text(event.payload["mode"]?.string == "ask" ? "Question from" : "From")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(palette.relayText)
                // The other session's address opens it, as the web client's relay link does.
                if let from = event.payload["from_session_id"]?.string {
                    Button {
                        app.open(sessionID: from)
                    } label: {
                        Text(app.summary(from)?.title ?? event.payload["from"]?.string ?? "another session")
                            .font(.caption.monospaced())
                            .underline()
                            .foregroundStyle(palette.relayText)
                    }
                    .buttonStyle(.plain)
                } else {
                    Text(event.payload["from"]?.string ?? "another session")
                        .font(.caption.monospaced())
                        .foregroundStyle(palette.text2)
                }
                Spacer()
                EventTime(event: event)
            }
            MarkdownText(event.text)
            let ids = event.payload["artifact_ids"]?.stringArray ?? []
            if !ids.isEmpty { ArtifactStrip(ids: ids) }
        }
        .padding(12)
        .background(palette.relay.opacity(0.08), in: .rect(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(palette.relay.opacity(0.32)))
    }
}

struct AgentMessage: View {
    @Environment(\.palette) private var palette
    @Environment(\.chatActions) private var actions
    let event: HubEvent
    @Environment(\.openURL) private var openURL
    @State private var quoting = false
    @State private var searching = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            MarkdownText(event.text)
                .opacity(event.payload["interrupted"]?.bool == true ? 0.55 : 1)
            HStack(spacing: 12) {
                if event.payload["interrupted"]?.bool == true {
                    Text("interrupted").font(.caption2).foregroundStyle(palette.muted)
                }
                Spacer()
                if let quote = actions.quote {
                    Button {
                        quote(Quoting.firstSentences(event.text))
                        Feedback.play(.tap)
                    } label: {
                        Image(systemName: "text.quote").font(.caption).frame(width: 22, height: 22).contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(palette.muted)
                    .accessibilityLabel("Quote")
                }
                CopyButton(text: event.text, label: "Copy answer")
                ShareLink(item: event.text) {
                    Image(systemName: "square.and.arrow.up").font(.caption)
                }
                .buttonStyle(.plain)
                .foregroundStyle(palette.muted)
                .accessibilityLabel("Share answer")
                EventTime(event: event)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contextMenu {
            if actions.quote != nil {
                Button("Quote", systemImage: "text.quote") { actions.quote?(Quoting.firstSentences(event.text)) }
                Button("Quote a part…", systemImage: "selection.pin.in.out") { quoting = true }
            }
            Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = event.text }
            ShareLink("Share", item: event.text)
            Button("Search in \(WebSearch.current.label)…", systemImage: "magnifyingglass") { searching = true }
        }
        .sheet(isPresented: $quoting) {
            QuoteSheet(text: event.text) { actions.quote?($0) }
        }
        .sheet(isPresented: $searching) {
            QuoteSheet(text: event.text, action: .search) { q in
                if let url = WebSearch.current.url(q) { openURL(url) }
            }
        }
    }
}

/// Copies the text, then shows a check for a moment, as the Claude app's copy button does.
struct CopyButton: View {
    @Environment(\.palette) private var palette
    let text: String
    var label = "Copy"
    @State private var copied = false

    var body: some View {
        Button {
            UIPasteboard.general.string = text
            Feedback.play(.copy)
            withAnimation(.snappy) { copied = true }
            Task {
                try? await Task.sleep(for: .seconds(1.5))
                withAnimation(.snappy) { copied = false }
            }
        } label: {
            Image(systemName: copied ? "checkmark" : "doc.on.doc")
                .font(.caption)
                .contentTransition(.symbolEffect(.replace))
                .frame(width: 22, height: 22)
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .foregroundStyle(copied ? palette.text : palette.muted)

        .accessibilityLabel(copied ? "Copied" : label)
    }
}

/// `1.2k → 340 tokens · cache 9k · $0.012`, under the answer a turn ended with.
struct UsageLine: View {
    @Environment(\.palette) private var palette
    let event: HubEvent

    var body: some View {
        Text(line)
            .font(.caption2.monospacedDigit())
            .foregroundStyle(palette.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, -6)
    }

    private var line: String {
        let p = event.payload
        var parts = ["\(compact(p["input_tokens"]?.double ?? 0)) → \(compact(p["output_tokens"]?.double ?? 0)) tokens"]
        if let c = p["cache_read"]?.double, c > 0 { parts.append("cache \(compact(c))") }
        if let cost = p["cost_usd"]?.double, cost > 0 { parts.append(String(format: "$%.3f", cost)) }
        return parts.joined(separator: " · ")
    }

    private func compact(_ n: Double) -> String {
        n >= 1000 ? String(format: n >= 10_000 ? "%.0fk" : "%.1fk", n / 1000) : String(Int(n))
    }
}

/// `› Thinking · the first line`, dimmed and in italics; it opens to all of it.
struct ThinkingView: View {
    @Environment(\.palette) private var palette
    let text: String
    let live: Bool
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                withAnimation(.snappy) { expanded.toggle() }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                    Text(live ? "Thinking…" : "Thinking")
                        .italic()
                    if !expanded {
                        Text("· " + text.replacingOccurrences(of: "\n", with: " "))
                            .italic()
                            .lineLimit(1)
                    }
                    Spacer(minLength: 0)
                }
                .font(.footnote)
                .foregroundStyle(palette.muted)
                .shimmering(live)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            if expanded {
                Text(text)
                    .font(.footnote)
                    .italic()
                    .foregroundStyle(palette.muted)
                    .textSelection(.enabled)
                    .padding(.leading, 16)
                    .transition(.opacity)
            }
        }
    }
}

/// A run of tool calls: one line, `› N tool calls · 5 s · $ cargo test · 1 test failed`, that
/// opens to a row per call, each opening to its arguments and output.
struct ToolGroup: View {
    @Environment(\.palette) private var palette
    let calls: [ToolCallItem]
    @State private var expanded = false

    private var running: Bool { calls.contains { $0.result == nil } }
    private var failed: Int { calls.filter(\.failed).count }
    private var seconds: Double { calls.compactMap(\.durationMs).reduce(0, +) / 1000 }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button {
                withAnimation(.snappy) { expanded.toggle() }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                        .foregroundStyle(palette.muted)
                    if running {
                        ProgressView().controlSize(.mini)
                    } else if failed > 0 {
                        Image(systemName: "xmark").font(.caption2.weight(.bold)).foregroundStyle(palette.danger)
                    }
                    Text(summary)
                        .font(.footnote)
                        .foregroundStyle(palette.muted)
                        .lineLimit(1)
                    if let last = calls.last {
                        let result = ToolText.resultLine(last)
                        Text(ToolText.callLine(last) + (result.isEmpty ? "" : "  ·  " + result))
                            .font(.footnote.monospaced())
                            .foregroundStyle(palette.text2.opacity(0.85))
                            .lineLimit(1)
                    }
                    Spacer(minLength: 0)
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)

            if expanded {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(calls) { ToolCallRow(item: $0) }
                }
                .padding(.leading, 14)
                .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
    }

    private var summary: String {
        var parts = ["\(calls.count) tool call\(calls.count == 1 ? "" : "s")"]
        if seconds >= 1 { parts.append(String(format: "%.0f s", seconds)) }
        return parts.joined(separator: " · ") + " ·"
    }
}

/// One call: `$ cargo test → 1 test failed`, its time and status; it opens to the details.
struct ToolCallRow: View {
    @Environment(\.palette) private var palette
    @Environment(\.chatActions) private var actions
    let item: ToolCallItem
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                withAnimation(.snappy) { open.toggle() }
            } label: {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(attributedLine)
                        .font(.footnote.monospaced())
                        .lineLimit(open ? nil : 1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if let ms = item.durationMs {
                        Text(ToolText.duration(ms)).font(.caption2.monospacedDigit()).foregroundStyle(palette.muted)
                    }
                    if item.result == nil, let id = item.call.callID, actions.foregroundCalls.contains(id),
                       let background = actions.background {
                        // A long command can go on in the background while the agent goes on.
                        Button {
                            background(id)
                            Feedback.play(.tap)
                        } label: {
                            Label("Background", systemImage: "arrow.down.right.square")
                                .font(.caption2.weight(.semibold))
                                .labelStyle(.titleAndIcon)
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.mini)
                    }
                    if item.result == nil {
                        ProgressView().controlSize(.mini)
                    } else if item.failed {
                        Image(systemName: "xmark").font(.caption2.weight(.bold)).foregroundStyle(palette.danger)
                    }
                }
                .padding(.vertical, 4)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)

            if open {
                VStack(alignment: .leading, spacing: 8) {
                    if !item.argsPreview.isEmpty {
                        CodeBlock(text: ToolText.pretty(item.argsPreview))
                    }
                    let output = ToolText.output(item)
                    if !output.isEmpty {
                        CodeBlock(text: output, tint: item.failed ? palette.danger : nil)
                    }
                    if let full = item.result?.payload["artifact_id"]?.string {
                        ArtifactView(id: full)
                    }
                    let images = item.result?.payload["artifact_ids"]?.stringArray ?? []
                    if !images.isEmpty { ArtifactStrip(ids: images) }
                }
                .padding(.bottom, 6)
            }
        }
    }

    private var attributedLine: AttributedString {
        var a = AttributedString(ToolText.callLine(item))
        a.foregroundColor = palette.text2
        let r = ToolText.resultLine(item)
        if !r.isEmpty {
            var tail = AttributedString(" → " + r)
            tail.foregroundColor = item.failed ? palette.danger : palette.muted
            a += tail
        }
        return a
    }
}

/// The web client's one-line forms of a call and its result (client/src/ui/feed.ts).
enum ToolText {
    private static let briefKeys = ["file_path", "notebook_path", "path", "pattern", "url", "query",
                                    "command", "description", "prompt", "skill", "summary", "message"]

    static func args(_ preview: String) -> [String: Any] {
        guard let d = preview.data(using: .utf8),
              let o = try? JSONSerialization.jsonObject(with: d) as? [String: Any] else { return [:] }
        return o
    }

    /// `$ command` for Bash (its first line, and how many follow), else the tool and its main
    /// argument; an MCP tool as `tool (server)`.
    static func callLine(_ item: ToolCallItem) -> String {
        let a = args(item.argsPreview)
        if item.tool == "Bash", let cmd = a["command"] as? String {
            let lines = cmd.trimmingCharacters(in: .whitespacesAndNewlines).components(separatedBy: "\n")
            let more = lines.dropFirst().filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty }.count
            return "$ " + lines[0] + (more > 0 ? "  +\(more) lines" : "")
        }
        let label = label(item.tool)
        if let key = briefKeys.first(where: { (a[$0] as? String)?.isEmpty == false }), let v = a[key] as? String {
            return label + " " + v.replacingOccurrences(of: "\n", with: " ")
        }
        if a.isEmpty { return label + " " + item.argsPreview.replacingOccurrences(of: "\n", with: " ") }
        let fields = a.sorted { $0.key < $1.key }.map { "\($0.key): \($0.value)" }.joined(separator: ", ")
        return label + " " + fields
    }

    static func label(_ tool: String) -> String {
        let parts = tool.components(separatedBy: "__")
        guard tool.hasPrefix("mcp__"), parts.count >= 3 else { return tool }
        var server = parts[1]
        if server.hasPrefix("plugin_"), let r = server.dropFirst(7).firstIndex(of: "_") {
            server = String(server[server.index(after: r)...])
        }
        return "\(parts.dropFirst(2).joined(separator: "__")) (\(server))"
    }

    /// The error, else the result, on one line; nothing for a file change that went through.
    static func resultLine(_ item: ToolCallItem) -> String {
        guard let r = item.result else { return "" }
        if !item.failed, ["Edit", "MultiEdit", "Write", "NotebookEdit"].contains(item.tool) { return "" }
        var text = item.failed ? (r.payload["error_text"]?.string ?? r.payload["result_preview"]?.string ?? "")
            : (r.payload["result_preview"]?.string ?? "")
        text = text.replacingOccurrences(of: "<tool_use_error>", with: "")
            .replacingOccurrences(of: "</tool_use_error>", with: " ")
        let first = text.trimmingCharacters(in: .whitespacesAndNewlines).components(separatedBy: "\n").first ?? ""
        return String(first.prefix(200))
    }

    static func output(_ item: ToolCallItem) -> String {
        guard let r = item.result else { return "" }
        let preview = r.payload["result_preview"]?.string ?? ""
        if item.failed, let e = r.payload["error_text"]?.string, !preview.hasPrefix(e) { return e + "\n" + preview }
        return preview
    }

    /// The arguments' JSON laid out, a Bash command as it is.
    static func pretty(_ preview: String) -> String {
        let a = args(preview)
        if let cmd = a["command"] as? String, a.count <= 3 { return cmd }
        guard !a.isEmpty, let d = try? JSONSerialization.data(withJSONObject: a, options: [.prettyPrinted, .sortedKeys]),
              let s = String(data: d, encoding: .utf8) else { return preview }
        return s
    }

    static func duration(_ ms: Double) -> String {
        ms < 1000 ? "\(Int(ms)) ms" : ms < 60_000 ? String(format: "%.0f s", ms / 1000)
            : String(format: "%.0f min", ms / 60_000)
    }
}

struct CodeBlock: View {
    @Environment(\.palette) private var palette
    let text: String
    var tint: Color?

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(text.count > 6000 ? String(text.prefix(6000)) + "\n…" : text)
                .font(.caption.monospaced())
                .foregroundStyle(tint ?? palette.text2)
                .textSelection(.enabled)
                .padding(10)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(palette.code, in: .rect(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(palette.border.opacity(0.6)))
    }
}

struct NoteRow: View {
    @Environment(\.palette) private var palette
    let text: String
    let level: FeedItem.NoteLevel

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            if level != .info { Image(systemName: "exclamationmark.triangle") }
            Text(text).lineLimit(4)
        }
        .font(.caption)
        .foregroundStyle(level == .error ? palette.danger : level == .warning ? palette.waiting : palette.muted)
        .frame(maxWidth: .infinity, alignment: .center)
        .multilineTextAlignment(.center)
    }
}

/// A permission request: the tool, what it would run, and the agent's real options as buttons,
/// on the web client's amber card.
struct ApprovalCard: View {
    @Environment(\.palette) private var palette
    @Environment(\.chatActions) private var actions
    let event: HubEvent
    let open: Bool
    let onAnswer: (ApprovalOption) async -> Void
    /// Gives an `hqc_secret`'s secret; else the chat's own.
    var giveSecret: ((String, SecretFields) async throws -> SecretGiven)?
    @State private var busy = false

    private var options: [ApprovalOption] { ApprovalOption.list(from: event.payload) }
    private var tool: String {
        ToolText.label(event.payload["display_name"]?.string ?? event.payload["tool"]?.string ?? "Tool")
    }

    var body: some View {
        if event.payload["tool"]?.string == "hqc_secret", event.payload["secret"]?["name"] != nil {
            // The agent asks for a secret: a form, not Allow and Deny (SPEC 0.147).
            SecretCard(event: event, open: open, onGive: giveSecret ?? actions.giveSecret) {
                if let deny = options.first(where: { $0.isDeny }) { await onAnswer(deny) }
            }
        } else {
            permission
        }
    }

    private var permission: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: open ? "hand.raised.fill" : "checkmark.seal")
                    .foregroundStyle(open ? palette.waiting : palette.muted)
                Text(open ? "Allow \(tool)?" : "\(tool): answered")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(palette.text)
                Spacer()
                EventTime(event: event)
            }
            if let reason = event.payload["reason"]?.string, !reason.isEmpty {
                Text(reason).font(.footnote).foregroundStyle(palette.text2)
            }
            let body = event.payload["args_preview"].map { ToolText.pretty($0.string ?? "") }
                ?? event.payload["description"]?.string ?? ""
            if !body.isEmpty { CodeBlock(text: body) }
            if open {
                FlowButtons(options: options, busy: busy) { option in
                    busy = true
                    await onAnswer(option)
                    busy = false
                }
            }
        }
        .padding(14)
        .background(open ? palette.permissionBG : palette.surface.opacity(0.5), in: .rect(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(open ? palette.permissionBorder : palette.border.opacity(0.5)))
        .opacity(open ? 1 : 0.75)
    }
}

/// A permission request's answers. Allow once and Deny side by side, equal and large; the
/// lasting answers (this session, always, a whole MCP server, always deny) below them as a grid
/// of equal tiles with an icon each. The answer being sent shows a spinner.
private struct FlowButtons: View {
    @Environment(\.palette) private var palette
    let options: [ApprovalOption]
    let busy: Bool
    let onTap: (ApprovalOption) async -> Void
    @State private var sending: String?

    private var once: ApprovalOption? { options.first { $0.canonical == "once" } }
    private var deny: ApprovalOption? { options.first { $0.canonical == "deny" } }
    private var rest: [ApprovalOption] {
        options.filter { $0.id != once?.id && $0.id != deny?.id }
    }

    var body: some View {
        VStack(spacing: 8) {
            if once != nil || deny != nil {
                HStack(spacing: 8) {
                    if let once { button(once, style: .primary) }
                    if let deny { button(deny, style: .danger) }
                }
            }
            if !rest.isEmpty {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 8)], spacing: 8) {
                    ForEach(rest) { option in
                        button(option, style: option.isDeny ? .quietDanger : .secondary)
                    }
                }
            }
        }
        .disabled(busy)
        .animation(.snappy, value: sending)
    }

    private enum Style { case primary, danger, secondary, quietDanger }

    private func icon(_ o: ApprovalOption) -> String {
        switch o.canonical {
        case "once": "checkmark"
        case "deny": "xmark"
        case "deny_always": "nosign"
        case "session": "clock.arrow.circlepath"
        case "always": "checkmark.seal"
        default: o.isDeny ? "xmark" : "checkmark.circle"
        }
    }

    private func button(_ option: ApprovalOption, style: Style) -> some View {
        let big = style == .primary || style == .danger
        let (fg, bg): (Color, Color) = switch style {
        case .primary: (.white, Color.accentColor)
        case .danger: (palette.danger, palette.danger.opacity(0.13))
        case .secondary: (palette.text, palette.text.opacity(0.07))
        case .quietDanger: (palette.danger, palette.danger.opacity(0.07))
        }
        return Button {
            sending = option.id
            Task {
                await onTap(option)
                sending = nil
            }
        } label: {
            HStack(spacing: 6) {
                if sending == option.id {
                    ProgressView().controlSize(.small).tint(fg)
                } else {
                    Image(systemName: icon(option))
                        .font(.system(size: big ? 14 : 12, weight: .semibold))
                }
                Text(option.label)
                    .font(big ? .subheadline.weight(.semibold) : .footnote.weight(.medium))
                    .multilineTextAlignment(.center)
                    .lineLimit(2)
                    .minimumScaleFactor(0.85)
            }
            .foregroundStyle(fg)
            .padding(.horizontal, 10)
            .frame(maxWidth: .infinity, minHeight: big ? 44 : 40)
            .background(bg, in: .rect(cornerRadius: 12, style: .continuous))
            .contentShape(.rect(cornerRadius: 12))
        }
        .buttonStyle(PressScale())
        .opacity(busy && sending != option.id ? 0.5 : 1)
        .accessibilityLabel(option.label)
    }
}

/// A button that sinks a little while pressed.
private struct PressScale: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .animation(.spring(duration: 0.2), value: configuration.isPressed)
    }
}

/// The agent's questions (Claude Code's AskUserQuestion), with an "Other" answer per question.
struct ClarifyCard: View {
    @Environment(\.palette) private var palette
    let event: HubEvent
    let open: Bool
    let onAnswer: ([String: Any]) async -> Void

    @State private var chosen: [String: Set<String>] = [:]
    @State private var other: [String: String] = [:]
    @State private var busy = false

    private var questions: [ClarifyQuestion] { ClarifyQuestion.list(from: event.payload) }

    private func answered(_ q: ClarifyQuestion) -> Bool {
        !(chosen[q.id] ?? []).isEmpty || !(other[q.id] ?? "").trimmingCharacters(in: .whitespaces).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Label(open ? "The agent asks" : "Answered",
                      systemImage: open ? "questionmark.bubble.fill" : "checkmark.bubble")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(open ? Color.accentColor : palette.muted)
                Spacer()
                EventTime(event: event)
            }
            ForEach(questions) { q in
                VStack(alignment: .leading, spacing: 8) {
                    if let h = q.header, !h.isEmpty {
                        Text(h.uppercased()).font(.caption2.weight(.bold)).foregroundStyle(palette.muted)
                    }
                    Text(q.text).font(.body.weight(.medium)).foregroundStyle(palette.text)
                    if open {
                        ForEach(Array(q.options.enumerated()), id: \.offset) { i, option in
                            optionRow(q, option, description: i < q.descriptions.count ? q.descriptions[i] : "")
                        }
                        TextField("Other answer", text: Binding(
                            get: { other[q.id] ?? "" }, set: { other[q.id] = $0 }))
                            .padding(.horizontal, 12)
                            .padding(.vertical, 8)
                            .background(palette.code, in: .rect(cornerRadius: 8))
                            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(palette.border))
                    }
                }
            }
            if open {
                Button {
                    Task {
                        busy = true
                        await onAnswer(answers())
                        busy = false
                    }
                } label: {
                    Text("Answer").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy || !questions.allSatisfy(answered))
            }
        }
        .padding(14)
        .background(Color.accentColor.opacity(open ? 0.07 : 0.03), in: .rect(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(Color.accentColor.opacity(open ? 0.35 : 0.12)))
    }

    private func optionRow(_ q: ClarifyQuestion, _ option: String, description: String) -> some View {
        let on = chosen[q.id]?.contains(option) == true
        return Button {
            var set = chosen[q.id] ?? []
            if q.multi {
                if on { set.remove(option) } else { set.insert(option) }
            } else {
                set = on ? [] : [option]
            }
            chosen[q.id] = set
        } label: {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: q.multi ? (on ? "checkmark.square.fill" : "square")
                                          : (on ? "largecircle.fill.circle" : "circle"))
                    .foregroundStyle(on ? Color.accentColor : palette.muted)
                VStack(alignment: .leading, spacing: 2) {
                    Text(option).foregroundStyle(palette.text)
                    if !description.isEmpty {
                        Text(description).font(.caption).foregroundStyle(palette.muted)
                    }
                }
                Spacer(minLength: 0)
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }

    /// As the web client: the typed text is the answer; with `multi`, an item beside the labels.
    private func answers() -> [String: Any] {
        var out: [String: Any] = [:]
        for q in questions {
            let typed = (other[q.id] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let picked = q.options.filter { chosen[q.id]?.contains($0) == true }
            if q.multi {
                out[q.id] = picked + (typed.isEmpty ? [] : [typed])
            } else {
                out[q.id] = typed.isEmpty ? (picked.first ?? "") : typed
            }
        }
        return out
    }
}

/// A light sweep across text while something is being written.
private struct Shimmer: ViewModifier {
    let active: Bool
    @State private var phase: CGFloat = -1

    func body(content: Content) -> some View {
        if active {
            content
                .overlay {
                    GeometryReader { g in
                        LinearGradient(colors: [.clear, .white.opacity(0.35), .clear],
                                       startPoint: .leading, endPoint: .trailing)
                            .frame(width: g.size.width * 0.4)
                            .offset(x: phase * g.size.width)
                    }
                    .mask(content)
                    .allowsHitTesting(false)
                }
                .onAppear {
                    withAnimation(.linear(duration: 1.6).repeatForever(autoreverses: false)) { phase = 1.4 }
                }
        } else {
            content
        }
    }
}

extension View {
    func shimmering(_ active: Bool) -> some View { modifier(Shimmer(active: active)) }
}

/// Lays its children out in rows, wrapping to the next row when one is full.
struct FlowLayout: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0, y: CGFloat = 0, row: CGFloat = 0, widest: CGFloat = 0
        for v in subviews {
            let s = v.sizeThatFits(.unspecified)
            if x > 0, x + s.width > width {
                y += row + spacing
                x = 0
                row = 0
            }
            x += s.width + spacing
            row = max(row, s.height)
            widest = max(widest, x - spacing)
        }
        return CGSize(width: min(widest, width), height: y + row)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX, y = bounds.minY, row: CGFloat = 0
        for v in subviews {
            let s = v.sizeThatFits(.unspecified)
            if x > bounds.minX, x + s.width > bounds.maxX {
                y += row + spacing
                x = bounds.minX
                row = 0
            }
            v.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(s))
            x += s.width + spacing
            row = max(row, s.height)
        }
    }
}

/// The runtime's reply to a model refusal (SPEC 0.168): its line, and an offer to switch the
/// session's model until the user has answered after it.
struct RefusalRow: View {
    @Environment(\.palette) private var palette
    @Environment(\.chatActions) private var actions
    let event: HubEvent
    let answered: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            NoteRow(text: event.text, level: .warning)
            if !answered, let switchModel = actions.switchModel {
                Button("Switch model", systemImage: "cpu") { switchModel() }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
        }
    }
}
