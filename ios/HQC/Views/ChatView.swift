import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

struct ChatView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var model: ChatModel
    @State private var draft = ""
    @State private var attachments: [PendingAttachment] = []
    @State private var showError = false
    @State private var sheet: ChatSheet?
    @State private var renaming = false
    @State private var newTitle = ""
    @State private var confirmDelete = false
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
    private var isPaused: Bool { model.info?.paused_at != nil || model.state == .paused }

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
            VStack(spacing: 6) {
                if !model.live.isEmpty {
                    LiveTasksBar(tasks: model.live) { t in Task { await model.stopTask(t) } }
                }
                if isPaused {
                    PausedBar { Task { await model.act("resume") } }
                }
                if canType {
                    Composer(
                        text: $draft, attachments: $attachments, busy: model.state.isBusy,
                        commands: model.info?.commands ?? [],
                        onSend: send, onStop: { Task { await model.stopAgent() } })
                } else if model.state == .locked_vscode {
                    InfoBar(text: "This session is open in VS Code. Close it there to continue here.",
                            systemImage: "chevron.left.forwardslash.chevron.right")
                } else if model.info?.archived == true {
                    Button("Unarchive", systemImage: "tray.and.arrow.up") { Task { await model.act("unarchive") } }
                        .buttonStyle(.glassProminent)
                        .padding(12)
                }
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
        .onAppear { app.watching = model.sessionID }
        .onDisappear {
            model.stop()
            if app.watching == model.sessionID { app.watching = nil }
        }
        .onChange(of: model.error) { _, e in showError = e != nil }
        .alert("Something went wrong", isPresented: $showError) {
            Button("OK") { model.error = nil }
        } message: {
            Text(model.error ?? "")
        }
        .alert("Rename session", isPresented: $renaming) {
            TextField("Title", text: $newTitle)
            Button("Rename") { Task { await model.patch(["title": newTitle]) } }
            Button("Cancel", role: .cancel) {}
        }
        .confirmationDialog("Delete this session and its history?", isPresented: $confirmDelete,
                            titleVisibility: .visible) {
            Button("Delete", role: .destructive) {
                Task {
                    try? await app.client?.deleteSession(model.sessionID)
                    dismiss()
                }
            }
        }
        .sheet(item: $sheet) { s in
            switch s {
            case .schedules:
                NavigationStack { SchedulesView(sessionID: model.sessionID) }
            case .model:
                ModelSheet(current: model.info?.model ?? "") { m in
                    Task { await model.patch(["model": m.isEmpty ? NSNull() : m]) }
                }
                .presentationDetents([.medium])
            case .roomAgents:
                if let info = model.info {
                    NavigationStack { RoomAgentsView(room: info) }
                }
            }
        }
        .sensoryFeedback(.success, trigger: model.items.count)
    }

    private var subtitle: String {
        var parts = [isPaused ? "Paused" : model.state.label]
        if let mode = model.info?.effective_permission_mode ?? model.info?.permission_mode, mode != .ask {
            parts.append(mode.label)
        }
        if let m = model.info?.model { parts.append(m) }
        if let r = model.info?.room_mode { parts.append(r == "council" ? "Council" : "Conversation") }
        return parts.joined(separator: " · ")
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        if model.info?.isRoom == true {
            ToolbarItem(placement: .primaryAction) {
                Button("Agents", systemImage: "person.3") { sheet = .roomAgents }
            }
        }
        ToolbarItem(placement: .primaryAction) {
            Menu {
                if model.info?.isRoom != true {
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
                    Section {
                        Button("Model…", systemImage: "cpu") { sheet = .model }
                        Button("Schedules", systemImage: "calendar.badge.clock") { sheet = .schedules }
                        if isPaused {
                            Button("Resume", systemImage: "play.fill") { Task { await model.act("resume") } }
                        } else {
                            Button("Pause", systemImage: "pause.fill") { Task { await model.act("pause") } }
                        }
                        Button("Restart agent", systemImage: "arrow.triangle.2.circlepath") {
                            Task { await model.act("restart") }
                        }
                        if model.state == .error {
                            Button("Retry", systemImage: "arrow.clockwise") { Task { await model.retry() } }
                        }
                        if model.state.isBusy {
                            Button("Stop", systemImage: "stop.fill", role: .destructive) {
                                Task { await model.stopAgent() }
                            }
                        }
                    }
                }
                Section {
                    Button("Rename", systemImage: "pencil") {
                        newTitle = summaryTitle
                        renaming = true
                    }
                    let fav = model.info?.favorite_at != nil
                    Button(fav ? "Remove from favorites" : "Add to favorites",
                           systemImage: fav ? "star.slash" : "star") {
                        Task { await model.setFavorite(!fav) }
                    }
                    if model.info?.archived == true {
                        Button("Unarchive", systemImage: "tray.and.arrow.up") { Task { await model.act("unarchive") } }
                    } else {
                        Button("Archive", systemImage: "archivebox") {
                            Task {
                                await model.act("archive")
                                dismiss()
                            }
                        }
                    }
                    Button("Delete", systemImage: "trash", role: .destructive) { confirmDelete = true }
                }
            } label: {
                Label("More", systemImage: "ellipsis")
            }
        }
    }

    private func send() {
        let text = draft
        let ready = attachments.compactMap(\.artifact)
        draft = ""
        attachments = []
        Task { await model.send(text, artifacts: ready) }
    }
}

enum ChatSheet: String, Identifiable {
    case schedules, model, roomAgents
    var id: String { rawValue }
}

/// A file picked for the next message: uploading, then the hub's artifact.
struct PendingAttachment: Identifiable {
    let id = UUID()
    let name: String
    var preview: UIImage?
    var artifact: Artifact?
    var failed = false
}

/// The input at the bottom: attachments, a glass field with slash-command completion, and the
/// send button that turns into Stop while the agent works.
struct Composer: View {
    @Environment(AppModel.self) private var app
    @Binding var text: String
    @Binding var attachments: [PendingAttachment]
    let busy: Bool
    let commands: [String]
    let onSend: () -> Void
    let onStop: () -> Void
    @FocusState private var focused: Bool
    @Namespace private var glass
    @State private var pickingPhotos = false
    @State private var pickingFiles = false
    @State private var photoItems: [PhotosPickerItem] = []

    private var empty: Bool { text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    private var uploading: Bool { attachments.contains { $0.artifact == nil && !$0.failed } }
    private var canSend: Bool { (!empty || attachments.contains { $0.artifact != nil }) && !uploading }

    /// `/` and the start of a command: the runtime's commands that match.
    private var suggestions: [String] {
        guard text.hasPrefix("/"), !text.contains(" "), !text.contains("\n") else { return [] }
        let q = text.dropFirst().lowercased()
        return Array(commands.filter { q.isEmpty || $0.lowercased().hasPrefix(q) }.prefix(6))
    }

    var body: some View {
        VStack(spacing: 8) {
            if !suggestions.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(suggestions, id: \.self) { c in
                            Button("/\(c)") { text = "/\(c) " }
                                .buttonStyle(.glass)
                                .font(.subheadline.monospaced())
                        }
                    }
                    .padding(.horizontal, 12)
                }
            }
            if !attachments.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(attachments) { a in attachmentChip(a) }
                    }
                    .padding(.horizontal, 12)
                }
            }
            GlassEffectContainer(spacing: 10) {
                HStack(alignment: .bottom, spacing: 10) {
                    Menu {
                        Button("Photos", systemImage: "photo.on.rectangle") { pickingPhotos = true }
                        Button("Files", systemImage: "folder") { pickingFiles = true }
                    } label: {
                        Image(systemName: "plus")
                            .font(.system(size: 17, weight: .semibold))
                            .frame(width: 44, height: 44)
                    }
                    .buttonStyle(.glass)
                    .buttonBorderShape(.circle)
                    .glassEffectID("attach", in: glass)
                    .accessibilityLabel("Attach")

                    TextField("Message", text: $text, axis: .vertical)
                        .lineLimit(1...8)
                        .focused($focused)
                        .padding(.horizontal, 18)
                        .padding(.vertical, 12)
                        .glassEffect(.regular.interactive(), in: .rect(cornerRadius: 22))
                        .glassEffectID("field", in: glass)

                    if busy && empty && attachments.isEmpty {
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
                            Group {
                                if uploading {
                                    ProgressView().tint(.white)
                                } else {
                                    Image(systemName: "arrow.up").font(.system(size: 17, weight: .bold))
                                }
                            }
                            .frame(width: 44, height: 44)
                        }
                        .buttonStyle(.glassProminent)
                        .buttonBorderShape(.circle)
                        .disabled(!canSend)
                        .glassEffectID("action", in: glass)
                        .accessibilityLabel("Send")
                    }
                }
                .animation(.spring(duration: 0.35), value: busy && empty)
            }
            .padding(.horizontal, 12)
        }
        .padding(.bottom, 8)
        .padding(.top, 4)
        .photosPicker(isPresented: $pickingPhotos, selection: $photoItems, maxSelectionCount: 10, matching: .images)
        .onChange(of: photoItems) { _, items in
            photoItems = []
            for item in items { Task { await addPhoto(item) } }
        }
        .fileImporter(isPresented: $pickingFiles, allowedContentTypes: [.item], allowsMultipleSelection: true) { r in
            guard case .success(let urls) = r else { return }
            for url in urls { Task { await addFile(url) } }
        }
    }

    private func attachmentChip(_ a: PendingAttachment) -> some View {
        HStack(spacing: 8) {
            if let p = a.preview {
                Image(uiImage: p).resizable().scaledToFill().frame(width: 32, height: 32).clipShape(.rect(cornerRadius: 8))
            } else {
                Image(systemName: "doc").frame(width: 32, height: 32)
            }
            Text(a.name).font(.caption).lineLimit(1).frame(maxWidth: 120)
            if a.failed {
                Image(systemName: "exclamationmark.circle.fill").foregroundStyle(.red)
            } else if a.artifact == nil {
                ProgressView().controlSize(.mini)
            }
            Button {
                attachments.removeAll { $0.id == a.id }
            } label: {
                Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Remove \(a.name)")
        }
        .padding(6)
        .glassEffect(.regular, in: .rect(cornerRadius: 14))
    }

    private func addPhoto(_ item: PhotosPickerItem) async {
        guard let data = try? await item.loadTransferable(type: Data.self) else { return }
        let type = item.supportedContentTypes.first ?? .jpeg
        let image = UIImage(data: data)
        // HEIC from the library goes up as JPEG, which every agent and browser reads.
        let (bytes, mime, ext): (Data, String, String) =
            type.conforms(to: .heic) || type.conforms(to: .heif)
            ? (image?.jpegData(compressionQuality: 0.9) ?? data, "image/jpeg", "jpg")
            : (data, type.mime, type.preferredFilenameExtension ?? "jpg")
        let name = "photo-\(Int(Date().timeIntervalSince1970)).\(ext)"
        await upload(bytes, name: name, mime: mime, preview: image)
    }

    private func addFile(_ url: URL) async {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        guard let data = try? Data(contentsOf: url) else { return }
        let type = UTType(filenameExtension: url.pathExtension) ?? .data
        await upload(data, name: url.lastPathComponent, mime: type.mime,
                     preview: type.conforms(to: .image) ? UIImage(data: data) : nil)
    }

    private func upload(_ data: Data, name: String, mime: String, preview: UIImage?) async {
        let pending = PendingAttachment(name: name, preview: preview)
        attachments.append(pending)
        do {
            guard let client = app.client else { throw HubError.signInRequired }
            let a = try await client.upload(data: data, name: name, mime: mime)
            if let i = attachments.firstIndex(where: { $0.id == pending.id }) { attachments[i].artifact = a }
        } catch {
            if let i = attachments.firstIndex(where: { $0.id == pending.id }) { attachments[i].failed = true }
        }
    }
}

/// What the agent runs in the background now, each with Stop where the runtime allows it.
struct LiveTasksBar: View {
    let tasks: [LiveTask]
    let onStop: (LiveTask) -> Void
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.snappy) { expanded.toggle() }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: "circle.hexagongrid")
                        .symbolEffect(.rotate, options: .repeating)
                        .foregroundStyle(.tint)
                    Text(tasks.count == 1 ? tasks[0].description : "\(tasks.count) tasks running")
                        .font(.footnote.weight(.medium))
                        .lineLimit(1)
                    Spacer()
                    Image(systemName: "chevron.up")
                        .font(.caption2.weight(.bold))
                        .rotationEffect(.degrees(expanded ? 180 : 0))
                        .foregroundStyle(.secondary)
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            if expanded {
                ForEach(tasks) { t in
                    HStack(spacing: 10) {
                        Image(systemName: t.symbol).frame(width: 18).foregroundStyle(.secondary)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(t.description).font(.footnote).lineLimit(1)
                            Text(detail(t)).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                        }
                        Spacer()
                        if t.stoppable {
                            Button("Stop") { onStop(t) }
                                .font(.caption.weight(.semibold))
                                .buttonStyle(.glass)
                                .tint(.red)
                        }
                    }
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .glassEffect(.regular, in: .rect(cornerRadius: 20))
        .padding(.horizontal, 12)
    }

    private func detail(_ t: LiveTask) -> String {
        var parts: [String] = []
        if let a = t.activity { parts.append(a) }
        if let tok = t.tokens { parts.append("\(tok.formatted(.number.notation(.compactName))) tokens") }
        if let u = t.toolUses { parts.append("\(u) tools") }
        parts.append(Duration.milliseconds(t.durationMs).formatted(.units(allowed: [.minutes, .seconds], width: .narrow)))
        return parts.joined(separator: " · ")
    }
}

struct PausedBar: View {
    let onResume: () -> Void

    var body: some View {
        HStack {
            Label("Paused: messages wait until you resume", systemImage: "pause.circle")
                .font(.footnote)
            Spacer()
            Button("Resume", action: onResume).buttonStyle(.glassProminent).controlSize(.small)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .glassEffect(.regular.tint(.orange.opacity(0.2)), in: .rect(cornerRadius: 20))
        .padding(.horizontal, 12)
    }
}

struct ModelSheet: View {
    @Environment(\.dismiss) private var dismiss
    let current: String
    let onPick: (String) -> Void
    @State private var custom = ""

    private let families = [
        ("Default", ""), ("Opus", "opus"), ("Sonnet", "sonnet"), ("Haiku", "haiku"), ("Fable", "fable"),
    ]

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    ForEach(families, id: \.1) { name, value in
                        Button {
                            onPick(value)
                            dismiss()
                        } label: {
                            HStack {
                                Text(name).foregroundStyle(.primary)
                                Spacer()
                                if current == value { Image(systemName: "checkmark").foregroundStyle(.tint) }
                            }
                        }
                    }
                } footer: {
                    Text("A running agent switches at its next start; Default is Claude Code's own model.")
                }
                Section("Another model") {
                    TextField("claude-opus-5-5", text: $custom)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button("Use this model") {
                        onPick(custom.trimmingCharacters(in: .whitespaces))
                        dismiss()
                    }
                    .disabled(custom.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .navigationTitle("Model")
            .navigationBarTitleDisplayMode(.inline)
            .onAppear { custom = families.contains { $0.1 == current } ? "" : current }
        }
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
