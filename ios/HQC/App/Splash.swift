import SwiftUI

/// The launch: the icon breathes in, then the whole splash pushes through the viewer
/// (`pushThrough`, applied by `RootView`) while the app rises to full size beneath it. It
/// never waits for the hub: the app opens at once and shows skeletons until data arrives.
struct SplashView: View {
    @Environment(\.palette) private var palette
    let onFinish: () -> Void

    @State private var shown = false

    var body: some View {
        ZStack {
            palette.bg.ignoresSafeArea()
            Image("Logo")
                .resizable()
                .scaledToFit()
                .frame(width: 112, height: 112)
                .clipShape(.rect(cornerRadius: 26, style: .continuous))
                .overlay {
                    // A hairline, so the dark icon still has an edge on a dark palette.
                    RoundedRectangle(cornerRadius: 26, style: .continuous)
                        .strokeBorder(palette.text.opacity(0.12), lineWidth: 1)
                }
                .shadow(color: .black.opacity(shown ? 0.22 : 0), radius: 22, y: 10)
                .scaleEffect(shown ? 1 : 0.9)
                .opacity(shown ? 1 : 0)
        }
        .task {
            // Breathe in, a beat at full size, then go.
            withAnimation(.easeOut(duration: 0.38)) { shown = true }
            try? await Task.sleep(for: .milliseconds(520))
            Feedback.play(.arrive)
            onFinish()
        }
        .accessibilityHidden(true)
    }
}
