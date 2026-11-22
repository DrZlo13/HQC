import SwiftUI

struct FeedRow: View {
    let item: FeedItem
    let model: ChatModel

    var body: some View {
        switch item {
        case .user(let ev):
            UserBubble(event: ev)
        case .peer(let ev):
            PeerBubble(event: ev)
        case .agent(let ev):
            MarkdownText(ev.text)
                .frame(maxWidth: .infinity, alignment: .leading)
                .opacity(ev.payload["interrupted"]?.bool == true ? 0.55 : 1)
                .contextMenu {
                    Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = ev.text }
                }
        case .thinking(let ev):
            ThinkingView(text: ev.text, live: false)
        case .tool(let call, let result):
            ToolRow(call: call, result: result)
        case .approval(let ev, let open):
            ApprovalCard(event: ev, open: open) { option in
                if let id = ev.requestID { await model.approve(requestID: id, option: option) }
            }
        case .clarify(let ev, let open):
            ClarifyCard(event: ev, open: open) { answers in
                if let id = ev.requestID { await model.clarify(requestID: id, answers: answers) }
            }
        case .attachment(let ev):
            Label(ev.payload["name"]?.string ?? "Attachment", systemImage: "paperclip")
                .font(.subheadline)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(.fill.tertiary, in: .capsule)
        case .note(_, let text, let level):
            NoteRow(text: text, level: level)
        }
    }
}

struct UserBubble: View {
    let event: HubEvent

    var body: some View {
        HStack {
            Spacer(minLength: 48)
            VStack(alignment: .trailing, spacing: 4) {
                Text(event.text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .foregroundStyle(.white)
                    .background(Color.accentColor.gradient, in: .rect(cornerRadius: 20))
                    .contextMenu {
                        Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = event.text }
                    }
                switch event.delivery {
                case "sending":
                    Label("Sending", systemImage: "clock").labelStyle(.iconOnly)
                        .font(.caption2).foregroundStyle(.secondary)
                case "pending":
                    Text("Awaiting delivery").font(.caption2).foregroundStyle(.secondary)
                case "failed":
                    Label("Not delivered", systemImage: "exclamationmark.circle")
                        .font(.caption2).foregroundStyle(.red)
                default:
                    EmptyView()
                }
            }
        }
    }
}

struct PeerBubble: View {
    let event: HubEvent

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Label(event.payload["from"]?.string ?? "Another session", systemImage: "arrow.triangle.branch")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
            Text(event.text)
                .textSelection(.enabled)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(.fill.secondary, in: .rect(cornerRadius: 20))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct ThinkingView: View {
    let text: String
    let live: Bool
    @State private var expanded = false

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            Text(text)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, 4)
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "brain")
                    .symbolEffect(.pulse, options: .repeating, isActive: live)
                Text(live ? "Thinking…" : "Thought")
                if !expanded {
                    Text(text.replacingOccurrences(of: "\n", with: " "))
                        .lineLimit(1)
                        .foregroundStyle(.tertiary)
                }
            }
            .font(.subheadline.italic())
            .foregroundStyle(.secondary)
        }
        .tint(.secondary)
    }
}

struct ToolRow: View {
    let call: HubEvent
    let result: HubEvent?
    @State private var expanded = false

    private var tool: String { call.payload["tool"]?.string ?? "Tool" }
    private var args: String { call.payload["args_preview"]?.string ?? "" }
    private var failed: Bool { result?.payload["error"]?.bool == true }
    private var output: String {
        let preview = result?.payload["result_preview"]?.string ?? ""
        if failed, let e = result?.payload["error_text"]?.string, !preview.hasPrefix(e) { return e + "\n" + preview }
        return preview
    }

    private var icon: String {
        switch tool {
        case "Bash": "terminal"
        case "Read": "doc.text"
        case "Edit", "Write", "MultiEdit": "pencil"
        case "Grep", "Glob": "magnifyingglass"
        case "WebFetch", "WebSearch": "globe"
        case "Agent", "Task": "person.2"
        case "TodoWrite": "checklist"
        default: tool.hasPrefix("mcp__") ? "puzzlepiece.extension" : "wrench.and.screwdriver"
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                withAnimation(.snappy) { expanded.toggle() }
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: icon)
                        .frame(width: 20)
                        .foregroundStyle(failed ? .red : .secondary)
                    Text(tool).font(.subheadline.weight(.semibold))
                    Text(args.replacingOccurrences(of: "\n", with: " "))
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    if result == nil {
                        ProgressView().controlSize(.mini)
                    } else if let ms = result?.payload["duration_ms"]?.double {
                        Text(Duration.milliseconds(ms), format: .units(allowed: [.seconds, .milliseconds], width: .narrow, maximumUnitCount: 1))
                            .font(.caption2.monospacedDigit())
                            .foregroundStyle(.tertiary)
                    }
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)

            if expanded {
                VStack(alignment: .leading, spacing: 8) {
                    if !args.isEmpty { CodeBlock(text: args) }
                    if !output.isEmpty { CodeBlock(text: output, tint: failed ? .red : nil) }
                }
                .padding([.horizontal, .bottom], 10)
                .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .background(.fill.quaternary, in: .rect(cornerRadius: 14))
        .clipShape(.rect(cornerRadius: 14))
    }
}

struct CodeBlock: View {
    let text: String
    var tint: Color?

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(text.count > 4000 ? String(text.prefix(4000)) + "\n…" : text)
                .font(.caption.monospaced())
                .foregroundStyle(tint ?? .primary)
                .textSelection(.enabled)
                .padding(10)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground), in: .rect(cornerRadius: 10))
    }
}

struct NoteRow: View {
    let text: String
    let level: FeedItem.NoteLevel

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: level == .info ? "info.circle" : "exclamationmark.triangle")
            Text(text).lineLimit(4)
        }
        .font(.caption)
        .foregroundStyle(level == .error ? .red : level == .warning ? .orange : .secondary)
        .frame(maxWidth: .infinity, alignment: .center)
        .multilineTextAlignment(.center)
    }
}

/// A permission request: the tool, what it would run, and the agent's real options as buttons.
struct ApprovalCard: View {
    let event: HubEvent
    let open: Bool
    let onAnswer: (ApprovalOption) async -> Void
    @State private var busy = false

    private var options: [ApprovalOption] { ApprovalOption.list(from: event.payload) }
    private var tool: String {
        event.payload["display_name"]?.string ?? event.payload["tool"]?.string ?? "Tool"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: open ? "hand.raised.fill" : "checkmark.seal")
                    .foregroundStyle(open ? .orange : .secondary)
                Text(open ? "Permission needed" : "Permission answered")
                    .font(.subheadline.weight(.semibold))
                Spacer()
                Text(tool).font(.caption.monospaced()).foregroundStyle(.secondary)
            }
            if let reason = event.payload["reason"]?.string, !reason.isEmpty {
                Text(reason).font(.footnote).foregroundStyle(.secondary)
            }
            let body = event.payload["args_preview"]?.string ?? event.payload["description"]?.string ?? ""
            if !body.isEmpty { CodeBlock(text: body) }
            if open {
                GlassEffectContainer(spacing: 8) {
                    FlowButtons(options: options, busy: busy) { option in
                        busy = true
                        await onAnswer(option)
                        busy = false
                    }
                }
            }
        }
        .padding(14)
        .background(.orange.opacity(open ? 0.10 : 0.03), in: .rect(cornerRadius: 18))
        .overlay(RoundedRectangle(cornerRadius: 18).strokeBorder(.orange.opacity(open ? 0.35 : 0.1)))
        .opacity(open ? 1 : 0.7)
    }
}

private struct FlowButtons: View {
    let options: [ApprovalOption]
    let busy: Bool
    let onTap: (ApprovalOption) async -> Void

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 8) { buttons }
            VStack(alignment: .leading, spacing: 8) { buttons }
        }
        .disabled(busy)
    }

    @ViewBuilder private var buttons: some View {
        ForEach(options) { option in
            if option.isDeny {
                Button(option.label, role: .destructive) { Task { await onTap(option) } }
                    .buttonStyle(.glass)
                    .tint(.red)
            } else if option.canonical == "once" {
                Button(option.label) { Task { await onTap(option) } }
                    .buttonStyle(.glassProminent)
            } else {
                Button(option.label) { Task { await onTap(option) } }
                    .buttonStyle(.glass)
            }
        }
    }
}

/// The agent's questions (Claude Code's AskUserQuestion), with an "Other" answer per question.
struct ClarifyCard: View {
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
            Label(open ? "The agent asks" : "Answered",
                  systemImage: open ? "questionmark.bubble.fill" : "checkmark.bubble")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(open ? Color.accentColor : .secondary)
            ForEach(questions) { q in
                VStack(alignment: .leading, spacing: 8) {
                    if let h = q.header, !h.isEmpty {
                        Text(h.uppercased()).font(.caption2.weight(.bold)).foregroundStyle(.secondary)
                    }
                    Text(q.text).font(.body.weight(.medium))
                    if open {
                        ForEach(Array(q.options.enumerated()), id: \.offset) { i, option in
                            optionRow(q, option, description: i < q.descriptions.count ? q.descriptions[i] : "")
                        }
                        TextField("Other answer", text: Binding(
                            get: { other[q.id] ?? "" }, set: { other[q.id] = $0 }))
                            .textFieldStyle(.roundedBorder)
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
                .buttonStyle(.glassProminent)
                .disabled(busy || !questions.allSatisfy(answered))
            }
        }
        .padding(14)
        .background(Color.accentColor.opacity(open ? 0.08 : 0.03), in: .rect(cornerRadius: 18))
        .overlay(RoundedRectangle(cornerRadius: 18).strokeBorder(Color.accentColor.opacity(open ? 0.3 : 0.1)))
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
                    .foregroundStyle(on ? Color.accentColor : .secondary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(option)
                    if !description.isEmpty {
                        Text(description).font(.caption).foregroundStyle(.secondary)
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
