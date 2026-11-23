import AVFoundation
import CoreHaptics
import SwiftUI

/// What the app answers with a tap and, where it fits, a tone: UI moments and what agents do.
nonisolated enum Cue: Hashable, Sendable {
    /// A light tick for a control.
    case tap
    /// A selection moved (a rail tile, a tab, a pane).
    case select
    /// The user's message went out.
    case send
    /// The agent started a tool call.
    case tool
    /// A tool call finished well.
    case toolDone
    /// A tool call failed.
    case toolFailed
    /// The agent asks for permission.
    case approval
    /// The agent asks a question.
    case question
    /// The user answered a request.
    case answered
    /// A message from another session.
    case peer
    /// The agent finished its turn.
    case done
    /// The turn failed.
    case error
    /// The Claude subscription's 5-hour window is used up.
    case limitFiveHour
    /// The weekly window is used up.
    case limitWeekly
    /// Copied to the clipboard.
    case copy
    /// The splash opens onto the app.
    case arrive
    /// Onboarding moves a step forward.
    case step
    /// One of onboarding's lines appears; the index walks up the scale.
    case reveal(Int)
    /// Signed in, set up.
    case success
}

/// Plays cues: Core Haptics patterns and soft synthesized tones made from one list of notes, so
/// every tap lands on its note. Tones go through the `.ambient` audio session: they mix with
/// music, stay quiet under the ring/silent switch and never take the audio focus.
///
/// This facade only decides (settings, bursts); the engines run on `FeedbackEngine`'s own
/// queue. `CHHapticEngine`'s start and the audio session block their thread until the system
/// answers, with no bound, and a cue can be played during launch: on the main thread that
/// could hold the first frame with the watchdog running.
@MainActor
final class Feedback {
    static let shared = Feedback()

    nonisolated static let hapticsKey = "feedback.haptics"
    nonisolated static let soundsKey = "feedback.sounds"

    /// - Parameter haptic: false plays only the tone, for a moment that brings its own
    ///   haptic pattern (the splash).
    static func play(_ cue: Cue, haptic: Bool = true) { shared.play(cue, haptic: haptic) }

    private var lastPlayed: [Cue: Date] = [:]

    private init() {
        UserDefaults.standard.register(defaults: [Self.hapticsKey: true, Self.soundsKey: true])
    }

    func play(_ cue: Cue, haptic: Bool = true) {
        // Bursts (a run of tool calls) give one cue, not a drum roll.
        let now = Date()
        if let last = lastPlayed[cue], now.timeIntervalSince(last) < Self.spec(cue).minInterval { return }
        lastPlayed[cue] = now
        let defaults = UserDefaults.standard
        FeedbackEngine.shared.play(cue,
                                   sound: defaults.bool(forKey: Self.soundsKey),
                                   haptic: haptic && defaults.bool(forKey: Self.hapticsKey))
    }
}

/// The engines behind `Feedback`, owned by one serial queue and touched nowhere else.
nonisolated final class FeedbackEngine: @unchecked Sendable {
    static let shared = FeedbackEngine()

    private let queue = DispatchQueue(label: "xyz.drewnja.hqc.feedback", qos: .userInitiated)
    private let supportsHaptics = CHHapticEngine.capabilitiesForHardware().supportsHaptics
    private var haptics: CHHapticEngine?
    private var audio: AVAudioEngine?
    private var players: [AVAudioPlayerNode] = []
    private var nextPlayer = 0
    private var buffers: [Cue: AVAudioPCMBuffer] = [:]
    private let format = AVAudioFormat(standardFormatWithSampleRate: 44_100, channels: 1)!

    func play(_ cue: Cue, sound: Bool, haptic: Bool) {
        guard sound || haptic else { return }
        queue.async { self.perform(cue, sound: sound, haptic: haptic) }
    }

    private func perform(_ cue: Cue, sound: Bool, haptic: Bool) {
        let spec = Feedback.spec(cue)
        var latency: TimeInterval = 0
        if sound, !spec.notes.isEmpty, playSound(cue, spec) {
            latency = AVAudioSession.sharedInstance().outputLatency
        }
        if haptic { playHaptic(spec, delay: latency) }
    }

    // MARK: haptics

    private func playHaptic(_ spec: CueSpec, delay: TimeInterval) {
        guard supportsHaptics else { return }
        do {
            let engine = try hapticEngine()
            let pattern = try CHHapticPattern(events: spec.hapticEvents, parameters: [])
            let player = try engine.makePlayer(with: pattern)
            try player.start(atTime: delay > 0 ? engine.currentTime + delay : CHHapticTimeImmediate)
        } catch {
            haptics = nil
        }
    }

    private func hapticEngine() throws -> CHHapticEngine {
        if let haptics { return haptics }
        let engine = try CHHapticEngine()
        engine.playsHapticsOnly = true
        engine.isAutoShutdownEnabled = true
        // Both handlers come on an arbitrary thread: back to the queue before touching state.
        engine.resetHandler = {
            let me = FeedbackEngine.shared
            me.queue.async { try? me.haptics?.start() }
        }
        engine.stoppedHandler = { _ in
            let me = FeedbackEngine.shared
            me.queue.async { me.haptics = nil }
        }
        try engine.start()
        haptics = engine
        return engine
    }

    // MARK: sound

    private func playSound(_ cue: Cue, _ spec: CueSpec) -> Bool {
        do {
            let engine = try audioEngine()
            if !engine.isRunning { try engine.start() }
            let buffer = buffers[cue] ?? render(spec)
            buffers[cue] = buffer
            let player = players[nextPlayer]
            nextPlayer = (nextPlayer + 1) % players.count
            player.stop()
            player.scheduleBuffer(buffer, at: nil)
            player.play()
            return true
        } catch {
            audio = nil
            return false
        }
    }

    private func audioEngine() throws -> AVAudioEngine {
        if let audio { return audio }
        try AVAudioSession.sharedInstance().setCategory(.ambient, options: [.mixWithOthers])
        try AVAudioSession.sharedInstance().setActive(true)
        let engine = AVAudioEngine()
        var nodes: [AVAudioPlayerNode] = []
        for _ in 0..<4 {
            let node = AVAudioPlayerNode()
            engine.attach(node)
            engine.connect(node, to: engine.mainMixerNode, format: format)
            nodes.append(node)
        }
        engine.mainMixerNode.outputVolume = 1
        engine.prepare()
        try engine.start()
        audio = engine
        players = nodes
        return engine
    }

    /// A soft mallet tone per note: a sine with a quieter octave that fades first, a 4 ms attack
    /// and an exponential tail, mixed and kept well under full scale.
    private func render(_ spec: CueSpec) -> AVAudioPCMBuffer {
        let rate = format.sampleRate
        let length = (spec.notes.map { $0.at + $0.length }.max() ?? 0.1) + 0.05
        let frames = AVAudioFrameCount(length * rate)
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames)!
        buffer.frameLength = frames
        let out = buffer.floatChannelData![0]
        for i in 0..<Int(frames) { out[i] = 0 }
        for note in spec.notes {
            let start = Int(note.at * rate)
            let count = Int(note.length * rate)
            let tau = note.length / 4.5
            for i in 0..<count where start + i < Int(frames) {
                let t = Double(i) / rate
                let attack = min(1, t / 0.004, Double(count - i) / (0.005 * rate))
                let body = exp(-t / tau)
                let shimmer = exp(-t / (tau * 0.35))
                let phase = 2 * Double.pi * note.freq * t
                let sample = sin(phase) + 0.28 * shimmer * sin(2 * phase) + 0.06 * shimmer * sin(3 * phase)
                out[start + i] += Float(attack * body * sample * note.gain)
            }
        }
        // Never louder than a gentle chime, however many notes overlap.
        var peak: Float = 0
        for i in 0..<Int(frames) { peak = max(peak, abs(out[i])) }
        let ceiling: Float = 0.22 * spec.volume
        if peak > 0 {
            let k = ceiling / peak
            for i in 0..<Int(frames) { out[i] *= k }
        }
        return buffer
    }
}

// MARK: cue design

/// A note of a cue: its start, pitch, how long it rings and how strong it is (0…1). The
/// haptic tap at the same moment takes its strength from `gain` and its sharpness from pitch.
nonisolated struct Note {
    var at: TimeInterval
    var freq: Double
    var length: TimeInterval
    var gain: Double
}

nonisolated struct CueSpec {
    var notes: [Note]
    /// Taps without a note (ticks), and swells.
    var extraHaptics: [CHHapticEvent] = []
    /// Notes that sound but give no tap of their own.
    var silentHapticNotes = false
    var volume: Float = 1
    var minInterval: TimeInterval = 0.06

    var hapticEvents: [CHHapticEvent] {
        var events = extraHaptics
        if !silentHapticNotes {
            for n in notes {
                let sharp = Float(min(1, max(0.15, (n.freq - 200) / 1100 + 0.08)))
                events.append(CHHapticEvent(
                    eventType: .hapticTransient,
                    parameters: [
                        CHHapticEventParameter(parameterID: .hapticIntensity, value: Float(min(1, 0.42 + n.gain * 0.75))),
                        CHHapticEventParameter(parameterID: .hapticSharpness, value: sharp),
                    ],
                    relativeTime: n.at))
            }
        }
        return events
    }
}

/// Pitches of A major's pentatonic scale and a few below, so cues sound like one family.
nonisolated private enum Pitch {
    static let a3 = 220.0, d4 = 293.66, e4 = 329.63, f4 = 349.23
    static let a4 = 440.0, cs5 = 554.37, d5 = 587.33, e5 = 659.25, fs5 = 739.99
    static let a5 = 880.0, b5 = 987.77, cs6 = 1108.73, e6 = 1318.51
    static let scale = [a4, cs5, e5, fs5, a5, b5, cs6, e6]
}

/// Every tap and swell a step firmer than its design value, so cues are felt through a pocket
/// or a case, without turning into buzzes: about a quarter more, capped at full.
nonisolated private func punch(_ x: Float) -> Float { min(1, x * 1.25 + 0.06) }

nonisolated private func tick(_ at: TimeInterval, intensity: Float, sharpness: Float) -> CHHapticEvent {
    CHHapticEvent(eventType: .hapticTransient,
                  parameters: [CHHapticEventParameter(parameterID: .hapticIntensity, value: punch(intensity)),
                               CHHapticEventParameter(parameterID: .hapticSharpness, value: sharpness)],
                  relativeTime: at)
}

nonisolated private func swell(_ at: TimeInterval, _ duration: TimeInterval, intensity: Float, sharpness: Float) -> CHHapticEvent {
    CHHapticEvent(eventType: .hapticContinuous,
                  parameters: [CHHapticEventParameter(parameterID: .hapticIntensity, value: punch(intensity)),
                               CHHapticEventParameter(parameterID: .hapticSharpness, value: sharpness),
                               CHHapticEventParameter(parameterID: .attackTime, value: 0.02),
                               CHHapticEventParameter(parameterID: .decayTime, value: Float(duration)),
                               CHHapticEventParameter(parameterID: .sustained, value: 0)],
                  relativeTime: at, duration: duration)
}

extension Feedback {
    nonisolated static func spec(_ cue: Cue) -> CueSpec {
        switch cue {
        case .tap:
            return CueSpec(notes: [], extraHaptics: [tick(0, intensity: 0.45, sharpness: 0.6)])
        case .select:
            return CueSpec(notes: [], extraHaptics: [tick(0, intensity: 0.35, sharpness: 0.85)])
        case .copy:
            return CueSpec(notes: [Note(at: 0, freq: Pitch.cs6, length: 0.12, gain: 0.25)], volume: 0.5)
        case .send:
            // A quick upward flick: two notes and a short rising swell.
            return CueSpec(
                notes: [Note(at: 0, freq: Pitch.e5, length: 0.12, gain: 0.35),
                        Note(at: 0.055, freq: Pitch.a5, length: 0.22, gain: 0.45)],
                extraHaptics: [swell(0, 0.09, intensity: 0.35, sharpness: 0.5)],
                volume: 0.7)
        case .tool:
            // The quietest cue: a glassy click, at most a few a second.
            return CueSpec(notes: [Note(at: 0, freq: Pitch.e6, length: 0.07, gain: 0.15)],
                           volume: 0.35, minInterval: 0.3)
        case .toolDone:
            // A tool call came back fine: two tiny rising notes under the start click, quieter
            // than everything else, and folded when results arrive in a burst.
            return CueSpec(notes: [Note(at: 0, freq: Pitch.a5, length: 0.07, gain: 0.1),
                                   Note(at: 0.045, freq: Pitch.e6, length: 0.12, gain: 0.14)],
                           volume: 0.28, minInterval: 0.35)
        case .limitFiveHour:
            // The session's window is used up: a falling triad that settles, with a swell under
            // it, clear enough to notice and calm enough to hear twice.
            return CueSpec(notes: [Note(at: 0, freq: Pitch.e5, length: 0.3, gain: 0.6),
                                   Note(at: 0.14, freq: Pitch.cs5, length: 0.3, gain: 0.6),
                                   Note(at: 0.28, freq: Pitch.a4, length: 0.8, gain: 0.7)],
                           extraHaptics: [swell(0.28, 0.35, intensity: 0.4, sharpness: 0.25)],
                           volume: 0.85, minInterval: 30)
        case .limitWeekly:
            // The week is used up: lower and longer, a repeated note that falls a fifth.
            return CueSpec(notes: [Note(at: 0, freq: Pitch.a4, length: 0.28, gain: 0.65),
                                   Note(at: 0.16, freq: Pitch.a4, length: 0.28, gain: 0.65),
                                   Note(at: 0.34, freq: Pitch.d4, length: 1.0, gain: 0.8)],
                           extraHaptics: [swell(0.34, 0.5, intensity: 0.5, sharpness: 0.15)],
                           volume: 0.9, minInterval: 30)
        case .toolFailed:
            return CueSpec(
                notes: [Note(at: 0, freq: Pitch.f4, length: 0.18, gain: 0.45),
                        Note(at: 0.11, freq: Pitch.d4, length: 0.3, gain: 0.5)],
                volume: 0.6, minInterval: 0.5)
        case .approval:
            // A knock-knock: two firm taps on a fifth, asking for a decision.
            return CueSpec(
                notes: [Note(at: 0, freq: Pitch.e5, length: 0.25, gain: 0.7),
                        Note(at: 0.14, freq: Pitch.a5, length: 0.45, gain: 0.85)],
                volume: 0.9, minInterval: 0.8)
        case .question:
            // A rising three-note question.
            return CueSpec(
                notes: [Note(at: 0, freq: Pitch.cs5, length: 0.2, gain: 0.5),
                        Note(at: 0.1, freq: Pitch.e5, length: 0.2, gain: 0.6),
                        Note(at: 0.2, freq: Pitch.b5, length: 0.5, gain: 0.75)],
                volume: 0.9, minInterval: 0.8)
        case .answered:
            return CueSpec(notes: [Note(at: 0, freq: Pitch.a5, length: 0.25, gain: 0.5)], volume: 0.6)
        case .peer:
            return CueSpec(notes: [Note(at: 0, freq: Pitch.fs5, length: 0.35, gain: 0.45)], volume: 0.55)
        case .done:
            // A soft major-third chime with a gentle swell under it.
            return CueSpec(
                notes: [Note(at: 0, freq: Pitch.a5, length: 0.6, gain: 0.55),
                        Note(at: 0.08, freq: Pitch.cs6, length: 0.8, gain: 0.45)],
                extraHaptics: [swell(0.08, 0.25, intensity: 0.3, sharpness: 0.3)],
                volume: 0.75, minInterval: 1)
        case .error:
            return CueSpec(
                notes: [Note(at: 0, freq: Pitch.d5, length: 0.25, gain: 0.6),
                        Note(at: 0.13, freq: Pitch.a4, length: 0.45, gain: 0.6)],
                volume: 0.7, minInterval: 1)
        case .arrive:
            // The app opens: a rising arpeggio and a final bloom.
            let notes = [Pitch.a4, Pitch.cs5, Pitch.e5, Pitch.a5].enumerated().map { i, f in
                Note(at: Double(i) * 0.06, freq: f, length: i == 3 ? 0.9 : 0.35, gain: 0.4 + Double(i) * 0.1)
            }
            return CueSpec(notes: notes, extraHaptics: [swell(0.18, 0.3, intensity: 0.45, sharpness: 0.35)],
                           volume: 0.7, minInterval: 1)
        case .step:
            return CueSpec(notes: [Note(at: 0, freq: Pitch.e5, length: 0.2, gain: 0.4)], volume: 0.5)
        case .reveal(let i):
            let f = Pitch.scale[min(max(i, 0), Pitch.scale.count - 1)]
            return CueSpec(notes: [Note(at: 0, freq: f, length: 0.3, gain: 0.3 + 0.05 * Double(i))],
                           volume: 0.45, minInterval: 0.04)
        case .success:
            let notes = [Pitch.e5, Pitch.a5, Pitch.cs6, Pitch.e6].enumerated().map { i, f in
                Note(at: Double(i) * 0.07, freq: f, length: i == 3 ? 0.8 : 0.3, gain: 0.4 + Double(i) * 0.12)
            }
            return CueSpec(notes: notes, volume: 0.75, minInterval: 1)
        }
    }
}
