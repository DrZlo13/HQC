import Foundation

extension Notification.Name {
    static let serversDidChange = Notification.Name("ServersDidChange")
}

// Persists the list of configured servers in UserDefaults as JSON.
final class ServerStore {

    private static let defaultsKey = "servers"

    private(set) var servers: [Server]

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        // Load on init; fall back to an empty list if missing or corrupt.
        if let data = defaults.data(forKey: Self.defaultsKey),
           let decoded = try? JSONDecoder().decode([Server].self, from: data) {
            self.servers = decoded
        } else {
            self.servers = []
        }
    }

    // MARK: - Mutations

    func add(name: String, urlString: String) {
        guard let url = Self.normalizeURL(urlString) else { return }
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        // Fall back to the host as the display name if the user left it blank.
        let finalName = trimmedName.isEmpty ? (url.host ?? urlString) : trimmedName
        servers.append(Server(name: finalName, url: url))
        persistAndNotify()
    }

    func update(_ server: Server) {
        guard let index = servers.firstIndex(where: { $0.id == server.id }) else { return }
        servers[index] = server
        persistAndNotify()
    }

    func remove(id: UUID) {
        servers.removeAll { $0.id == id }
        persistAndNotify()
    }

    // Reorders a server. `destinationIndex` follows the NSTableView drop convention:
    // it is the index in the array BEFORE removal where the item should land, so we
    // subtract one when moving an item forward (destination > source).
    func move(from sourceIndex: Int, to destinationIndex: Int) {
        guard servers.indices.contains(sourceIndex),
              destinationIndex >= 0, destinationIndex <= servers.count else { return }
        let item = servers.remove(at: sourceIndex)
        let target = destinationIndex > sourceIndex ? destinationIndex - 1 : destinationIndex
        servers.insert(item, at: min(target, servers.count))
        persistAndNotify()
    }

    // MARK: - Persistence

    private func persistAndNotify() {
        if let data = try? JSONEncoder().encode(servers) {
            defaults.set(data, forKey: Self.defaultsKey)
        }
        NotificationCenter.default.post(name: .serversDidChange, object: self)
    }

    // MARK: - Helpers

    // Normalize a user-entered URL string: trim, default to https:// when no
    // scheme is present, and require a host. Returns nil for invalid input.
    static func normalizeURL(_ raw: String) -> URL? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }

        // Default to https:// when the user did not type a scheme.
        let withScheme = trimmed.contains("://") ? trimmed : "https://\(trimmed)"
        // Only http/https are valid for a web server; reject anything else (ftp:, etc.).
        guard let url = URL(string: withScheme),
              let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https",
              let host = url.host, !host.isEmpty else {
            return nil
        }
        return url
    }
}
