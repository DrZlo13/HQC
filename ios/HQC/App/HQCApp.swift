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
                    if phase == .background, app.phase == .ready { app.background() }
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

    /// Decided once, before the first frame, from what is saved: a first launch has the
    /// welcome deck draw the launch moment itself (its logo flies up into place); any other
    /// launch has the splash breathe and vanish onto the app.
    @State private var deckLaunch = !UserDefaults.standard.bool(forKey: "onboarded")
        && UserDefaults.standard.string(forKey: "hubURL") == nil
    /// The splash is on screen (not on a deck launch).
    @State private var splashUp = !(!UserDefaults.standard.bool(forKey: "onboarded")
        && UserDefaults.standard.string(forKey: "hubURL") == nil)
    /// The launch moment is over: the splash vanishes, or the deck reveals itself.
    @State private var launchDone = false

    /// The main screen is mounted: connected, or connecting with a set-up app (it shows
    /// skeletons). It stays mounted under onboarding's last step, so finishing it pushes the
    /// deck through onto an app that is already built.
    private var mountsMain: Bool {
        app.phase == .ready || (app.onboarded && app.phase == .connecting)
    }
    private var showsOnboarding: Bool {
        !(app.onboarded && (app.phase == .ready || app.phase == .connecting))
    }
    private var onboardingStart: OnboardingView.Step {
        switch app.phase {
        case .signIn: .signIn
        case .ready: .notifications
        case .setup, .connecting: app.hubURL == nil && !app.onboarded ? .welcome : .hub
        }
    }

    var body: some View {
        ZStack {
            // Behind everything, so a screen scaled below full size never shows the black
            // window at its edges.
            PaletteFill()
            if mountsMain {
                AdaptiveRoot()
                    // The counter-motion to the deck pushing through: the app rises to full size.
                    .scaleEffect(showsOnboarding ? 0.94 : 1)
            }
            if showsOnboarding {
                OnboardingView(start: onboardingStart, revealed: !deckLaunch || launchDone)
                    .transition(.pushThrough)
                    .zIndex(1)
            }
            if splashUp {
                SplashView(isFinishing: launchDone) {
                    splashUp = false
                }
                .zIndex(2)
            }
        }
        .themed()
        .task { await runLaunch() }
    }

    /// A short, certain launch moment: at least 0.8 s, longer only while the hub has not
    /// answered, and never past 2 s (the app shows skeletons and a bubble meanwhile).
    private func runLaunch() async {
        try? await Task.sleep(for: .milliseconds(800))
        let deadline = Date().addingTimeInterval(1.2)
        while app.phase == .connecting && app.onboarded && Date() < deadline {
            try? await Task.sleep(for: .milliseconds(50))
        }
        if deckLaunch {
            withAnimation(.spring(response: 0.7, dampingFraction: 0.8)) { launchDone = true }
        } else {
            launchDone = true
        }
        app.splashDone = true
    }
}

/// The palette's background, edge to edge.
private struct PaletteFill: View {
    @Environment(\.palette) private var palette
    var body: some View { palette.bg.ignoresSafeArea() }
}

