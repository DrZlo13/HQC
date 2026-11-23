import SwiftUI

// The rest of the web client's project and Settings screens (client/src/ui/ProjectSettings.tsx,
// Rules.tsx, secrets.tsx, Providers.tsx): a folder's settings, rules and ssh keys, deleting a
// folder hqc made, cloning, worktrees, importing Claude Code's folders, and model providers.

// MARK: a folder

/// A folder of a project: its label, VS Code sessions, the rules that decide the agent's tool
/// calls there (hqc's own, added and removed here, and the files the worker reads), its ssh
/// keys and its removal.
struct WorkspaceDetailView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @Environment(\.palette) private var palette
    let workspace: Workspace
    let onChange: () async -> Void

    @State private var label = ""
    @State private var mirror = true
    @State private var rules: [WorkspaceRule] = []
    @State private var permissions: WorkspacePermissions?
    @State private var offline = false
    @State private var keys: [WorkspaceKey] = []
    @State private var newRule = ""
    @State private var command = ""
    @State private var check: CheckResult?
    @State private var error: String?
    @State private var deleting = false

    var body: some View {
        Form {
            Section {
                TextField("Label", text: $label)
                    .onSubmit { patch(["label": label]) }
                // Saved only when the user flips it, not when it is loaded.
                Toggle("VS Code sessions", isOn: Binding(get: { mirror }, set: { on in
                    mirror = on
                    patch(["mirror_enabled": on])
                }))
            } footer: {
                Text("\(workspace.path)\(workspace.branch.map { " · \($0)" } ?? "")")
                    .font(.caption.monospaced())
            }

            Section {
                ForEach(rules) { r in
                    Text(r.rule).font(.callout.monospaced())
                }
                .onDelete { idx in
                    for i in idx { remove(rules[i]) }
                }
                HStack {
                    TextField("Bash(npm test:*)", text: $newRule)
                        .font(.callout.monospaced())
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .onSubmit(addRule)
                    Button("Add", systemImage: "plus.circle.fill", action: addRule)
                        .labelStyle(.iconOnly)
                        .disabled(newRule.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            } header: {
                Text("hqc's allow rules")
            } footer: {
                Text("Allowed without asking in this folder. Running sessions use them at their next request.")
            }

            Section {
                HStack {
                    TextField("A command to check, e.g. git push", text: $command)
                        .font(.callout.monospaced())
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .onSubmit { Task { await loadPermissions() } }
                    Button("Check") { Task { await loadPermissions() } }
                        .disabled(command.trimmingCharacters(in: .whitespaces).isEmpty)
                }
                if let check {
                    Label(verdictText(check), systemImage: check.verdict == "allow" ? "checkmark.circle.fill"
                          : check.verdict == "deny" ? "xmark.octagon.fill" : "hand.raised.fill")
                        .foregroundStyle(check.verdict == "allow" ? palette.online
                                         : check.verdict == "deny" ? palette.danger : palette.waiting)
                        .font(.footnote)
                }
                if offline {
                    Text("The worker is offline: only hqc's rules are known.").font(.footnote).foregroundStyle(palette.muted)
                }
                ForEach(permissions?.sources ?? [], id: \.source) { src in
                    DisclosureGroup {
                        if let e = src.error { Text(e).font(.caption).foregroundStyle(palette.danger) }
                        ruleList("Denied", src.deny, palette.danger)
                        ruleList("Allowed", src.allow, palette.online)
                        ruleList("Asks", src.ask, palette.waiting)
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text((src.source as NSString).lastPathComponent).font(.subheadline)
                            Text("\(src.deny.count) denied · \(src.allow.count) allowed · \(src.ask.count) ask")
                                .font(.caption).foregroundStyle(palette.muted)
                        }
                    }
                }
            } header: {
                Text("What decides the agent's tool calls")
            } footer: {
                Text("The worker's own files are read-only here: worker.toml and Claude Code's settings.")
            }

            if !keys.isEmpty {
                Section("SSH keys") {
                    ForEach(keys) { k in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(k.name).font(.callout.monospaced())
                            Text(k.fingerprint).font(.caption2.monospaced()).foregroundStyle(palette.muted)
                        }
                    }
                    .onDelete { idx in
                        for i in idx { forget(keys[i]) }
                    }
                }
            }

            if let error {
                Section { Text(error).foregroundStyle(palette.danger).font(.footnote) }
            }

            Section {
                Button("Remove folder from the project", systemImage: "trash", role: .destructive) { deleting = true }
            } footer: {
                Text(workspace.created_by_hqc == true
                     ? "hqc made this folder: removing it can delete the folder on the machine too."
                     : "The folder on the machine stays.")
            }
        }
        .navigationTitle(workspace.displayName)
        .navigationBarTitleDisplayMode(.inline)
        .paletteBackground()
        .task {
            label = workspace.label ?? ""
            mirror = workspace.mirror_enabled ?? true
            await load()
        }
        .sheet(isPresented: $deleting) {
            DeleteFolderSheet(workspace: workspace) {
                await onChange()
                dismiss()
            }
        }
    }

    private func ruleList(_ title: String, _ list: [String], _ color: Color) -> some View {
        Group {
            if !list.isEmpty {
                Text(title).font(.caption.weight(.semibold)).foregroundStyle(color)
                ForEach(list, id: \.self) { Text($0).font(.caption.monospaced()) }
            }
        }
    }

    private func verdictText(_ c: CheckResult) -> String {
        let v = c.verdict == "allow" ? "Allowed" : c.verdict == "deny" ? "Denied" : "Asks you"
        if let rule = c.rule { return "\(v) by \(rule)\(c.source.map { " (\(($0 as NSString).lastPathComponent))" } ?? "")" }
        return v
    }

    private func load() async {
        guard let client = app.client else { return }
        rules = (try? await client.workspaceRules(workspace.id)) ?? []
        keys = (try? await client.workspaceKeys(workspace.id)) ?? []
        await loadPermissions()
    }

    private func loadPermissions() async {
        guard let client = app.client else { return }
        do {
            let p = try await client.workspacePermissions(workspace.id, command: command.trimmingCharacters(in: .whitespaces))
            permissions = p
            check = p.check
            offline = false
        } catch HubError.http(let status, _, _) where status == 409 {
            offline = true
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func patch(_ p: [String: Any]) {
        Task {
            do { _ = try await app.client?.patchWorkspace(workspace.id, p) } catch { self.error = error.localizedDescription }
        }
    }

    private func addRule() {
        let r = newRule.trimmingCharacters(in: .whitespaces)
        guard !r.isEmpty else { return }
        Task {
            do {
                rules = try await app.client?.addWorkspaceRules(workspace.id, [r]) ?? rules
                newRule = ""
                error = nil
                Feedback.play(.success)
                await loadPermissions()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    private func remove(_ r: WorkspaceRule) {
        Task {
            do {
                try await app.client?.deleteWorkspaceRule(workspace.id, ruleID: r.id)
                rules.removeAll { $0.id == r.id }
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    private func forget(_ k: WorkspaceKey) {
        Task {
            do {
                try await app.client?.deleteWorkspaceKey(workspace.id, name: k.name)
                keys.removeAll { $0.name == k.name }
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// Removing a folder from its project; for a folder hqc made, what is uncommitted or unpushed
/// first, and whether to delete the folder too.
struct DeleteFolderSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @Environment(\.palette) private var palette
    let workspace: Workspace
    let onDone: () async -> Void
    @State private var inspection: WorkspaceInspection?
    @State private var removeFolder = false
    @State private var archiveSessions = true
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                if workspace.created_by_hqc == true {
                    Section {
                        Toggle("Delete the folder on the machine", isOn: $removeFolder)
                    } footer: {
                        if let i = inspection, removeFolder, i.dirty_count > 0 || !i.unpushed.isEmpty {
                            Text("\(i.dirty_count) uncommitted file\(i.dirty_count == 1 ? "" : "s")"
                                 + (i.unpushed.isEmpty ? "" : ", unpushed commits on " + i.unpushed.map { "\($0.branch) (\($0.commits))" }.joined(separator: ", "))
                                 + " will be lost.")
                                .foregroundStyle(palette.danger)
                        }
                    }
                }
                Section {
                    Toggle("Archive the sessions that work in it", isOn: $archiveSessions)
                }
                if let error { Section { Text(error).foregroundStyle(palette.danger).font(.footnote) } }
            }
            .navigationTitle("Remove \(workspace.displayName)")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", systemImage: "xmark") { dismiss() }
                }
                ToolbarItem(placement: .destructiveAction) {
                    Button("Remove", role: .destructive) { remove() }.disabled(busy)
                }
            }
            .task {
                if workspace.created_by_hqc == true {
                    inspection = try? await app.client?.inspectWorkspace(workspace.id)
                }
            }
        }
        .presentationDetents([.medium])
    }

    private func remove() {
        busy = true
        Task {
            do {
                let dirty = (inspection?.dirty_count ?? 0) > 0 || !(inspection?.unpushed.isEmpty ?? true)
                try await app.client?.deleteWorkspace(workspace.id, removeFolder: removeFolder,
                                                      force: removeFolder && dirty, archiveSessions: archiveSessions)
                dismiss()
                await onDone()
            } catch {
                self.error = error.localizedDescription
            }
            busy = false
        }
    }
}

// MARK: new folders

/// A folder from elsewhere: a clone of a repository, a git worktree of a folder of the
/// project, or a folder where Claude Code already has sessions.
struct NewFolderSheet: View {
    enum Kind: String, Identifiable { case clone, worktree, importClaude; var id: String { rawValue } }

    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let kind: Kind
    let project: Project
    let workers: [WorkerInfo]
    let workspaces: [Workspace]
    let onDone: () async -> Void

    @State private var workerID: String?
    @State private var url = ""
    @State private var parent: String?
    @State private var folderName = ""
    @State private var originID: String?
    @State private var branch = ""
    @State private var newBranch = true
    @State private var candidates: [Candidate] = []
    @State private var busy = false
    @State private var error: String?

    private var worker: WorkerInfo? { workers.first { $0.id == workerID } }

    var body: some View {
        NavigationStack {
            Form {
                switch kind {
                case .clone: cloneForm
                case .worktree: worktreeForm
                case .importClaude: importForm
                }
                if let error { Section { Text(error).foregroundStyle(.red).font(.footnote) } }
            }
            .navigationTitle(kind == .clone ? "Clone a repository" : kind == .worktree ? "New worktree" : "Import from Claude Code")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", systemImage: "xmark") { dismiss() }
                }
                if kind != .importClaude {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Create", systemImage: "checkmark") { create() }.disabled(!ready || busy)
                    }
                }
            }
            .task {
                workerID = workers.first(where: \.isOnline)?.id
                originID = workspaces.first?.id
            }
            .task(id: workerID) {
                guard kind == .importClaude, let workerID else { return }
                candidates = (try? await app.client?.candidates(workerID: workerID)) ?? []
            }
        }
    }

    private var workerPicker: some View {
        Picker("Machine", selection: $workerID) {
            ForEach(workers.filter(\.isOnline)) { w in Text(w.name).tag(String?.some(w.id)) }
        }
    }

    @ViewBuilder private var cloneForm: some View {
        Section("Repository") {
            TextField("https://github.com/owner/repo.git", text: $url)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .keyboardType(.URL)
                .onChange(of: url) { _, u in
                    if folderName.isEmpty || url.hasPrefix(folderName) == false {
                        let last = (u as NSString).lastPathComponent.replacingOccurrences(of: ".git", with: "")
                        folderName = last
                    }
                }
        }
        Section("Into") {
            workerPicker
            if let worker {
                NavigationLink {
                    FolderBrowser(worker: worker, path: nil) { parent = $0 }
                } label: {
                    LabeledContent("Parent folder", value: parent.map { ($0 as NSString).lastPathComponent } ?? "Choose…")
                }
            }
            TextField("Folder name", text: $folderName)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
        }
    }

    @ViewBuilder private var worktreeForm: some View {
        Section("Of") {
            Picker("Folder", selection: $originID) {
                ForEach(workspaces) { w in Text(w.displayName).tag(String?.some(w.id)) }
            }
        }
        Section {
            TextField("Branch", text: $branch)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Toggle("A new branch from the folder's HEAD", isOn: $newBranch)
        } footer: {
            Text("Made next to the folder as <folder>-<branch>.")
        }
    }

    @ViewBuilder private var importForm: some View {
        Section { workerPicker }
        Section {
            ForEach(candidates) { c in
                Button {
                    Task { await importFolder(c) }
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text((c.path as NSString).lastPathComponent).font(.body.weight(.medium))
                        Text(c.path).font(.caption.monospaced()).foregroundStyle(.secondary)
                        Text("\(c.sessions_count) session\(c.sessions_count == 1 ? "" : "s")")
                            .font(.caption2).foregroundStyle(.tertiary)
                    }
                }
                .foregroundStyle(.primary)
                .disabled(workspaces.contains { $0.path == c.path && $0.worker_id == workerID })
            }
            if candidates.isEmpty {
                Text("No folder with Claude Code sessions on this machine.").foregroundStyle(.secondary)
            }
        } header: {
            Text("Folders with Claude Code sessions")
        } footer: {
            Text("Its sessions show up in the project, and VS Code's new ones too.")
        }
    }

    private var ready: Bool {
        switch kind {
        case .clone: !url.isEmpty && parent != nil && !folderName.isEmpty && workerID != nil
        case .worktree: originID != nil && !branch.trimmingCharacters(in: .whitespaces).isEmpty
        case .importClaude: false
        }
    }

    private func create() {
        busy = true
        var body: [String: Any] = ["project_id": project.id]
        switch kind {
        case .clone:
            body["worker_id"] = workerID
            body["kind"] = "clone"
            body["url"] = url
            body["path"] = ((parent ?? "") as NSString).appendingPathComponent(folderName)
        case .worktree:
            let origin = workspaces.first { $0.id == originID }
            body["worker_id"] = origin?.worker_id
            body["kind"] = "worktree"
            body["origin_workspace_id"] = originID
            body["branch"] = branch.trimmingCharacters(in: .whitespaces)
            body["new_branch"] = newBranch
        case .importClaude:
            break
        }
        Task {
            do {
                _ = try await app.client?.createWorkspace(body)
                Feedback.play(.success)
                dismiss()
                await onDone()
            } catch {
                self.error = error.localizedDescription
            }
            busy = false
        }
    }

    private func importFolder(_ c: Candidate) async {
        guard let workerID else { return }
        do {
            _ = try await app.client?.createWorkspace(["project_id": project.id, "worker_id": workerID,
                                                       "path": c.path, "mirror_enabled": true])
            Feedback.play(.success)
            dismiss()
            await onDone()
        } catch {
            self.error = error.localizedDescription
        }
    }
}

// MARK: model providers

/// Settings → Models: the owner's model providers, an Anthropic-compatible endpoint with one
/// model each, which a session picks in its model sheet. A worker runs one only if its
/// worker.toml allows the name with the same base_url, and keeps the key itself.
struct ProvidersView: View {
    @Environment(AppModel.self) private var app
    @State private var providers: [ModelProvider] = []
    @State private var workers: [WorkerInfo] = []
    @State private var editing: ModelProvider?
    @State private var adding = false
    @State private var error: String?

    var body: some View {
        List {
            Section {
                ForEach(providers) { p in
                    Button {
                        editing = p
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(p.label.isEmpty ? p.name : p.label).font(.body.weight(.medium))
                            Text("\(p.model) · \(p.base_url)").font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1)
                            Text(allowedBy(p)).font(.caption2).foregroundStyle(.tertiary)
                        }
                    }
                    .foregroundStyle(.primary)
                }
                .onDelete { idx in
                    for i in idx { delete(providers[i]) }
                }
            } footer: {
                Text("A worker runs a provider only if its worker.toml allows the name with the same base_url, and keeps the key there.")
            }
            if let error { Section { Text(error).foregroundStyle(.red).font(.footnote) } }
        }
        .navigationTitle("Models")
        .paletteBackground()
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("Add", systemImage: "plus") { adding = true }
            }
        }
        .overlay {
            if providers.isEmpty {
                ContentUnavailableView("No providers", systemImage: "cpu",
                                       description: Text("Add an Anthropic-compatible endpoint to pick it as a session's model."))
            }
        }
        .task { await load() }
        .sheet(item: $editing) { p in ProviderEditor(provider: p) { await load() } }
        .sheet(isPresented: $adding) { ProviderEditor(provider: nil) { await load() } }
    }

    private func allowedBy(_ p: ModelProvider) -> String {
        let names = workers.filter { p.workers.contains($0.id) }.map(\.name)
        return names.isEmpty ? "No worker allows it yet" : "Allowed on " + names.joined(separator: ", ")
    }

    private func load() async {
        guard let client = app.client else { return }
        do {
            providers = try await client.providers()
            workers = (try? await client.workers()) ?? []
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func delete(_ p: ModelProvider) {
        Task {
            do {
                try await app.client?.deleteProvider(p.id)
                providers.removeAll { $0.id == p.id }
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

private struct ProviderEditor: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let provider: ModelProvider?
    let onSaved: () async -> Void
    @State private var name = ""
    @State private var label = ""
    @State private var baseURL = ""
    @State private var model = ""
    @State private var context = ""
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Name (as in worker.toml)", text: $name)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                    TextField("Label", text: $label)
                    TextField("Base URL", text: $baseURL)
                        .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
                    TextField("Model", text: $model)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                    TextField("Context tokens (optional)", text: $context).keyboardType(.numberPad)
                }
                if let error { Section { Text(error).foregroundStyle(.red).font(.footnote) } }
            }
            .navigationTitle(provider == nil ? "New model" : "Model")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", systemImage: "xmark") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", systemImage: "checkmark") { save() }
                        .disabled(name.isEmpty || baseURL.isEmpty || model.isEmpty)
                }
            }
            .onAppear {
                guard let p = provider else { return }
                name = p.name
                label = p.label
                baseURL = p.base_url
                model = p.model
                context = p.context_tokens.map(String.init) ?? ""
            }
        }
    }

    private func save() {
        var fields: [String: Any] = ["name": name, "label": label.isEmpty ? name : label, "base_url": baseURL, "model": model]
        if let n = Int(context) { fields["context_tokens"] = n }
        Task {
            do {
                _ = try await app.client?.saveProvider(id: provider?.id, fields)
                await onSaved()
                dismiss()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}
