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
    private var hintTask: Task<Void, Never>?
    private var startAttemptID = UUID()

    static let cancelThreshold: CGFloat = -105
    static let lockThreshold: CGFloat = -66

    var isActive: Bool { phase != .idle }
    var isRecording: Bool {
        if case .recording = phase { return true }
        return false
    }
    var isLocked: Bool { phase == .recording(locked: true) }

    /// 0…1 progress toward the left cancel gesture.
    var cancelProgress: CGFloat {
        guard case .recording(locked: false) = phase else { return 0 }
        return min(1, max(0, -dragOffset.width / abs(Self.cancelThreshold)))
    }

    /// 0…1 progress toward the upward lock gesture.
    var lockProgress: CGFloat {
        guard case .recording(locked: false) = phase else { return 0 }
        return min(1, max(0, -dragOffset.height / abs(Self.lockThreshold)))
    }

    /// Begins hold-to-talk recording when the user presses down on the mic button.
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
        do {
            try recorder.start { [weak self] in
                self?.finishRecording(onTranscript: onTranscript)
            }
            phase = .recording(locked: false)
            Feedback.play(.voiceStart)
            // Warm up the Core ML model on the ANE while the user is speaking.
            VoiceStore.shared.warmUpIfReady()
        } catch {
            showError(error.localizedDescription)
            Feedback.play(.error)
        }
    }

    func dragChanged(_ translation: CGSize) {
        guard case .recording(locked: false) = phase else { return }
        // Lock the dominant axis (either sliding left to cancel OR sliding up to lock), like Telegram.
        if abs(translation.height) > abs(translation.width) && translation.height < 0 {
            dragOffset = CGSize(width: 0, height: max(translation.height, Self.lockThreshold - 24))
            if translation.height <= Self.lockThreshold {
                withAnimation(.spring(response: 0.28, dampingFraction: 0.78)) {
                    phase = .recording(locked: true)
                    dragOffset = .zero
                }
                Feedback.play(.voiceLock)
            }
        } else if translation.width < 0 {
            dragOffset = CGSize(width: max(translation.width, Self.cancelThreshold - 20), height: 0)
            if translation.width <= Self.cancelThreshold {
                cancelRecording()
            }
        } else {
            dragOffset = .zero
        }
    }

    func dragEnded(onTranscript: @escaping (String) -> Void) {
        // Invalidate any pending async permission check if finger lifted.
        startAttemptID = UUID()
        guard case .recording(locked: false) = phase else { return }
        if dragOffset.width <= Self.cancelThreshold {
            cancelRecording()
        } else {
            finishRecording(onTranscript: onTranscript)
        }
    }

    func cancelRecording() {
        startAttemptID = UUID()
        transcribeTask?.cancel()
        transcribeTask = nil
        recorder.cancel()
        withAnimation(.spring(response: 0.28, dampingFraction: 0.82)) {
            phase = .idle
            dragOffset = .zero
        }
        Feedback.play(.voiceCancel)
    }

    func finishRecording(onTranscript: @escaping (String) -> Void) {
        guard case .recording = phase else { return }
        let (samples, elapsed) = recorder.finish()
        if elapsed < VoiceRecorder.minDuration || samples.count < 3200 {
            withAnimation(.spring(response: 0.26, dampingFraction: 0.82)) {
                phase = .idle
                dragOffset = .zero
            }
            showHint("Hold to record")
            Feedback.play(.tap)
            return
        }

        withAnimation(.spring(response: 0.28, dampingFraction: 0.82)) {
            phase = .transcribing
            dragOffset = .zero
        }
        Feedback.play(.tap)

        transcribeTask?.cancel()
        transcribeTask = Task { [weak self] in
            guard let self else { return }
            do {
                let text = try await VoiceStore.shared.transcribe(samples: samples)
                guard !Task.isCancelled else { return }
                withAnimation(.spring(response: 0.28, dampingFraction: 0.85)) {
                    self.phase = .idle
                }
                if text.isEmpty {
                    self.showHint("No speech detected")
                } else {
                    onTranscript(text)
                    Feedback.play(.answered)
                }
            } catch is CancellationError {
                withAnimation(.spring(response: 0.25)) { self.phase = .idle }
            } catch {
                withAnimation(.spring(response: 0.25)) { self.phase = .idle }
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
                            .padding(.horizontal, 8)
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
                    .transition(.scale.combined(with: .opacity))
                } else {
                    // Reserve space on the right for the expanding Telegram mic orb overlay.
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

/// Floating Telegram-style lock capsule (`lock.open` + `chevron.up`) and audio-reactive blue mic orb
/// rendered over the bottom-right corner of the composer while holding to record.
struct VoiceFloatingOverlay: View {
    @Environment(\.palette) private var palette
    let session: VoiceSession

    private var level: CGFloat { CGFloat(session.recorder.level) }

    var body: some View {
        if case .recording(locked: false) = session.phase {
            VStack(spacing: 14) {
                // Floating lock capsule above the mic orb (matches Telegram's Liquid Glass padlock pill).
                VStack(spacing: 6) {
                    Image(systemName: session.lockProgress > 0.72 ? "lock.fill" : "lock.open")
                        .font(.system(size: 17, weight: .medium))
                        .foregroundStyle(session.lockProgress > 0.72 ? Color.accentColor : palette.text)
                        .contentTransition(.symbolEffect(.replace))
                    Image(systemName: "chevron.up")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(palette.text2)
                        .opacity(Double(max(0, 1 - session.lockProgress * 1.3)))
                }
                .frame(width: 44, height: 68)
                .glassEffect(.regular, in: .capsule)
                .offset(y: session.dragOffset.height * 0.45)
                .scaleEffect(1.0 - session.cancelProgress * 0.25)
                .opacity(Double(max(0, 1 - session.cancelProgress * 1.4)))
                .transition(.move(edge: .bottom).combined(with: .opacity).combined(with: .scale(scale: 0.75)))

                // Audio-reactive expanding mic orb.
                ZStack {
                    Circle()
                        .fill(Color.accentColor.opacity(0.18))
                        .frame(width: 112, height: 112)
                        .scaleEffect(0.86 + level * 0.52)
                    Circle()
                        .fill(Color.accentColor.opacity(0.32))
                        .frame(width: 96, height: 96)
                        .scaleEffect(0.90 + level * 0.34)
                    Circle()
                        .fill(Color.accentColor.gradient)
                        .frame(width: 78, height: 78)
                        .shadow(color: Color.accentColor.opacity(0.45), radius: 14, y: 4)
                    Image(systemName: "mic.fill")
                        .font(.system(size: 28, weight: .semibold))
                        .foregroundStyle(.white)
                }
                .animation(.easeOut(duration: 0.08), value: session.recorder.level)
                .offset(
                    x: session.dragOffset.width * 0.35,
                    y: session.dragOffset.height * 0.35
                )
                .scaleEffect(1.0 - session.cancelProgress * 0.22)
                .transition(.scale(scale: 0.45).combined(with: .opacity))
            }
            .allowsHitTesting(false)
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
            .opacity(session.isRecording && !session.isLocked ? 0.01 : 1)
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
