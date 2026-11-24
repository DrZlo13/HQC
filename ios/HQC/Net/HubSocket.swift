import Foundation

/// `/api/ws` (SPEC §6): `subscribe {session_id, after_seq}` per open chat, `subscribe_summary`
/// for the session list and the open requests. On every (re)connect each subscription is sent
/// again from the last `seq` its subscriber has seen, as the web client's `ws.ts` does.
@MainActor
final class HubSocket {
    enum State: Equatable { case connecting, open, closed }

    struct Subscriber {
        var afterSeq: () -> Int
        var onEvent: (HubEvent) -> Void
        var onDelivery: (_ eventID: String, _ delivery: String) -> Void
        /// The socket dropped: transport-only state (`agent.delta`) is gone.
        var onReset: () -> Void
    }

    private(set) var state: State = .closed {
        didSet { if state != oldValue { onState?(state) } }
    }

    var onState: ((State) -> Void)?
    var onSummary: (([SessionSummary]) -> Void)?
    var onRequests: (([OpenRequest]) -> Void)?
    var onSignInRequired: (() -> Void)?

    private let endpoint: HubEndpoint
    private let urlSession = URLSession(configuration: .default)
    private var task: URLSessionWebSocketTask?
    private var subs: [String: Subscriber] = [:]
    private var stopped = true
    private var attempt = 0
    private var reconnect: Task<Void, Never>?
    private var pinger: Task<Void, Never>?
    private static let decoder = JSONDecoder()

    init(endpoint: HubEndpoint) {
        self.endpoint = endpoint
    }

    func start() {
        guard stopped else { return }
        stopped = false
        open()
    }

    func stop() {
        stopped = true
        reconnect?.cancel()
        pinger?.cancel()
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        state = .closed
    }

    /// The app went to the background: close the socket, keeping the subscriptions, so the hub
    /// no longer counts this app as showing them and sends their notices to the phone (SPEC §16).
    func suspend() {
        guard !stopped else { return }
        stopped = true
        reconnect?.cancel()
        pinger?.cancel()
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        // A text streaming when the app left is sent again whole on the next subscribe.
        for sub in subs.values { sub.onReset() }
        state = .closed
    }

    /// Back in the foreground after `suspend`: open again; every chat resubscribes from its `seq`.
    func resume() {
        guard stopped else { return }
        stopped = false
        attempt = 0
        open()
    }

    /// Reconnect now if the socket is down (the app came to the foreground).
    func nudge() {
        guard !stopped, state == .closed else { return }
        reconnect?.cancel()
        attempt = 0
        open()
    }

    func subscribe(_ sessionID: String, _ sub: Subscriber) {
        subs[sessionID] = sub
        send(["type": "subscribe", "session_id": sessionID, "after_seq": sub.afterSeq()])
    }

    func unsubscribe(_ sessionID: String) {
        guard subs.removeValue(forKey: sessionID) != nil else { return }
        send(["type": "unsubscribe", "session_id": sessionID])
    }

    private func send(_ frame: [String: Any]) {
        guard state == .open, let task,
              let data = try? JSONSerialization.data(withJSONObject: frame),
              let text = String(data: data, encoding: .utf8) else { return }
        task.send(.string(text)) { _ in }
    }

    private func open() {
        state = .connecting
        var req = URLRequest(url: endpoint.socketURL)
        // Browsers always send Origin on an upgrade, and the hub refuses one without it (SPEC §20).
        req.setValue(endpoint.origin, forHTTPHeaderField: "Origin")
        endpoint.authorize(&req)
        let t = urlSession.webSocketTask(with: req)
        task = t
        t.resume()
        receive(on: t)
        // The first frame proves the upgrade worked; `subscribe_summary` gets one at once.
        t.send(.string(#"{"type":"subscribe_summary"}"#)) { [weak self] error in
            Task { @MainActor in
                guard let self, self.task === t else { return }
                if error != nil { self.closed(t) } else { self.opened(t) }
            }
        }
    }

    private func opened(_ t: URLSessionWebSocketTask) {
        attempt = 0
        state = .open
        for (id, sub) in subs {
            send(["type": "subscribe", "session_id": id, "after_seq": sub.afterSeq()])
        }
        pinger?.cancel()
        pinger = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(20))
                guard let self, self.task === t else { return }
                self.send(["type": "ping"])
            }
        }
    }

    private func receive(on t: URLSessionWebSocketTask) {
        t.receive { [weak self] result in
            Task { @MainActor in
                guard let self, self.task === t else { return }
                switch result {
                case .success(let message):
                    switch message {
                    case .string(let s): self.dispatch(Data(s.utf8))
                    case .data(let d): self.dispatch(d)
                    @unknown default: break
                    }
                    self.receive(on: t)
                case .failure:
                    if let http = t.response as? HTTPURLResponse,
                       [301, 302, 303, 307, 401, 403].contains(http.statusCode) {
                        self.onSignInRequired?()
                    }
                    self.closed(t)
                }
            }
        }
    }

    private func closed(_ t: URLSessionWebSocketTask) {
        guard task === t else { return }
        task = nil
        pinger?.cancel()
        for sub in subs.values { sub.onReset() }
        state = .closed
        guard !stopped else { return }
        let delay = min(15.0, 0.5 * pow(2, Double(attempt))) * Double.random(in: 0.75...1.25)
        attempt += 1
        reconnect = Task { [weak self] in
            try? await Task.sleep(for: .seconds(delay))
            guard let self, !Task.isCancelled, !self.stopped else { return }
            self.open()
        }
    }

    private func dispatch(_ data: Data) {
        guard let frame = try? Self.decoder.decode(JSONValue.self, from: data),
              let type = frame["type"]?.string else { return }
        switch type {
        case "event":
            guard let raw = frame["event"],
                  let ev = try? Self.decoder.decode(HubEvent.self, from: JSONEncoder().encode(raw)) else { return }
            subs[ev.session_id]?.onEvent(ev)
        case "delivery":
            if let sid = frame["session_id"]?.string, let eid = frame["event_id"]?.string,
               let d = frame["delivery"]?.string {
                subs[sid]?.onDelivery(eid, d)
            }
        case "summary":
            if let list = try? Self.decoder.decode([SessionSummary].self, from: JSONEncoder().encode(frame["sessions"] ?? .array([]))) {
                onSummary?(list)
            }
        case "session.summary":
            if let raw = frame["session"],
               let s = try? Self.decoder.decode(SessionSummary.self, from: JSONEncoder().encode(raw)) {
                onSummary?([s])
            }
        case "requests":
            // One request this build cannot read must not hide the others.
            let items = frame["requests"]?.array ?? []
            let list = items.compactMap { try? Self.decoder.decode(OpenRequest.self, from: JSONEncoder().encode($0)) }
            onRequests?(list)
        default:
            break // subscribed, pong, error and frames this build does not know
        }
    }
}
