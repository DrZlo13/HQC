import SwiftUI

/// A new room (SPEC §7.1.1): its agents, each a session of its own with a name, a model and a
/// system prompt, and how they talk. The first message is written in the room's chat.
struct NewRoomSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let project: Project
    let onCreated: (SessionInfo) -> Void

    @State private var title = ""
    @State private var mode: RoomMode = .conversation
    @State private var agents = [RoomAgentDraft(name: "Agent 1"), RoomAgentDraft(name: "Agent 2")]
    @State private var workspaces: [Workspace] = []
    @State private var workspaceID: String?
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Title (optional)", text: $title)
                    Picker("Mode", selection: $mode) {
                        ForEach(RoomMode.allCases) { Text($0.label).tag($0) }
                    }
                    .pickerStyle(.segmented)
                } footer: {
                    Text(mode.explanation)
                }
                ForEach($agents) { $agent in
                    Section {
                        TextField("Name", text: $agent.name)
                        TextField("Model (default if empty)", text: $agent.model)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                        TextField("System prompt: who this agent is", text: $agent.prompt, axis: .vertical)
                            .lineLimit(2...6)
                    } header: {
                        HStack {
                            Text("Agent")
                            Spacer()
                            if agents.count > 1 {
                                Button("Remove", role: .destructive) {
                                    agents.removeAll { $0.id == agent.id }
                                }
                                .font(.caption)
                            }
                        }
                    }
                }
                Section {
                    Button("Add an agent", systemImage: "plus") {
                        agents.append(RoomAgentDraft(name: "Agent \(agents.count + 1)"))
                    }
                }
                Section("Folder the agents work in") {
                    WorkspacePicker(workspaces: workspaces, selection: $workspaceID, project: project)
                }
                if let error {
                    Section { Text(error).foregroundStyle(.red).font(.footnote) }
                }
            }
            .navigationTitle("New room")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", systemImage: "xmark") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create", systemImage: "checkmark") { Task { await create() } }
                        .disabled(busy || agents.isEmpty || workspaces.isEmpty)
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
            let room = try await client.createRoom(
                projectID: project.id, title: title, mode: mode, workspaceID: workspaceID, agents: agents)
            onCreated(room)
        } catch {
            self.error = error.localizedDescription
        }
    }
}

/// A room's agents, each opening its own session: what it ran, its tools and its answers.
struct RoomAgentsView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let room: SessionInfo
    @State private var agents: [SessionInfo] = []

    var body: some View {
        List {
            ForEach(agents) { a in
                Button {
                    dismiss()
                    app.path.append(.session(id: a.id, title: a.title))
                } label: {
                    SessionRow(session: a, summary: app.summary(a.id), waiting: !app.requests(for: a.id).isEmpty)
                }
                .foregroundStyle(.primary)
            }
        }
        .navigationTitle("Agents")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done", systemImage: "checkmark") { dismiss() }
            }
        }
        .task {
            let all = (try? await app.client?.sessions(projectID: room.project_id)) ?? []
            // The room's order is its agents' creation order; their ids are UUIDv7, so by id.
            agents = all.filter { $0.room_id == room.id }.sorted { $0.id < $1.id }
        }
        .overlay {
            if agents.isEmpty { ProgressView() }
        }
    }
}
