import SwiftUI

@main
struct HQCApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @State private var app = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(app)
                .task {
                    AppDelegate.model = app
                    if app.phase == .connecting { await app.connect() }
                }
                .onChange(of: scenePhase) { _, phase in
                    if phase == .active, app.phase == .ready { app.foreground() }
                }
                .onChange(of: app.phase) { _, phase in
                    if phase == .ready { Task { await AppDelegate.requestPush() } }
                }
                .onOpenURL { url in app.handle(url: url) }
        }
    }
}

struct RootView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Group {
            switch app.phase {
            case .setup:
                SetupView()
            case .connecting:
                ProgressView("Connecting…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(BackdropGradient())
            case .signIn:
                SignInView()
            case .ready:
                MainTabs()
            }
        }
        .animation(.smooth, value: app.phase)
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
        .sheet(item: $app.enrollCode) { c in
            NavigationStack { EnrollView(initialCode: c.code) }
        }
    }
}
