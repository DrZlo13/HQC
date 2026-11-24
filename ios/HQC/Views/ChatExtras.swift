import NaturalLanguage
import SwiftUI
import UIKit

// What the web client's chat does beyond reading and sending (client/src/ui/cards.tsx,
// selection.ts, secrets.tsx, limits.tsx, LiveSubagents.tsx): quote, fork, quick messages,
// secrets, moving a call to the background, usage limits and a subagent's own steps.

/// The chat's actions, for the feed's rows; absent where the chat cannot do them (no input,
/// a room).
struct ChatActions {
    /// Puts text of the feed into the message being written as a quote.
    var quote: ((String) -> Void)?
    /// A new session from before this message of the user.
    var fork: ((HubEvent) -> Void)?
    /// Keeps the text as one of the project's quick messages.
    var addQuick: ((String) -> Void)?
    /// Moves a running foreground call to the background.
    var background: ((String) -> Void)?
    var foregroundCalls: Set<String> = []
    /// Gives the secret an agent's `hqc_secret` asks for.
    var giveSecret: ((_ requestID: String, SecretFields) async throws -> SecretGiven)?
    /// Opens the session's model sheet (the offer under a model's refusal).
    var switchModel: (() -> Void)?
}

/// "Search in …" of a message (SPEC §6): the web search picked in Settings, kept on this device.
enum WebSearch: String, CaseIterable, Identifiable {
    case google, duckduckgo, bing, yandex
    var id: String { rawValue }

    static let key = "webSearch"
    static var current: WebSearch {
        UserDefaults.standard.string(forKey: key).flatMap(WebSearch.init(rawValue:)) ?? .google
    }

    var label: String {
        switch self {
        case .google: "Google"
        case .duckduckgo: "DuckDuckGo"
        case .bing: "Bing"
        case .yandex: "Yandex"
        }
    }

    func url(_ q: String) -> URL? {
        var c: URLComponents
        switch self {
        case .google: c = URLComponents(string: "https://www.google.com/search")!; c.queryItems = [.init(name: "q", value: q)]
        case .duckduckgo: c = URLComponents(string: "https://duckduckgo.com/")!; c.queryItems = [.init(name: "q", value: q)]
        case .bing: c = URLComponents(string: "https://www.bing.com/search")!; c.queryItems = [.init(name: "q", value: q)]
        case .yandex: c = URLComponents(string: "https://yandex.ru/search/")!; c.queryItems = [.init(name: "text", value: q)]
        }
        return c.url
    }
}

private struct ChatActionsKey: EnvironmentKey {
    static let defaultValue = ChatActions()
}

extension EnvironmentValues {
    var chatActions: ChatActions {
        get { self[ChatActionsKey.self] }
        set { self[ChatActionsKey.self] = newValue }
    }
}

// MARK: quotes

enum Quoting {
    /// The draft with `quote` after it as `> ` lines (a blank line between them when the draft
    /// has text) and a blank line after, where the reply goes: the web client's `withQuote`.
    static func insert(_ quote: String, into draft: String) -> String {
        let lines = quote
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { $0.trimmingCharacters(in: .whitespaces).isEmpty ? ">" : "> \($0)" }
            .joined(separator: "\n")
        let head = draft.replacingOccurrences(of: "\\s+$", with: "", options: .regularExpression)
        return "\(head.isEmpty ? "" : "\(head)\n\n")\(lines)\n\n"
    }

    /// The first two sentences, with "…" when more follow: what Quote takes of a whole message,
    /// as the web client's `firstSentences`.
    static func firstSentences(_ text: String) -> String {
        let tokenizer = NLTokenizer(unit: .sentence)
        tokenizer.string = text
        var ranges: [Range<String.Index>] = []
        tokenizer.enumerateTokens(in: text.startIndex..<text.endIndex) { r, _ in
            if !text[r].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { ranges.append(r) }
            return ranges.count < 3
        }
        guard let first = ranges.first else { return text }
        let last = ranges[min(1, ranges.count - 1)]
        let head = text[first.lowerBound..<last.upperBound].trimmingCharacters(in: .whitespacesAndNewlines)
        guard ranges.count > 2 else { return head }
        return head.last.map { ".!?…".contains($0) } == true ? "\(head) …" : "\(head)…"
    }
}

/// The message as selectable text, to quote a part of it: the phone's way of the web client's
/// "select, then Quote". Nothing selected quotes the whole message.
struct QuoteSheet: View {
    enum Action { case quote, search }

    @Environment(\.dismiss) private var dismiss
    @Environment(\.palette) private var palette
    let text: String
    var action: Action = .quote
    let onQuote: (String) -> Void
    @State private var selection = ""

    init(text: String, action: Action = .quote, onQuote: @escaping (String) -> Void) {
        self.text = text
        self.action = action
        self.onQuote = onQuote
    }

    private var title: String { action == .quote ? "Quote" : "Search in \(WebSearch.current.label)" }
    private var buttonLabel: String {
        switch (action, selection.isEmpty) {
        case (.quote, true): "Quote all"
        case (.quote, false): "Quote selection"
        case (.search, true): "Search the first sentences"
        case (.search, false): "Search selection"
        }
    }

    var body: some View {
        NavigationStack {
            SelectableText(text: text, textColor: UIColor(palette.text), selection: $selection)
                .padding(.horizontal, 12)
                .paletteBackground()
                .navigationTitle(title)
                .navigationBarTitleDisplayMode(.inline)
                .safeAreaInset(edge: .top) {
                    Text(action == .quote ? "Select what to quote, or quote it all." : "Select what to search for.")
                        .font(.footnote)
                        .foregroundStyle(palette.muted)
                        .padding(.vertical, 6)
                }
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel", systemImage: "xmark") { dismiss() }
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button(buttonLabel) {
                            let whole = action == .quote ? text : Quoting.firstSentences(text)
                            onQuote(selection.isEmpty ? whole : selection)
                            dismiss()
                        }
                        .buttonStyle(.glassProminent)
                    }
                }
        }
        .presentationDetents([.medium, .large])
    }
}

/// A read-only text view that reports its selection.
private struct SelectableText: UIViewRepresentable {
    let text: String
    let textColor: UIColor
    @Binding var selection: String

    func makeUIView(context: Context) -> UITextView {
        let v = UITextView()
        v.isEditable = false
        v.isSelectable = true
        v.backgroundColor = .clear
        v.font = .preferredFont(forTextStyle: .body)
        v.adjustsFontForContentSizeCategory = true
        v.textContainerInset = UIEdgeInsets(top: 12, left: 4, bottom: 24, right: 4)
        v.delegate = context.coordinator
        return v
    }

    func updateUIView(_ v: UITextView, context: Context) {
        if v.text != text { v.text = text }
        v.textColor = textColor
    }

    func makeCoordinator() -> Coordinator { Coordinator(selection: $selection) }

    final class Coordinator: NSObject, UITextViewDelegate {
        let selection: Binding<String>
        init(selection: Binding<String>) { self.selection = selection }
        func textViewDidChangeSelection(_ v: UITextView) {
            guard let r = v.selectedTextRange, let s = v.text(in: r) else { return }
            selection.wrappedValue = s
        }
    }
}

// MARK: secrets

/// What a secret form sends; the value lives only in the form's state while it is open.
struct SecretFields {
    var kind = "secret"
    var name = ""
    var value = ""
    var passphrase = ""
    var remember = false
}

/// A secret or an ssh key for the running agent (SPEC 0.147): never in the draft, the feed or
/// any storage; the agent is told only a note, the path or the fingerprint.
struct SecretForm: View {
    @Environment(\.palette) private var palette
    /// The agent's `hqc_secret`: its name and kind are fixed.
    var askedName: String?
    var askedKind: String?
    let giveLabel: String
    let onGive: (SecretFields) async throws -> SecretGiven
    var onCancel: (() -> Void)?
    var cancelLabel = "Cancel"

    @State private var fields = SecretFields()
    @State private var busy = false
    @State private var error: String?
    @State private var given: SecretGiven?

    private var kind: String { askedKind ?? fields.kind }
    /// Letters, digits, `_`, `-` and `.`, not starting with `.` or `-`, 1 to 64 characters.
    private var nameOK: Bool {
        let n = askedName ?? fields.name
        return n.range(of: "^[A-Za-z0-9_][A-Za-z0-9_.-]{0,63}$", options: .regularExpression) != nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let given {
                Label(given.note, systemImage: "checkmark.seal.fill")
                    .font(.footnote)
                    .foregroundStyle(palette.online)
            } else {
                if askedKind == nil {
                    Picker("Kind", selection: $fields.kind) {
                        Text("Secret").tag("secret")
                        Text("SSH key").tag("ssh_key")
                    }
                    .pickerStyle(.segmented)
                }
                if askedName == nil {
                    field {
                        TextField("Name, e.g. GITHUB_TOKEN", text: $fields.name)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .font(.callout.monospaced())
                    }
                }
                field {
                    if kind == "ssh_key" {
                        SecureField("Private key (paste)", text: $fields.value)
                            .font(.callout.monospaced())
                    } else {
                        SecureField("Value", text: $fields.value)
                            .font(.callout.monospaced())
                    }
                }
                .textContentType(.password)
                if kind == "ssh_key" {
                    field { SecureField("Passphrase (optional)", text: $fields.passphrase) }
                    Toggle("Remember for this folder", isOn: $fields.remember)
                        .font(.footnote)
                }
                if let error {
                    Text(error).font(.footnote).foregroundStyle(palette.danger)
                }
                HStack(spacing: 8) {
                    if let onCancel {
                        Button(cancelLabel, action: onCancel)
                            .buttonStyle(.bordered)
                            .tint(palette.danger)
                    }
                    Spacer()
                    Button {
                        give()
                    } label: {
                        if busy { ProgressView().controlSize(.small) } else { Text(giveLabel) }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(busy || fields.value.isEmpty || !nameOK)
                }
            }
        }
    }

    private func field<C: View>(@ViewBuilder _ content: () -> C) -> some View {
        content()
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(palette.text.opacity(0.06), in: .rect(cornerRadius: 10))
    }

    private func give() {
        busy = true
        error = nil
        var f = fields
        f.kind = kind
        if let askedName { f.name = askedName }
        Task {
            do {
                let g = try await onGive(f)
                // The value is dropped the moment it is given.
                fields = SecretFields()
                withAnimation(.snappy) { given = g }
            } catch {
                self.error = error.localizedDescription
            }
            busy = false
        }
    }
}

/// The hub's card of an agent's `hqc_secret`: what it asks for and why, and the form.
struct SecretCard: View {
    @Environment(\.palette) private var palette
    let event: HubEvent
    let open: Bool
    let onGive: ((String, SecretFields) async throws -> SecretGiven)?
    let onDecline: () async -> Void

    private var name: String { event.payload["secret"]?["name"]?.string ?? "secret" }
    private var kind: String { event.payload["secret"]?["kind"]?.string ?? "secret" }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: open ? "key.fill" : "checkmark.seal")
                    .foregroundStyle(open ? palette.waiting : palette.muted)
                Text(open ? "The agent asks for \(kind == "ssh_key" ? "an ssh key" : "a secret")"
                          : "\(kind == "ssh_key" ? "ssh key" : "secret") \(name): answered")
                    .font(.subheadline.weight(.semibold))
                Spacer()
                EventTime(event: event)
            }
            if open {
                Text(name).font(.callout.monospaced()).foregroundStyle(palette.text)
                if let why = event.payload["description"]?.string, !why.isEmpty {
                    Text(why).font(.footnote).foregroundStyle(palette.text2)
                }
                if let onGive, let id = event.requestID {
                    SecretForm(askedName: name, askedKind: kind, giveLabel: "Give",
                               onGive: { try await onGive(id, $0) },
                               onCancel: { Task { await onDecline() } }, cancelLabel: "Decline")
                } else {
                    HStack {
                        Text("Open the session to give it.").font(.footnote).foregroundStyle(palette.muted)
                        Spacer()
                        Button("Decline") { Task { await onDecline() } }.buttonStyle(.bordered).tint(palette.danger)
                    }
                }
            }
        }
        .padding(14)
        .background(open ? palette.permissionBG : palette.surface.opacity(0.5), in: .rect(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(open ? palette.permissionBorder : palette.border.opacity(0.5)))
        .opacity(open ? 1 : 0.75)
    }
}

/// The composer's "Secret": a secret or an ssh key for the agent that runs now.
struct SecretSheet: View {
    @Environment(\.dismiss) private var dismiss
    let onGive: (SecretFields) async throws -> SecretGiven

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    Text("The value goes to the worker, never into the chat or the hub's history. The agent is told only where it is.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    SecretForm(giveLabel: "Give to the agent", onGive: onGive)
                }
                .padding(20)
            }
            .paletteBackground()
            .navigationTitle("Secret")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done", systemImage: "xmark") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }
}

// MARK: usage limits

/// The 5-hour and 7-day windows of the session's worker account, as the worker last reported
/// them: in the model sheet, as the web client puts them under the model pill's menu.
struct LimitsSection: View {
    let reported: WorkerLimits
    private var limits: WorkerLimits { reported.current() }

    init(limits: WorkerLimits) { reported = limits }

    var body: some View {
        Section {
            if let w = limits.five_hour { UsageBar(label: "5-hour window", window: w, notice: 70) }
            if let w = limits.seven_day { UsageBar(label: "Weekly", window: w, notice: 70) }
        } header: {
            Text("Usage")
        } footer: {
            if limits.status == "rejected" {
                Text("The account has hit a limit.")
            }
        }
    }
}

struct LimitRow: View {
    @Environment(\.palette) private var palette
    let label: String
    let window: LimitWindow

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(label).font(.subheadline)
                Spacer()
                Text("\(Int(window.percent.rounded())) %")
                    .font(.subheadline.monospacedDigit())
                    .foregroundStyle(window.percent >= 90 ? palette.danger : palette.text2)
            }
            ProgressView(value: window.percent, total: 100)
                .tint(window.percent >= 90 ? palette.danger : window.percent >= 70 ? palette.waiting : .accentColor)
            Text(window.hasReset ? "Reset since the last report"
                 : "Resets \(window.resetsAt.formatted(.relative(presentation: .named)))")
                .font(.caption)
                .foregroundStyle(palette.muted)
        }
        .padding(.vertical, 2)
    }
}

/// Above the input while a usage window is at the hub's share (70 % by default) or the account
/// has hit a limit.
struct LimitBar: View {
    @Environment(\.palette) private var palette
    let reported: WorkerLimits
    let noticePercent: Double

    init(limits: WorkerLimits, noticePercent: Double) {
        reported = limits
        self.noticePercent = noticePercent
    }

    var body: some View {
        // Looked at again every half minute, so the bar leaves when its window resets even
        // before the worker reports again.
        TimelineView(.periodic(from: .now, by: 30)) { _ in
            bar(reported.current())
        }
    }

    private func worst(_ limits: WorkerLimits) -> (String, LimitWindow)? {
        [("5-hour", limits.five_hour), ("weekly", limits.seven_day)]
            .compactMap { name, w in w.map { (name, $0) } }
            .max { $0.1.percent < $1.1.percent }
    }

    @ViewBuilder private func bar(_ limits: WorkerLimits) -> some View {
        if limits.status == "rejected" || (worst(limits)?.1.percent ?? 0) >= noticePercent,
           let (name, w) = worst(limits), !w.hasReset {
            HStack(spacing: 8) {
                Image(systemName: limits.status == "rejected" ? "exclamationmark.octagon.fill" : "gauge.with.dots.needle.67percent")
                    .foregroundStyle(limits.status == "rejected" ? palette.danger : palette.waiting)
                Text(limits.status == "rejected"
                     ? "Usage limit reached · resets \(w.resetsAt.formatted(.relative(presentation: .named)))"
                     : "\(Int(w.percent.rounded())) % of the \(name) limit used · resets \(w.resetsAt.formatted(.relative(presentation: .named)))")
                    .font(.footnote)
                    .lineLimit(2)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            .glassEffect(.regular.tint((limits.status == "rejected" ? Color.red : .orange).opacity(0.18)), in: .rect(cornerRadius: 18))
            .padding(.horizontal, 12)
        }
    }
}

// MARK: a subagent's history

/// What a subagent has done, from its transcript on the worker, drawn as the feed draws the
/// agent's own steps; fetched again every few seconds while it is open.
struct SubagentHistorySheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @Environment(\.palette) private var palette
    let sessionID: String
    let task: LiveTask
    @State private var events: [HubEvent] = []
    @State private var truncated = false
    @State private var error: String?
    @State private var loading = true

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    if truncated {
                        Text("Earlier steps are left out.").font(.footnote).foregroundStyle(palette.muted)
                    }
                    ForEach(ChatModel.fold(events)) { item in
                        HistoryRow(item: item)
                    }
                }
                .padding(16)
            }
            .defaultScrollAnchor(.bottom)
            .paletteBackground()
            .overlay {
                if loading && events.isEmpty {
                    ChatSkeleton()
                } else if let error, events.isEmpty {
                    ContentUnavailableView("Not available", systemImage: "exclamationmark.triangle", description: Text(error))
                }
            }
            .navigationTitle(task.description)
            .navigationSubtitle(task.activity ?? "Subagent")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done", systemImage: "xmark") { dismiss() }
                }
            }
            .task {
                while !Task.isCancelled {
                    await load()
                    try? await Task.sleep(for: .seconds(3))
                }
            }
        }
    }

    private func load() async {
        guard let client = app.client else { return }
        do {
            let h = try await client.subagentHistory(sessionID: sessionID, taskID: task.id,
                                                     after: events.last?.event_id)
            if events.isEmpty || h.continues != true {
                events = h.continues == true ? events + h.events : h.events
            } else {
                events += h.events
            }
            truncated = h.truncated
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        loading = false
    }
}

/// A history row: the feed's own rows, without the actions of a live chat.
private struct HistoryRow: View {
    let item: FeedItem
    var body: some View {
        switch item {
        case .agent(let ev): AgentMessage(event: ev)
        case .thinking(let ev): ThinkingView(text: ev.text, live: false)
        case .tools(let calls): ToolGroup(calls: calls)
        case .user(let ev), .peer(let ev): Text(ev.text).font(.callout).foregroundStyle(.secondary)
        case .note(_, let text, let level): NoteRow(text: text, level: level)
        default: EmptyView()
        }
    }
}

// MARK: usage

/// Each machine's Claude account usage: the 5-hour and the weekly window as bars in percent,
/// with when each resets, as the worker last reported them. Refreshed while open.
struct UsageView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.palette) private var palette
    /// The worker of the chat this was opened from, listed first.
    var focusWorker: String?
    @State private var workers: [WorkerInfo] = []
    @State private var loaded = false

    private var sorted: [WorkerInfo] {
        workers.filter { !$0.isRevoked }.sorted { a, b in
            if (a.id == focusWorker) != (b.id == focusWorker) { return a.id == focusWorker }
            return a.name.localizedCaseInsensitiveCompare(b.name) == .orderedAscending
        }
    }

    var body: some View {
        List {
            ForEach(sorted) { w in
                Section {
                    if let limits = w.limits?.current() {
                        if let five = limits.five_hour {
                            UsageBar(label: "5-hour window", window: five, notice: app.me?.limit_notice_percent ?? 70)
                        }
                        if let week = limits.seven_day {
                            UsageBar(label: "Weekly", window: week, notice: app.me?.limit_notice_percent ?? 70)
                        }
                        if limits.five_hour == nil && limits.seven_day == nil {
                            Text("No usage reported yet.").foregroundStyle(palette.muted)
                        }
                    } else {
                        Text("No usage reported yet: it comes with the first turn on this machine.")
                            .font(.footnote).foregroundStyle(palette.muted)
                    }
                } header: {
                    HStack(spacing: 6) {
                        Circle().fill(w.isOnline ? Color.green : Color.gray).frame(width: 7, height: 7)
                        Text(w.name)
                    }
                } footer: {
                    // The worker reports usage only while a turn of hqc runs there, so it can lag
                    // behind what Claude's own apps show.
                    if let at = w.limits?.reported_at.flatMap(HubEvent.parseDate) {
                        Text("As of \(at.formatted(.relative(presentation: .named))), the last turn run through hqc on this machine.")
                    }
                }
            }
        }
        .navigationTitle("Usage")
        .navigationBarTitleDisplayMode(.inline)
        .paletteBackground()
        .overlay {
            if !loaded { SkeletonList(rows: 2, icon: 0) }
            else if sorted.isEmpty { ContentUnavailableView("No machines", systemImage: "desktopcomputer") }
        }
        .task {
            while !Task.isCancelled {
                if let w = try? await app.client?.workers() { workers = w }
                loaded = true
                try? await Task.sleep(for: .seconds(30))
            }
        }
    }
}

/// One window: its name, the percent used, a bar, and when it resets.
struct UsageBar: View {
    @Environment(\.palette) private var palette
    let label: String
    let window: LimitWindow
    let notice: Double

    private var percent: Double { window.percent }
    private var tint: Color {
        percent >= 100 ? palette.danger : percent >= notice ? palette.waiting : .accentColor
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Text(label).font(.subheadline.weight(.medium))
                Spacer()
                Text("\(Int(percent.rounded())) %")
                    .font(.title3.weight(.semibold).monospacedDigit())
                    .foregroundStyle(tint)
                    .contentTransition(.numericText())
            }
            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    Capsule().fill(palette.text.opacity(0.08))
                    Capsule().fill(tint.gradient)
                        .frame(width: max(6, geo.size.width * percent / 100))
                }
            }
            .frame(height: 10)
            .animation(.smooth, value: percent)
            Text(window.hasReset ? "Reset since the last report"
                 : "Resets \(window.resetsAt.formatted(.relative(presentation: .named))) · \(window.resetsAt.formatted(date: .omitted, time: .shortened))")
                .font(.caption)
                .foregroundStyle(palette.muted)
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }
}
