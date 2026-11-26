import CoreML
@preconcurrency import FluidAudio
import Foundation
import Observation
import UIKit
@preconcurrency import WhisperKit

/// Which on-device speech-to-text engine powers a model.
nonisolated enum VoiceBackend: String, Sendable {
    case parakeet
    case whisper

    var title: String {
        switch self {
        case .parakeet: "NVIDIA Parakeet (ANE)"
        case .whisper: "OpenAI Whisper (Core ML)"
        }
    }

    var shortName: String {
        switch self {
        case .parakeet: "Parakeet"
        case .whisper: "Whisper"
        }
    }
}

/// Curated on-device Core ML models running on the iPhone Neural Engine.
nonisolated enum VoiceModel: String, CaseIterable, Identifiable, Sendable {
    case parakeetV3
    case parakeetV2
    case whisperLargeV3Turbo
    case whisperSmall

    var id: String { rawValue }

    var backend: VoiceBackend {
        switch self {
        case .parakeetV3, .parakeetV2: .parakeet
        case .whisperLargeV3Turbo, .whisperSmall: .whisper
        }
    }

    var name: String {
        switch self {
        case .parakeetV3: "Parakeet TDT v3"
        case .parakeetV2: "Parakeet TDT v2"
        case .whisperLargeV3Turbo: "Whisper Large v3 Turbo"
        case .whisperSmall: "Whisper Small"
        }
    }

    var shortName: String {
        switch self {
        case .parakeetV3: "Parakeet v3"
        case .parakeetV2: "Parakeet v2"
        case .whisperLargeV3Turbo: "Whisper Turbo"
        case .whisperSmall: "Whisper Small"
        }
    }

    var badge: String {
        switch self {
        case .parakeetV3: "Recommended · NPU"
        case .parakeetV2: "English · NPU"
        case .whisperLargeV3Turbo: "99 languages"
        case .whisperSmall: "Compact"
        }
    }

    var languages: String {
        switch self {
        case .parakeetV3: "25 European languages (EN, RU, DE, FR, ES, …)"
        case .parakeetV2: "English only"
        case .whisperLargeV3Turbo: "99 languages (auto-detect)"
        case .whisperSmall: "99 languages (auto-detect)"
        }
    }

    var approxSize: String {
        switch self {
        case .parakeetV3: "~480 MB"
        case .parakeetV2: "~480 MB"
        case .whisperLargeV3Turbo: "~630 MB"
        case .whisperSmall: "~250 MB"
        }
    }

    var summary: String {
        switch self {
        case .parakeetV3:
            "Fastest on iPhone Neural Engine (~200 ms). High accuracy across 25 European languages including English and Russian, with strong mixed technical speech."
        case .parakeetV2:
            "English-only Neural Engine transducer. Same blistering speed as v3, tuned specifically for English dictation and coding terms."
        case .whisperLargeV3Turbo:
            "Quantized OpenAI Whisper Large v3 Turbo via Core ML. Supports 99 languages with broad code-switching; slower than Parakeet (~1 s)."
        case .whisperSmall:
            "Compact 99-language OpenAI Whisper model. Smaller download and lower RAM footprint; good fallback for languages outside Parakeet's 25."
        }
    }

    var parakeetVersion: AsrModelVersion? {
        switch self {
        case .parakeetV3: .v3
        case .parakeetV2: .v2
        case .whisperLargeV3Turbo, .whisperSmall: nil
        }
    }

    var whisperVariant: String? {
        switch self {
        case .whisperLargeV3Turbo: "openai_whisper-large-v3-v20240930_turbo_632MB"
        case .whisperSmall: "openai_whisper-small"
        case .parakeetV3, .parakeetV2: nil
        }
    }
}

/// Manages on-device voice model downloads, disk storage, selection, and ANE inference.
@MainActor
@Observable
final class VoiceStore {
    static let shared = VoiceStore()

    nonisolated static let enabledKey = "voice.enabled"
    nonisolated static let modelKey = "voice.model"
    nonisolated static let introSeenKey = "voice.introSeen"

    struct DownloadState: Equatable {
        var fraction: Double
        var phase: String
    }

    /// True once the user has gone through the voice onboarding step or dismissed the What's New popup.
    var introSeen: Bool {
        didSet { UserDefaults.standard.set(introSeen, forKey: Self.introSeenKey) }
    }

    /// Off by default: the composer mic button stays hidden until the user opts in and downloads a model.
    var enabled: Bool {
        didSet {
            UserDefaults.standard.set(enabled, forKey: Self.enabledKey)
            if !enabled { unloadFromMemory() }
        }
    }

    var selectedModel: VoiceModel {
        didSet {
            UserDefaults.standard.set(selectedModel.rawValue, forKey: Self.modelKey)
            if loadedModel != selectedModel { unloadFromMemory() }
        }
    }

    private(set) var downloadedSizes: [VoiceModel: Int64] = [:]
    private(set) var downloads: [VoiceModel: DownloadState] = [:]
    private(set) var loadedModel: VoiceModel?
    private(set) var isLoadingModel = false
    var lastError: String?

    private var downloadTasks: [VoiceModel: Task<Void, Never>] = [:]
    private let runner = VoiceRunner()
    private var memoryObserver: NSObjectProtocol?

    /// True only when voice input is enabled AND the active model is downloaded on disk.
    var showsMicButton: Bool { enabled && isDownloaded(selectedModel) }

    var anyDownloaded: Bool { !downloadedSizes.isEmpty }

    var totalDiskBytes: Int64 { downloadedSizes.values.reduce(0, +) }

    private init() {
        let savedEnabled = UserDefaults.standard.bool(forKey: Self.enabledKey)
        introSeen = savedEnabled || UserDefaults.standard.bool(forKey: Self.introSeenKey)
        enabled = savedEnabled
        if let raw = UserDefaults.standard.string(forKey: Self.modelKey),
           let saved = VoiceModel(rawValue: raw) {
            selectedModel = saved
        } else {
            selectedModel = .parakeetV3
        }
        refreshDiskState()
        memoryObserver = NotificationCenter.default.addObserver(
            forName: UIApplication.didReceiveMemoryWarningNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor in self?.unloadFromMemory() }
        }
    }

    func markIntroSeen() {
        if !introSeen { introSeen = true }
    }

    func isDownloaded(_ model: VoiceModel) -> Bool {
        downloadedSizes[model] != nil
    }

    func isDownloading(_ model: VoiceModel) -> Bool {
        downloads[model] != nil
    }

    func formattedSize(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    /// Re-scans local caches to reflect which models are complete and how much space they use.
    func refreshDiskState() {
        var sizes: [VoiceModel: Int64] = [:]
        for model in VoiceModel.allCases {
            if Self.modelExistsOnDisk(model), let dir = Self.storageDirectory(for: model) {
                sizes[model] = Self.directorySize(at: dir)
            }
        }
        downloadedSizes = sizes
        // If voice was on but the active model vanished, fall back to another installed model or off.
        if enabled && !isDownloaded(selectedModel) {
            if let fallback = VoiceModel.allCases.first(where: { sizes[$0] != nil }) {
                selectedModel = fallback
            }
        }
    }

    func select(_ model: VoiceModel) {
        selectedModel = model
        if isDownloaded(model) {
            enabled = true
        }
    }

    /// Downloads a model (or re-downloads / updates it when `force == true`).
    @discardableResult
    func download(_ model: VoiceModel, force: Bool = false, activateWhenDone: Bool = true) -> Task<Void, Never> {
        if let existing = downloadTasks[model] { return existing }
        lastError = nil
        if force && loadedModel == model {
            unloadFromMemory()
        }
        downloads[model] = DownloadState(fraction: 0, phase: force ? "Updating…" : "Preparing…")
        let task = Task { [weak self] in
            guard let self else { return }
            do {
                try await Self.performDownload(model, force: force) { fraction, phase in
                    Task { @MainActor in
                        guard self.downloads[model] != nil else { return }
                        self.downloads[model] = DownloadState(
                            fraction: min(max(fraction, 0), 1),
                            phase: phase
                        )
                    }
                }
                guard !Task.isCancelled else { return }
                self.downloads.removeValue(forKey: model)
                self.downloadTasks.removeValue(forKey: model)
                self.refreshDiskState()
                if activateWhenDone, self.isDownloaded(model) {
                    self.selectedModel = model
                    self.enabled = true
                }
                Feedback.play(.done)
            } catch is CancellationError {
                self.downloads.removeValue(forKey: model)
                self.downloadTasks.removeValue(forKey: model)
                self.refreshDiskState()
            } catch {
                self.downloads.removeValue(forKey: model)
                self.downloadTasks.removeValue(forKey: model)
                self.refreshDiskState()
                self.lastError = "\(model.shortName): \(error.localizedDescription)"
                Feedback.play(.error)
            }
        }
        downloadTasks[model] = task
        return task
    }

    func cancelDownload(_ model: VoiceModel) {
        downloadTasks[model]?.cancel()
        downloadTasks.removeValue(forKey: model)
        downloads.removeValue(forKey: model)
        refreshDiskState()
    }

    func delete(_ model: VoiceModel) {
        cancelDownload(model)
        if loadedModel == model { unloadFromMemory() }
        if let dir = Self.storageDirectory(for: model) {
            try? FileManager.default.removeItem(at: dir)
        }
        Self.clearWhisperFolderPath(for: model)
        refreshDiskState()
        if !anyDownloaded {
            enabled = false
        }
    }

    func deleteAll() {
        for model in VoiceModel.allCases {
            cancelDownload(model)
            if let dir = Self.storageDirectory(for: model) {
                try? FileManager.default.removeItem(at: dir)
            }
            Self.clearWhisperFolderPath(for: model)
        }
        unloadFromMemory()
        refreshDiskState()
        enabled = false
    }

    /// Preloads the active model onto the ANE while the user is speaking so release-to-text is instant.
    func warmUpIfReady() {
        let model = selectedModel
        guard enabled, isDownloaded(model), loadedModel != model, !isLoadingModel else { return }
        isLoadingModel = true
        let runner = self.runner
        Task { [weak self] in
            do {
                try await runner.ensureLoaded(model)
                await MainActor.run {
                    guard let self else { return }
                    self.isLoadingModel = false
                    if self.selectedModel == model { self.loadedModel = model }
                }
            } catch {
                await MainActor.run {
                    self?.isLoadingModel = false
                }
            }
        }
    }

    func unloadFromMemory() {
        loadedModel = nil
        isLoadingModel = false
        let runner = self.runner
        Task { await runner.unload() }
    }

    /// Transcribes 16 kHz mono Float32 PCM samples using the selected on-device model.
    func transcribe(samples: [Float]) async throws -> String {
        let model = selectedModel
        guard isDownloaded(model) else {
            throw VoiceError.modelNotDownloaded(model)
        }
        if loadedModel != model { isLoadingModel = true }
        defer { isLoadingModel = false }
        let text = try await runner.transcribe(samples: samples, model: model)
        loadedModel = model
        return text
    }

    // MARK: disk paths & download implementation

    private nonisolated static var whisperRootDir: URL {
        let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        return appSupport
            .appendingPathComponent("HQC", isDirectory: true)
            .appendingPathComponent("VoiceModels", isDirectory: true)
            .appendingPathComponent("Whisper", isDirectory: true)
    }

    nonisolated static func storageDirectory(for model: VoiceModel) -> URL? {
        switch model.backend {
        case .parakeet:
            guard let version = model.parakeetVersion else { return nil }
            return AsrModels.defaultCacheDirectory(for: version)
        case .whisper:
            return whisperRootDir.appendingPathComponent(model.rawValue, isDirectory: true)
        }
    }

    private nonisolated static func whisperFolderKey(_ model: VoiceModel) -> String {
        "voice.whisperFolder.\(model.rawValue)"
    }

    nonisolated static func saveWhisperFolderPath(_ path: String, for model: VoiceModel) {
        UserDefaults.standard.set(path, forKey: whisperFolderKey(model))
    }

    nonisolated static func clearWhisperFolderPath(for model: VoiceModel) {
        UserDefaults.standard.removeObject(forKey: whisperFolderKey(model))
    }

    nonisolated static func resolveWhisperFolder(for model: VoiceModel) -> URL? {
        let fm = FileManager.default
        if let saved = UserDefaults.standard.string(forKey: whisperFolderKey(model)),
           fm.fileExists(atPath: (saved as NSString).appendingPathComponent("AudioEncoder.mlmodelc")) {
            return URL(fileURLWithPath: saved, isDirectory: true)
        }
        guard let base = storageDirectory(for: model), fm.fileExists(atPath: base.path) else { return nil }
        if let enumerator = fm.enumerator(at: base, includingPropertiesForKeys: [.isDirectoryKey]) {
            for case let url as URL in enumerator where url.lastPathComponent == "AudioEncoder.mlmodelc" {
                let folder = url.deletingLastPathComponent()
                saveWhisperFolderPath(folder.path, for: model)
                return folder
            }
        }
        return nil
    }

    nonisolated static func modelExistsOnDisk(_ model: VoiceModel) -> Bool {
        switch model.backend {
        case .parakeet:
            guard let version = model.parakeetVersion,
                  let dir = storageDirectory(for: model) else { return false }
            return AsrModels.modelsExist(at: dir, version: version)
        case .whisper:
            return resolveWhisperFolder(for: model) != nil
        }
    }

    private nonisolated static func directorySize(at url: URL) -> Int64 {
        let fm = FileManager.default
        guard let enumerator = fm.enumerator(
            at: url,
            includingPropertiesForKeys: [.fileSizeKey, .isRegularFileKey],
            options: [.skipsHiddenFiles]
        ) else { return 0 }
        var total: Int64 = 0
        for case let fileURL as URL in enumerator {
            if let values = try? fileURL.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey]),
               values.isRegularFile == true,
               let size = values.fileSize {
                total += Int64(size)
            }
        }
        return total
    }

    private nonisolated static func performDownload(
        _ model: VoiceModel,
        force: Bool,
        onProgress: @escaping @Sendable (Double, String) -> Void
    ) async throws {
        switch model.backend {
        case .parakeet:
            guard let version = model.parakeetVersion else { throw VoiceError.modelNotDownloaded(model) }
            let dir = AsrModels.defaultCacheDirectory(for: version)
            _ = try await AsrModels.download(
                to: dir,
                force: force,
                version: version,
                progressHandler: { progress in
                    let phase: String = switch progress.phase {
                    case .listing: "Checking model files…"
                    case .downloading(let done, let total):
                        total > 0 ? "Downloading (\(done)/\(total))…" : "Downloading…"
                    case .compiling: "Compiling for Neural Engine…"
                    }
                    onProgress(progress.fractionCompleted, phase)
                }
            )
        case .whisper:
            guard let variant = model.whisperVariant,
                  let baseDir = storageDirectory(for: model) else {
                throw VoiceError.modelNotDownloaded(model)
            }
            let fm = FileManager.default
            if force, fm.fileExists(atPath: baseDir.path) {
                try? fm.removeItem(at: baseDir)
                clearWhisperFolderPath(for: model)
            }
            try fm.createDirectory(at: baseDir, withIntermediateDirectories: true)
            onProgress(0.02, "Contacting Hugging Face…")
            let folder = try await WhisperKit.download(
                variant: variant,
                downloadBase: baseDir,
                progressCallback: { progress in
                    onProgress(progress.fractionCompleted, "Downloading…")
                }
            )
            saveWhisperFolderPath(folder.path, for: model)
            // Trigger one-time Core ML specialization on-device right after download so first dictation is fast.
            onProgress(0.95, "Optimizing for Neural Engine…")
            let compute = ModelComputeOptions(
                melCompute: .cpuAndGPU,
                audioEncoderCompute: .cpuAndNeuralEngine,
                textDecoderCompute: .cpuAndNeuralEngine
            )
            let config = WhisperKitConfig(
                modelFolder: folder.path,
                computeOptions: compute,
                verbose: false,
                prewarm: true,
                load: false,
                download: false
            )
            let kit = try await WhisperKit(config)
            await kit.unloadModels()
            onProgress(1.0, "Ready")
        }
    }
}

nonisolated enum VoiceError: LocalizedError, Sendable {
    case modelNotDownloaded(VoiceModel)
    case microphoneDenied
    case recordingTooShort
    case emptyTranscript

    var errorDescription: String? {
        switch self {
        case .modelNotDownloaded(let m):
            "\(m.name) is not downloaded yet. Download it in Settings → Voice input."
        case .microphoneDenied:
            "Microphone access is turned off. Allow HQC in Settings to dictate."
        case .recordingTooShort:
            "Hold to record"
        case .emptyTranscript:
            "No speech detected."
        }
    }
}

/// Serializes heavy Core ML model loading and transcription off the main actor.
private actor VoiceRunner {
    private var loaded: VoiceModel?
    private var parakeet: AsrManager?
    private var whisper: WhisperKit?

    func ensureLoaded(_ model: VoiceModel) async throws {
        if loaded == model { return }
        await unload()
        switch model.backend {
        case .parakeet:
            guard let version = model.parakeetVersion else { throw VoiceError.modelNotDownloaded(model) }
            let dir = AsrModels.defaultCacheDirectory(for: version)
            guard AsrModels.modelsExist(at: dir, version: version) else {
                throw VoiceError.modelNotDownloaded(model)
            }
            let models = try await AsrModels.load(from: dir, version: version)
            let manager = AsrManager(config: .default)
            try await manager.loadModels(models)
            parakeet = manager
            loaded = model
        case .whisper:
            guard let folder = VoiceStore.resolveWhisperFolder(for: model) else {
                throw VoiceError.modelNotDownloaded(model)
            }
            let compute = ModelComputeOptions(
                melCompute: .cpuAndGPU,
                audioEncoderCompute: .cpuAndNeuralEngine,
                textDecoderCompute: .cpuAndNeuralEngine
            )
            let config = WhisperKitConfig(
                modelFolder: folder.path,
                computeOptions: compute,
                verbose: false,
                prewarm: false,
                load: true,
                download: false
            )
            whisper = try await WhisperKit(config)
            loaded = model
        }
    }

    func transcribe(samples: [Float], model: VoiceModel) async throws -> String {
        try await ensureLoaded(model)
        switch model.backend {
        case .parakeet:
            guard let manager = parakeet else { throw VoiceError.modelNotDownloaded(model) }
            var state = TdtDecoderState.make()
            let result = try await manager.transcribe(samples, decoderState: &state)
            return cleanTranscript(result.text)
        case .whisper:
            guard let kit = whisper else { throw VoiceError.modelNotDownloaded(model) }
            let options = DecodingOptions(
                task: .transcribe,
                language: nil,
                usePrefillPrompt: true,
                detectLanguage: true,
                skipSpecialTokens: true,
                withoutTimestamps: true
            )
            let results = try await kit.transcribe(audioArray: samples, decodeOptions: options)
            let joined = results.map(\.text).joined(separator: " ")
            return cleanTranscript(joined)
        }
    }

    func unload() async {
        await parakeet?.cleanup()
        parakeet = nil
        if let kit = whisper {
            await kit.unloadModels()
            whisper = nil
        }
        loaded = nil
    }

    private func cleanTranscript(_ raw: String) -> String {
        raw
            .replacingOccurrences(of: #"\s+"#, with: " ", options: .regularExpression)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
