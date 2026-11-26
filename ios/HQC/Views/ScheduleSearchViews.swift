import SwiftUI

/// A session's schedules (SPEC §16): messages the hub sends at a time or on a cron pattern.
struct SchedulesView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let sessionID: String
    @State private var schedules: [Schedule] = []
    @State private var creating = false
    @State private var error: String?

    var body: some View {
        List {
            ForEach(schedules) { s in
                VStack(alignment: .leading, spacing: 6) {
                    HStack {
                        Label(when(s), systemImage: s.cron != nil ? "repeat" : "clock")
                            .font(.subheadline.weight(.semibold))
                        Spacer()
                        Toggle("Enabled", isOn: Binding(get: { s.enabled }, set: { on in
                            Task { await toggle(s, on) }
                        }))
                        .labelsHidden()
                    }
                    Text(s.message).font(.callout).lineLimit(3)
                    if let next = s.next_run.flatMap(HubEvent.parseDate), s.enabled {
                        Text("Next \(next, format: .relative(presentation: .named))")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                .swipeActions {
                    Button("Delete", systemImage: "trash", role: .destructive) {
                        Task {
                            try? await app.client?.deleteSchedule(s.id)
                            await load()
                        }
                    }
                }
            }
        }
        .navigationTitle("Schedules")
        .paletteBackground()
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Done", systemImage: "checkmark") { dismiss() }
            }
            ToolbarItem(placement: .primaryAction) {
                Button("New schedule", systemImage: "plus") { creating = true }
            }
        }
        .overlay {
            if schedules.isEmpty {
                ContentUnavailableView("No schedules", systemImage: "calendar.badge.clock",
                                       description: Text(error ?? "Send this session a message later, or on a repeat."))
            }
        }
        .task { await load() }
        .refreshable { await load() }
        .sheet(isPresented: $creating) {
            NewScheduleSheet(sessionID: sessionID) { await load() }
                .presentationDetents([.medium, .large])
        }
    }

    private func when(_ s: Schedule) -> String {
        if let cron = s.cron { return "Cron \(cron)" }
        if let at = s.at.flatMap(HubEvent.parseDate) { return at.formatted(date: .abbreviated, time: .shortened) }
        return "Once"
    }

    private func toggle(_ s: Schedule, _ on: Bool) async {
        do {
            _ = try await app.client?.setSchedule(s.id, enabled: on)
            await load()
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func load() async {
        do { schedules = try await app.client?.schedules(sessionID: sessionID) ?? [] } catch {
            self.error = error.localizedDescription
        }
    }
}

struct NewScheduleSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let sessionID: String
    let onCreated: () async -> Void

    enum Kind: String, CaseIterable, Identifiable {
        case once = "Once", repeating = "Repeat"
        var id: String { rawValue }
    }

    @State private var kind: Kind = .once
    @State private var at = Date().addingTimeInterval(3600)
    @State private var cron = "0 9 * * 1-5"
    @State private var message = ""
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Picker("When", selection: $kind) {
                        ForEach(Kind.allCases) { Text($0.rawValue).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    if kind == .once {
                        DatePicker("At", selection: $at, in: Date()...)
                    } else {
                        TextField("Cron pattern", text: $cron)
                            .font(.body.monospaced())
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    }
                } footer: {
                    if kind == .repeating {
                        Text("Minute, hour, day of month, month, day of week, in \(TimeZone.current.identifier). “0 9 * * 1-5” is 9:00 on weekdays.")
                    }
                }
                Section("Message") {
                    TextField("What to tell the agent", text: $message, axis: .vertical).lineLimit(3...8)
                }
                if let error { Section { Text(error).foregroundStyle(.red).font(.footnote) } }
            }
            .navigationTitle("New schedule")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", systemImage: "checkmark") {
                        Task {
                            do {
                                _ = try await app.client?.createSchedule(
                                    sessionID: sessionID, message: message,
                                    cron: kind == .repeating ? cron : nil, at: kind == .once ? at : nil)
                                await onCreated()
                                dismiss()
                            } catch {
                                self.error = error.localizedDescription
                            }
                        }
                    }
                    .disabled(message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
    }
}

/// Search across every session (SPEC §15): messages, commands, output and answers.
struct SearchView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.palette) private var palette
    /// Put the cursor in the field on appear (the desktop layout's search pane).
    var autofocus = false
    @FocusState private var fieldFocused: Bool
    @State private var query = ""
    /// The hit last opened, marked while its chat shows beside the list.
    @State private var openedHit: SearchHit.ID?
    @State private var results: [SearchHit] = []
    @State private var next: String?
    @State private var searching = false
    @State private var error: String?

    var body: some View {
        List {
            ForEach(results) { hit in
                Button {
                    openedHit = hit.id
                    fieldFocused = false
                    app.open(sessionID: hit.session_id, title: hit.title)
                } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text(hit.title).font(.subheadline.weight(.semibold)).lineLimit(1)
                            if hit.archived {
                                Image(systemName: "archivebox").font(.caption2).foregroundStyle(.secondary)
                            }
                            Spacer()
                            if let d = HubEvent.parseDate(hit.ts) {
                                Text(d, format: .relative(presentation: .named))
                                    .font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                        Text(highlighted(hit.snippet)).font(.callout).lineLimit(3)
                        Text(hit.address).font(.caption2.monospaced()).foregroundStyle(.tertiary)
                    }
                }
                .foregroundStyle(.primary)
                .listRowInsets(EdgeInsets(top: 10, leading: 20, bottom: 10, trailing: 20))
                .listRowBackground(
                    autofocus && openedHit == hit.id && app.selectedSessionID == hit.session_id
                        ? RoundedRectangle(cornerRadius: 12).fill(Color.accentColor.opacity(0.16))
                            .padding(.horizontal, 8).padding(.vertical, 2)
                        : nil
                )
            }
            if next != nil {
                Button("More results") { Task { await search(more: true) } }
            }
        }
        .navigationTitle("Search")
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .paletteBackground(app.isRegular ? \.side : \.bg)
        .modifier(SearchField(inline: autofocus, query: $query, focused: $fieldFocused))
        .onAppear { if autofocus { fieldFocused = true } }
        .onSubmit(of: .search) { Task { await search() } }
        .task(id: query) {
            try? await Task.sleep(for: .milliseconds(350))
            guard !Task.isCancelled else { return }
            await search()
        }
        .overlay {
            if searching && results.isEmpty {
                ProgressView()
            } else if results.isEmpty {
                if query.trimmingCharacters(in: .whitespaces).count < 2 {
                    ContentUnavailableView("Search everything", systemImage: "magnifyingglass",
                                           description: Text("Every session's messages, commands and answers."))
                } else {
                    ContentUnavailableView.search(text: error ?? query)
                }
            }
        }
    }

    /// The snippet with the query's words in bold, as the web client marks them.
    private func highlighted(_ s: String) -> AttributedString {
        var out = AttributedString(s)
        let words = query.split(separator: " ").map(String.init).filter { $0.count >= 2 }
        for w in words {
            var searchRange = out.startIndex..<out.endIndex
            while let r = out[searchRange].range(of: w, options: [.caseInsensitive, .diacriticInsensitive]) {
                out[r].font = .callout.bold()
                out[r].foregroundColor = .accentColor
                searchRange = r.upperBound..<out.endIndex
            }
        }
        return out
    }

    private func search(more: Bool = false) async {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard q.count >= 2, let client = app.client else {
            results = []
            next = nil
            return
        }
        searching = true
        defer { searching = false }
        do {
            let page = try await client.search(q, cursor: more ? next : nil)
            results = more ? results + page.results : page.results
            next = page.next_cursor
            error = nil
        } catch {
            self.error = error.localizedDescription
            if !more { results = [] }
        }
    }
}

/// The search field: the system's search bar in the phone's Search tab; in the desktop layout's
/// side pane a field at the top of the pane, so it does not drop to the bottom across the rail.
private struct SearchField: ViewModifier {
    @Environment(\.palette) private var palette
    let inline: Bool
    @Binding var query: String
    var focused: FocusState<Bool>.Binding

    func body(content: Content) -> some View {
        if inline {
            content
                .safeAreaInset(edge: .top, spacing: 0) {
                    HStack(spacing: 8) {
                        Image(systemName: "magnifyingglass").foregroundStyle(palette.muted)
                        TextField("Messages, commands, output", text: $query)
                            .focused(focused)
                            .submitLabel(.search)
                            .autocorrectionDisabled()
                            .textInputAutocapitalization(.never)
                        if !query.isEmpty {
                            Button {
                                query = ""
                            } label: {
                                Image(systemName: "xmark.circle.fill").foregroundStyle(palette.muted)
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel("Clear")
                        }
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .glassEffect(.regular.interactive(), in: .capsule)
                    .padding(.horizontal, 12)
                    .padding(.top, 8)
                    .padding(.bottom, 6)
                }
        } else {
            content
                .searchable(text: $query, prompt: "Messages, commands, output")
                .searchFocused(focused)
        }
    }
}
