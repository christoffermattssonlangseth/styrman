// The live link to styrd: one WebSocket, the session list, and per-session timelines.
// Reconnects with backoff and resumes each open timeline from its last seq.

import Foundation
import Observation

@MainActor
@Observable
final class DaemonConnection {
    enum State: Equatable {
        case unpaired
        case connecting
        case connected
        /// Waiting to retry; the message says why.
        case offline(String)
        /// The token was rejected (device revoked): pair again.
        case unauthorized
    }

    private(set) var state: State = .unpaired
    private(set) var credentials: Credentials?
    private(set) var sessions: [SessionSummary] = []
    private(set) var host: String = ""
    /// Last error the daemon reported for something this phone asked (shown briefly).
    var lastError: String?
    /// Set when a notification was tapped: the list opens this session.
    var requestedSession: String?
    /// APNs device token (hex), sent to the daemon on every connection.
    @ObservationIgnored var pushToken: String? {
        didSet { if state == .connected { registerPush() } }
    }

    @ObservationIgnored private var timelines: [String: SessionTimeline] = [:]
    @ObservationIgnored private var watched: Set<String> = []
    @ObservationIgnored private var task: URLSessionWebSocketTask?
    @ObservationIgnored private var loop: Task<Void, Never>?
    @ObservationIgnored private var backoff: Duration = .seconds(1)
    @ObservationIgnored private let session: URLSession = {
        let cfg = URLSessionConfiguration.default
        cfg.waitsForConnectivity = false
        cfg.timeoutIntervalForRequest = 15
        return URLSession(configuration: cfg)
    }()

    init(credentials: Credentials? = CredentialStore.load()) {
        self.credentials = credentials
        self.host = credentials?.host ?? ""
        self.state = credentials == nil ? .unpaired : .offline("Not connected yet")
    }

    // MARK: Pairing

    func pair(with link: PairingLink, deviceName: String) async throws {
        let creds = try await Pairing.pair(link, deviceName: deviceName)
        try CredentialStore.save(creds)
        disconnect()
        credentials = creds
        host = creds.host
        sessions = []
        timelines = [:]
        connect()
    }

    func forget() {
        disconnect()
        CredentialStore.delete()
        credentials = nil
        sessions = []
        timelines = [:]
        state = .unpaired
    }

    // MARK: Connection lifecycle

    func connect() {
        guard credentials != nil, loop == nil else { return }
        loop = Task { [weak self] in await self?.run() }
    }

    func disconnect() {
        loop?.cancel()
        loop = nil
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        if credentials != nil, state != .unauthorized { state = .offline("Disconnected") }
    }

    private func run() async {
        while !Task.isCancelled, let creds = credentials {
            state = .connecting
            let ok = await runOnce(creds)
            if Task.isCancelled || state == .unauthorized { break }
            if ok { backoff = .seconds(1) }
            try? await Task.sleep(for: backoff)
            backoff = min(backoff * 2, .seconds(30))
        }
        loop = nil
    }

    /// One connection; returns whether it got as far as `hello`.
    private func runOnce(_ creds: Credentials) async -> Bool {
        var req = URLRequest(url: creds.wsURL)
        req.setValue("Bearer \(creds.token)", forHTTPHeaderField: "Authorization")
        let t = session.webSocketTask(with: req)
        task = t
        t.resume()
        var greeted = false
        defer { if task === t { task = nil } }
        do {
            while !Task.isCancelled {
                let msg = try await t.receive()
                guard case .string(let text) = msg, let data = text.data(using: .utf8) else { continue }
                let decoded = try JSONDecoder().decode(ServerMsg.self, from: data)
                if case .hello = decoded { greeted = true }
                handle(decoded)
            }
        } catch {
            if let http = t.response as? HTTPURLResponse, http.statusCode == 401 {
                state = .unauthorized
            } else if !Task.isCancelled {
                state = .offline(greeted ? "Connection lost" : "Can't reach \(creds.host)")
            }
        }
        for tl in timelines.values { tl.isLive = false }
        return greeted
    }

    private func send(_ msg: ClientMsg) {
        guard let task, let data = try? JSONEncoder().encode(msg), let text = String(data: data, encoding: .utf8) else { return }
        task.send(.string(text)) { _ in }
    }

    private func handle(_ msg: ServerMsg) {
        switch msg {
        case .hello(_, _, _, let host):
            self.host = host
            state = .connected
            for id in watched { subscribe(id) }
            registerPush()
        case .sessions(let list):
            sessions = list.sorted { $0.updatedMs > $1.updatedMs }
        case .sessionUpdated(let s):
            sessions.removeAll { $0.id == s.id }
            sessions.append(s)
            sessions.sort { $0.updatedMs > $1.updatedMs }
        case .sessionRemoved(let id):
            sessions.removeAll { $0.id == id }
        case .events(let sid, let events):
            timelines[sid]?.apply(events)
        case .subscribed(let sid, _):
            timelines[sid]?.isLive = true
        case .event(let e):
            timelines[e.session]?.apply([e])
        case .promptQueued(let sid, let text, _):
            timelines[sid]?.markQueued(text)
        case .error(let message):
            lastError = message
        case .unknown:
            break
        }
    }

    // MARK: Timelines

    func timeline(for id: String) -> SessionTimeline {
        if let t = timelines[id] { return t }
        let t = SessionTimeline(sessionId: id)
        timelines[id] = t
        return t
    }

    func watch(_ id: String) {
        watched.insert(id)
        if state == .connected { subscribe(id) }
    }

    func unwatch(_ id: String) {
        watched.remove(id)
        send(.unsubscribe(session: id))
        timelines[id]?.isLive = false
    }

    private func subscribe(_ id: String) {
        send(.subscribe(session: id, sinceSeq: timeline(for: id).lastSeq))
    }

    func session(_ id: String) -> SessionSummary? { sessions.first { $0.id == id } }

    private func registerPush() {
        guard let pushToken else { return }
        let env = Bundle.main.object(forInfoDictionaryKey: "StyrmanAPNsEnvironment") as? String ?? "sandbox"
        send(.registerPush(token: pushToken, env: env == "production" ? "production" : "sandbox"))
    }

    /// Send a prompt; it shows as pending until the agent picks it up.
    @discardableResult
    func sendPrompt(session: String, text: String) -> Bool {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard state == .connected, !text.isEmpty else { return false }
        timeline(for: session).addPending(text)
        send(.sendPrompt(session: session, text: text))
        return true
    }

    /// Answer a permission request. The card closes when the daemon's
    /// `permission_resolved` event arrives; returns false if not connected.
    @discardableResult
    func resolvePermission(session: String, request: String, option: String) -> Bool {
        guard state == .connected else { return false }
        send(.resolvePermission(session: session, request: request, option: option))
        return true
    }
}
