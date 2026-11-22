import SwiftUI

@main
struct HQCApp: App {
    @State private var app = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(app)
                .task {
                    if app.phase == .connecting { await app.connect() }
                }
                .onChange(of: scenePhase) { _, phase in
                    if phase == .active, app.phase == .ready { app.foreground() }
                }
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
        TabView {
            Tab("Projects", systemImage: "square.grid.2x2") {
                NavigationStack { ProjectsView() }
            }
            .badge(app.unreadCount)

            Tab("Requests", systemImage: "hand.raised") {
                NavigationStack { RequestsView() }
            }
            .badge(app.requests.count)

            Tab("Settings", systemImage: "gearshape") {
                NavigationStack { SettingsView() }
            }
        }
        .tabBarMinimizeBehavior(.onScrollDown)
    }
}
