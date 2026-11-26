import SwiftUI

/// The projects, the personal one first, then favorites, then by name.
struct ProjectsView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.denseUI) private var dense
    @State private var creating = false
    @State private var editMode: EditMode = .inactive
    /// Every project's live sessions instead of the projects (the title's menu).
    @AppStorage("homeShowsLive") private var showsLive = false

    private var sorted: [Project] { app.sortedProjects }

    var body: some View {
        List {
            if !app.projectsLoaded && app.projects.isEmpty {
                ForEach(0..<4, id: \.self) { i in
                    SkeletonRow(icon: dense ? 22 : 44, widths: [0.45 - Double(i % 2) * 0.12, 0.7])
                }
            }
            ForEach(sorted) { project in
                NavigationLink(value: Route.project(project)) {
                    ProjectRow(project: project, unread: unread(project))
                }
                .listRowInsets(dense ? EdgeInsets(top: 4, leading: 12, bottom: 4, trailing: 12) : nil)
                .listRowBackground(dense ? Color.clear : nil)
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
            .onMove { app.moveProjects(from: $0, to: $1) }
        }
        .denseListStyle(dense)
        .environment(\.defaultMinListRowHeight, dense ? 32 : 44)
        .overlay {
            if showsLive {
                LiveSessionsList().paletteBackground()
            }
        }
        .navigationTitle(showsLive ? "Live sessions" : "Projects")
        .navigationBarTitleDisplayMode(dense ? .inline : .automatic)
        .toolbarTitleMenu {
            Picker("Show", selection: $showsLive) {
                Label("Projects", systemImage: "square.grid.2x2").tag(false)
                Label("Live sessions", systemImage: "bolt.horizontal").tag(true)
            }
        }
        .paletteBackground()
        .environment(\.editMode, $editMode)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Button("New project", systemImage: "plus") { creating = true }
                    Button(editMode.isEditing ? "Done reordering" : "Reorder projects", systemImage: "line.3.horizontal") {
                        withAnimation { editMode = editMode.isEditing ? .inactive : .active }
                    }
                    NavigationLink {
                        ArchivedProjectsView()
                    } label: {
                        Label("Archived projects", systemImage: "archivebox")
                    }
                } label: {
                    Label("Projects", systemImage: "plus")
                }
            }
        }
        .refreshable { await app.refresh() }
        .overlay {
            if app.projects.isEmpty && app.projectsLoaded {
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

private extension View {
    @ViewBuilder func denseListStyle(_ dense: Bool) -> some View {
        if dense {
            self.listStyle(.plain)
        } else {
            self.listStyle(.insetGrouped)
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
            .overlay {
                if project.icon?.kind == "image", let url = project.icon?.url {
                    ProjectIconImage(url: url)
                        .clipShape(.rect(cornerRadius: size * 0.28))
                }
            }
    }
}

struct ProjectRow: View {
    @Environment(AppModel.self) private var app
    @Environment(\.palette) private var palette
    @Environment(\.denseUI) private var dense
    let project: Project
    var unread = 0

    var body: some View {
        if dense {
            HStack(spacing: 9) {
                ProjectBadge(project: project, size: 22)
                Text(project.displayName)
                    .font(.system(size: 14.5, weight: unread > 0 ? .semibold : .regular))
                    .lineLimit(1)
                if project.favorite_at != nil {
                    Image(systemName: "star.fill").font(.system(size: 9)).foregroundStyle(.yellow)
                }
                Spacer(minLength: 4)
                if app.projectWorking(project.id) {
                    Text("\(Int(app.projectRate(project.id))) tok/s")
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(palette.text2)
                        .contentTransition(.numericText())
                }
                if unread > 0 {
                    Text("\(unread)")
                        .font(.system(size: 10.5, weight: .bold).monospacedDigit())
                        .padding(.horizontal, 6)
                        .padding(.vertical, 1)
                        .background(.tint, in: .capsule)
                        .foregroundStyle(.white)
                }
            }
        } else {
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
}

/// The bubble floating at the top while the app is not live with its hub.
struct ConnectionBanner: View {
    /// Never live since launch: "Connecting…"; live before and lost: "Offline, reconnecting…".
    let connecting: Bool

    var body: some View {
        HStack(spacing: 8) {
            ProgressView().controlSize(.small)
            Text(connecting ? "Connecting…" : "Offline, reconnecting…")
                .font(.footnote.weight(.medium))
                .contentTransition(.opacity)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .glassEffect(.regular.tint(.orange.opacity(0.25)), in: .capsule)
    }
}

/// One project's sessions and rooms, favorites first, then the newest; archived ones on demand.
struct SessionListView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @Environment(\.denseUI) private var dense
    let project: Project
    @State private var sessions: [SessionInfo] = []
    @State private var loading = true
    @State private var error: String?
    @State private var sheet: ProjectSheet?
    @State private var showArchived = false
    @State private var renaming = false
    @State private var newName = ""
    /// The list's order, kept on the hub per project (the web client's State, Recent, Custom).
    @State private var mode: SessionListMode = .state
    @State private var placed: [String] = []
    @State private var editMode: EditMode = .inactive

    private var current: Project { app.project(project.id) ?? project }

    private var visible: [SessionInfo] {
        sessions.filter { $0.archived == showArchived && $0.room_id == nil }
    }

    private var groups: [SessionGroup] {
        if showArchived {
            return [SessionGroup(id: "archived", label: nil,
                                 items: visible.sorted { ($0.archived_at ?? "") > ($1.archived_at ?? "") })]
        }
        return SessionGrouping.groups(visible, mode: mode, placed: placed) { app.summary($0) }
    }

    var body: some View {
        List {
            ForEach(groups) { g in
                Section {
                    ForEach(g.items) { s in
                        row(s)
                            .listRowInsets(dense ? EdgeInsets(top: 3, leading: 12, bottom: 3, trailing: 12) : nil)
                    }
                    .onMove(perform: mode == .custom && !showArchived ? { from, to in reorder(g.items, from, to) } : nil)
                } header: {
                    if let label = g.label {
                        Text(label)
                            .font(dense ? .system(size: 11, weight: .semibold) : .footnote.weight(.semibold))
                            .foregroundStyle(g.id == "wait" ? .orange : .secondary)
                    }
                }
            }
        }
        .navigationTitle(showArchived ? "Archived" : current.displayName)
        .navigationBarTitleDisplayMode(dense ? .inline : .automatic)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Button("New session", systemImage: "square.and.pencil") { sheet = .newSession }
                    Button("New room", systemImage: "person.3") { sheet = .newRoom }
                    Divider()
                    if !showArchived {
                        Picker(selection: Binding(get: { mode }, set: { setMode($0) })) {
                            ForEach(SessionListMode.allCases) { m in Text(m.label).tag(m) }
                        } label: {
                            Label("Order: \(mode.label)", systemImage: "arrow.up.arrow.down")
                        }
                        .pickerStyle(.menu)
                        if mode == .custom {
                            Button(editMode.isEditing ? "Done reordering" : "Reorder…", systemImage: "line.3.horizontal") {
                                withAnimation { editMode = editMode.isEditing ? .inactive : .active }
                            }
                        }
                    }
                    Button("Project settings…", systemImage: "slider.horizontal.3") { sheet = .settings }
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
                SkeletonList(rows: 5, icon: 10)
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
        .listSectionSpacing(dense ? 2 : 12)
        .environment(\.defaultMinListRowHeight, dense ? 30 : 44)
        .paletteBackground(app.isRegular ? \.side : \.bg)
        .environment(\.editMode, $editMode)
        .refreshable { await load() }
        .task {
            await load()
            if let o = try? await app.client?.sessionOrder(projectID: project.id) {
                mode = SessionListMode(rawValue: o.mode) ?? .state
                placed = o.session_ids
            }
        }
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
            case .settings:
                ProjectSettingsSheet(project: current)
            }
        }
    }

    /// Saves the Custom order after a drag.
    private func reorder(_ items: [SessionInfo], _ from: IndexSet, _ to: Int) {
        var ids = items.map(\.id)
        ids.move(fromOffsets: from, toOffset: to)
        placed = ids
        Feedback.play(.select)
        Task { _ = try? await app.client?.putSessionOrder(projectID: project.id, ids: ids) }
    }

    fileprivate func setMode(_ m: SessionListMode) {
        mode = m
        Feedback.play(.select)
        Task { _ = try? await app.client?.putSessionOrder(projectID: project.id, mode: m.rawValue) }
    }

    @ViewBuilder fileprivate func row(_ s: SessionInfo) -> some View {
                Group {
                    if app.isRegular {
                        Button {
                            if app.selectedSessionID != s.id { Feedback.play(.select) }
                            app.selectedSessionID = s.id
                            app.detailPath = []
                        } label: {
                            SessionRow(session: s, summary: app.summary(s.id), waiting: !app.requests(for: s.id).isEmpty)
                                .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                        .listRowBackground(
                            RoundedRectangle(cornerRadius: 10)
                                .fill(app.selectedSessionID == s.id ? current.tint.opacity(0.22) : .clear)
                                .padding(.horizontal, 6)
                        )
                    } else {
                        NavigationLink(value: Route.session(id: s.id, title: s.title)) {
                            SessionRow(session: s, summary: app.summary(s.id), waiting: !app.requests(for: s.id).isEmpty)
                        }
                        .listRowBackground(Color.clear)
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
    case newSession, newRoom, workspaces, settings
    var id: String { rawValue }
}

struct SessionRow: View {
    @Environment(\.denseUI) private var dense
    let session: SessionInfo
    let summary: SessionSummary?
    let waiting: Bool

    private var state: SessionState { waiting ? .waiting : (summary?.state ?? session.state) }
    private var unread: Int { summary?.unread ?? session.unread }

    var body: some View {
        if dense {
            // Single-line compact row matching the web client's `list-compact` (30 px, state in the dot).
            HStack(spacing: 8) {
                StateDot(state: state, size: 7)
                if session.isRoom {
                    Image(systemName: "person.3.fill").font(.system(size: 10)).foregroundStyle(.secondary)
                }
                if session.system {
                    Image(systemName: "desktopcomputer").font(.system(size: 10)).foregroundStyle(.secondary)
                }
                Text(summary?.title ?? session.title)
                    .font(.system(size: 14, weight: unread > 0 ? .semibold : .regular))
                    .lineLimit(1)
                if session.favorite_at != nil {
                    Image(systemName: "star.fill").font(.system(size: 9)).foregroundStyle(.yellow)
                }
                Spacer(minLength: 4)
                if state.isBusy, let r = summary?.output_per_s, r > 0 {
                    Text("\(Int(r)) tok/s")
                        .font(.system(size: 11).monospacedDigit())
                        .foregroundStyle(.secondary)
                } else if let n = summary?.subagents, n > 0 {
                    Text("\(n) sub")
                        .font(.system(size: 11).monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                if unread > 0 {
                    Text("\(unread)")
                        .font(.system(size: 10.5, weight: .bold).monospacedDigit())
                        .padding(.horizontal, 6)
                        .padding(.vertical, 1)
                        .background(.tint, in: .capsule)
                        .foregroundStyle(.white)
                }
            }
        } else {
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
                        Text(session.paused_at != nil ? "Paused"
                             : state == .locked_vscode ? (session.held_by.map { "Open in \($0.holder)" } ?? state.label)
                             : state.label).lineLimit(1).fixedSize()
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
}

struct StateDot: View {
    let state: SessionState
    var size: CGFloat = 9

    var body: some View {
        Circle()
            .fill(state.color)
            .frame(width: size, height: size)
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
    /// A Claude model ("" for Claude Code's default) or a provider, as the web's model chip.
    @State private var model = ""
    @State private var providerID: String?
    @State private var providers: [ModelProvider] = []
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
                Section("Model") {
                    Picker("Model", selection: Binding(
                        get: { providerID.map { "provider:\($0)" } ?? model },
                        set: { v in
                            if v.hasPrefix("provider:") {
                                providerID = String(v.dropFirst(9))
                                model = ""
                            } else {
                                providerID = nil
                                model = v
                            }
                        })) {
                        Text("Default").tag("")
                        Text("Opus").tag("opus")
                        Text("Sonnet").tag("sonnet")
                        Text("Haiku").tag("haiku")
                        Text("Fable").tag("fable")
                        ForEach(ModelSheet.versions, id: \.self) { Text($0).tag($0) }
                        ForEach(providers) { p in Text(p.label.isEmpty ? p.name : p.label).tag("provider:\(p.id)") }
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
                await loadWorkspaces()
                providers = (try? await app.client?.providers()) ?? []
            }
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
                projectID: project.id, title: title, workspaceID: workspaceID, permissionMode: mode,
                model: model, providerID: providerID)
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
