import SwiftUI

/// The projects, the personal one first, then favorites, then by name.
struct ProjectsView: View {
    @Environment(AppModel.self) private var app
    @State private var creating = false

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
                NavigationLink(value: Route.project(project)) {
                    ProjectRow(project: project, unread: unread(project))
                }
                .swipeActions(edge: .leading) {
                    Button(project.favorite_at == nil ? "Favorite" : "Unfavorite",
                           systemImage: project.favorite_at == nil ? "star" : "star.slash") {
                        Task {
                            try? await app.client?.setProjectFavorite(project.id, project.favorite_at == nil)
                            await app.refresh()
                        }
                    }
                    .tint(.yellow)
                }
            }
        }
        .navigationTitle("Projects")
        .paletteBackground()
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("New project", systemImage: "plus") { creating = true }
            }
        }
        .refreshable { await app.refresh() }
        .overlay {
            if app.projects.isEmpty {
                ContentUnavailableView("No projects yet", systemImage: "folder",
                                       description: Text("Create one with +."))
            }
        }
        .sheet(isPresented: $creating) {
            NewProjectSheet().presentationDetents([.medium])
        }
    }

    private func unread(_ p: Project) -> Int {
        app.summaries.values.filter { $0.project_id == p.id && $0.unread > 0 }.count
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
    @Environment(AppModel.self) private var app
    @Environment(\.palette) private var palette
    let project: Project
    var unread = 0

    var body: some View {
        HStack(spacing: 12) {
            ProjectBadge(project: project)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(project.displayName).font(.headline)
                    if project.favorite_at != nil {
                        Image(systemName: "star.fill").font(.caption2).foregroundStyle(.yellow)
                    }
                }
                if app.projectWorking(project.id) {
                    let agents = app.projectAgents(project.id)
                    Text("\(agents) agent\(agents == 1 ? "" : "s") · \(Int(app.projectRate(project.id))) tokens/s")
                        .font(.subheadline.monospacedDigit())
                        .foregroundStyle(palette.text2)
                        .contentTransition(.numericText())
                } else if let d = project.description, !d.isEmpty {
                    Text(d).font(.subheadline).foregroundStyle(palette.muted).lineLimit(1)
                }
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

/// One project's sessions and rooms, favorites first, then the newest; archived ones on demand.
struct SessionListView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let project: Project
    @State private var sessions: [SessionInfo] = []
    @State private var loading = true
    @State private var error: String?
    @State private var sheet: ProjectSheet?
    @State private var showArchived = false
    @State private var renaming = false
    @State private var newName = ""

    private var current: Project { app.project(project.id) ?? project }

    private var visible: [SessionInfo] {
        sessions
            .filter { $0.archived == showArchived && $0.room_id == nil }
            .sorted { a, b in
                if (a.favorite_at != nil) != (b.favorite_at != nil) { return a.favorite_at != nil }
                return a.updated_at > b.updated_at
            }
    }

    var body: some View {
        List {
            ForEach(visible) { s in
                Group {
                    if app.isRegular {
                        Button {
                            app.selectedSessionID = s.id
                            app.detailPath = []
                        } label: {
                            SessionRow(session: s, summary: app.summary(s.id), waiting: !app.requests(for: s.id).isEmpty)
                                .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                        .listRowBackground(
                            app.selectedSessionID == s.id
                                ? RoundedRectangle(cornerRadius: 10).fill(current.tint.opacity(0.22)).padding(.horizontal, 6)
                                : nil
                        )
                    } else {
                        NavigationLink(value: Route.session(id: s.id, title: s.title)) {
                            SessionRow(session: s, summary: app.summary(s.id), waiting: !app.requests(for: s.id).isEmpty)
                        }
                    }
                }
                .swipeActions {
                    if s.archived {
                        Button("Unarchive", systemImage: "tray.and.arrow.up") {
                            Task {
                                try? await app.client?.sessionAction(s.id, "unarchive")
                                await load()
                            }
                        }
                        .tint(.green)
                    } else {
                        Button("Archive", systemImage: "archivebox") {
                            Task {
                                try? await app.client?.archive(sessionID: s.id)
                                await load()
                            }
                        }
                        .tint(.indigo)
                    }
                }
                .swipeActions(edge: .leading) {
                    Button(s.favorite_at == nil ? "Favorite" : "Unfavorite",
                           systemImage: s.favorite_at == nil ? "star" : "star.slash") {
                        Task {
                            try? await app.client?.setSessionFavorite(s.id, s.favorite_at == nil)
                            await load()
                        }
                    }
                    .tint(.yellow)
                }
            }
        }
        .navigationTitle(showArchived ? "Archived" : current.displayName)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Button("New session", systemImage: "square.and.pencil") { sheet = .newSession }
                    Button("New room", systemImage: "person.3") { sheet = .newRoom }
                    Divider()
                    Button("Workspaces", systemImage: "folder") { sheet = .workspaces }
                    Toggle(isOn: $showArchived) { Label("Show archived", systemImage: "archivebox") }
                    Button(current.favorite_at == nil ? "Add to favorites" : "Remove from favorites",
                           systemImage: current.favorite_at == nil ? "star" : "star.slash") {
                        Task {
                            try? await app.client?.setProjectFavorite(project.id, current.favorite_at == nil)
                            await app.refresh()
                        }
                    }
                    if !project.is_personal {
                        Button("Rename", systemImage: "pencil") {
                            newName = current.name
                            renaming = true
                        }
                        Button("Archive project", systemImage: "archivebox", role: .destructive) {
                            Task {
                                try? await app.client?.archiveProject(project.id)
                                await app.refresh()
                                dismiss()
                            }
                        }
                    }
                } label: {
                    Label("New", systemImage: "plus")
                }
            }
        }
        .overlay {
            if loading && sessions.isEmpty {
                ProgressView()
            } else if visible.isEmpty {
                ContentUnavailableView {
                    Label(showArchived ? "Nothing archived" : "No sessions", systemImage: "bubble.left.and.bubble.right")
                } description: {
                    Text(error ?? (showArchived ? "" : "Start one and talk to an agent on your machine."))
                } actions: {
                    if !showArchived {
                        Button("New session") { sheet = .newSession }.buttonStyle(.glassProminent)
                    }
                }
            }
        }
        .listStyle(.plain)
        .paletteBackground(app.isRegular ? \.side : \.bg)
        .refreshable { await load() }
        .task { await load() }
        .onChange(of: app.summaries.count) { _, _ in Task { await load() } }
        .alert("Rename project", isPresented: $renaming) {
            TextField("Name", text: $newName)
            Button("Rename") {
                Task {
                    _ = try? await app.client?.patchProject(project.id, ["name": newName])
                    await app.refresh()
                }
            }
            Button("Cancel", role: .cancel) {}
        }
        .sheet(item: $sheet) { s in
            switch s {
            case .newSession:
                NewSessionSheet(project: project) { created in
                    sheet = nil
                    sessions.insert(created, at: 0)
                    openCreated(created)
                }
                .presentationDetents([.medium, .large])
            case .newRoom:
                NewRoomSheet(project: project) { room in
                    sheet = nil
                    sessions.insert(room, at: 0)
                    openCreated(room)
                }
            case .workspaces:
                NavigationStack { WorkspacesView(project: project) }
            }
        }
    }

    private func openCreated(_ s: SessionInfo) {
        if app.isRegular {
            app.selectedSessionID = s.id
            app.detailPath = []
        } else {
            app.path.append(.session(id: s.id, title: s.title))
        }
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

enum ProjectSheet: String, Identifiable {
    case newSession, newRoom, workspaces
    var id: String { rawValue }
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
                    if session.system {
                        Image(systemName: "desktopcomputer").font(.caption).foregroundStyle(.secondary)
                    }
                    Text(summary?.title ?? session.title)
                        .font(unread > 0 ? .body.weight(.semibold) : .body)
                        .lineLimit(1)
                    if session.favorite_at != nil {
                        Image(systemName: "star.fill").font(.caption2).foregroundStyle(.yellow)
                    }
                }
                HStack(spacing: 6) {
                    Text(session.paused_at != nil ? "Paused" : state.label).lineLimit(1).fixedSize()
                    if let n = summary?.subagents, n > 0 {
                        Text("· \(n) subagent\(n == 1 ? "" : "s")")
                    }
                    if state.isBusy, let r = summary?.output_per_s, r > 0 {
                        Text("· \(Int(r)) tok/s").monospacedDigit()
                    }
                    if let date = HubEvent.parseDate(session.updated_at) {
                        Text("· \(date, format: .relative(presentation: .named))")
                    }
                }
                .lineLimit(1)
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

struct NewProjectSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var description = ""
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                TextField("Name", text: $name)
                TextField("Description (optional)", text: $description, axis: .vertical)
                if let error { Text(error).foregroundStyle(.red).font(.footnote) }
            }
            .navigationTitle("New project")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create", systemImage: "checkmark") {
                        Task {
                            do {
                                let p = try await app.client?.createProject(name: name, description: description)
                                await app.refresh()
                                dismiss()
                                if let p { app.path = [.project(p)] }
                            } catch {
                                self.error = error.localizedDescription
                            }
                        }
                    }
                    .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
                }
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
                    WorkspacePicker(workspaces: workspaces, selection: $workspaceID, project: project)
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
            .task { await loadWorkspaces() }
        }
    }

    private func loadWorkspaces() async {
        workspaces = (try? await app.client?.workspaces(projectID: project.id)) ?? []
        workspaceID = workspaceID ?? project.default_workspace_id ?? workspaces.first?.id
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

/// The project's workspaces to pick from, or a way to add the first one.
struct WorkspacePicker: View {
    let workspaces: [Workspace]
    @Binding var selection: String?
    let project: Project

    var body: some View {
        if workspaces.isEmpty {
            NavigationLink {
                WorkspacesView(project: project)
            } label: {
                Label("Add a workspace first", systemImage: "folder.badge.plus")
            }
        } else {
            Picker("Workspace", selection: $selection) {
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
}
