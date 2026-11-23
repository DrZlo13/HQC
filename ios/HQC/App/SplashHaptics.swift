// Ported from SwiftBooking (Components/SplashHaptics.swift), the same author's app.
import CoreHaptics
import QuartzCore
import SwiftUI

/// The two splash patterns, which `UIFeedbackGenerator` can't express.
///
/// `UIImpactFeedbackGenerator` only makes discrete taps. The logo doesn't tap
/// — it breathes — so matching it means driving the Taptic Engine directly
/// through Core Haptics: a continuous event whose intensity envelope traces
/// the same curve the logo's scale does, looped for as long as the app is
/// still booting, then answered by a two-beat resolution when it isn't.
///
/// Every entry point is a no-op where there is no Taptic Engine to drive —
/// every simulator, and iPhone 7 and earlier — so callers never check first.
///
/// Nothing here runs on the main thread, and that is load-bearing rather than
/// tidiness: `CHHapticEngine`'s initialiser and its `start()` both block the
/// calling thread until the haptic server answers, and there is no bound on
/// how long that takes — a wedged server never answers at all. The splash
/// calls in from a SwiftUI update during launch, so a block there lands
/// before the first frame is presented and the app would sit on the static
/// launch screen with the watchdog running. No simulator can catch that:
/// without a Taptic Engine none of this code runs there in the first place.
///
/// So the engine, its players and every piece of state below belong to
/// `queue` and are touched nowhere else; the public methods hand work to it
/// and return immediately. `@unchecked Sendable` is that discipline being
/// asserted rather than proved.
nonisolated final class SplashHaptics: @unchecked Sendable {
    static let shared = SplashHaptics()

    /// Serial: the ordering between "stop the loop" and "play the resolution"
    /// is the difference between a cadence and two things at once.
    private let queue = DispatchQueue(label: "xyz.drewnja.hqc.splash-haptics",
                                       qos: .userInitiated)

    /// One full breath: 0.9s in, 0.9s out, which is the period of the
    /// `repeatForever(autoreverses:)` animation on the logo's scale. The two
    /// are separate clocks and will drift apart eventually, but the splash is
    /// seconds long — inside that, a shared start is enough to hold them.
    static let breathPeriod: TimeInterval = 1.8

    private var engine: CHHapticEngine?
    private var breathPlayer: (any CHHapticAdvancedPatternPlayer)?
    /// What the caller has asked for, as opposed to what the engine is doing.
    /// The engine stops itself on backgrounding and on a media-server reset;
    /// this is what tells the restart whether to pick the loop back up.
    private var wantsBreathing = false
    /// When the logo's scale animation began, on `CACurrentMediaTime`'s clock.
    private var pulseStart: CFTimeInterval?

    private init() {}

    /// A Taptic Engine to drive, and the user's Haptics setting on.
    private var isSupported: Bool {
        CHHapticEngine.capabilitiesForHardware().supportsHaptics
            && UserDefaults.standard.object(forKey: Feedback.hapticsKey) as? Bool ?? true
    }

    // MARK: Entry points

    /// - Parameter pulseStart: the `CACurrentMediaTime()` at which the logo's
    ///   scale animation started, so the loop can be entered in step with it.
    func startBreathing(pulseStart: CFTimeInterval) {
        queue.async {
            self.pulseStart = pulseStart
            self.beginBreathing()
        }
    }
    func stopBreathing() { queue.async { self.endBreathing() } }
    func playResolved() { queue.async { self.resolve() } }
    func shutDown() { queue.async { self.tearDown() } }

    // MARK: Breathing

    private func beginBreathing() {
        guard isSupported, !wantsBreathing else { return }
        wantsBreathing = true
        do {
            let engine = try runningEngine()
            let player = try engine.makeAdvancedPlayer(with: Self.breathPattern())
            player.loopEnabled = true
            player.loopEnd = Self.breathPeriod
            // Enter the loop wherever the logo already is in its own cycle.
            // Bringing the engine up takes an unbounded moment, and the scale
            // animation has been running throughout it, so starting the
            // pattern from its own zero puts the swell at whatever offset the
            // hardware happened to take — the thing that felt out of step.
            // Computed here rather than on entry so the engine's own start-up
            // time is inside the measurement.
            var startTime = CHHapticTimeImmediate
            if let pulseStart {
                let phase = (CACurrentMediaTime() - pulseStart)
                    .truncatingRemainder(dividingBy: Self.breathPeriod)
                do {
                    try player.seek(toOffset: phase)
                } catch {
                    // Nothing in the API promises a stopped player can be
                    // seeked. Fall back to waiting for the logo's next cycle
                    // boundary and starting from the pattern's own zero:
                    // under a second of silence, against a swell that would
                    // otherwise be permanently out of step.
                    startTime = engine.currentTime + (Self.breathPeriod - phase)
                }
            }
            try player.start(atTime: startTime)
            breathPlayer = player
        } catch {
            // A silent splash is a far better outcome than a crashed one, and
            // there is nothing to retry: the engine either came up or didn't.
            wantsBreathing = false
            breathPlayer = nil
        }
    }

    private func endBreathing() {
        wantsBreathing = false
        try? breathPlayer?.stop(atTime: CHHapticTimeImmediate)
        breathPlayer = nil
    }

    // MARK: Resolution

    /// The answer to the breathing: a soft beat, a firm one on top of it, and
    /// a short decaying tail. Played as the logo leaves, so the boot ends on a
    /// resolution rather than the loop simply being cut off.
    private func resolve() {
        endBreathing()
        guard UserDefaults.standard.object(forKey: Feedback.hapticsKey) as? Bool ?? true else { return }
        guard isSupported else {
            // Two taps is the nearest thing the generator API has.
            Task { @MainActor in UINotificationFeedbackGenerator().notificationOccurred(.success) }
            return
        }
        do {
            let engine = try runningEngine()
            try engine.makePlayer(with: Self.resolvedPattern())
                .start(atTime: CHHapticTimeImmediate)
        } catch {
            Task { @MainActor in UINotificationFeedbackGenerator().notificationOccurred(.success) }
        }
        // The splash is the only thing here that needs the engine, and it has
        // just finished. Let the tail play out, then give the hardware back.
        queue.asyncAfter(deadline: .now() + 0.6) { self.tearDown() }
    }

    private func tearDown() {
        endBreathing()
        engine?.stop()
        engine = nil
    }

    // MARK: Engine

    private func runningEngine() throws -> CHHapticEngine {
        if let engine { return engine }
        let created = try CHHapticEngine()
        // No audio session, and therefore nothing for the ringer switch to
        // silence — this is a haptic, not a sound with a haptic attached.
        created.playsHapticsOnly = true
        // The loop has long near-silent stretches, which auto-shutdown reads
        // as idle; it would pull the engine out from under the next swell.
        created.isAutoShutdownEnabled = false
        // Both handlers arrive on an arbitrary thread, so each one hands
        // itself straight back to `queue` before touching any state.
        created.resetHandler = {
            SplashHaptics.shared.queue.async {
                SplashHaptics.shared.restartAfterInterruption()
            }
        }
        created.stoppedHandler = { _ in
            SplashHaptics.shared.queue.async { SplashHaptics.shared.engineStopped() }
        }
        try created.start()
        engine = created
        return created
    }

    /// The media server restarted underneath us: the engine object survives
    /// but its players don't, so the loop has to be rebuilt.
    private func restartAfterInterruption() {
        guard wantsBreathing else { return }
        wantsBreathing = false
        breathPlayer = nil
        beginBreathing()
    }

    /// The engine stopped on its own — backgrounding, most often. Drop the
    /// stale objects; `beginBreathing` builds fresh ones if the splash is
    /// still up when the app comes back.
    private func engineStopped() {
        breathPlayer = nil
        engine = nil
        wantsBreathing = false
    }

    // MARK: Patterns

    private static func breathPattern() throws -> CHHapticPattern {
        // One continuous event spanning the whole cycle. A flat continuous
        // haptic reads as a phone fault, so the shape is carried entirely by
        // the envelope below — the event itself is held at full intensity and
        // the curve does the work.
        let breath = CHHapticEvent(
            eventType: .hapticContinuous,
            parameters: [
                CHHapticEventParameter(parameterID: .hapticIntensity, value: 1),
                CHHapticEventParameter(parameterID: .hapticSharpness, value: 0.2),
            ],
            relativeTime: 0,
            duration: breathPeriod)

        // Rises the way the logo grows, peaks where the logo is biggest, and
        // is back at silence before the next breath begins — the gap is what
        // makes it read as breathing rather than as a buzz.
        let swell = CHHapticParameterCurve(
            parameterID: .hapticIntensityControl,
            controlPoints: [
                .init(relativeTime: 0.00, value: 0.00),
                .init(relativeTime: 0.30, value: 0.13),
                .init(relativeTime: 0.62, value: 0.36),
                .init(relativeTime: 0.90, value: 0.45),
                .init(relativeTime: 1.18, value: 0.33),
                .init(relativeTime: 1.50, value: 0.10),
                .init(relativeTime: 1.80, value: 0.00),
            ],
            relativeTime: 0)

        // Dull at the edges, a little more defined at the top, so the swell
        // has a centre instead of being a uniform hum getting louder.
        let focus = CHHapticParameterCurve(
            parameterID: .hapticSharpnessControl,
            controlPoints: [
                .init(relativeTime: 0.00, value: -0.3),
                .init(relativeTime: 0.90, value: 0.15),
                .init(relativeTime: 1.80, value: -0.3),
            ],
            relativeTime: 0)

        return try CHHapticPattern(events: [breath], parameterCurves: [swell, focus])
    }

    private static func resolvedPattern() throws -> CHHapticPattern {
        let events = [
            CHHapticEvent(eventType: .hapticTransient, parameters: [
                CHHapticEventParameter(parameterID: .hapticIntensity, value: 0.55),
                CHHapticEventParameter(parameterID: .hapticSharpness, value: 0.30),
            ], relativeTime: 0),
            CHHapticEvent(eventType: .hapticTransient, parameters: [
                CHHapticEventParameter(parameterID: .hapticIntensity, value: 1),
                CHHapticEventParameter(parameterID: .hapticSharpness, value: 0.60),
            ], relativeTime: 0.09),
            CHHapticEvent(eventType: .hapticContinuous, parameters: [
                CHHapticEventParameter(parameterID: .hapticIntensity, value: 0.38),
                CHHapticEventParameter(parameterID: .hapticSharpness, value: 0.25),
            ], relativeTime: 0.09, duration: 0.26),
        ]

        // The tail under the second beat, falling away rather than stopping.
        let decay = CHHapticParameterCurve(
            parameterID: .hapticIntensityControl,
            controlPoints: [
                .init(relativeTime: 0.00, value: 1.00),
                .init(relativeTime: 0.12, value: 0.45),
                .init(relativeTime: 0.26, value: 0.00),
            ],
            relativeTime: 0.09)

        return try CHHapticPattern(events: events, parameterCurves: [decay])
    }
}

/// Runs the breathing loop for as long as the splash is up *and* the app is
/// genuinely on screen.
///
/// Both conditions matter. iOS discards haptics while the scene is merely
/// `.inactive`, which is exactly what it is during launch — starting the loop
/// from `onAppear` would drop the first breath or two — and the engine stops
/// itself when the app leaves, so coming back has to start a fresh one.
struct BreathingHaptic: ViewModifier {
    var isActive: Bool
    /// When the logo's scale animation started. `nil` until it has — with
    /// Reduce Motion it never does, and there is then no pulse for the haptic
    /// to be the felt half of.
    var pulseStart: CFTimeInterval?

    @Environment(\.scenePhase) private var scenePhase

    func body(content: Content) -> some View {
        content
            .onAppear(perform: sync)
            .onChange(of: scenePhase) { _, _ in sync() }
            .onChange(of: isActive) { _, _ in sync() }
            // The pulse usually starts after this modifier first appears, so
            // its arrival is what actually gets the loop going.
            .onChange(of: pulseStart) { _, _ in sync() }
            .onDisappear { SplashHaptics.shared.stopBreathing() }
    }

    private func sync() {
        guard isActive, scenePhase == .active, let pulseStart else {
            SplashHaptics.shared.stopBreathing()
            return
        }
        SplashHaptics.shared.startBreathing(pulseStart: pulseStart)
    }
}

extension View {
    /// The looping swell under a pulsing launch logo, entered in step with it.
    func breathingHaptic(isActive: Bool, pulseStart: CFTimeInterval?) -> some View {
        modifier(BreathingHaptic(isActive: isActive, pulseStart: pulseStart))
    }
}
