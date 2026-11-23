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
                    // Asked once, in onboarding; after that only registered again.
                    if phase == .ready, app.onboarded { Task { await AppDelegate.registerIfAllowed() } }
                }
                .onOpenURL { url in app.handle(url: url) }
        }
    }
}

struct RootView: View {
    @Environment(AppModel.self) private var app
    /// The splash covers the launch until the hub answered (or the app knows it needs setup).
    @State private var splash = true

    var body: some View {
        ZStack {
            content
            if splash {
                SplashView(ready: app.phase != .connecting) {
                    splash = false
                    app.splashDone = true
                }
                .transition(.identity)
                .zIndex(1)
            }
        }
        .themed()
    }

    private var content: some View {
        Group {
            switch app.phase {
            case .setup:
                OnboardingView(start: app.hubURL == nil && !app.onboarded ? .welcome : .hub)
            case .connecting:
                ProgressView()
                    .controlSize(.large)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(BackdropGradient())
            case .signIn:
                OnboardingView(start: .signIn)
            case .ready:
                if app.onboarded {
                    AdaptiveRoot()
                } else {
                    OnboardingView(start: .notifications)
                }
            }
        }
        .animation(.smooth, value: app.phase)
    }
}

