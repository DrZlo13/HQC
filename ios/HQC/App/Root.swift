import SwiftUI

/// The app's frame, chosen by the space it has, never by the device: compact width (an iPhone,
/// iPhone Duo's outer display, a narrow Split View) gets tabs and a navigation stack; regular
/// width (iPhone Duo's inner display, an iPad) gets the web client's desktop layout, the project
/// rail and the session list beside the chat.
struct AdaptiveRoot: View {
    @Environment(AppModel.self) private var app
    @Environment(\.horizontalSizeClass) private var width

    var body: some View {
        Group {
            if width == .regular {
                DesktopLayout()
            } else {
                MainTabs()
            }
        }
        .onChange(of: width, initial: true) { _, w in app.isRegular = w == .regular }
        .sheet(item: Bindable(app).enrollCode) { c in
            NavigationStack { EnrollView(initialCode: c.code) }
        }
    }
}

struct MainTabs: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        @Bindable var app = app
        TabView(selection: $app.tab) {
            Tab("Projects", systemImage: "square.grid.2x2", value: MainTab.projects) {
                NavigationStack(path: $app.path) {
                    ProjectsView()
                        .navigationDestination(for: Route.self) { route in
                            switch route {
                            case .project(let p): SessionListView(project: p)
                            case .session(let id, let title): ChatView(sessionID: id, title: title)
                            }
                        }
                }
            }
            .badge(app.unreadCount)

            Tab("Requests", systemImage: "hand.raised", value: MainTab.requests) {
                NavigationStack { RequestsView() }
            }
            .badge(app.requests.count)

            Tab("Settings", systemImage: "gearshape", value: MainTab.settings) {
                NavigationStack { SettingsView() }
            }

            Tab(value: MainTab.search, role: .search) {
                NavigationStack { SearchView() }
            }
        }
        .tabBarMinimizeBehavior(.onScrollDown)
        .onChange(of: app.tab) { _, _ in Feedback.play(.select) }
    }
}

/// The desktop layout (the web client at ≥ 900 px): a rail of project tiles, the project's
/// sessions, and the chat. Half folded, the column edge moves onto the fold, so the list and
/// the chat each own one half and nothing sits in the curve.
struct DesktopLayout: View {
    @Environment(AppModel.self) private var app
    @Environment(\.palette) private var palette
    @State private var visibility: NavigationSplitViewVisibility = .all
    @State private var fold: CGFloat?
    @State private var sheet: DesktopSheet?
    /// What the column beside the rail shows: a project's sessions, search or requests. Search
    /// and requests live there, not in a sheet, so a result opens in the chat beside them and
    /// the list stays for the next one, as the web client's side panel does.
    @State private var pane: SidebarPane = .sessions

    var body: some View {
        @Bindable var app = app
        NavigationSplitView(columnVisibility: $visibility) {
            HStack(spacing: 0) {
                ProjectRail(sheet: $sheet, pane: $pane)
                Rectangle().fill(palette.border.opacity(0.6)).frame(width: 0.5)
                switch pane {
                case .search:
                    SearchView(autofocus: true)
                case .requests:
                    RequestsView()
                case .sessions:
                    if let project = app.selectedProject {
                        SessionListView(project: project)
                            .id(project.id)
                    } else {
                        ContentUnavailableView("No project", systemImage: "folder")
                    }
                }
            }
            .animation(.snappy(duration: 0.25), value: pane)
            // The rail's color runs to the screen's bottom edge, under the home indicator.
            .background(alignment: .leading) {
                palette.rail.frame(width: 64).ignoresSafeArea()
            }
            .navigationSplitViewColumnWidth(fold.map { max(300, $0) } ?? 380)
            .paletteBackground(\.side)
            .toolbar(removing: .sidebarToggle)
        } detail: {
            NavigationStack(path: $app.detailPath) {
                Group {
                    if let s = app.selectedSessionID {
                        ChatView(sessionID: s, title: app.summary(s)?.title ?? "")
                            .id(s)
                    } else {
                        ContentUnavailableView {
                            Label("Pick a session", systemImage: "bubble.left.and.bubble.right")
                        } description: {
                            Text("Or start one from the session list.")
                        }
                        .paletteBackground()
                    }
                }
                .navigationDestination(for: Route.self) { route in
                    switch route {
                    case .project(let p): SessionListView(project: p)
                    case .session(let id, let title): ChatView(sessionID: id, title: title)
                    }
                }
            }
        }
        .navigationSplitViewStyle(.balanced)
        .onGeometryChange(for: CGFloat?.self) { proxy in
            if #available(iOS 27.1, *) {
                return proxy.reservedRegions(kind: .division).first(where: \.isActive)?.frame.minX
            }
            return nil
        } action: { fold = $0 }
        .onAppear {
            if app.selectedProjectID == nil { app.selectedProjectID = app.projects.first?.id }
        }
        .onChange(of: app.projects) { _, list in
            if app.selectedProjectID == nil || !list.contains(where: { $0.id == app.selectedProjectID }) {
                app.selectedProjectID = list.first?.id
            }
        }
        .sheet(item: $sheet) { s in
            NavigationStack {
                switch s {
                case .settings: SettingsView()
                case .newProject: NewProjectSheet()
                }
            }
            .presentationSizing(.form)
        }
    }
}

enum SidebarPane: Hashable {
    case sessions, search, requests
}

enum DesktopSheet: String, Identifiable {
    case settings, newProject
    var id: String { rawValue }
}

/// The project bar: a tile per project with its unread badge and the run ring while it works,
/// search and requests above, new project and settings below.
struct ProjectRail: View {
    @Environment(AppModel.self) private var app
    @Environment(\.palette) private var palette
    @Binding var sheet: DesktopSheet?
    @Binding var pane: SidebarPane

    var body: some View {
        VStack(spacing: 10) {
            railButton("magnifyingglass", "Search", active: pane == .search) { toggle(.search) }
                .keyboardShortcut("f", modifiers: .command)
            railButton("hand.raised", "Requests", badge: app.requests.count, active: pane == .requests) {
                toggle(.requests)
            }
            Rectangle().fill(palette.border.opacity(0.6)).frame(width: 28, height: 0.5)
            ScrollView(showsIndicators: false) {
                VStack(spacing: 10) {
                    ForEach(app.sortedProjects) { p in
                        let selected = pane == .sessions && p.id == app.selectedProjectID
                        Button {
                            if !selected { Feedback.play(.select) }
                            app.selectedProjectID = p.id
                            pane = .sessions
                        } label: {
                            ProjectBadge(project: p, size: 40)
                                .overlay(alignment: .topTrailing) {
                                    let unread = app.unread(project: p.id)
                                    if unread > 0 {
                                        Circle().fill(Color(hex: 0x5B8DEF)).frame(width: 10, height: 10)
                                            .overlay(Circle().stroke(palette.rail, lineWidth: 2))
                                    }
                                }
                        }
                        .buttonStyle(.plain)
                        .overlay(alignment: .leading) {
                            Capsule().fill(palette.text)
                                .frame(width: 3, height: selected ? 24 : 0)
                                .offset(x: -9)
                                .animation(.snappy, value: selected)
                        }
                        .accessibilityLabel(p.displayName)
                        .accessibilityAddTraits(selected ? .isSelected : [])
                    }
                }
                .padding(.vertical, 4)
            }
            Spacer(minLength: 0)
            railButton("plus", "New project") { sheet = .newProject }
            railButton("gearshape", "Settings") { sheet = .settings }
        }
        .padding(.vertical, 10)
        .frame(width: 64)
        .background(palette.rail.ignoresSafeArea())
    }

    /// A second tap on Search or Requests goes back to the project's sessions.
    private func toggle(_ p: SidebarPane) {
        Feedback.play(.select)
        pane = pane == p ? .sessions : p
    }

    private func railButton(_ icon: String, _ label: String, badge: Int = 0, active: Bool = false,
                            action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 16, weight: .medium))
                .foregroundStyle(active ? palette.text : palette.text2)
                .frame(width: 40, height: 40)
                .background(active ? palette.text.opacity(0.12) : .clear, in: .rect(cornerRadius: 10))
                .contentShape(.rect)
                .overlay(alignment: .topTrailing) {
                    if badge > 0 {
                        Text("\(badge)")
                            .font(.caption2.bold())
                            .foregroundStyle(Color(hex: 0x1A1206))
                            .padding(.horizontal, 5)
                            .background(palette.waiting, in: .capsule)
                    }
                }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .accessibilityAddTraits(active ? .isSelected : [])
    }
}
