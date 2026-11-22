import SwiftUI

/// Every open permission request and question of the user, across sessions (SPEC §6, T37):
/// answer them here without opening each chat.
struct RequestsView: View {
    @Environment(AppModel.self) private var app
    @State private var error: String?

    var body: some View {
        ScrollView {
            LazyVStack(spacing: 16) {
                ForEach(app.requests) { request in
                    VStack(alignment: .leading, spacing: 8) {
                        NavigationLink {
                            ChatView(sessionID: request.session_id, title: request.title)
                        } label: {
                            HStack(spacing: 6) {
                                if let p = app.project(forAddress: request.address) {
                                    ProjectTile(project: p, size: 20)
                                }
                                Text(request.title).font(.subheadline.weight(.semibold)).lineLimit(1)
                                if let w = request.worker_name {
                                    Text("· \(w)").font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer()
                                Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
                            }
                        }
                        .buttonStyle(.plain)

                        if request.event.type == "approval.request" {
                            ApprovalCard(event: request.event, open: true) { option in
                                do { try await app.answer(request, option: option) } catch {
                                    self.error = error.localizedDescription
                                }
                            }
                        } else {
                            ClarifyCard(event: request.event, open: true) { answers in
                                do {
                                    try await app.client?.clarify(
                                        sessionID: request.session_id, requestID: request.request_id, answers: answers)
                                    app.requests.removeAll { $0.id == request.id }
                                } catch {
                                    self.error = error.localizedDescription
                                }
                            }
                        }
                    }
                    .transition(.asymmetric(insertion: .opacity, removal: .scale(scale: 0.9).combined(with: .opacity)))
                }
            }
            .padding(16)
            .animation(.spring, value: app.requests.map(\.id))
        }
        .navigationTitle("Requests")
        .refreshable { await app.refresh() }
        .overlay {
            if app.requests.isEmpty {
                ContentUnavailableView("All clear", systemImage: "checkmark.circle",
                                       description: Text("Permission requests and questions from your agents show up here."))
            }
        }
        .alert("Couldn't answer", isPresented: Binding(get: { error != nil }, set: { if !$0 { error = nil } })) {
            Button("OK") {}
        } message: {
            Text(error ?? "")
        }
    }
}

extension AppModel {
    /// A request's project by the session address `<project-slug>/<session-slug>`.
    func project(forAddress address: String) -> Project? {
        guard let slug = address.split(separator: "/").first else { return nil }
        return projects.first { $0.slug == slug }
    }
}

struct SettingsView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Form {
            Section("Account") {
                LabeledContent("Signed in as", value: app.me?.email ?? "—")
                LabeledContent("Hub", value: app.hubURL?.host() ?? "—")
                LabeledContent("Connection") {
                    HStack(spacing: 6) {
                        Circle()
                            .fill(app.socketState == .open ? .green : .orange)
                            .frame(width: 8, height: 8)
                        Text(app.socketState == .open ? "Live" : app.socketState == .connecting ? "Connecting" : "Offline")
                    }
                }
            }
            Section {
                if let url = app.hubURL {
                    Link(destination: url) {
                        Label("Open the web app", systemImage: "safari")
                    }
                }
            } footer: {
                Text("Workers, workspaces, rooms and schedules are managed in the web app.")
            }
            Section {
                Button("Sign out", role: .destructive) { app.signOut() }
                Button("Use another hub", role: .destructive) { app.forgetHub() }
            }
            Section {
                LabeledContent("Version", value: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "")
            }
        }
        .navigationTitle("Settings")
    }
}
