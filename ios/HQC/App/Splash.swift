import SwiftUI

/// The first thing on screen: the icon pulsing with a felt heartbeat while the app connects to
/// its hub behind it, then blooming open onto the app with the arrival chime.
struct SplashView: View {
    @Environment(\.palette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// The app behind is ready: finish the current beat, bloom, then call `onFinish`.
    let ready: Bool
    let onFinish: () -> Void

    @State private var beat = false
    @State private var bloom = false
    @State private var appeared = false
    @State private var ring = false

    var body: some View {
        ZStack {
            palette.bg.ignoresSafeArea()
            BackdropGradient()
                .opacity(appeared ? 1 : 0)
            ZStack {
                // A soft ring that ripples out on every beat.
                RoundedRectangle(cornerRadius: 34, style: .continuous)
                    .stroke(Color.accentColor.opacity(ring ? 0 : 0.35), lineWidth: 2)
                    .frame(width: 132, height: 132)
                    .scaleEffect(ring ? 1.5 : 1)
                Image("Logo")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 132, height: 132)
                    .clipShape(.rect(cornerRadius: 30, style: .continuous))
                    .shadow(color: .black.opacity(0.22), radius: beat ? 26 : 16, y: beat ? 12 : 8)
                    .scaleEffect(bloom ? 1.35 : beat ? 1.07 : 1)
            }
            .scaleEffect(appeared ? 1 : 0.7)
            .opacity(bloom ? 0 : appeared ? 1 : 0)
            .blur(radius: bloom ? 12 : 0)
        }
        .opacity(bloom ? 0 : 1)
        .task { await run() }
        .accessibilityHidden(true)
    }

    private func run() async {
        withAnimation(.spring(duration: 0.55, bounce: 0.35)) { appeared = true }
        Feedback.play(.tap)
        try? await Task.sleep(for: .milliseconds(450))
        // At least one beat, so the splash never flickers by; then until the app is ready.
        var beats = 0
        while !Task.isCancelled && (beats < 1 || !ready) && beats < 40 {
            await pulse()
            beats += 1
        }
        Feedback.play(.arrive)
        withAnimation(reduceMotion ? .easeOut(duration: 0.25) : .spring(duration: 0.55, bounce: 0.1)) { bloom = true }
        try? await Task.sleep(for: .milliseconds(450))
        onFinish()
    }

    /// One heartbeat: the icon swells on the "lub", the ring ripples, and it settles.
    private func pulse() async {
        Feedback.play(.heartbeat)
        withAnimation(.easeOut(duration: 0.16)) { beat = true }
        if !reduceMotion {
            ring = false
            withAnimation(.easeOut(duration: 0.9)) { ring = true }
        }
        try? await Task.sleep(for: .milliseconds(170))
        withAnimation(.spring(duration: 0.5, bounce: 0.4)) { beat = false }
        try? await Task.sleep(for: .milliseconds(780))
    }
}
