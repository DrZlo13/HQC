import SwiftUI

/// State machine for Telegram-style hold-to-talk recording, slide-left cancel, slide-up lock,
/// and on-device transcription.
@MainActor
@Observable
final class VoiceSession {
    enum Phase: Equatable {
        case idle
        case recording(locked: Bool)
        case transcribing
    }

    var phase: Phase = .idle
    var dragOffset: CGSize = .zero
    var hint: String?
    var error: String?

    let recorder = VoiceRecorder()
    private var transcribeTask: Task<Void, Never>?
    private var warmUpTask: Task<Void, Never>?
    private var hintTask: Task<Void, Never>?
    private var startAttemptID = UUID()

    static let cancelThreshold: CGFloat = -100
    static let lockThreshold: CGFloat = -64
    private static let dragDeadzone: CGFloat = 6

    var isActive: Bool { phase != .idle }
    var isRecording: Bool {
        if case .recording = phase { return true }
        return false
    }
    var isHolding: Bool { phase == .recording(locked: false) }
    var isLocked: Bool { phase == .recording(locked: true) }

    /// 0…1 progress toward the left cancel gesture.
    var cancelProgress: CGFloat {
        guard isHolding else { return 0 }
        return min(1, max(0, -dragOffset.width / abs(Self.cancelThreshold)))
    }

    /// 0…1 progress toward the upward lock gesture.
    var lockProgress: CGFloat {
        guard isHolding else { return 0 }
        return min(1, max(0, -dragOffset.height / abs(Self.lockThreshold)))
    }

    /// Begins hold-to-talk recording immediately on frame 0 when the user presses down.
    func pressDown(onTranscript: @escaping (String) -> Void) {
        guard phase == .idle else { return }
        error = nil
        hint = nil
        dragOffset = .zero
        let attempt = UUID()
        startAttemptID = attempt

        if VoiceRecorder.permissionStatus == .granted {
            beginRecording(onTranscript: onTranscript)
            return
        }

        Task { [weak self] in
            let granted = await VoiceRecorder.ensurePermission()
            guard let self, self.startAttemptID == attempt else { return }
            guard granted else {
                self.showError(VoiceError.microphoneDenied.localizedDescription)
                Feedback.play(.error)
                return
            }
            // Permission sheet broke the touch gesture; prompt to hold again.
            self.showHint("Microphone allowed — hold to record")
            Feedback.play(.done)
        }
    }

    private func beginRecording(onTranscript: @escaping (String) -> Void) {
        // Flip UI & haptic synchronously on frame 0 (0 ms lag); AVAudioSession/AVAudioEngine boot off-main.
        withAnimation(.spring(response: 0.24, dampingFraction: 0.72)) {
            phase = .recording(locked: false)
        }
        Feedback.play(.voiceStart)

        recorder.start(
            onError: { [weak self] err in
                guard let self else { return }
                withAnimation(.spring(response: 0.24, dampingFraction: 0.82)) {
                    self.phase = .idle
                    self.dragOffset = .zero
                }
                self.showError(err.localizedDescription)
                Feedback.play(.error)
            },
            onAutoStop: { [weak self] in
                self?.finishRecording(onTranscript: onTranscript)
            }
        )

        // Defer Core ML warm-up slightly so the touch-down spring animation stays 120 Hz.
        warmUpTask?.cancel()
        warmUpTask = Task {
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            VoiceStore.shared.warmUpIfReady()
        }
    }

    func dragChanged(_ translation: CGSize) {
        guard isHolding else { return }
        if hypot(translation.width, translation.height) < Self.dragDeadzone {
            dragOffset = .zero
            return
        }
        // Lock to the dominant axis (slide left to cancel OR slide up to lock), like Telegram.
        if abs(translation.height) > abs(translation.width) && translation.height < 0 {
            dragOffset = CGSize(width: 0, height: max(translation.height, Self.lockThreshold - 22))
            if translation.height <= Self.lockThreshold {
                withAnimation(.spring(response: 0.28, dampingFraction: 0.78)) {
                    phase = .recording(locked: true)
                    dragOffset = .zero
                }
                Feedback.play(.voiceLock)
            }
        } else if translation.width < 0 {
            dragOffset = CGSize(width: max(translation.width, Self.cancelThreshold - 18), height: 0)
            if translation.width <= Self.cancelThreshold {
                cancelRecording()
            }
        } else {
            dragOffset = .zero
        }
    }

    func dragEnded(onTranscript: @escaping (String) -> Void) {
        startAttemptID = UUID()
        guard isHolding else { return }
        if dragOffset.width <= Self.cancelThreshold {
            cancelRecording()
        } else {
            finishRecording(onTranscript: onTranscript)
        }
    }

    func cancelRecording() {
        startAttemptID = UUID()
        warmUpTask?.cancel()
        warmUpTask = nil
        transcribeTask?.cancel()
        transcribeTask = nil
        recorder.cancel()
        withAnimation(.spring(response: 0.25, dampingFraction: 0.82)) {
            phase = .idle
            dragOffset = .zero
        }
        Feedback.play(.voiceCancel)
    }

    func finishRecording(onTranscript: @escaping (String) -> Void) {
        guard isRecording else { return }
        warmUpTask?.cancel()
        warmUpTask = nil

        if recorder.currentElapsed < VoiceRecorder.minDuration {
            recorder.cancel()
            withAnimation(.spring(response: 0.24, dampingFraction: 0.82)) {
                phase = .idle
                dragOffset = .zero
            }
            showHint("Hold to record")
            Feedback.play(.tap)
            return
        }

        withAnimation(.spring(response: 0.26, dampingFraction: 0.84)) {
            phase = .transcribing
            dragOffset = .zero
        }
        Feedback.play(.tap)

        transcribeTask?.cancel()
        transcribeTask = Task { [weak self] in
            guard let self else { return }
            let (samples, elapsed) = await self.recorder.finish()
            guard !Task.isCancelled else { return }
            if elapsed < VoiceRecorder.minDuration || samples.count < 1600 {
                withAnimation(.spring(response: 0.25, dampingFraction: 0.85)) {
                    self.phase = .idle
                }
                self.showHint("Hold to record")
                return
            }
            do {
                let text = try await VoiceStore.shared.transcribe(samples: samples)
                guard !Task.isCancelled else { return }
                withAnimation(.spring(response: 0.26, dampingFraction: 0.86)) {
                    self.phase = .idle
                }
                if text.isEmpty {
                    self.showHint("No speech detected")
                } else {
                    onTranscript(text)
                    Feedback.play(.answered)
                }
            } catch is CancellationError {
                withAnimation(.spring(response: 0.24)) { self.phase = .idle }
            } catch {
                withAnimation(.spring(response: 0.24)) { self.phase = .idle }
                self.showError(error.localizedDescription)
                Feedback.play(.error)
            }
        }
    }

    private func showHint(_ message: String) {
        hintTask?.cancel()
        withAnimation(.spring(response: 0.25)) { hint = message }
        hintTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(1.8))
            guard !Task.isCancelled else { return }
            withAnimation(.easeOut(duration: 0.2)) { self?.hint = nil }
        }
    }

    private func showError(_ message: String) {
        hintTask?.cancel()
        withAnimation(.spring(response: 0.25)) { error = message }
        hintTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(3.5))
            guard !Task.isCancelled else { return }
            withAnimation(.easeOut(duration: 0.2)) { self?.error = nil }
        }
    }
}

/// Telegram-style recording & transcribing pill that replaces the composer row while active.
struct VoiceRecordingPill: View {
    @Environment(\.palette) private var palette
    let session: VoiceSession
    let onFinish: () -> Void

    @State private var dotPulse = false

    var body: some View {
        HStack(spacing: 12) {
            switch session.phase {
            case .recording(let locked):
                HStack(spacing: 8) {
                    Circle()
                        .fill(Color.red)
                        .frame(width: 9, height: 9)
                        .opacity(dotPulse ? 0.3 : 1.0)
                        .animation(.easeInOut(duration: 0.65).repeatForever(autoreverses: true), value: dotPulse)
                    Text(VoiceRecorder.formatDuration(session.recorder.duration))
                        .font(.system(.subheadline, design: .rounded).monospacedDigit().weight(.medium))
                        .foregroundStyle(palette.text)
                        .contentTransition(.identity)
                }
                .onAppear { dotPulse = true }
                .onDisappear { dotPulse = false }

                Spacer(minLength: 4)

                if locked {
                    Button {
                        session.cancelRecording()
                    } label: {
                        Text("Cancel")
                            .font(.subheadline.weight(.medium))
                            .foregroundStyle(.red)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 4)
                    }
                    .buttonStyle(.plain)
                    .transition(.opacity.combined(with: .scale(scale: 0.92)))
                } else {
                    HStack(spacing: 4) {
                        Image(systemName: "chevron.left")
                            .font(.caption.weight(.semibold))
                        Text("Slide to cancel")
                            .font(.subheadline)
                    }
                    .foregroundStyle(palette.text2)
                    .offset(x: session.dragOffset.width * 0.55)
                    .opacity(Double(max(0, 1 - session.cancelProgress * 1.15)))
                    .transition(.opacity)
                }

                Spacer(minLength: 4)

                if locked {
                    Button(action: onFinish) {
                        Image(systemName: "arrow.up")
                            .font(.system(size: 15, weight: .bold))
                            .foregroundStyle(.white)
                            .frame(width: Composer.control, height: Composer.control)
                            .background(Color.accentColor.gradient, in: .circle)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Finish and transcribe")
                    .transition(.scale(scale: 0.7).combined(with: .opacity))
                } else {
                    // Leave breathing room on the right for the expanding mic orb centered on the mic button.
                    Color.clear
                        .frame(width: Composer.control + 16, height: Composer.control)
                }

            case .transcribing:
                ProgressView()
                    .controlSize(.small)
                    .tint(Color.accentColor)
                Text("Transcribing with \(VoiceStore.shared.selectedModel.shortName)…")
                    .font(.subheadline)
                    .foregroundStyle(palette.text2)
                    .lineLimit(1)
                Spacer(minLength: 4)
                Button {
                    session.cancelRecording()
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 18))
                        .foregroundStyle(palette.muted)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Cancel transcription")

            case .idle:
                EmptyView()
            }
        }
        .padding(.leading, 14)
        .padding(.trailing, session.isLocked || session.phase == .transcribing ? 4 : 8)
        .frame(minHeight: Composer.control + 6)
        .glassEffect(.regular, in: .capsule)
    }
}

/// Telegram-style floating lock capsule (`lock.open` + `chevron.up`) and audio-reactive mic orb,
/// anchored pixel-for-pixel on the `VoiceMicButton`'s exact center so the orb blooms smoothly
/// directly out of the mic button on touch-down without jumping.
struct VoiceFloatingOverlay: View {
    @Environment(\.palette) private var palette
    let session: VoiceSession
    /// Exact center of `VoiceMicButton` in the composer's coordinate space.
    let micCenter: CGPoint

    private var active: Bool { session.isHolding }
    private var level: CGFloat { CGFloat(session.recorder.level) }

    private static let orbSize: CGFloat = 82
    private static let collapsedScale: CGFloat = Composer.control / orbSize

    var body: some View {
        if micCenter != .zero {
            ZStack {
                // Floating Liquid Glass lock capsule directly above the mic button center.
                VStack(spacing: 6) {
                    Image(systemName: session.lockProgress > 0.72 ? "lock.fill" : "lock.open")
                        .font(.system(size: 17, weight: .medium))
                        .foregroundStyle(session.lockProgress > 0.72 ? Color.accentColor : palette.text)
                        .contentTransition(.symbolEffect(.replace))
                    Image(systemName: "chevron.up")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(palette.text2)
                        .opacity(Double(max(0, 1 - session.lockProgress * 1.35)))
                }
                .frame(width: 44, height: 70)
                .glassEffect(.regular, in: .capsule)
                .scaleEffect(active ? (1.0 - session.cancelProgress * 0.25) : 0.4)
                .opacity(active ? Double(max(0, 1 - session.cancelProgress * 1.45)) : 0)
                .position(x: micCenter.x, y: micCenter.y - (active ? 104 : 24))
                .offset(y: active ? session.dragOffset.height * 0.45 : 0)
                .animation(.spring(response: 0.28, dampingFraction: 0.72), value: active)

                // Expanding mic orb + sensitive organic audio-reactive rings, centered on micCenter.
                TimelineView(.animation(paused: !active)) { timeline in
                    let t = timeline.date.timeIntervalSinceReferenceDate
                    let wobbleA = active ? (0.04 + level * 0.05) * sin(t * 5.2) : 0
                    let wobbleB = active ? (0.04 + level * 0.05) * cos(t * 4.3 + 1.1) : 0
                    let outerScale = 0.94 + level * 0.96
                    let innerScale = 0.96 + level * 0.58

                    ZStack {
                        // Outer soft reactive wave halo (expands up to ~175pt on speech).
                        Circle()
                            .fill(Color.accentColor.opacity(active ? (0.16 + Double(level) * 0.16) : 0))
                            .frame(width: 92, height: 92)
                            .scaleEffect(
                                x: active ? outerScale * (1 + wobbleA) : Self.collapsedScale,
                                y: active ? outerScale * (1 - wobbleA) : Self.collapsedScale
                            )

                        // Inner reactive halo (expands up to ~135pt on speech).
                        Circle()
                            .fill(Color.accentColor.opacity(active ? (0.28 + Double(level) * 0.20) : 0))
                            .frame(width: 86, height: 86)
                            .scaleEffect(
                                x: active ? innerScale * (1 - wobbleB) : Self.collapsedScale,
                                y: active ? innerScale * (1 + wobbleB) : Self.collapsedScale
                            )

                        // Solid accent mic orb blooming directly from the 36pt mic button.
                        Circle()
                            .fill(Color.accentColor.gradient)
                            .frame(width: Self.orbSize, height: Self.orbSize)
                            .shadow(color: Color.accentColor.opacity(active ? 0.42 : 0), radius: 14, y: 4)
                            .scaleEffect(active ? (1.0 + level * 0.08) : Self.collapsedScale)

                        Image(systemName: "mic.fill")
                            .font(.system(size: 28, weight: .semibold))
                            .foregroundStyle(.white)
                            .scaleEffect(active ? 1.0 : 0.55)
                    }
                    .animation(.interactiveSpring(response: 0.12, dampingFraction: 0.72), value: session.recorder.level)
                }
                .scaleEffect(active ? (1.0 - session.cancelProgress * 0.25) : 1.0)
                .position(x: micCenter.x, y: micCenter.y)
                .offset(
                    x: active ? session.dragOffset.width * 0.50 : 0,
                    y: active ? session.dragOffset.height * 0.42 : 0
                )
                .animation(.spring(response: 0.24, dampingFraction: 0.66), value: active)
                .opacity(active ? 1 : 0)
                .animation(.easeOut(duration: active ? 0.06 : 0.16), value: active)
            }
            .allowsHitTesting(false)
            .accessibilityHidden(true)
        }
    }
}

/// Hold-to-talk microphone button for the composer bar.
struct VoiceMicButton: View {
    @Environment(\.palette) private var palette
    let session: VoiceSession
    /// Prominent accent style when standing in for the primary action button; glass style beside Send/Stop.
    let prominent: Bool
    let onTranscript: (String) -> Void

    @State private var pressed = false

    var body: some View {
        Image(systemName: "mic.fill")
            .font(.system(size: 15, weight: .semibold))
            .foregroundStyle(prominent ? Color.accentColor : palette.text)
            .frame(width: Composer.control, height: Composer.control)
            .glassEffect(.regular.interactive(), in: .circle)
            .opacity(session.isActive ? 0.001 : 1)
            .contentShape(.circle)
            .highPriorityGesture(
                DragGesture(minimumDistance: 0, coordinateSpace: .global)
                    .onChanged { value in
                        if !pressed {
                            pressed = true
                            session.pressDown(onTranscript: onTranscript)
                        } else {
                            session.dragChanged(value.translation)
                        }
                    }
                    .onEnded { _ in
                        pressed = false
                        session.dragEnded(onTranscript: onTranscript)
                    }
            )
            .accessibilityLabel("Hold to dictate")
            .accessibilityHint("Press and hold to record, slide left to cancel, or slide up to lock.")
    }
}
