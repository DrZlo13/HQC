import QuartzCore
import SwiftUI

/// The launch on a set-up app (ported from SwiftBooking's `SplashScreen`). Its first frame is
/// the system launch screen (`UILaunchScreen`: `LaunchLogo` centred on `LaunchBackground`), so
/// the hand-over from the OS-drawn screen is invisible. The logo breathes, felt as a swell in
/// step with it, while the app loads underneath; when `isFinishing` flips, it shrinks and fades
/// in one motion with a resolving beat and the arrival chime, and `onVanished` unmounts it.
///
/// A first launch never shows this: the welcome deck draws the same moment itself and flies the
/// logo up into place (`OnboardingView`).
struct SplashView: View {
    var isFinishing: Bool
    var onVanished: () -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var scale: CGFloat = 1
    @State private var opacity: Double = 1
    /// When the pulse started, so the haptic swell can join it in phase.
    @State private var pulseStart: CFTimeInterval?

    var body: some View {
        ZStack {
            Color("LaunchBackground").ignoresSafeArea()
            Image("LaunchLogo")
                .resizable()
                .scaledToFit()
                .frame(width: SplashLogo.size, height: SplashLogo.size)
                .scaleEffect(scale)
        }
        .opacity(opacity)
        .breathingHaptic(isActive: !isFinishing, pulseStart: pulseStart)
        .onAppear(perform: startPulsing)
        .onChange(of: isFinishing, initial: true) { _, finishing in
            if finishing { vanish() }
        }
        .accessibilityHidden(true)
    }

    private func startPulsing() {
        guard !reduceMotion else { return }
        pulseStart = CACurrentMediaTime()
        withAnimation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true)) {
            scale = 1.05
        }
    }

    private func vanish() {
        SplashHaptics.shared.playResolved()
        Feedback.play(.arrive, haptic: false)
        withAnimation(.easeInOut(duration: 0.5), completionCriteria: .logicallyComplete) {
            scale = 0.3
            opacity = 0
        } completion: {
            onVanished()
        }
    }
}

/// The launch logo's size, the same as `LaunchLogo`'s point size, shared with the welcome deck
/// so its launch state lines up with the OS launch screen to the pixel.
nonisolated enum SplashLogo {
    static let size: CGFloat = 112
}
