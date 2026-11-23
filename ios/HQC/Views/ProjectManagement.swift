import PhotosUI
import SwiftUI

// The web client's project and session management that the app lacked (client/src/ui/
// Appearance.tsx, ProjectSettings.tsx, Archive.tsx, sessions.ts): a project's icon image,
// colour and quick messages, archived projects, the session list's order, moving a session.

// MARK: the icon image

/// A project's uploaded icon, fetched signed in and kept for the run of the app.
struct ProjectIconImage: View {
    @Environment(AppModel.self) private var app
    let url: String
    @State private var image: UIImage?

    private static var cache: [String: UIImage] = [:]

    var body: some View {
        Group {
            if let image = image ?? Self.cache[url] {
                Image(uiImage: image).resizable().scaledToFill()
            } else {
                Color.clear
            }
        }
        .task(id: url) {
            guard Self.cache[url] == nil, let client = app.client,
                  let data = try? await client.bytes(at: url), let img = UIImage(data: data) else { return }
            Self.cache[url] = img
            image = img
        }
    }
}

// MARK: project settings

/// A project's name, description, colour, icon and quick messages.
struct ProjectSettingsSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @Environment(\.palette) private var palette
    let project: Project

    @State private var name = ""
    @State private var about = ""
    @State private var color = "teal"
    @State private var iconKind = "letters"
    @State private var iconValue = ""
    @State private var quick: [String] = []
    @State private var newQuick = ""
    @State private var photo: PhotosPickerItem?
    @State private var busy = false
    @State private var error: String?
    @State private var confirmDelete = false

    static let colors = ["teal", "plum", "blue", "rose", "moss", "coral", "cyan", "slate"]

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    HStack {
                        Spacer()
                        preview
                        Spacer()
                    }
                    .listRowBackground(Color.clear)
                }
                if !project.is_personal {
                    Section("Name") {
                        TextField("Name", text: $name)
                        TextField("Description", text: $about, axis: .vertical).lineLimit(1...3)
                    }
                }
                Section("Colour") {
                    LazyVGrid(columns: Array(repeating: GridItem(.flexible()), count: 8), spacing: 10) {
                        ForEach(Self.colors, id: \.self) { c in
                            Circle()
                                .fill(Project.tint(of: c))
                                .frame(width: 28, height: 28)
                                .overlay(Circle().stroke(palette.text, lineWidth: color == c ? 2 : 0).padding(-3))
                                .onTapGesture {
                                    color = c
                                    Feedback.play(.select)
                                }
                                .accessibilityLabel(c)
                                .accessibilityAddTraits(color == c ? .isSelected : [])
                        }
                    }
                    .padding(.vertical, 4)
                }
                Section {
                    Picker("Icon", selection: $iconKind) {
                        Text("Letters").tag("letters")
                        Text("Emoji").tag("emoji")
                        Text("Image").tag("image")
                    }
                    .pickerStyle(.segmented)
                    switch iconKind {
                    case "emoji":
                        TextField("One emoji", text: $iconValue)
                            .onChange(of: iconValue) { _, v in
                                if let first = v.first, v.count > 1 { iconValue = String(first) }
                            }
                    case "image":
                        PhotosPicker(selection: $photo, matching: .images) {
                            Label(project.icon?.kind == "image" ? "Choose another image" : "Choose an image",
                                  systemImage: "photo")
                        }
                    default:
                        TextField("Up to 2 letters", text: $iconValue)
                            .textInputAutocapitalization(.characters)
                            .onChange(of: iconValue) { _, v in
                                if v.count > 2 { iconValue = String(v.prefix(2)) }
                            }
                    }
                } header: {
                    Text("Icon")
                } footer: {
                    if iconKind == "image" {
                        Text("Cropped to a square and uploaded at once.")
                    }
                }
                Section {
                    ForEach(quick, id: \.self) { q in Text(q).lineLimit(2) }
                        .onDelete { quick.remove(atOffsets: $0) }
                        .onMove { quick.move(fromOffsets: $0, toOffset: $1) }
                    HStack {
                        TextField("New quick message", text: $newQuick, axis: .vertical)
                        Button("Add", systemImage: "plus.circle.fill") {
                            let t = newQuick.trimmingCharacters(in: .whitespacesAndNewlines)
                            guard !t.isEmpty, !quick.contains(t) else { return }
                            quick.append(t)
                            newQuick = ""
                        }
                        .labelStyle(.iconOnly)
                        .disabled(newQuick.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                } header: {
                    Text("Quick messages")
                } footer: {
                    Text("Sent again with one tap from the input's + menu.")
                }
                if let error {
                    Section { Text(error).foregroundStyle(palette.danger).font(.footnote) }
                }
                if !project.is_personal {
                    Section {
                        Button("Delete project", systemImage: "trash", role: .destructive) { confirmDelete = true }
                    } footer: {
                        Text("Deletes the project with its sessions and their history. The folders on the machines stay.")
                    }
                }
            }
            .navigationTitle("Project")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", systemImage: "xmark") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", systemImage: "checkmark") { save() }.disabled(busy)
                }
            }
            .onAppear(perform: load)
            .onChange(of: photo) { _, item in
                guard let item else { return }
                Task { await uploadIcon(item) }
            }
            .confirmationDialog("Delete \(project.displayName) with its sessions?", isPresented: $confirmDelete,
                                titleVisibility: .visible) {
                Button("Delete", role: .destructive) {
                    Task {
                        do {
                            try await app.client?.deleteProject(project.id)
                            await app.refresh()
                            dismiss()
                        } catch {
                            self.error = error.localizedDescription
                        }
                    }
                }
            }
        }
    }

    private var preview: some View {
        let letters = iconKind == "letters" && !iconValue.isEmpty ? iconValue.uppercased()
            : iconKind == "emoji" && !iconValue.isEmpty ? iconValue : project.monogram
        return ZStack {
            Text(letters)
                .font(.system(size: 28, weight: .bold, design: .rounded))
                .foregroundStyle(.white)
            if iconKind == "image", project.icon?.kind == "image", let url = project.icon?.url {
                ProjectIconImage(url: url)
            }
        }
        .frame(width: 72, height: 72)
        .background(Project.tint(of: color).gradient, in: .rect(cornerRadius: 20))
        .clipShape(.rect(cornerRadius: 20))
    }

    private func load() {
        name = project.name
        about = project.description ?? ""
        color = project.color ?? "teal"
        iconKind = project.icon?.kind ?? "letters"
        iconValue = project.icon?.value ?? ""
        quick = project.quick_messages ?? []
    }

    private func save() {
        busy = true
        var patch: [String: Any] = ["color": color, "quick_messages": quick]
        if !project.is_personal {
            patch["name"] = name
            patch["description"] = about
        }
        if iconKind != "image" {
            let v = iconValue.trimmingCharacters(in: .whitespaces)
            if !v.isEmpty { patch["icon"] = ["kind": iconKind, "value": iconKind == "letters" ? v.uppercased() : v] }
        }
        Task {
            do {
                _ = try await app.client?.patchProject(project.id, patch)
                await app.refresh()
                Feedback.play(.success)
                dismiss()
            } catch {
                self.error = error.localizedDescription
            }
            busy = false
        }
    }

    /// Crops the picked image to a centred square of 512 px and uploads it as JPEG under 256 KB.
    private func uploadIcon(_ item: PhotosPickerItem) async {
        guard let data = try? await item.loadTransferable(type: Data.self), let img = UIImage(data: data) else { return }
        let side = min(img.size.width, img.size.height)
        let crop = CGRect(x: (img.size.width - side) / 2, y: (img.size.height - side) / 2, width: side, height: side)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let square = UIGraphicsImageRenderer(size: CGSize(width: 512, height: 512), format: format).image { _ in
            img.draw(in: CGRect(x: -crop.minX * 512 / side, y: -crop.minY * 512 / side,
                                width: img.size.width * 512 / side, height: img.size.height * 512 / side))
        }
        var quality: CGFloat = 0.85
        var jpeg = square.jpegData(compressionQuality: quality) ?? Data()
        while jpeg.count > 250_000 && quality > 0.3 {
            quality -= 0.15
            jpeg = square.jpegData(compressionQuality: quality) ?? jpeg
        }
        do {
            try await app.client?.uploadProjectIcon(project.id, data: jpeg, mime: "image/jpeg")
            await app.refresh()
            Feedback.play(.success)
        } catch {
            self.error = error.localizedDescription
        }
    }
}

extension Project {
    /// A colour name's tint, as `tint` maps the project's own.
    static func tint(of color: String) -> Color {
        switch color {
        case "plum": Color(hex: 0xB57BD6)
        case "blue": Color(hex: 0x6E9BFF)
        case "rose": Color(hex: 0xE77A9A)
        case "moss": Color(hex: 0x8DBA5B)
        case "coral": Color(hex: 0xF08A5D)
        case "cyan": Color(hex: 0x4FC3E8)
        case "slate": Color(hex: 0x8A93A6)
        default: Color(hex: 0x3FB8A8)
        }
    }
}

// MARK: archived projects

/// The projects taken off the bar; their sessions stay as they were.
struct ArchivedProjectsView: View {
    @Environment(AppModel.self) private var app
    @State private var projects: [Project] = []
    @State private var loading = true
    @State private var error: String?

    var body: some View {
        List {
            ForEach(projects) { p in
                HStack(spacing: 12) {
                    ProjectTile(project: p, size: 36)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(p.displayName).font(.headline)
                        if let d = p.description, !d.isEmpty {
                            Text(d).font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
                        }
                    }
                    Spacer()
                    Button("Restore") { Task { await restore(p) } }
                        .buttonStyle(.bordered)
                }
            }
        }
        .navigationTitle("Archived projects")
        .paletteBackground()
        .overlay {
            if loading && projects.isEmpty {
                SkeletonList(rows: 3, icon: 36)
            } else if projects.isEmpty {
                ContentUnavailableView("Nothing archived", systemImage: "archivebox",
                                       description: Text(error ?? "Archived projects show up here."))
            }
        }
        .task { await load() }
        .refreshable { await load() }
    }

    private func load() async {
        do { projects = try await app.client?.archivedProjects() ?? [] } catch { self.error = error.localizedDescription }
        loading = false
    }

    private func restore(_ p: Project) async {
        do {
            try await app.client?.unarchiveProject(p.id)
            withAnimation { projects.removeAll { $0.id == p.id } }
            await app.refresh()
            Feedback.play(.success)
        } catch {
            self.error = error.localizedDescription
        }
    }
}

// MARK: the session list's order

/// The web client's list modes: State (needs you, running, then idle by recency), Recent, and
/// the user's own Custom order, kept on the hub per project.
enum SessionListMode: String, CaseIterable, Identifiable {
    case state, recent, custom
    var id: String { rawValue }
    var label: String {
        switch self {
        case .state: "State"
        case .recent: "Recent"
        case .custom: "Custom"
        }
    }
}

struct SessionGroup: Identifiable {
    let id: String
    let label: String?
    let items: [SessionInfo]
}

enum SessionGrouping {
    static func groups(_ list: [SessionInfo], mode: SessionListMode, placed: [String], summary: (String) -> SessionSummary?) -> [SessionGroup] {
        let sorted = list.sorted { a, b in
            a.updated_at != b.updated_at ? a.updated_at > b.updated_at : a.last_seq > b.last_seq
        }
        switch mode {
        case .custom:
            let at = Dictionary(uniqueKeysWithValues: placed.enumerated().map { ($1, $0) })
            let missing = sorted.filter { at[$0.id] == nil }
            let kept = sorted.filter { at[$0.id] != nil }.sorted { at[$0.id]! < at[$1.id]! }
            return [SessionGroup(id: "custom", label: nil, items: missing + kept)]
        case .recent:
            return byRecency(sorted, prefix: "")
        case .state:
            func bucket(_ s: SessionInfo) -> String {
                let st = summary(s.id)?.state ?? s.state
                if st == .waiting || st == .error { return "wait" }
                if st == .running || st == .queued { return "run" }
                if (summary(s.id)?.subagents ?? 0) > 0 { return "run" }
                return "idle"
            }
            return ([SessionGroup(id: "wait", label: "Needs you", items: sorted.filter { bucket($0) == "wait" }),
                     SessionGroup(id: "run", label: "Running", items: sorted.filter { bucket($0) == "run" })]
                    + byRecency(sorted.filter { bucket($0) == "idle" }, prefix: "Idle · "))
                .filter { !$0.items.isEmpty }
        }
    }

    private static func byRecency(_ list: [SessionInfo], prefix: String) -> [SessionGroup] {
        let cal = Calendar.current
        let now = Date()
        func recency(_ s: SessionInfo) -> Int {
            guard let d = HubEvent.parseDate(s.updated_at) else { return 4 }
            let days = cal.dateComponents([.day], from: cal.startOfDay(for: d), to: cal.startOfDay(for: now)).day ?? 0
            if days <= 0 { return 0 }
            if days == 1 { return 1 }
            if days < 7 { return 2 }
            return cal.isDate(d, equalTo: now, toGranularity: .month) ? 3 : 4
        }
        let labels = ["Today", "Yesterday", "Last 7 days", "This month", "Earlier"]
        return labels.enumerated().map { i, l in
            SessionGroup(id: "\(prefix)\(i)", label: "\(prefix)\(l)", items: list.filter { recency($0) == i })
        }
        .filter { !$0.items.isEmpty }
    }
}

// MARK: moving a session

/// The session continues in another folder of its worker, in another project or a new one
/// named after the folder; no input is queued.
struct MoveSessionSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let session: SessionInfo
    let onMoved: (SessionInfo) -> Void
    @State private var worker: WorkerInfo?
    @State private var path: String?
    @State private var projectID: String?
    @State private var create = false
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    if let worker {
                        NavigationLink {
                            FolderBrowser(worker: worker, path: nil) { p in path = p }
                        } label: {
                            LabeledContent("Folder", value: path.map { ($0 as NSString).lastPathComponent } ?? "Choose…")
                        }
                    } else {
                        ProgressView()
                    }
                    Toggle("Create the folder if it is missing", isOn: $create)
                } header: {
                    Text("On \(worker?.name ?? "its machine")")
                } footer: {
                    if let path { Text(path).font(.caption.monospaced()) }
                }
                Section("Project") {
                    Picker("Project", selection: $projectID) {
                        Text("A new project named after the folder").tag(String?.none)
                        ForEach(app.sortedProjects) { p in Text(p.displayName).tag(String?.some(p.id)) }
                    }
                    .pickerStyle(.inline)
                    .labelsHidden()
                }
                if let error {
                    Section { Text(error).foregroundStyle(.red).font(.footnote) }
                }
            }
            .navigationTitle("Move session")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", systemImage: "xmark") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Move", systemImage: "checkmark") { move() }
                        .disabled(path == nil || busy)
                }
            }
            .task {
                projectID = session.project_id
                worker = try? await app.client?.workers().first { $0.id == session.worker_id }
            }
        }
    }

    private func move() {
        guard let path else { return }
        busy = true
        Task {
            do {
                if let moved = try await app.client?.moveSession(session.id, path: path, projectID: projectID, create: create) {
                    await app.refresh()
                    Feedback.play(.success)
                    dismiss()
                    onMoved(moved)
                }
            } catch {
                self.error = error.localizedDescription
            }
            busy = false
        }
    }
}
