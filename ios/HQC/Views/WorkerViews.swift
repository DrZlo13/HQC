import SwiftUI

/// The user's workers (SPEC §5.1, §6): online state, Claude Code's sign-in, revoking.
struct WorkersView: View {
    @Environment(AppModel.self) private var app
    @State private var workers: [WorkerInfo] = []
    @State private var error: String?
    @State private var revoking: WorkerInfo?
    @State private var enrolling = false

    var body: some View {
        List {
            Section {
                ForEach(workers) { w in
                    NavigationLink {
                        WorkerDetailView(worker: w)
                    } label: {
                        HStack(spacing: 12) {
                            Image(systemName: "desktopcomputer")
                                .foregroundStyle(w.isOnline ? Color.accentColor : .secondary)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(w.name).font(.body.weight(.medium))
                                Text([w.os, w.isRevoked ? "Revoked" : (w.isOnline ? "Online" : "Offline")]
                                    .compactMap { $0 }.joined(separator: " · "))
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                    .swipeActions {
                        if !w.isRevoked {
                            Button("Revoke", systemImage: "xmark.shield", role: .destructive) { revoking = w }
                        }
                    }
                }
            } footer: {
                Text("Add a machine with `hqc-worker enroll \(app.hubURL?.absoluteString ?? "<hub>")` on it, then confirm its code here.")
            }
            Section {
                Button("Confirm a worker code", systemImage: "number") { enrolling = true }
            }
        }
        .navigationTitle("Workers")
        .paletteBackground()
        .task { await load() }
        .refreshable { await load() }
        .overlay {
            if let error, workers.isEmpty { ContentUnavailableView("Couldn't load workers", systemImage: "exclamationmark.triangle", description: Text(error)) }
        }
        .confirmationDialog(
            "Revoke \(revoking?.name ?? "")? Its token stops working and its connection drops.",
            isPresented: Binding(get: { revoking != nil }, set: { if !$0 { revoking = nil } }),
            titleVisibility: .visible
        ) {
            Button("Revoke", role: .destructive) {
                guard let w = revoking else { return }
                Task {
                    try? await app.client?.revokeWorker(w.id)
                    await load()
                }
            }
        }
        .sheet(isPresented: $enrolling, onDismiss: { Task { await load() } }) {
            NavigationStack { EnrollView(initialCode: "") }
        }
    }

    private func load() async {
        do { workers = try await app.client?.workers() ?? [] } catch { self.error = error.localizedDescription }
    }
}

/// One worker: Claude Code's sign-in there, from the phone (SPEC 0.185).
struct WorkerDetailView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.openURL) private var openURL
    let worker: WorkerInfo
    @State private var status: ClaudeAuthStatus?
    @State private var login: ClaudeLogin?
    @State private var code = ""
    @State private var message: String?
    @State private var busy = false

    var body: some View {
        Form {
            Section("Machine") {
                LabeledContent("Name", value: worker.name)
                if let os = worker.os { LabeledContent("System", value: os) }
                LabeledContent("Status", value: worker.isRevoked ? "Revoked" : (worker.isOnline ? "Online" : "Offline"))
                if !worker.isOnline, let seen = worker.last_seen.flatMap(HubEvent.parseDate) {
                    LabeledContent("Last seen") { Text(seen, format: .relative(presentation: .named)) }
                }
            }
            if worker.isOnline {
                Section {
                    if let status {
                        LabeledContent("Claude Code", value: status.logged_in ? "Signed in" : "Signed out")
                        if let e = status.email { LabeledContent("Account", value: e) }
                        if let p = status.subscription_type { LabeledContent("Plan", value: p) }
                    } else {
                        ProgressView()
                    }
                    if let login {
                        Button("Open the sign-in page", systemImage: "safari") {
                            if let u = URL(string: login.url) { openURL(u) }
                        }
                        TextField("Code from the page", text: $code)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                        Button("Send code") { Task { await sendCode(login) } }
                            .disabled(code.trimmingCharacters(in: .whitespaces).isEmpty || busy)
                    } else {
                        Button(status?.logged_in == true ? "Sign in again" : "Sign in") {
                            Task { await startLogin() }
                        }
                        .disabled(busy)
                    }
                    if let message { Text(message).font(.footnote).foregroundStyle(.secondary) }
                } header: {
                    Text("Claude Code on this machine")
                } footer: {
                    Text("The worker runs `claude auth login` itself; the tokens stay on the machine.")
                }
            }
        }
        .navigationTitle(worker.name)
        .task { if worker.isOnline { status = try? await app.client?.claudeStatus(workerID: worker.id) } }
    }

    private func startLogin() async {
        busy = true
        defer { busy = false }
        do {
            let l = try await app.client?.claudeLogin(workerID: worker.id)
            login = l
            if let u = l.flatMap({ URL(string: $0.url) }) { openURL(u) }
        } catch {
            message = error.localizedDescription
        }
    }

    private func sendCode(_ l: ClaudeLogin) async {
        busy = true
        defer { busy = false }
        do {
            let done = try await app.client?.claudeLoginCode(workerID: worker.id, loginID: l.login_id, code: code)
            message = done?.ok == true ? "Signed in." : (done?.message ?? "Not signed in.")
            login = nil
            code = ""
            status = try? await app.client?.claudeStatus(workerID: worker.id)
        } catch {
            message = error.localizedDescription
        }
    }
}

/// Confirming a machine's enrollment code (SPEC §5.1): what is asking, and whether it replaces
/// one of the user's workers.
struct EnrollView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let initialCode: String
    @State private var code = ""
    @State private var info: EnrollInfo?
    @State private var replace: String?
    @State private var error: String?
    @State private var done: EnrollConfirmed?

    var body: some View {
        Form {
            if let done {
                Section {
                    Label("Enrolled \(done.name)", systemImage: "checkmark.seal.fill").foregroundStyle(.green)
                    if let r = done.replaced { Text("Replaced \(r).").font(.footnote) }
                }
            } else {
                Section {
                    TextField("XXXX-XXXX", text: $code)
                        .font(.title3.monospaced())
                        .textInputAutocapitalization(.characters)
                        .autocorrectionDisabled()
                        .onSubmit { Task { await lookUp() } }
                    Button("Look up") { Task { await lookUp() } }
                        .disabled(code.trimmingCharacters(in: .whitespaces).count < 8)
                } footer: {
                    Text("The code `hqc-worker enroll` printed on the machine.")
                }
                if let info {
                    Section("This machine asks to join") {
                        LabeledContent("Name", value: info.name)
                        LabeledContent("System", value: info.os)
                        if let c = info.country { LabeledContent("Country", value: c) }
                        if let d = HubEvent.parseDate(info.created_at) {
                            LabeledContent("Asked") { Text(d, format: .relative(presentation: .named)) }
                        }
                    }
                    if let r = info.replaceable, !r.isEmpty {
                        Section {
                            Picker("Worker", selection: $replace) {
                                Text("A new worker").tag(String?.none)
                                ForEach(r) { w in
                                    Text("The same machine as \(w.name) (\(w.sessions_count) sessions)").tag(Optional(w.id))
                                }
                            }
                            .pickerStyle(.inline)
                            .labelsHidden()
                        } header: {
                            Text("Is it one of these?")
                        }
                    }
                    Section {
                        Button(replace == nil ? "Confirm" : "Confirm replacement") { Task { await confirm() } }
                            .buttonStyle(.glassProminent)
                            .frame(maxWidth: .infinity)
                    } footer: {
                        Text("Confirm only a machine you are enrolling yourself right now.")
                    }
                }
            }
            if let error { Section { Text(error).foregroundStyle(.red).font(.footnote) } }
        }
        .navigationTitle("Confirm a worker")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Close", systemImage: "xmark") { dismiss() }
            }
        }
        .task {
            if !initialCode.isEmpty {
                code = initialCode
                await lookUp()
            }
        }
    }

    private func lookUp() async {
        do {
            info = try await app.client?.enrollInfo(code: code.trimmingCharacters(in: .whitespaces))
            replace = info?.replaceable?.first(where: { $0.same_machine == true })?.id
            error = nil
        } catch {
            info = nil
            self.error = error.localizedDescription
        }
    }

    private func confirm() async {
        do {
            done = try await app.client?.enrollConfirm(code: code.trimmingCharacters(in: .whitespaces), replace: replace)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }
}

/// A project's workspaces (SPEC §3 Workspace): folders on workers its sessions run in.
struct WorkspacesView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let project: Project
    @State private var workspaces: [Workspace] = []
    @State private var workers: [WorkerInfo] = []
    @State private var error: String?
    @State private var browsing: WorkerInfo?

    var body: some View {
        List {
            Section {
                ForEach(workspaces) { w in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(w.displayName).font(.body.weight(.medium))
                        Text(w.path).font(.caption.monospaced()).foregroundStyle(.secondary)
                        if let worker = workers.first(where: { $0.id == w.worker_id }) {
                            Text(worker.name).font(.caption2).foregroundStyle(.tertiary)
                        }
                    }
                    .swipeActions {
                        Button("Remove", systemImage: "trash", role: .destructive) {
                            Task {
                                do {
                                    try await app.client?.deleteWorkspace(w.id)
                                    await load()
                                } catch {
                                    self.error = error.localizedDescription
                                }
                            }
                        }
                    }
                }
            } footer: {
                Text("Removing a workspace leaves the folder on the machine.")
            }
            Section("Add a folder") {
                ForEach(workers.filter(\.isOnline)) { w in
                    Button {
                        browsing = w
                    } label: {
                        Label(w.name, systemImage: "desktopcomputer")
                    }
                }
                if !workers.contains(where: \.isOnline) {
                    Text("No worker is online.").foregroundStyle(.secondary)
                }
            }
            if let error { Section { Text(error).foregroundStyle(.red).font(.footnote) } }
        }
        .navigationTitle("Workspaces")
        .paletteBackground()
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done", systemImage: "checkmark") { dismiss() }
            }
        }
        .task { await load() }
        .sheet(item: $browsing) { w in
            NavigationStack {
                FolderBrowser(worker: w, path: nil) { path in
                    browsing = nil
                    Task { await add(worker: w, path: path) }
                }
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel", systemImage: "xmark") { browsing = nil }
                    }
                }
            }
        }
    }

    private func load() async {
        guard let client = app.client else { return }
        workspaces = (try? await client.workspaces(projectID: project.id)) ?? []
        workers = ((try? await client.workers()) ?? []).filter { !$0.isRevoked }
    }

    private func add(worker: WorkerInfo, path: String) async {
        do {
            _ = try await app.client?.createWorkspace(projectID: project.id, workerID: worker.id, path: path, label: nil)
            error = nil
            await load()
        } catch {
            self.error = error.localizedDescription
        }
    }
}

/// The folders of a worker inside its `allowed_roots`, one level at a time.
struct FolderBrowser: View {
    @Environment(AppModel.self) private var app
    let worker: WorkerInfo
    let path: String?
    let onPick: (String) -> Void
    @State private var entries: [DirEntry] = []
    @State private var loading = true
    @State private var error: String?

    var body: some View {
        List {
            if let path {
                Section {
                    Button {
                        onPick(path)
                    } label: {
                        Label("Use this folder", systemImage: "checkmark.circle.fill")
                    }
                } footer: {
                    Text(path).font(.caption.monospaced())
                }
            }
            Section(path == nil ? "Allowed roots" : "Folders") {
                ForEach(entries) { e in
                    NavigationLink {
                        FolderBrowser(worker: worker, path: e.path, onPick: onPick)
                    } label: {
                        Label(e.name, systemImage: "folder")
                    }
                }
            }
        }
        .navigationTitle(path.map { ($0 as NSString).lastPathComponent } ?? worker.name)
        .navigationBarTitleDisplayMode(.inline)
        .overlay {
            if loading { ProgressView() } else if let error, entries.isEmpty {
                ContentUnavailableView("Couldn't list folders", systemImage: "folder.badge.questionmark", description: Text(error))
            }
        }
        .task {
            do { entries = try await app.client?.dirs(workerID: worker.id, path: path) ?? [] } catch {
                self.error = error.localizedDescription
            }
            loading = false
        }
    }
}
