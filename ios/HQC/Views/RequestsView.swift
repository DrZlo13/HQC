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
                        RequestHeaderLink(request: request) {
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
                                    Feedback.play(.answered)
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
        .paletteBackground(app.isRegular ? \.side : \.bg)
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

/// The request's session: pushed in a phone's Requests tab, opened in the chat column on the
/// desktop layout, where the requests list stays beside it.
private struct RequestHeaderLink<Label: View>: View {
    @Environment(AppModel.self) private var app
    let request: OpenRequest
    @ViewBuilder let label: Label

    var body: some View {
        if app.isRegular {
            Button { app.open(sessionID: request.session_id, title: request.title) } label: { label.contentShape(.rect) }
        } else {
            NavigationLink { ChatView(sessionID: request.session_id, title: request.title) } label: { label }
        }
    }
}

struct SettingsView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.openURL) private var openURL
    @AppStorage(Feedback.hapticsKey) private var haptics = true
    @AppStorage(Feedback.soundsKey) private var sounds = true

    private var notificationState: String {
        if !app.pushEnabledOnHub { return "Off on the hub" }
        return app.deviceToken == nil ? "Not allowed" : "On"
    }

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
            Section("Appearance") {
                Toggle("Match system light and dark", isOn: Bindable(app).followSystem)
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 96), spacing: 10)], spacing: 10) {
                    ForEach(app.followSystem ? ThemeName.allCases.filter { !$0.isLight } : ThemeName.allCases) { t in
                        ThemeSwatch(theme: t, selected: app.followSystem ? app.theme.family == t.family : app.theme == t) {
                            app.setTheme(app.followSystem ? t.variant(light: app.theme.isLight) : t)
                        }
                    }
                }
                .padding(.vertical, 4)
            }
            Section {
                Toggle(isOn: $haptics) {
                    Label("Haptics", systemImage: "iphone.radiowaves.left.and.right")
                }
                .onChange(of: haptics) { _, on in if on { Feedback.play(.done) } }
                Toggle(isOn: $sounds) {
                    Label("Sounds", systemImage: "speaker.wave.2")
                }
                .onChange(of: sounds) { _, on in if on { Feedback.play(.done) } }
                if haptics || sounds {
                    Menu {
                        Button("Message sent") { Feedback.play(.send) }
                        Button("Tool call") { Feedback.play(.tool) }
                        Button("Tool failed") { Feedback.play(.toolFailed) }
                        Button("Permission request") { Feedback.play(.approval) }
                        Button("Question") { Feedback.play(.question) }
                        Button("Answer finished") { Feedback.play(.done) }
                        Button("Turn failed") { Feedback.play(.error) }
                    } label: {
                        Label("Try them", systemImage: "play.circle")
                    }
                }
            } header: {
                Text("Sound and haptics")
            } footer: {
                Text("What agents do, felt and heard while a chat is open: tool calls, requests, questions, a finished answer. Sounds follow the ring/silent switch and mix with your music.")
            }
            Section("Machines") {
                NavigationLink {
                    WorkersView()
                } label: {
                    Label("Workers", systemImage: "desktopcomputer")
                }
                Button {
                    app.enrollCode = EnrollCode(code: "")
                } label: {
                    Label("Confirm a worker code", systemImage: "number")
                }
            }
            Section {
                LabeledContent("Notifications") {
                    Text(notificationState)
                }
                Button("Notification settings", systemImage: "bell.badge") {
                    if let u = URL(string: UIApplication.openNotificationSettingsURLString) { openURL(u) }
                }
            } footer: {
                if !app.pushEnabledOnHub {
                    Text("The hub sends no Apple push yet: add HQC_APNS_RELAY_URL=https://hqc.drewnja.xyz to its settings and restart it (RUNBOOK §1.10).")
                }
            }
            Section {
                if let url = app.hubURL {
                    Link(destination: url) {
                        Label("Open the web app", systemImage: "safari")
                    }
                }
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
        .paletteBackground()
    }
}

/// A palette's tile: its background, surface and text, as the web client's Appearance shows them.
struct ThemeSwatch: View {
    let theme: ThemeName
    let selected: Bool
    let action: () -> Void

    var body: some View {
        let p = Palette.of(theme)
        Button(action: action) {
            VStack(alignment: .leading, spacing: 6) {
                RoundedRectangle(cornerRadius: 6).fill(p.surface).frame(height: 14)
                    .overlay(alignment: .leading) {
                        Capsule().fill(p.text).frame(width: 30, height: 4).padding(.leading, 6)
                    }
                Capsule().fill(p.muted).frame(width: 44, height: 4)
                Text(theme.label + (theme.isLight ? " light" : ""))
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(p.text)
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(p.bg, in: .rect(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12)
                .strokeBorder(selected ? Color.accentColor : p.border, lineWidth: selected ? 2.5 : 1))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(theme.label + (theme.isLight ? " light" : " dark"))
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}
