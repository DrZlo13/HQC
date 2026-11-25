import AVFoundation
import Foundation
import Observation

/// Captures 16 kHz mono Float32 PCM audio from the microphone while reporting smoothed live level
/// (0…1) and elapsed time for the Telegram-style recording bar and pulsing mic halo.
@MainActor
@Observable
final class VoiceRecorder {
    private(set) var isRecording = false
    private(set) var duration: TimeInterval = 0
    /// Smoothed normalized mic amplitude (0…1) for the audio-reactive halo around the mic button.
    private(set) var level: Float = 0

    static let minDuration: TimeInterval = 0.30
    static let maxDuration: TimeInterval = 300

    private var engine: AVAudioEngine?
    private var ticker: Task<Void, Never>?
    private var startedAt: Date?
    private let buffer = SampleBuffer()

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

    func start(onAutoStop: (() -> Void)? = nil) throws {
        stopEngineOnly()
        buffer.reset()

        // Release Feedback's `.ambient` engine before switching session category to `.playAndRecord`.
        FeedbackEngine.shared.suspendAudio()

        let session = AVAudioSession.sharedInstance()
        try session.setCategory(
            .playAndRecord,
            mode: .measurement,
            options: [.defaultToSpeaker, .allowBluetoothHFP, .duckOthers]
        )
        try session.setActive(true, options: [])

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

        Self.installTap(
            on: input,
            format: hwFormat,
            converter: converter,
            targetFormat: targetFormat,
            into: buffer
        )

        engine.prepare()
        try engine.start()
        self.engine = engine
        startedAt = Date()
        duration = 0
        level = 0
        isRecording = true

        ticker?.cancel()
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(30))
                guard let self, self.isRecording, let startedAt = self.startedAt else { return }
                let elapsed = Date().timeIntervalSince(startedAt)
                self.duration = elapsed
                let raw = self.buffer.takePeakLevel()
                // Smooth rise/fall so the Telegram halo breathes naturally with speech.
                self.level = raw > self.level
                    ? self.level * 0.45 + raw * 0.55
                    : self.level * 0.78 + raw * 0.22
                if elapsed >= Self.maxDuration {
                    onAutoStop?()
                    return
                }
            }
        }
    }

    /// Stops recording and returns the captured 16 kHz mono Float32 samples and elapsed duration.
    func finish() -> (samples: [Float], duration: TimeInterval) {
        let elapsed = startedAt.map { Date().timeIntervalSince($0) } ?? duration
        stopEngineOnly()
        let samples = buffer.drain()
        return (samples, elapsed)
    }

    func cancel() {
        stopEngineOnly()
        buffer.reset()
    }

    private func stopEngineOnly() {
        ticker?.cancel()
        ticker = nil
        if let engine {
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
        }
        engine = nil
        startedAt = nil
        isRecording = false
        level = 0
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
        FeedbackEngine.shared.resumeAudio()
    }

    /// Installed from a `nonisolated` function so Swift 6 does not infer `@MainActor` on the
    /// Obj-C audio tap closure (which runs on AVFAudio's real-time render thread and would trap
    /// with `dispatch_assert_queue_fail` if MainActor-isolated).
    private nonisolated static func installTap(
        on input: AVAudioInputNode,
        format: AVAudioFormat,
        converter: AVAudioConverter,
        targetFormat: AVAudioFormat,
        into store: SampleBuffer
    ) {
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { pcm, _ in
            consume(pcm: pcm, converter: converter, targetFormat: targetFormat, into: store)
        }
    }

    private nonisolated static func consume(
        pcm: AVAudioPCMBuffer,
        converter: AVAudioConverter,
        targetFormat: AVAudioFormat,
        into store: SampleBuffer
    ) {
        guard pcm.frameLength > 0, pcm.format.sampleRate > 0 else { return }

        // Compute live RMS from input buffer for visual halo reactivity.
        if let channel = pcm.floatChannelData?[0] {
            let n = Int(pcm.frameLength)
            var sum: Float = 0
            for i in 0..<n {
                let s = channel[i]
                sum += s * s
            }
            let rms = sqrt(sum / Float(n))
            // Map typical voice RMS (-50 dB … -8 dB) into 0…1.
            let db = 20 * log10(max(rms, 1e-6))
            let norm = max(0, min(1, (db + 50) / 42))
            store.recordLevel(norm)
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

    /// Formats elapsed duration like Telegram's `0:07,16`.
    static func formatDuration(_ t: TimeInterval) -> String {
        let clamped = max(0, t)
        let minutes = Int(clamped) / 60
        let seconds = Int(clamped) % 60
        let centiseconds = Int((clamped - floor(clamped)) * 100) % 100
        return String(format: "%d:%02d,%02d", minutes, seconds, centiseconds)
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
