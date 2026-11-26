import Foundation

/// One event of a session (SPEC §4): the envelope typed, the payload kept as JSON and read per
/// type, so an event this build does not know still decodes and is simply not drawn.
nonisolated struct HubEvent: Codable, Hashable, Sendable, Identifiable {
    let session_id: String
    let event_id: String
    let seq: Int?
    let ts: String
    let type: String
    let author: Author?
    var payload: JSONValue
    let client_op_id: String?
    var delivery: String?

    var id: String { event_id }

    nonisolated struct Author: Codable, Hashable, Sendable {
        let kind: String
        let id: String?
    }

    var date: Date? { HubEvent.parseDate(ts) }

    static func parseDate(_ s: String) -> Date? {
        if let d = fractional.date(from: s) { return d }
        return plain.date(from: s)
    }

    private nonisolated(unsafe) static let fractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    private nonisolated(unsafe) static let plain: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f
    }()

    // MARK: payload readers

    var text: String { payload["text"]?.string ?? "" }
    var isFinal: Bool { payload["final"]?.bool ?? false }
    var requestID: String? { payload["request_id"]?.string }
    var callID: String? { payload["call_id"]?.string }
}

/// An approval option (`approval.request.options[]`).
nonisolated struct ApprovalOption: Identifiable, Hashable, Sendable {
    let id: String
    let label: String
    /// once / session / always / deny / deny_always, or another value this build does not know.
    let canonical: String

    var isDeny: Bool { canonical == "deny" || canonical == "deny_always" }

    static func list(from payload: JSONValue) -> [ApprovalOption] {
        (payload["options"]?.array ?? []).compactMap { o in
            guard let id = o["id"]?.string else { return nil }
            return ApprovalOption(
                id: id,
                label: o["label"]?.string ?? id,
                canonical: o["canonical"]?.string ?? "unknown"
            )
        }
    }
}

/// A question of a `clarify.request`.
nonisolated struct ClarifyQuestion: Identifiable, Hashable, Sendable {
    let id: String
    let text: String
    let header: String?
    let options: [String]
    let descriptions: [String]
    let multi: Bool

    static func list(from payload: JSONValue) -> [ClarifyQuestion] {
        (payload["questions"]?.array ?? []).compactMap { q in
            guard let id = q["id"]?.string else { return nil }
            return ClarifyQuestion(
                id: id,
                text: q["text"]?.string ?? "",
                header: q["header"]?.string,
                options: q["options"]?.stringArray ?? [],
                descriptions: q["option_descriptions"]?.stringArray ?? [],
                multi: q["multi"]?.bool ?? false
            )
        }
    }
}
