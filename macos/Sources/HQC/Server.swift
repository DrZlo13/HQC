import Foundation

// A single hqc hub configured by the user.
struct Server: Codable, Identifiable, Equatable {
    let id: UUID
    var name: String
    var url: URL

    // Default `id` to a fresh UUID so callers only need to supply name + url.
    init(id: UUID = UUID(), name: String, url: URL) {
        self.id = id
        self.name = name
        self.url = url
    }

    /// True when `url` is on a different host than this server — i.e. an external site.
    /// The comparison is case-insensitive; a `url` (or the server URL) without a host is
    /// treated as internal (returns false).
    func isExternalURL(_ url: URL?) -> Bool {
        guard let host = url?.host, let serverHost = self.url.host else { return false }
        return host.caseInsensitiveCompare(serverHost) != .orderedSame
    }

    /// True when a page at `scheme://host:port` is this server's own: the scheme, the host
    /// (case-insensitive) and the port all equal those of `url`, a port left out being the
    /// scheme's default (80 for http, 443 for https), as a browser compares origins. A page on
    /// `http://`, or on another port of the same name, is not the server's.
    func isOwnOrigin(scheme: String?, host: String?, port: Int?) -> Bool {
        guard let own = Origin(scheme: url.scheme, host: url.host, port: url.port),
              let page = Origin(scheme: scheme, host: host, port: port) else { return false }
        return page == own
    }
}

// An address's scheme, host and port, lowercased, the port filled in for http and https.
private struct Origin: Equatable {
    let scheme: String
    let host: String
    let port: Int?

    init?(scheme: String?, host: String?, port: Int?) {
        guard let scheme = scheme?.lowercased(), let host = host?.lowercased(), !host.isEmpty else {
            return nil
        }
        self.scheme = scheme
        self.host = host
        self.port = port ?? ["http": 80, "https": 443][scheme]
    }
}
