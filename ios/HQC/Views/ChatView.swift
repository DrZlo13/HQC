import SwiftUI

struct ChatView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.scenePhase) private var scenePhase
    @State private var model: ChatModel
    @State private var draft = ""
    @State private var showError = false
    let title: String

    init(sessionID: String, title: String) {
        _model = State(initialValue: ChatModel(sessionID: sessionID))
        self.title = title
    }

    private var summaryTitle: String { app.summary(model.sessionID)?.title ?? model.info?.title ?? title }
    private var canType: Bool {
        guard let info = model.info else { return false }
        return !info.system && !info.archived && model.state != .locked_vscode
    }

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 14) {
                if model.hasOlder {
                    Button("Load earlier messages") { Task { await model.loadOlder() } }
                        .buttonStyle(.glass)
                        .frame(maxWidth: .infinity)
                }
                ForEach(model.items) { item in
                    FeedRow(item: item, model: model)
                        .id(item.id)
                }
                if !model.thinking.isEmpty {
                    ThinkingView(text: model.thinking, live: true)
                }
                if !model.streaming.isEmpty {
                    MarkdownText(model.streaming)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .transition(.opacity)
                }
                if model.state.isBusy && model.streaming.isEmpty && model.thinking.isEmpty {
                    WorkingIndicator()
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .scrollTargetLayout()
        }
        .defaultScrollAnchor(.bottom)
        .scrollDismissesKeyboard(.interactively)
        .overlay {
            if model.loading { ProgressView() }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if canType {
                Composer(text: $draft, busy: model.state.isBusy,
                         onSend: send, onStop: { Task { await model.stopAgent() } })
            } else if model.state == .locked_vscode {
                InfoBar(text: "This session is open in VS Code. Close it there to continue here.",
                        systemImage: "chevron.left.forwardslash.chevron.right")
            }
        }
        .navigationTitle(summaryTitle)
        .navigationSubtitle(subtitle)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbar }
        .toolbar(.hidden, for: .tabBar)
        .task {
            await model.start(app: app)
        }
        .onDisappear { model.stop() }
        .onChange(of: model.error) { _, e in showError = e != nil }
        .alert("Something went wrong", isPresented: $showError) {
            Button("OK") { model.error = nil }
        } message: {
            Text(model.error ?? "")
        }
        .sensoryFeedback(.success, trigger: model.items.count)
    }

    private var subtitle: String {
        var parts = [model.state.label]
        if let mode = model.info?.effective_permission_mode ?? model.info?.permission_mode, mode != .ask {
            parts.append(mode.label)
        }
        if let m = model.info?.model { parts.append(m) }
        return parts.joined(separator: " · ")
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .primaryAction) {
            Menu {
                Section("Permissions") {
                    ForEach(PermissionMode.allCases) { mode in
                        Button {
                            Task { await model.setMode(mode) }
                        } label: {
                            if (model.info?.permission_mode ?? .ask) == mode {
                                Label(mode.label, systemImage: "checkmark")
                            } else {
                                Text(mode.label)
                            }
                        }
                    }
                }
                if model.state == .error {
                    Button("Retry", systemImage: "arrow.clockwise") { Task { await model.retry() } }
                }
                if model.state.isBusy {
                    Button("Stop", systemImage: "stop.fill", role: .destructive) { Task { await model.stopAgent() } }
                }
            } label: {
                Label("More", systemImage: "ellipsis")
            }
        }
    }

    private func send() {
        let text = draft
        draft = ""
        Task { await model.send(text) }
    }
}

/// The input at the bottom: a glass capsule with the field and a send (or stop) button.
struct Composer: View {
    @Binding var text: String
    let busy: Bool
    let onSend: () -> Void
    let onStop: () -> Void
    @FocusState private var focused: Bool
    @Namespace private var glass

    private var empty: Bool { text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        GlassEffectContainer(spacing: 10) {
            HStack(alignment: .bottom, spacing: 10) {
                TextField("Message", text: $text, axis: .vertical)
                    .lineLimit(1...8)
                    .focused($focused)
                    .padding(.horizontal, 18)
                    .padding(.vertical, 12)
                    .glassEffect(.regular.interactive(), in: .rect(cornerRadius: 22))
                    .glassEffectID("field", in: glass)

                if busy && empty {
                    Button(action: onStop) {
                        Image(systemName: "stop.fill")
                            .font(.system(size: 17, weight: .bold))
                            .frame(width: 44, height: 44)
                    }
                    .buttonStyle(.glass)
                    .buttonBorderShape(.circle)
                    .tint(.red)
                    .glassEffectID("action", in: glass)
                    .accessibilityLabel("Stop the agent")
                } else {
                    Button(action: onSend) {
                        Image(systemName: "arrow.up")
                            .font(.system(size: 17, weight: .bold))
                            .frame(width: 44, height: 44)
                    }
                    .buttonStyle(.glassProminent)
                    .buttonBorderShape(.circle)
                    .disabled(empty)
                    .glassEffectID("action", in: glass)
                    .accessibilityLabel("Send")
                }
            }
            .animation(.spring(duration: 0.35), value: busy && empty)
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 8)
        .padding(.top, 4)
    }
}

struct InfoBar: View {
    let text: String
    let systemImage: String

    var body: some View {
        Label(text, systemImage: systemImage)
            .font(.footnote)
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .glassEffect(.regular, in: .rect(cornerRadius: 20))
            .padding(12)
    }
}

struct WorkingIndicator: View {
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "sparkles")
                .symbolEffect(.variableColor.iterative, options: .repeating)
                .foregroundStyle(.tint)
            Text("Working…").foregroundStyle(.secondary)
        }
        .font(.subheadline)
    }
}
