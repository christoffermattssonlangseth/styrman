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

    private(set) var state: State = .unpaired {
        didSet { trackTrouble() }
    }
    /// The connection has been down for a while (not just a quick reconnect, like
    /// coming back to the app). The UI only shows connection problems when this is set.
    private(set) var troubled = false
    @ObservationIgnored private var troubleTimer: Task<Void, Never>?

    private func trackTrouble() {
        switch state {
        case .connected, .unpaired:
            troubleTimer?.cancel()
            troubleTimer = nil
            troubled = false
        case .unauthorized:
            troubled = true
        case .connecting, .offline:
            guard !troubled, troubleTimer == nil else { return }
            troubleTimer = Task { [weak self] in
                try? await Task.sleep(for: .seconds(3))
                guard let self, !Task.isCancelled else { return }
                self.troubleTimer = nil
                if self.state != .connected { self.troubled = true }
            }
        }
    }
    private(set) var credentials: Credentials?
    private(set) var sessions: [SessionSummary] = []
    /// The Mac has sent its session list since connecting (empty means really empty).
    private(set) var sessionsLoaded = false
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
    /// Uncommitted changes asked for, per session.
    private(set) var changes: [String: (branch: String?, files: [FileDiff], truncated: Bool, ahead: Int?)] = [:]
    /// Sessions with a commit or push on its way.
    private(set) var committing: Set<String> = []
    /// What the last commit or push from the phone did, per session ("Committed abc1234 Fix · pushed").
    var commitResult: [String: String] = [:]
    /// Terminal screens being watched: session → (text, columns).
    private(set) var screens: [String: (text: String, cols: Int)] = [:]
    /// Models each session's agent can be switched to, as asked for.
    private(set) var models: [String: [ModelChoice]] = [:]
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
    @ObservationIgnored private var retryDelay: Task<Void, Never>?
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

    func retryNow() {
        if state == .connected {
            send(.listSessions)
        } else if state != .unauthorized {
            backoff = .seconds(1)
            retryDelay?.cancel()
            connect()
        }
    }

    func disconnect() {
        retryDelay?.cancel()
        retryDelay = nil
        backoff = .seconds(1)
        loop?.cancel()
        loop = nil
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        sessionsLoaded = false
        timelines.values.forEach { $0.isLive = false }
        if credentials != nil, state != .unauthorized { state = .offline("Disconnected") }
    }

    private func run() async {
        while !Task.isCancelled, let creds = credentials {
            state = .connecting
            let ok = await runOnce(creds)
            if Task.isCancelled || state == .unauthorized { break }
            if ok { backoff = .seconds(1) }
            let delay = backoff
            let sleeper = Task<Void, Never> {
                try? await Task.sleep(for: delay)
            }
            retryDelay = sleeper
            await sleeper.value
            guard !Task.isCancelled else { return }
            retryDelay = nil
            if !sleeper.isCancelled { backoff = min(delay * 2, .seconds(30)) }
        }
        if !Task.isCancelled { loop = nil }
    }

    /// One connection; returns whether it got as far as `hello`.
    private func runOnce(_ creds: Credentials) async -> Bool {
        var req = URLRequest(url: creds.wsURL)
        req.setValue("Bearer \(creds.token)", forHTTPHeaderField: "Authorization")
        let t = session.webSocketTask(with: req)
        // The default is 1 MiB: a bigger message (a long history from an older Mac
        // app) would drop the connection, and again on every reconnect.
        t.maximumMessageSize = 16 * 1024 * 1024
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
                guard !Task.isCancelled else { return greeted }
                guard case .string(let text) = msg, let data = text.data(using: .utf8) else { continue }
                // One unreadable message must not drop the connection (it would come
                // back on reconnect and drop it again, forever).
                guard let decoded = try? JSONDecoder().decode(ServerMsg.self, from: data) else { continue }
                if case .hello = decoded { greeted = true }
                handle(decoded)
            }
        } catch {
            guard !Task.isCancelled else { return greeted }
            if let http = t.response as? HTTPURLResponse, http.statusCode == 401 {
                state = .unauthorized
            } else if !Task.isCancelled {
                state = .offline(greeted ? "Connection lost" : "Can't reach \(creds.host)")
            }
        }
        sessionsLoaded = false
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
            for id in watched.union(preloaded) { subscribe(id) }
            registerPush()
            for (kind, token) in activityTokens { send(.registerActivityToken(kind: kind, token: token ?? nil)) }
        case .sessions(let list):
            sessions = list.sorted { $0.updatedMs > $1.updatedMs }
            sessionsLoaded = true
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
        case .changes(let sid, let branch, let diff, let truncated, let ahead):
            changes[sid] = (branch, GitChanges.files(diff), truncated, ahead)
        case .models(let sid, let list):
            models[sid] = list
        case .committed(let sid, let commit, let pushed):
            committing.remove(sid)
            commitResult[sid] = CommitText.result(commit: commit, pushed: pushed)
        case .screen(let sid, let text, let cols):
            screens[sid] = (text, cols)
        case .preview(let sid, let text):
            timelines[sid]?.showPreview(text)
        case .sessionStarting(let sid):
            // Open it right away; it fills in as the agent starts.
            if let p = startPrompt { timeline(for: sid).addPending(p); startPrompt = nil }
            requestedSession = sid
            send(.listSessions)
        case .error(let message):
            lastError = message
            committing.removeAll()
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
        let already = preloaded.contains(id) && timelines[id]?.isLive == true
        watched.insert(id)
        if state == .connected, !already { subscribe(id) }
    }

    func unwatch(_ id: String) {
        watched.remove(id)
        // Preloaded sessions stay subscribed, so going back in is instant.
        guard !preloaded.contains(id) else { return }
        send(.unsubscribe(session: id))
        timelines[id]?.isLive = false
    }

    /// Sessions whose conversation is fetched before you open them (the active ones).
    @ObservationIgnored private var preloaded: Set<String> = []

    /// Keep the conversations of `ids` loaded and live; drop the ones no longer in it.
    func preload(_ ids: [String]) {
        let want = Set(ids)
        for id in preloaded.subtracting(want) where !watched.contains(id) {
            send(.unsubscribe(session: id))
            timelines[id]?.isLive = false
        }
        let new = want.subtracting(preloaded).subtracting(watched)
        preloaded = want
        if state == .connected { for id in new { subscribe(id) } }
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
    /// Switch a session's permission mode (Claude in `styr claude`). The Mac presses
    /// Shift+Tab until its footer shows it; the session's `mode` follows. If it
    /// hasn't after a few seconds, say so (that version of Claude may not have it).
    func setMode(_ session: String, to mode: PermissionMode) {
        guard state == .connected else { return }
        send(.setMode(session: session, mode: mode.rawValue))
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(6))
            guard let self, let s = self.sessions.first(where: { $0.id == session }),
                  s.mode != mode.rawValue, self.lastError == nil else { return }
            self.lastError = "Claude didn't switch to \(mode.label). This version may not have that mode, or a dialog is open in its terminal."
        }
    }

    /// Ask what this session's agent can be switched to (arrives in `models`).
    func getModels(_ session: String) {
        guard state == .connected else { return }
        send(.getModels(session: session))
    }

    /// Restart the agent with another model and/or effort, resuming the conversation.
    func setModel(_ session: String, model: String?, effort: String?) {
        guard state == .connected else { lastError = "Not connected to your Mac."; return }
        send(.setModel(session: session, model: model, effort: effort))
    }

    /// Ask for the session folder's uncommitted changes (they arrive in `changes`).
    func getChanges(_ session: String) {
        guard state == .connected else { return }
        changes[session] = nil
        send(.getChanges(session: session))
    }

    /// Commit everything in the session's folder (skipped if `message` is empty), then
    /// push if asked. Fresh changes follow.
    func commit(_ session: String, message: String, push: Bool) {
        guard state == .connected else { lastError = "Not connected to your Mac."; return }
        committing.insert(session)
        commitResult[session] = nil
        send(.commit(session: session, message: message, push: push))
    }

    /// Ask for a session's terminal screen for the next 20 s (call again to keep it).
    func watchScreen(_ session: String) {
        guard state == .connected else { return }
        send(.watchScreen(session: session))
    }

    /// Stop the agent's current turn (Esc in its terminal).
    func interrupt(_ session: String) {
        guard state == .connected else { return }
        send(.interrupt(session: session))
    }

    /// Quit the agent on the Mac (closing the tab if the phone opened it) and drop it
    /// from the list.
    func endSession(_ session: String) {
        guard state == .connected else { return }
        sessions.removeAll { $0.id == session }
        send(.endSession(session: session))
    }

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
    /// Save an image or file on the Mac for a message; returns its path there.
    func upload(session: String, name: String, data: Data) async throws -> String {
        guard let creds = credentials else { throw QuickAnswer.Failure.status(0, "Not paired") }
        var c = URLComponents(url: creds.httpBase.appending(path: "v1/upload").appending(path: session),
                              resolvingAgainstBaseURL: false)!
        c.queryItems = [URLQueryItem(name: "name", value: name)]
        var req = URLRequest(url: c.url!)
        req.httpMethod = "POST"
        req.setValue("Bearer \(creds.token)", forHTTPHeaderField: "Authorization")
        req.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        req.timeoutInterval = 60
        let (body, resp) = try await URLSession.shared.upload(for: req, from: data)
        let status = (resp as? HTTPURLResponse)?.statusCode ?? 0
        let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any]
        guard status == 200, let path = json?["path"] as? String else {
            let why = status == 413 ? "\(name) is too large (25 MB at most)." : (json?["error"] as? String ?? "Upload failed (\(status)).")
            throw QuickAnswer.Failure.status(status, why)
        }
        return path
    }

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
