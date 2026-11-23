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
    /// The launch splash, until its short animation is done; it never waits for the hub.
    @State private var splash = true

    /// A set-up app opens its main screen at once, also while it connects: the lists show
    /// skeletons and a "Connecting…" bubble until the hub answers. One branch for both phases,
    /// so the main screen keeps its identity (and its navigation) when the connection lands.
    private var showsMain: Bool {
        app.onboarded && (app.phase == .ready || app.phase == .connecting)
    }

    var body: some View {
        ZStack {
            content
                // The counter-motion to the splash pushing through: the app rises to full size.
                .scaleEffect(splash ? 0.94 : 1)
            if splash {
                SplashView {
                    // Near-critically damped and unhurried, so the push-through registers.
                    withAnimation(.spring(response: 0.75, dampingFraction: 0.98)) { splash = false }
                    app.splashDone = true
                }
                .transition(.pushThrough)
                .zIndex(1)
            }
        }
        .themed()
    }

    private var content: some View {
        Group {
            if showsMain {
                AdaptiveRoot()
            } else {
                switch app.phase {
                case .setup, .connecting:
                    OnboardingView(start: app.hubURL == nil && !app.onboarded ? .welcome : .hub)
                case .signIn:
                    OnboardingView(start: .signIn)
                case .ready:
                    OnboardingView(start: .notifications)
                }
            }
        }
        .animation(.smooth, value: showsMain)
    }
}

