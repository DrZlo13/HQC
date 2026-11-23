import QuickLook
import SwiftUI

/// Artifacts by id, loaded once per launch: the metadata, and the bytes of what was opened or
/// shown as an image (SPEC §12).
@MainActor
@Observable
final class ArtifactStore {
    private(set) var meta: [String: Artifact] = [:]
    private(set) var images: [String: UIImage] = [:]
    private var files: [String: URL] = [:]
    private var loading = Set<String>()

    private var done = Set<String>()

    /// Loads once per id. The work outlives the view that asked: a message re-drawn when the
    /// hub's copy replaces the one being sent must not cancel its image half-way.
    func load(_ id: String, client: HubClient?) async {
        guard let client, !done.contains(id), !loading.contains(id) else { return }
        loading.insert(id)
        let work = Task { () -> (Artifact?, Data?) in
            guard let a = try? await client.artifact(id) else { return (nil, nil) }
            guard a.isImage, (a.size ?? 0) < 20_000_000 else { return (a, nil) }
            return (a, try? await client.artifactData(id))
        }
        let (a, data) = await work.value
        loading.remove(id)
        guard let a else { return }
        meta[id] = a
        if let data, let image = UIImage(data: data) {
            images[id] = image
            files[id] = write(data, name: a.name, id: id)
        }
        done.insert(id)
    }

    /// A local copy for QuickLook, named as the file.
    func file(_ id: String, client: HubClient?) async throws -> URL {
        if let f = files[id] { return f }
        guard let client else { throw HubError.signInRequired }
        let a: Artifact
        if let m = meta[id] { a = m } else { a = try await client.artifact(id) }
        let data = try await client.artifactData(id)
        let url = write(data, name: a.name, id: id)
        files[id] = url
        return url
    }

    private func write(_ data: Data, name: String, id: String) -> URL {
        let dir = FileManager.default.temporaryDirectory.appending(path: "artifacts/\(id)", directoryHint: .isDirectory)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let safe = name.replacingOccurrences(of: "/", with: "_")
        let url = dir.appending(path: safe.isEmpty ? "file" : safe)
        try? data.write(to: url)
        return url
    }
}

/// An artifact in the feed: an image as a thumbnail, anything else as a chip; a tap opens it
/// in QuickLook (or a published page at its public link).
struct ArtifactView: View {
    @Environment(AppModel.self) private var app
    let id: String
    var compact = false
    @State private var unpublished = false
    @State private var preview: URL?
    @State private var opening = false

    private var meta: Artifact? { app.artifacts.meta[id] }

    var body: some View {
        Group {
            if let image = app.artifacts.images[id] {
                Button { open() } label: {
                    Image(uiImage: image)
                        .resizable()
                        .scaledToFill()
                        .frame(maxWidth: compact ? 120 : 260, maxHeight: compact ? 120 : 260)
                        .clipShape(.rect(cornerRadius: 14))
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
            } else if let meta, meta.kind == "site", let link = meta.public_url, let url = URL(string: link) {
                Link(destination: url) {
                    chip(icon: "globe", title: meta.name, detail: unpublished ? "Unpublished" : "Published page")
                }
                .disabled(unpublished)
                .contextMenu {
                    Button("Copy link", systemImage: "link") { UIPasteboard.general.string = link }
                    ShareLink("Share", item: url)
                    if !unpublished {
                        Button("Unpublish", systemImage: "eye.slash", role: .destructive) {
                            Task {
                                do {
                                    try await app.client?.unpublishArtifact(id)
                                    unpublished = true
                                    Feedback.play(.success)
                                } catch {}
                            }
                        }
                    }
                }
            } else {
                Button { open() } label: {
                    chip(icon: icon, title: meta?.name ?? "Attachment",
                         detail: meta?.size.map { ByteCountFormatter.string(fromByteCount: Int64($0), countStyle: .file) })
                }
                .buttonStyle(.plain)
            }
        }
        .task { await app.artifacts.load(id, client: app.client) }
        .quickLookPreview($preview)
    }

    private var icon: String {
        switch meta?.kind {
        case "diff": "plus.forwardslash.minus"
        case "html": "chevron.left.forwardslash.chevron.right"
        case "text": "doc.plaintext"
        case "image": "photo"
        default: "doc"
        }
    }

    private func chip(icon: String, title: String, detail: String?) -> some View {
        HStack(spacing: 10) {
            if opening {
                ProgressView().controlSize(.small)
            } else {
                Image(systemName: icon).foregroundStyle(.tint)
            }
            VStack(alignment: .leading, spacing: 1) {
                Text(title).font(.subheadline.weight(.medium)).lineLimit(1)
                if let detail { Text(detail).font(.caption2).foregroundStyle(.secondary) }
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.fill.tertiary, in: .rect(cornerRadius: 14))
    }

    private func open() {
        Task {
            opening = true
            defer { opening = false }
            preview = try? await app.artifacts.file(id, client: app.client)
        }
    }
}

/// Several artifacts in a row that wraps.
struct ArtifactStrip: View {
    let ids: [String]
    var alignment: HorizontalAlignment = .leading

    var body: some View {
        VStack(alignment: alignment, spacing: 6) {
            ForEach(ids, id: \.self) { ArtifactView(id: $0) }
        }
    }
}
