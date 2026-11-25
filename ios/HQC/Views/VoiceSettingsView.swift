import AVFoundation
import SwiftUI

/// Settings → Voice input: opt-in toggle, model selection across Parakeet (ANE) and Whisper
/// (Core ML), downloading, re-downloading/updating, deleting models, and memory control.
struct VoiceSettingsView: View {
    @Environment(\.openURL) private var openURL
    @State private var voice = VoiceStore.shared
    @State private var confirmDeleteAll = false
    @State private var confirmDelete: VoiceModel?

    var body: some View {
        Form {
            Section {
                Toggle(isOn: Binding(
                    get: { voice.enabled },
                    set: { on in
                        voice.enabled = on
                        if on { voice.markIntroSeen() }
                        Feedback.play(.tap)
                    }
                )) {
                    Label("Voice input", systemImage: "mic.fill")
                }
            } footer: {
                if voice.enabled && !voice.isDownloaded(voice.selectedModel) {
                    Text("Download a model below to show the hold-to-talk microphone button in chat.")
                } else {
                    Text("Off by default. When enabled, hold the microphone button in any chat to dictate on device—slide left to cancel or slide up to lock hands-free. Audio never leaves your iPhone.")
                }
            }

            if let err = voice.lastError {
                Section {
                    Label(err, systemImage: "exclamationmark.triangle.fill")
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            }

            Section {
                ForEach(VoiceModel.allCases.filter { $0.backend == .parakeet }) { model in
                    VoiceModelSettingsRow(model: model, voice: voice) {
                        confirmDelete = model
                    }
                }
            } header: {
                Text("NVIDIA Parakeet · Neural Engine")
            } footer: {
                Text("FastConformer-TDT compiled for the Apple Neural Engine via FluidAudio. Sub-second transcription with low battery and thermal impact.")
            }

            Section {
                ForEach(VoiceModel.allCases.filter { $0.backend == .whisper }) { model in
                    VoiceModelSettingsRow(model: model, voice: voice) {
                        confirmDelete = model
                    }
                }
            } header: {
                Text("OpenAI Whisper · Core ML")
            } footer: {
                Text("WhisperKit Core ML models with 99-language auto-detection. Slower than Parakeet, useful for languages outside Parakeet's 25 European languages.")
            }

            Section("Storage & memory") {
                LabeledContent("Downloaded models", value: voice.anyDownloaded ? voice.formattedSize(voice.totalDiskBytes) : "None")
                LabeledContent("Loaded in memory", value: voice.loadedModel?.shortName ?? "Idle")
                if voice.loadedModel != nil {
                    Button("Unload from memory") {
                        voice.unloadFromMemory()
                        Feedback.play(.tap)
                    }
                }
                if voice.anyDownloaded {
                    Button("Delete all models", role: .destructive) {
                        confirmDeleteAll = true
                    }
                }
            }

            Section {
                LabeledContent("Microphone") {
                    Text(micStatusLabel)
                        .foregroundStyle(VoiceRecorder.permissionStatus == .denied ? .red : .secondary)
                }
                if VoiceRecorder.permissionStatus == .denied {
                    Button("Open iOS Settings", systemImage: "gear") {
                        if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                    }
                }
            } footer: {
                Text("Models unload automatically when HQC goes to the background or under memory pressure.")
            }
        }
        .navigationTitle("Voice input")
        .navigationBarTitleDisplayMode(.inline)
        .paletteBackground()
        .onAppear { voice.refreshDiskState() }
        .confirmationDialog(
            "Delete \(confirmDelete?.name ?? "model")?",
            isPresented: Binding(get: { confirmDelete != nil }, set: { if !$0 { confirmDelete = nil } }),
            titleVisibility: .visible
        ) {
            if let model = confirmDelete {
                Button("Delete", role: .destructive) {
                    voice.delete(model)
                    confirmDelete = nil
                    Feedback.play(.tap)
                }
            }
        } message: {
            Text("Removes the model files from this device. You can download it again anytime.")
        }
        .confirmationDialog(
            "Delete all voice models?",
            isPresented: $confirmDeleteAll,
            titleVisibility: .visible
        ) {
            Button("Delete all", role: .destructive) {
                voice.deleteAll()
                Feedback.play(.tap)
            }
        } message: {
            Text("Frees \(voice.formattedSize(voice.totalDiskBytes)) and turns off voice input.")
        }
    }

    private var micStatusLabel: String {
        switch VoiceRecorder.permissionStatus {
        case .granted: "Allowed"
        case .denied: "Denied"
        case .undetermined: "Not asked yet"
        @unknown default: "Unknown"
        }
    }
}

private struct VoiceModelSettingsRow: View {
    let model: VoiceModel
    let voice: VoiceStore
    let onRequestDelete: () -> Void

    private var downloaded: Bool { voice.isDownloaded(model) }
    private var selected: Bool { voice.selectedModel == model && downloaded }
    private var progress: VoiceStore.DownloadState? { voice.downloads[model] }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(model.name)
                    .font(.headline)
                Text(model.badge)
                    .font(.caption2.weight(.semibold))
                    .padding(.horizontal, 7)
                    .padding(.vertical, 2)
                    .background(Color.accentColor.opacity(0.15), in: .capsule)
                    .foregroundStyle(Color.accentColor)
                Spacer(minLength: 4)
                if selected {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(Color.accentColor)
                }
            }

            Text(model.summary)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: 12) {
                Label(model.languages, systemImage: "globe")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Spacer()
                if let bytes = voice.downloadedSizes[model] {
                    Text(voice.formattedSize(bytes))
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                } else {
                    Text(model.approxSize)
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.tertiary)
                }
            }

            if let progress {
                VStack(alignment: .leading, spacing: 4) {
                    ProgressView(value: progress.fraction)
                        .tint(.accentColor)
                    HStack {
                        Text(progress.phase)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Text("\(Int((progress.fraction * 100).rounded()))%")
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                        Button("Cancel") {
                            voice.cancelDownload(model)
                            Feedback.play(.tap)
                        }
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.red)
                    }
                }
                .padding(.top, 2)
            } else if downloaded {
                HStack(spacing: 10) {
                    if !selected || !voice.enabled {
                        Button("Use") {
                            voice.select(model)
                            Feedback.play(.select)
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                    } else {
                        Text("Active")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.green)
                    }
                    Spacer()
                    Button {
                        voice.download(model, force: true, activateWhenDone: selected)
                        Feedback.play(.tap)
                    } label: {
                        Label("Update", systemImage: "arrow.clockwise")
                            .font(.caption.weight(.medium))
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)

                    Button(role: .destructive) {
                        onRequestDelete()
                    } label: {
                        Image(systemName: "trash")
                            .font(.caption.weight(.medium))
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .accessibilityLabel("Delete \(model.name)")
                }
                .padding(.top, 2)
            } else {
                HStack {
                    Button {
                        voice.download(model, force: false, activateWhenDone: true)
                        Feedback.play(.tap)
                    } label: {
                        Label("Download (\(model.approxSize))", systemImage: "arrow.down.circle")
                            .font(.subheadline.weight(.medium))
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    Spacer()
                }
                .padding(.top, 2)
            }
        }
        .padding(.vertical, 4)
        .contentShape(.rect)
        .onTapGesture {
            if downloaded {
                voice.select(model)
                Feedback.play(.select)
            }
        }
    }
}

/// Compact glass model cards for the onboarding voice step.
struct VoiceOnboardingPicker: View {
    @Environment(\.palette) private var palette
    @Binding var picked: VoiceModel
    let voice: VoiceStore

    var body: some View {
        VStack(spacing: 10) {
            ForEach(VoiceModel.allCases) { model in
                let active = picked == model
                let progress = voice.downloads[model]
                let ready = voice.isDownloaded(model)
                Button {
                    picked = model
                    Feedback.play(.select)
                } label: {
                    VStack(alignment: .leading, spacing: 6) {
                        HStack(spacing: 8) {
                            Image(systemName: active ? "checkmark.circle.fill" : "circle")
                                .foregroundStyle(active ? Color.accentColor : palette.muted)
                            Text(model.name)
                                .font(.subheadline.weight(.semibold))
                                .foregroundStyle(palette.text)
                            Text(model.badge)
                                .font(.caption2.weight(.semibold))
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(Color.accentColor.opacity(0.16), in: .capsule)
                                .foregroundStyle(Color.accentColor)
                            Spacer(minLength: 4)
                            Text(ready ? "Ready" : model.approxSize)
                                .font(.caption2.monospacedDigit())
                                .foregroundStyle(ready ? .green : palette.text2)
                        }
                        Text(model.summary)
                            .font(.caption)
                            .foregroundStyle(palette.text2)
                            .multilineTextAlignment(.leading)
                            .fixedSize(horizontal: false, vertical: true)
                        if let progress {
                            VStack(alignment: .leading, spacing: 3) {
                                ProgressView(value: progress.fraction)
                                    .tint(.accentColor)
                                HStack {
                                    Text(progress.phase)
                                        .font(.caption2)
                                        .foregroundStyle(palette.text2)
                                    Spacer()
                                    Text("\(Int((progress.fraction * 100).rounded()))%")
                                        .font(.caption2.monospacedDigit())
                                        .foregroundStyle(palette.text2)
                                }
                            }
                            .padding(.top, 2)
                        }
                    }
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .glassEffect(.regular, in: .rect(cornerRadius: 16))
                    .overlay {
                        RoundedRectangle(cornerRadius: 16)
                            .strokeBorder(active ? Color.accentColor : palette.border.opacity(0.6),
                                          lineWidth: active ? 1.8 : 0.8)
                    }
                }
                .buttonStyle(.plain)
            }
        }
    }
}

/// Full-screen onboarding-style announcement shown once to existing beta users updating to a build
/// with on-device voice transcription, letting them opt in and pick a model or dismiss it.
struct VoiceAnnouncementView: View {
    @Environment(\.palette) private var palette
    @State private var voice = VoiceStore.shared
    @State private var picked: VoiceModel = .parakeetV3
    @State private var pickingModel = false
    @State private var appeared = false

    let onDone: () -> Void

    var body: some View {
        ZStack {
            BackdropGradient()
            VStack(spacing: 0) {
                Spacer(minLength: 20)
                VStack(spacing: 22) {
                    VStack(spacing: 14) {
                        Text("WHAT'S NEW")
                            .font(.caption.weight(.bold))
                            .tracking(1.2)
                            .padding(.horizontal, 12)
                            .padding(.vertical, 5)
                            .background(Color.accentColor.opacity(0.18), in: .capsule)
                            .foregroundStyle(Color.accentColor)

                        Image(systemName: "waveform.and.mic")
                            .font(.system(size: 46, weight: .semibold))
                            .foregroundStyle(.tint)
                            .symbolEffect(.bounce.up, options: .nonRepeating, value: appeared)

                        Text(pickingModel ? "Choose a voice model" : "On-device voice input")
                            .font(.title.weight(.bold))
                            .multilineTextAlignment(.center)
                            .fixedSize(horizontal: false, vertical: true)

                        Text(pickingModel
                             ? "Models run locally on your iPhone's Neural Engine—nothing is sent to Apple or the cloud."
                             : "Hold to dictate messages right into chat, with slide-left to cancel and slide-up to lock hands-free. Powered on device by Parakeet or Whisper.")
                            .font(.body)
                            .foregroundStyle(palette.text2)
                            .multilineTextAlignment(.center)
                    }

                    if pickingModel {
                        ScrollView(showsIndicators: false) {
                            VoiceOnboardingPicker(picked: $picked, voice: voice)
                                .padding(.vertical, 2)
                        }
                        .frame(maxHeight: 330)
                        .transition(.move(edge: .trailing).combined(with: .opacity))

                        if let err = voice.lastError {
                            Text(err)
                                .font(.footnote)
                                .foregroundStyle(palette.danger)
                                .multilineTextAlignment(.center)
                        }

                        let downloading = voice.isDownloading(picked)
                        let ready = voice.isDownloaded(picked)
                        if ready {
                            primary("Use \(picked.shortName) & continue") {
                                voice.select(picked)
                                onDone()
                            }
                        } else if downloading {
                            primary("Continue while downloading") {
                                onDone()
                            }
                        } else {
                            primary("Download \(picked.shortName) (\(picked.approxSize))") {
                                let model = picked
                                Task {
                                    let task = voice.download(model, force: false, activateWhenDone: true)
                                    await task.value
                                    if voice.isDownloaded(model), !voice.introSeen {
                                        onDone()
                                    }
                                }
                            }
                        }
                        Button("Not now") { onDone() }
                            .font(.subheadline)
                    } else {
                        VStack(alignment: .leading, spacing: 14) {
                            bullet("cpu", "Runs on iPhone Neural Engine",
                                   "Choose NVIDIA Parakeet (~200 ms, EN + RU + 23 languages) or OpenAI Whisper (99 languages).")
                            bullet("mic.badge.plus", "Hold to talk, slide to lock",
                                   "Press and hold the mic button in chat—slide left to cancel or slide up to record hands-free.")
                            bullet("lock.shield", "Private & off by default",
                                   "Audio stays on your device. Manage, update or delete models anytime in Settings.")
                        }
                        .padding(.vertical, 6)
                        .transition(.opacity)

                        primary("Set up voice input") {
                            withAnimation(.snappy(duration: 0.38)) { pickingModel = true }
                            Feedback.play(.step)
                        }
                        Button("Not now") { onDone() }
                            .font(.subheadline)
                    }
                }
                .frame(maxWidth: 460)
                Spacer(minLength: 20)
            }
            .padding(.horizontal, 28)
        }
        .onAppear {
            appeared = true
            Feedback.play(.arrive, haptic: false)
        }
    }

    private func bullet(_ icon: String, _ title: String, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 14) {
            Image(systemName: icon)
                .font(.title3)
                .foregroundStyle(.tint)
                .frame(width: 30)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.headline)
                Text(text).font(.subheadline).foregroundStyle(palette.text2)
            }
        }
    }

    private func primary(_ title: String, action: @escaping () -> Void) -> some View {
        Button {
            Feedback.play(.tap)
            action()
        } label: {
            Text(title)
                .font(.headline)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 4)
        }
        .buttonStyle(.glassProminent)
        .controlSize(.large)
    }
}
