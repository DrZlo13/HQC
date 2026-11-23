import SwiftUI

/// The launch: the icon breathes in once, then lifts away while a circle opens from its centre
/// onto the app. About 0.8 s; it waits for the hub's answer at most 2 s, then leaves anyway
/// (the app shows its own connecting state behind).
struct SplashView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.palette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let onFinish: () -> Void

    @State private var shown = false
    @State private var lift = false
    @State private var open = false

    var body: some View {
        GeometryReader { geo in
            let diagonal = (geo.size.width * geo.size.width + geo.size.height * geo.size.height).squareRoot()
            ZStack {
                // The cover, with a hole that grows from the centre.
                palette.bg
                    .mask {
                        Rectangle()
                            .overlay {
                                Circle()
                                    .frame(width: open ? diagonal * 1.1 : 0, height: open ? diagonal * 1.1 : 0)
                                    .blendMode(.destinationOut)
                            }
                            .compositingGroup()
                    }
                Image("Logo")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 112, height: 112)
                    .clipShape(.rect(cornerRadius: 26, style: .continuous))
                    .shadow(color: .black.opacity(shown ? 0.18 : 0), radius: 20, y: 10)
                    .scaleEffect(lift ? 1.12 : shown ? 1 : 0.9)
                    .opacity(lift ? 0 : shown ? 1 : 0)
            }
            .frame(width: geo.size.width, height: geo.size.height)
        }
        .ignoresSafeArea()
        .allowsHitTesting(!open)
        .task { await run() }
        .accessibilityHidden(true)
    }

    private func run() async {
        // Breathe in.
        withAnimation(.easeOut(duration: 0.4)) { shown = true }
        try? await Task.sleep(for: .milliseconds(450))
        // Wait for the hub, briefly: `app` is read live here, not captured.
        let deadline = Date().addingTimeInterval(1.6)
        while app.phase == .connecting && Date() < deadline && !Task.isCancelled {
            try? await Task.sleep(for: .milliseconds(50))
        }
        // Reveal.
        Feedback.play(.arrive)
        withAnimation(.easeIn(duration: 0.25)) { lift = true }
        withAnimation(reduceMotion ? .easeOut(duration: 0.2) : .easeInOut(duration: 0.45)) { open = true }
        try? await Task.sleep(for: .milliseconds(460))
        onFinish()
    }
}
