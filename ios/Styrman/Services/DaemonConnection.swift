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
    /// Notes from the Mac (e.g. an agent version Styrman hasn't been checked with).
    private(set) var usageSupported = false
    private(set) var usage: [UsageProvider]?
    private(set) var warnings: [String] = []
    /// Last error the daemon reported for something this phone asked (shown briefly).
    var lastError: String?
    /// Set when a notification was tapped: the list opens this session.
    var requestedSession: String?
    /// The new-session picker's current folder listing.
    private(set) var folders: FolderListing?
    /// The first message of a session being started, shown as pending once it opens.
    @ObservationIgnored private var startPrompt: String?
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
        usage = nil
        usageSupported = false
        connect()
    }

    func forget() {
        disconnect()
        CredentialStore.delete()
        credentials = nil
        sessions = []
        timelines = [:]
        usage = nil
        usageSupported = false
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
        timelines.values.forEach { $0.isLive = false }
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
        // Heartbeat: a silently dead connection (network switch, Tailscale reconnect)
        // otherwise looks "connected" while nothing arrives. No pong in 10 s → reconnect.
        let heartbeat = Task { [weak t] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(15))
                guard let t, !Task.isCancelled else { return }
                let pong = PongFlag()
                t.sendPing { error in if error == nil { pong.set() } }
                try? await Task.sleep(for: .seconds(10))
                if Task.isCancelled { return }
                if !pong.isSet {
                    t.cancel(with: .goingAway, reason: nil) // receive() throws → reconnect
                    return
                }
            }
        }
        defer {
            heartbeat.cancel()
            if task === t { task = nil }
        }
        do {
            while !Task.isCancelled {
                let msg = try await t.receive()
                guard case .string(let text) = msg, let data = text.data(using: .utf8) else { continue }
                // One unreadable message must not drop the connection (it would come
                // back on reconnect and drop it again, forever).
                guard let decoded = try? JSONDecoder().decode(ServerMsg.self, from: data) else { continue }
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

    /// Send a message; `failed` runs (on the main actor) if it didn't leave the phone.
    private func send(_ msg: ClientMsg, failed: (@MainActor () -> Void)? = nil) {
        guard let task, let data = try? JSONEncoder().encode(msg), let text = String(data: data, encoding: .utf8) else {
            failed?()
            return
        }
        task.send(.string(text)) { error in
            guard error != nil, let failed else { return }
            Task { @MainActor in failed() }
        }
    }

    private func handle(_ msg: ServerMsg) {
        switch msg {
        case .hello(_, _, _, let host, let warnings, let usageSupported):
            self.usageSupported = usageSupported
            self.host = host
            self.warnings = warnings
            state = .connected
            for id in watched { subscribe(id) }
            registerPush()
            for (kind, token) in activityTokens { send(.registerActivityToken(kind: kind, token: token ?? nil)) }
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
        case .promptDelivered(let sid, let text):
            timelines[sid]?.markDelivered(text)
        case .usage(let providers):
            usage = providers
        case .folders(let listing):
            folders = listing
        case .sessionStarting(let sid):
            // Open it right away; it fills in as the agent starts.
            if let p = startPrompt { timeline(for: sid).addPending(p); startPrompt = nil }
            requestedSession = sid
            send(.listSessions)
        case .error(let message):
            lastError = message
        case .unknown:
            break
        }
    }

    func refreshUsage() {
        if state == .connected && usageSupported { send(.getUsage) }
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

    /// Live Activity tokens, kept so they're sent again after reconnecting.
    @ObservationIgnored private var activityTokens: [String: String?] = [:]

    func registerActivityToken(kind: String, token: String?) {
        if activityTokens[kind] == .some(token) { return }
        activityTokens[kind] = .some(token)
        if state == .connected { send(.registerActivityToken(kind: kind, token: token)) }
    }

    private func registerPush() {
        guard let pushToken else { return }
        let env = Bundle.main.object(forInfoDictionaryKey: "StyrmanAPNsEnvironment") as? String ?? "sandbox"
        send(.registerPush(token: pushToken, env: env == "production" ? "production" : "sandbox"))
    }

    /// Stop sharing a session with the phones (the session keeps running).
    func stopSharing(_ session: String) {
        guard state == .connected else { return }
        sessions.removeAll { $0.id == session }
        send(.stopSharing(session: session))
    }

    /// Make a folder inside `parent`; the picker then shows (and can select) it.
    func createFolder(in parent: String, named name: String) {
        guard state == .connected else { return }
        send(.createFolder(parent: parent, name: name))
    }

    func listFolders(_ path: String? = nil) {
        guard state == .connected else { return }
        send(.listFolders(path: path))
    }

    /// Start `agent` in `cwd` in a new cmux tab on the Mac.
    func startSession(agent: AgentKind, cwd: String, prompt: String?) -> Bool {
        guard state == .connected else { return false }
        let p = prompt?.trimmingCharacters(in: .whitespacesAndNewlines)
        startPrompt = (p?.isEmpty ?? true) ? nil : p
        send(.startSession(agent: agent, cwd: cwd, prompt: startPrompt))
        return true
    }

    /// Send a prompt; it shows as pending until the agent picks it up.
    @discardableResult
    func sendPrompt(session: String, text: String) -> Bool {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard state == .connected, !text.isEmpty else { return false }
        let tl = timeline(for: session)
        let id = tl.addPending(text)
        send(.sendPrompt(session: session, text: text)) {
            tl.markUnconfirmed(id)
        }
        Task { @MainActor [weak tl] in
            try? await Task.sleep(for: .seconds(15))
            tl?.markUnconfirmed(id)
        }
        return true
    }

    /// Answer a permission request. The card closes when the daemon's
    /// `permission_resolved` event arrives; returns false if not connected.
    @discardableResult
    func resolvePermission(session: String, request: String, option: String,
                           failed: @escaping @MainActor () -> Void = {}) -> Bool {
        guard state == .connected else { return false }
        send(.resolvePermission(session: session, request: request, option: option)) { [weak self] in
            self?.lastError = "Your answer didn't reach the Mac. Try again, or answer in the terminal."
            failed()
        }
        return true
    }
}

/// Set from URLSession's callback thread, read by the heartbeat.
private final class PongFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false
    func set() { lock.withLock { value = true } }
    var isSet: Bool { lock.withLock { value } }
}
