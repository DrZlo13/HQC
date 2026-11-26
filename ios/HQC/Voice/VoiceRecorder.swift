import AVFoundation
import Foundation
import Observation

/// Captures 16 kHz mono Float32 PCM audio from the microphone on a dedicated `.userInteractive`
/// queue so touch-down never blocks `@MainActor`, while reporting sensitive live speech amplitude
/// (0…1) and elapsed time for the Telegram-style recording bar and pulsing mic halo.
@MainActor
@Observable
final class VoiceRecorder {
    private(set) var isRecording = false
    private(set) var duration: TimeInterval = 0
    /// Smoothed normalized mic amplitude (0…1) for the audio-reactive halo around the mic button.
    private(set) var level: Float = 0

    static let minDuration: TimeInterval = 0.28
    static let maxDuration: TimeInterval = 300

    private let backend = AudioBackend()
    private let buffer = SampleBuffer()
    private var ticker: Task<Void, Never>?
    private var startedAt: Date?
    private var onStartError: ((Error) -> Void)?

    nonisolated static var permissionStatus: AVAudioApplication.recordPermission {
        AVAudioApplication.shared.recordPermission
    }

    nonisolated static func ensurePermission() async -> Bool {
        switch AVAudioApplication.shared.recordPermission {
        case .granted:
            return true
        case .denied:
            return false
        case .undetermined:
            return await AVAudioApplication.requestRecordPermission()
        @unknown default:
            return false
        }
    }

    /// Starts recording immediately on frame 0 without blocking `@MainActor`; audio session
    /// activation and `AVAudioEngine` startup run asynchronously on a `.userInteractive` queue.
    func start(
        onError: @escaping (Error) -> Void,
        onAutoStop: (() -> Void)? = nil
    ) {
        ticker?.cancel()
        ticker = nil
        buffer.reset()

        let now = Date()
        startedAt = now
        onStartError = onError
        duration = 0
        level = 0
        isRecording = true

        backend.start(into: buffer) { [weak self] error in
            Task { @MainActor [weak self] in
                guard let self, self.isRecording, self.startedAt == now else { return }
                let handler = self.onStartError
                self.cancel()
                handler?(error)
            }
        }

        // 60 fps display ticker so the timer and audio-reactive rings track speech crisply.
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(16))
                guard let self, self.isRecording, let startedAt = self.startedAt else { return }
                let elapsed = Date().timeIntervalSince(startedAt)
                self.duration = elapsed
                let raw = self.buffer.takePeakLevel()
                // Fast attack so every syllable pops immediately; smooth release so rings breathe.
                self.level = raw > self.level
                    ? self.level * 0.30 + raw * 0.70
                    : self.level * 0.78 + raw * 0.22
                if elapsed >= Self.maxDuration {
                    onAutoStop?()
                    return
                }
            }
        }
    }

    /// Elapsed time since recording started (usable synchronously before awaiting `finish()`).
    var currentElapsed: TimeInterval {
        startedAt.map { Date().timeIntervalSince($0) } ?? duration
    }

    /// Stops recording off the main thread and returns captured 16 kHz mono Float32 samples.
    func finish() async -> (samples: [Float], duration: TimeInterval) {
        let elapsed = currentElapsed
        ticker?.cancel()
        ticker = nil
        startedAt = nil
        onStartError = nil
        isRecording = false
        level = 0
        let samples = await backend.stopAndDrain(buffer: buffer)
        return (samples, elapsed)
    }

    /// Cancels recording immediately without blocking `@MainActor`.
    func cancel() {
        ticker?.cancel()
        ticker = nil
        startedAt = nil
        onStartError = nil
        isRecording = false
        level = 0
        backend.cancel(buffer: buffer)
    }

    /// Formats elapsed duration like Telegram's `0:07,16`.
    static func formatDuration(_ t: TimeInterval) -> String {
        let clamped = max(0, t)
        let minutes = Int(clamped) / 60
        let seconds = Int(clamped) % 60
        let centiseconds = Int((clamped - floor(clamped)) * 100) % 100
        return String(format: "%d:%02d,%02d", minutes, seconds, centiseconds)
    }
}

/// Owns `AVAudioSession` and `AVAudioEngine` on a single serial `.userInteractive` queue so
/// neither starting nor stopping ever stalls the UI thread or races across rapid presses.
private nonisolated final class AudioBackend: @unchecked Sendable {
    private let queue = DispatchQueue(label: "xyz.drewnja.hqc.voice.audio", qos: .userInteractive)
    private let genLock = NSLock()
    private var generation: UInt64 = 0
    private var engine: AVAudioEngine?
    private var sessionActive = false

    private func nextGeneration() -> UInt64 {
        genLock.withLock {
            generation &+= 1
            return generation
        }
    }

    private func currentGeneration() -> UInt64 {
        genLock.withLock { generation }
    }

    func start(into store: SampleBuffer, onError: @escaping @Sendable (Error) -> Void) {
        let myGen = nextGeneration()
        queue.async {
            guard self.currentGeneration() == myGen else { return }
            self.teardownEngine()

            // Stop Feedback's `.ambient` engine off the main thread before claiming `.playAndRecord`.
            FeedbackEngine.shared.suspendAudio()

            do {
                let session = AVAudioSession.sharedInstance()
                // `.default` keeps standard iOS microphone gain (much livelier than `.measurement`).
                try session.setCategory(
                    .playAndRecord,
                    mode: .default,
                    options: [.defaultToSpeaker, .allowBluetoothHFP]
                )
                try session.setActive(true, options: [])
                self.sessionActive = true

                guard self.currentGeneration() == myGen else {
                    self.deactivateSessionLater(ifGenerationStill: myGen)
                    return
                }

                let engine = AVAudioEngine()
                let input = engine.inputNode
                let hwFormat = input.outputFormat(forBus: 0)
                guard hwFormat.sampleRate > 0, hwFormat.channelCount > 0 else {
                    throw VoiceError.microphoneDenied
                }
                guard let targetFormat = AVAudioFormat(
                    commonFormat: .pcmFormatFloat32,
                    sampleRate: 16_000,
                    channels: 1,
                    interleaved: false
                ), let converter = AVAudioConverter(from: hwFormat, to: targetFormat) else {
                    throw VoiceError.microphoneDenied
                }

                // Installed inside this nonisolated class so Swift 6 never attaches a @MainActor
                // queue assertion to the real-time audio tap closure.
                input.installTap(onBus: 0, bufferSize: 1024, format: hwFormat) { pcm, _ in
                    Self.consume(pcm: pcm, converter: converter, targetFormat: targetFormat, into: store)
                }

                engine.prepare()
                try engine.start()

                guard self.currentGeneration() == myGen else {
                    input.removeTap(onBus: 0)
                    engine.stop()
                    self.deactivateSessionLater(ifGenerationStill: myGen)
                    return
                }

                self.engine = engine
            } catch {
                self.teardownEngine()
                self.deactivateSessionLater(ifGenerationStill: myGen)
                if self.currentGeneration() == myGen {
                    onError(error)
                }
            }
        }
    }

    func stopAndDrain(buffer: SampleBuffer) async -> [Float] {
        let stopGen = nextGeneration()
        return await withCheckedContinuation { cont in
            queue.async {
                self.teardownEngine()
                let samples = buffer.drain()
                cont.resume(returning: samples)
                // Deactivate the audio session after handing samples back so transcription starts immediately.
                self.deactivateSessionLater(ifGenerationStill: stopGen)
            }
        }
    }

    func cancel(buffer: SampleBuffer) {
        let cancelGen = nextGeneration()
        queue.async {
            self.teardownEngine()
            buffer.reset()
            self.deactivateSessionLater(ifGenerationStill: cancelGen)
        }
    }

    private func teardownEngine() {
        if let engine {
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
            self.engine = nil
        }
    }

    private func deactivateSessionLater(ifGenerationStill gen: UInt64) {
        queue.async {
            guard self.currentGeneration() == gen else { return }
            if self.sessionActive {
                try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
                self.sessionActive = false
            }
            FeedbackEngine.shared.resumeAudio()
        }
    }

    private static func consume(
        pcm: AVAudioPCMBuffer,
        converter: AVAudioConverter,
        targetFormat: AVAudioFormat,
        into store: SampleBuffer
    ) {
        guard pcm.frameLength > 0, pcm.format.sampleRate > 0 else { return }

        // Sensitive speech meter: blend RMS + peak mapped over -58 dB … -16 dB with a perceptual
        // power curve so normal speaking drives the Telegram halo strongly without jitter on silence.
        if let channel = pcm.floatChannelData?[0] {
            let n = Int(pcm.frameLength)
            var sum: Float = 0
            var peak: Float = 0
            for i in 0..<n {
                let s = channel[i]
                let a = abs(s)
                if a > peak { peak = a }
                sum += s * s
            }
            let rms = sqrt(sum / Float(n))
            let blended = rms * 0.65 + peak * 0.35
            let db = 20 * log10(max(blended, 1e-6))
            let linear = max(0, min(1, (db + 62) / 44))
            let curved = powf(linear, 0.62)
            store.recordLevel(curved)
        }

        let ratio = targetFormat.sampleRate / pcm.format.sampleRate
        let outFrames = AVAudioFrameCount(max(1, ceil(Double(pcm.frameLength) * ratio))) + 64
        guard let converted = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: outFrames) else { return }

        var consumed = false
        var error: NSError?
        converter.convert(to: converted, error: &error) { _, outStatus in
            if consumed {
                outStatus.pointee = .noDataNow
                return nil
            }
            consumed = true
            outStatus.pointee = .haveData
            return pcm
        }
        guard error == nil, let data = converted.floatChannelData?[0], converted.frameLength > 0 else { return }
        store.append(data, count: Int(converted.frameLength))
    }
}

/// Lock-protected PCM sample accumulator written from the real-time audio tap thread.
private nonisolated final class SampleBuffer: @unchecked Sendable {
    private let lock = NSLock()
    private var samples: [Float] = []
    private var peakLevel: Float = 0

    func reset() {
        lock.withLock {
            samples.removeAll(keepingCapacity: true)
            peakLevel = 0
        }
    }

    func append(_ ptr: UnsafePointer<Float>, count: Int) {
        let slice = UnsafeBufferPointer(start: ptr, count: count)
        lock.withLock {
            samples.append(contentsOf: slice)
        }
    }

    func recordLevel(_ value: Float) {
        lock.withLock {
            if value > peakLevel { peakLevel = value }
        }
    }

    func takePeakLevel() -> Float {
        lock.withLock {
            let v = peakLevel
            peakLevel = 0
            return v
        }
    }

    func drain() -> [Float] {
        lock.withLock {
            let out = samples
            samples.removeAll(keepingCapacity: false)
            peakLevel = 0
            return out
        }
    }
}
