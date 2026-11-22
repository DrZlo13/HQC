import SwiftUI

/// The projects, the personal one first, then favorites, then by name.
struct ProjectsView: View {
    @Environment(AppModel.self) private var app

    private var sorted: [Project] {
        app.projects.sorted { a, b in
            if a.is_personal != b.is_personal { return a.is_personal }
            if (a.favorite_at != nil) != (b.favorite_at != nil) { return a.favorite_at != nil }
            return a.name.localizedCaseInsensitiveCompare(b.name) == .orderedAscending
        }
    }

    var body: some View {
        List {
            if app.socketState != .open {
                ConnectionBanner(state: app.socketState)
                    .listRowBackground(Color.clear)
                    .listRowInsets(EdgeInsets())
            }
            ForEach(sorted) { project in
                NavigationLink(value: project) {
                    ProjectRow(project: project)
                }
            }
        }
        .navigationTitle("Projects")
        .navigationDestination(for: Project.self) { SessionListView(project: $0) }
        .navigationDestination(for: SessionInfo.self) { ChatView(sessionID: $0.id, title: $0.title) }
        .refreshable { await app.refresh() }
        .overlay {
            if app.projects.isEmpty {
                ContentUnavailableView("No projects yet", systemImage: "folder",
                                       description: Text("Create one in the web app."))
            }
        }
    }
}

struct ProjectTile: View {
    let project: Project
    var size: CGFloat = 40

    var body: some View {
        Text(project.monogram)
            .font(.system(size: size * 0.4, weight: .bold, design: .rounded))
            .foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(project.tint.gradient, in: .rect(cornerRadius: size * 0.28))
    }
}

struct ProjectRow: View {
    let project: Project

    var body: some View {
        HStack(spacing: 14) {
            ProjectTile(project: project)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(project.displayName).font(.headline)
                    if project.favorite_at != nil {
                        Image(systemName: "star.fill").font(.caption2).foregroundStyle(.yellow)
                    }
                }
                if let d = project.description, !d.isEmpty {
                    Text(d).font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
                }
            }
        }
        .padding(.vertical, 4)
    }
}

struct ConnectionBanner: View {
    let state: HubSocket.State

    var body: some View {
        HStack(spacing: 8) {
            ProgressView().controlSize(.small)
            Text(state == .connecting ? "Connecting…" : "Offline, reconnecting…")
                .font(.footnote.weight(.medium))
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .glassEffect(.regular.tint(.orange.opacity(0.25)), in: .capsule)
        .frame(maxWidth: .infinity)
        .padding(.vertical, 6)
    }
}

/// One project's live sessions, newest first, with their state from the live summaries.
struct SessionListView: View {
    @Environment(AppModel.self) private var app
    let project: Project
    @State private var sessions: [SessionInfo] = []
    @State private var loading = true
    @State private var error: String?
    @State private var showNew = false
    @State private var opened: SessionInfo?

    private var visible: [SessionInfo] {
        sessions.filter { !$0.archived }.sorted { $0.updated_at > $1.updated_at }
    }

    var body: some View {
        List {
            ForEach(visible) { s in
                NavigationLink(value: s) {
                    SessionRow(session: s, summary: app.summary(s.id), waiting: !app.requests(for: s.id).isEmpty)
                }
                .swipeActions {
                    Button("Archive", systemImage: "archivebox") {
                        Task {
                            try? await app.client?.archive(sessionID: s.id)
                            await load()
                        }
                    }
                    .tint(.indigo)
                }
            }
        }
        .navigationTitle(project.displayName)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("New session", systemImage: "square.and.pencil") { showNew = true }
            }
        }
        .overlay {
            if loading && sessions.isEmpty {
                ProgressView()
            } else if visible.isEmpty {
                ContentUnavailableView {
                    Label("No sessions", systemImage: "bubble.left.and.bubble.right")
                } description: {
                    Text(error ?? "Start one and talk to an agent on your machine.")
                } actions: {
                    Button("New session") { showNew = true }.buttonStyle(.glassProminent)
                }
            }
        }
        .refreshable { await load() }
        .task { await load() }
        .sheet(isPresented: $showNew) {
            NewSessionSheet(project: project) { created in
                showNew = false
                sessions.insert(created, at: 0)
                opened = created
            }
            .presentationDetents([.medium, .large])
        }
        .navigationDestination(item: $opened) { ChatView(sessionID: $0.id, title: $0.title) }
    }

    private func load() async {
        guard let client = app.client else { return }
        do {
            sessions = try await client.sessions(projectID: project.id)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        loading = false
    }
}

struct SessionRow: View {
    let session: SessionInfo
    let summary: SessionSummary?
    let waiting: Bool

    private var state: SessionState { waiting ? .waiting : (summary?.state ?? session.state) }
    private var unread: Int { summary?.unread ?? session.unread }

    var body: some View {
        HStack(spacing: 12) {
            StateDot(state: state)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    if session.isRoom {
                        Image(systemName: "person.3.fill").font(.caption).foregroundStyle(.secondary)
                    }
                    Text(summary?.title ?? session.title)
                        .font(unread > 0 ? .body.weight(.semibold) : .body)
                        .lineLimit(1)
                }
                HStack(spacing: 6) {
                    Text(state.label)
                    if let n = summary?.subagents, n > 0 {
                        Text("· \(n) subagent\(n == 1 ? "" : "s")")
                    }
                    if let date = HubEvent.parseDate(session.updated_at) {
                        Text("· \(date, format: .relative(presentation: .named))")
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer()
            if unread > 0 {
                Text("\(unread)")
                    .font(.caption2.bold())
                    .padding(.horizontal, 7)
                    .padding(.vertical, 2)
                    .background(.tint, in: .capsule)
                    .foregroundStyle(.white)
            }
        }
        .padding(.vertical, 2)
    }
}

struct StateDot: View {
    let state: SessionState

    var body: some View {
        Circle()
            .fill(state.color)
            .frame(width: 9, height: 9)
            .overlay {
                if state.isBusy {
                    Circle().stroke(state.color.opacity(0.5), lineWidth: 2)
                        .phaseAnimator([1.0, 2.2]) { c, scale in
                            c.scaleEffect(scale).opacity(2.2 - scale)
                        } animation: { _ in .easeOut(duration: 1.2) }
                }
            }
    }
}

struct NewSessionSheet: View {
    @Environment(AppModel.self) private var app
    let project: Project
    let onCreated: (SessionInfo) -> Void

    @State private var title = ""
    @State private var workspaces: [Workspace] = []
    @State private var workspaceID: String?
    @State private var mode: PermissionMode?
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Title (optional)", text: $title)
                }
                Section("Folder") {
                    if workspaces.isEmpty {
                        Text("This project has no workspace yet. Add one in the web app: project → Workspaces.")
                            .font(.footnote).foregroundStyle(.secondary)
                    } else {
                        Picker("Workspace", selection: $workspaceID) {
                            ForEach(workspaces) { w in
                                VStack(alignment: .leading) {
                                    Text(w.displayName)
                                    Text(w.path).font(.caption).foregroundStyle(.secondary)
                                }
                                .tag(Optional(w.id))
                            }
                        }
                        .pickerStyle(.inline)
                        .labelsHidden()
                    }
                }
                Section("Permissions") {
                    Picker("Mode", selection: $mode) {
                        Text("Worker default").tag(PermissionMode?.none)
                        ForEach(PermissionMode.allCases) { Text($0.label).tag(Optional($0)) }
                    }
                }
                if let error {
                    Section { Text(error).foregroundStyle(.red).font(.footnote) }
                }
            }
            .navigationTitle("New session")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Start", systemImage: "checkmark") { Task { await create() } }
                        .disabled(busy || workspaces.isEmpty)
                }
            }
            .task {
                workspaces = (try? await app.client?.workspaces(projectID: project.id)) ?? []
                workspaceID = project.default_workspace_id ?? workspaces.first?.id
            }
        }
    }

    private func create() async {
        guard let client = app.client else { return }
        busy = true
        defer { busy = false }
        do {
            let s = try await client.createSession(
                projectID: project.id, title: title, workspaceID: workspaceID, permissionMode: mode)
            onCreated(s)
        } catch {
            self.error = error.localizedDescription
        }
    }
}
