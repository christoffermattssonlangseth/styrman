// Swift mirror of the styrd phone protocol (crates/styr-core/src/{event,ipc,phone}.rs).
// Field names are the Rust serde names; decoding is tolerant: unknown event or
// message types decode to `.unknown` so an older app keeps working with a newer daemon.

import Foundation

// MARK: - JSON value (tool inputs, raw outputs)

enum JSONValue: Codable, Hashable, Sendable {
    case null, bool(Bool), number(Double), string(String)
    case array([JSONValue]), object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let b = try? c.decode(Bool.self) { self = .bool(b) }
        else if let n = try? c.decode(Double.self) { self = .number(n) }
        else if let s = try? c.decode(String.self) { self = .string(s) }
        else if let a = try? c.decode([JSONValue].self) { self = .array(a) }
        else { self = .object(try c.decode([String: JSONValue].self)) }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let b): try c.encode(b)
        case .number(let n): try c.encode(n)
        case .string(let s): try c.encode(s)
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        }
    }

    subscript(key: String) -> JSONValue? {
        if case .object(let o) = self { return o[key] }
        return nil
    }

    var string: String? {
        if case .string(let s) = self { return s }
        return nil
    }

    /// Compact, human-readable rendering (pretty for containers).
    var pretty: String {
        if case .string(let s) = self { return s }
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        return (try? String(data: enc.encode(self), encoding: .utf8)) ?? ""
    }
}

// MARK: - Sessions

enum AgentKind: String, Codable, Sendable {
    case claude, antigravity, codex, pty
    case unknown

    init(from decoder: Decoder) throws {
        self = AgentKind(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }

    var displayName: String {
        switch self {
        case .claude: "Claude"
        case .antigravity: "agy"
        case .codex: "Codex"
        case .pty: "Shell"
        case .unknown: "Agent"
        }
    }
}

enum SessionStatus: String, Codable, Sendable {
    case working, needsApproval = "needs_approval", waiting, error, done
    case unknown

    init(from decoder: Decoder) throws {
        self = SessionStatus(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }

    var needsUser: Bool { self == .needsApproval || self == .waiting }
}

struct SessionSummary: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let agent: AgentKind
    let nativeId: String
    let cwd: String?
    let title: String?
    let model: String?
    let shared: Bool
    let status: SessionStatus
    let createdMs: Int64
    let updatedMs: Int64
    let lastSeq: UInt64

    enum CodingKeys: String, CodingKey {
        case id, agent, cwd, title, model, shared, status
        case nativeId = "native_id", createdMs = "created_ms", updatedMs = "updated_ms", lastSeq = "last_seq"
    }

    /// Repository / folder name for the list.
    var repoName: String {
        guard let cwd, !cwd.isEmpty else { return title ?? "Session" }
        return URL(fileURLWithPath: cwd).lastPathComponent
    }

    var updated: Date { Date(timeIntervalSince1970: Double(updatedMs) / 1000) }
}

// MARK: - Events

enum ToolKind: String, Codable, Sendable {
    case read, edit, delete, move, search, execute, think, fetch, mcp, other
    init(from decoder: Decoder) throws {
        self = ToolKind(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .other
    }
}

enum ToolStatus: String, Codable, Sendable {
    case pending, running, completed, failed
    init(from decoder: Decoder) throws {
        self = ToolStatus(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .pending
    }
}

struct ToolInfo: Codable, Hashable, Sendable {
    let name: String
    let kind: ToolKind
    let title: String
    let input: JSONValue
    let locations: [String]?
}

enum ToolOutput: Codable, Hashable, Sendable {
    case text(String, truncated: Bool)
    case diff(path: String, diff: String)
    case json(JSONValue)

    private enum K: String, CodingKey { case type, text, truncated, path, diff, value }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: K.self)
        switch try c.decode(String.self, forKey: .type) {
        case "text": self = .text(try c.decode(String.self, forKey: .text), truncated: (try? c.decode(Bool.self, forKey: .truncated)) ?? false)
        case "diff": self = .diff(path: try c.decode(String.self, forKey: .path), diff: try c.decode(String.self, forKey: .diff))
        default: self = .json((try? c.decode(JSONValue.self, forKey: .value)) ?? .null)
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: K.self)
        switch self {
        case .text(let t, let tr): try c.encode("text", forKey: .type); try c.encode(t, forKey: .text); try c.encode(tr, forKey: .truncated)
        case .diff(let p, let d): try c.encode("diff", forKey: .type); try c.encode(p, forKey: .path); try c.encode(d, forKey: .diff)
        case .json(let v): try c.encode("json", forKey: .type); try c.encode(v, forKey: .value)
        }
    }

    var displayText: String {
        switch self {
        case .text(let t, let truncated): truncated ? t + "\n…" : t
        case .diff(_, let d): d
        case .json(let v): v.pretty
        }
    }
}

enum OptionKind: String, Codable, Sendable {
    case allowOnce = "allow_once", allowAlways = "allow_always", rejectOnce = "reject_once", rejectAlways = "reject_always"
    var isAllow: Bool { self == .allowOnce || self == .allowAlways }
}

struct PermissionOption: Codable, Hashable, Sendable, Identifiable {
    let id: String
    let kind: OptionKind
    let label: String
}

struct PlanEntry: Codable, Hashable, Sendable {
    let content: String
    let status: String // pending | in_progress | completed
}

enum Resolver: Hashable, Sendable {
    case phone(String), terminal, otherClient, daemon
}

enum PermissionOutcome: Hashable, Sendable {
    case selected(option: String, kind: OptionKind), cancelled, timedOut
}

enum EventKind: Sendable, Hashable {
    case sessionStarted(cwd: String?, model: String?, start: String)
    case sessionEnded(reason: String?)
    case sharingChanged(shared: Bool)
    case turnStarted
    case userMessage(text: String, fromPhone: Bool)
    case assistantMessage(id: String, text: String, delta: Bool)
    case reasoning(id: String, text: String, delta: Bool)
    case plan([PlanEntry])
    case toolCall(call: String, tool: ToolInfo, status: ToolStatus, output: ToolOutput?)
    case permissionRequested(request: String, tool: ToolInfo, reason: String?, options: [PermissionOption], expiresAtMs: Int64?)
    case permissionResolved(request: String, outcome: PermissionOutcome, by: Resolver)
    case turnEnded(reason: String, lastMessage: String?)
    case notice(text: String, warning: Bool)
    case error(message: String, fatal: Bool)
    case unknown(type: String)
}

extension EventKind: Decodable {
    private enum K: String, CodingKey {
        case type, cwd, model, start, reason, shared, text, origin, from, message, mode, entries
        case call, tool, status, output, request, options, outcome, option, kind, by, level, fatal
        case expiresAtMs = "expires_at_ms", lastMessage = "last_message"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: K.self)
        let type = try c.decode(String.self, forKey: .type)
        func s(_ k: K) -> String? { (try? c.decodeIfPresent(String.self, forKey: k)) ?? nil }
        switch type {
        case "session_started":
            self = .sessionStarted(cwd: s(.cwd), model: s(.model), start: s(.start) ?? "new")
        case "session_ended":
            self = .sessionEnded(reason: s(.reason))
        case "sharing_changed":
            self = .sharingChanged(shared: try c.decode(Bool.self, forKey: .shared))
        case "turn_started":
            self = .turnStarted
        case "user_message":
            let origin = try c.nestedContainer(keyedBy: K.self, forKey: .origin)
            let from = try origin.decode(String.self, forKey: .from)
            self = .userMessage(text: try c.decode(String.self, forKey: .text), fromPhone: from == "remote")
        case "assistant_message", "reasoning":
            let id = try c.decode(String.self, forKey: .message)
            let text = try c.decode(String.self, forKey: .text)
            let delta = s(.mode) == "delta"
            self = type == "reasoning" ? .reasoning(id: id, text: text, delta: delta) : .assistantMessage(id: id, text: text, delta: delta)
        case "plan":
            self = .plan(try c.decode([PlanEntry].self, forKey: .entries))
        case "tool_call":
            self = .toolCall(
                call: try c.decode(String.self, forKey: .call),
                tool: try c.decode(ToolInfo.self, forKey: .tool),
                status: try c.decode(ToolStatus.self, forKey: .status),
                output: try c.decodeIfPresent(ToolOutput.self, forKey: .output))
        case "permission_requested":
            self = .permissionRequested(
                request: try c.decode(String.self, forKey: .request),
                tool: try c.decode(ToolInfo.self, forKey: .tool),
                reason: s(.reason),
                options: try c.decode([PermissionOption].self, forKey: .options),
                expiresAtMs: try c.decodeIfPresent(Int64.self, forKey: .expiresAtMs))
        case "permission_resolved":
            let o = try c.nestedContainer(keyedBy: K.self, forKey: .outcome)
            let outcome: PermissionOutcome
            switch try o.decode(String.self, forKey: .outcome) {
            case "selected":
                outcome = .selected(option: try o.decode(String.self, forKey: .option), kind: try o.decode(OptionKind.self, forKey: .kind))
            case "timed_out": outcome = .timedOut
            default: outcome = .cancelled
            }
            let b = try c.nestedContainer(keyedBy: DynamicKey.self, forKey: .by)
            let by: Resolver
            switch try b.decode(String.self, forKey: DynamicKey("by")) {
            case "phone": by = .phone((try? b.decode(String.self, forKey: DynamicKey("device"))) ?? "")
            case "terminal": by = .terminal
            case "other_client": by = .otherClient
            default: by = .daemon
            }
            self = .permissionResolved(request: try c.decode(String.self, forKey: .request), outcome: outcome, by: by)
        case "turn_ended":
            self = .turnEnded(reason: s(.reason) ?? "end_turn", lastMessage: s(.lastMessage))
        case "notice":
            self = .notice(text: try c.decode(String.self, forKey: .text), warning: s(.level) == "warning")
        case "error":
            self = .error(message: try c.decode(String.self, forKey: .message), fatal: (try? c.decode(Bool.self, forKey: .fatal)) ?? false)
        default:
            self = .unknown(type: type)
        }
    }
}

struct DynamicKey: CodingKey {
    var stringValue: String
    var intValue: Int? { nil }
    init(_ s: String) { stringValue = s }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { nil }
}

struct Event: Decodable, Sendable, Hashable {
    let session: String
    let seq: UInt64
    let atMs: Int64
    let source: String
    let kind: EventKind

    enum CodingKeys: String, CodingKey { case session, seq, source, kind, atMs = "at_ms" }

    var at: Date { Date(timeIntervalSince1970: Double(atMs) / 1000) }
}

// MARK: - Server → phone

enum ServerMsg: Decodable, Sendable {
    case hello(protocolVersion: Int, daemonVersion: String, deviceId: String, host: String)
    case sessions([SessionSummary])
    case sessionUpdated(SessionSummary)
    case sessionRemoved(id: String)
    case events(session: String, events: [Event])
    case subscribed(session: String, lastSeq: UInt64)
    case event(Event)
    case promptQueued(session: String, text: String, position: Int)
    case error(String)
    case unknown(type: String)

    private enum K: String, CodingKey {
        case type, `protocol`, host, sessions, session, id, events, event, message, text, position
        case daemonVersion = "daemon_version", deviceId = "device_id", lastSeq = "last_seq"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: K.self)
        let type = try c.decode(String.self, forKey: .type)
        switch type {
        case "hello":
            self = .hello(
                protocolVersion: try c.decode(Int.self, forKey: .protocol),
                daemonVersion: try c.decode(String.self, forKey: .daemonVersion),
                deviceId: try c.decode(String.self, forKey: .deviceId),
                host: try c.decode(String.self, forKey: .host))
        case "sessions": self = .sessions(try c.decode([SessionSummary].self, forKey: .sessions))
        case "session_updated": self = .sessionUpdated(try c.decode(SessionSummary.self, forKey: .session))
        case "session_removed": self = .sessionRemoved(id: try c.decode(String.self, forKey: .id))
        case "events": self = .events(session: try c.decode(String.self, forKey: .session), events: try c.decode([Event].self, forKey: .events))
        case "subscribed": self = .subscribed(session: try c.decode(String.self, forKey: .session), lastSeq: try c.decode(UInt64.self, forKey: .lastSeq))
        case "event": self = .event(try c.decode(Event.self, forKey: .event))
        case "prompt_queued":
            self = .promptQueued(session: try c.decode(String.self, forKey: .session),
                                 text: try c.decode(String.self, forKey: .text),
                                 position: try c.decode(Int.self, forKey: .position))
        case "error": self = .error(try c.decode(String.self, forKey: .message))
        default: self = .unknown(type: type)
        }
    }
}

// MARK: - Phone → server

enum ClientMsg: Encodable, Sendable {
    case listSessions
    case subscribe(session: String, sinceSeq: UInt64)
    case unsubscribe(session: String)
    case resolvePermission(session: String, request: String, option: String)
    case sendPrompt(session: String, text: String)
    case registerPush(token: String, env: String)

    private enum K: String, CodingKey { case type, session, command, request, option, text, token, env, sinceSeq = "since_seq" }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: K.self)
        switch self {
        case .listSessions:
            try c.encode("list_sessions", forKey: .type)
        case .subscribe(let s, let since):
            try c.encode("subscribe", forKey: .type); try c.encode(s, forKey: .session); try c.encode(since, forKey: .sinceSeq)
        case .unsubscribe(let s):
            try c.encode("unsubscribe", forKey: .type); try c.encode(s, forKey: .session)
        case .resolvePermission(let s, let request, let option):
            try c.encode("command", forKey: .type); try c.encode(s, forKey: .session)
            var cmd = c.nestedContainer(keyedBy: K.self, forKey: .command)
            try cmd.encode("resolve_permission", forKey: .type)
            try cmd.encode(request, forKey: .request)
            try cmd.encode(option, forKey: .option)
        case .registerPush(let token, let env):
            try c.encode("register_push", forKey: .type); try c.encode(token, forKey: .token); try c.encode(env, forKey: .env)
        case .sendPrompt(let s, let text):
            try c.encode("command", forKey: .type); try c.encode(s, forKey: .session)
            var cmd = c.nestedContainer(keyedBy: K.self, forKey: .command)
            try cmd.encode("send_prompt", forKey: .type)
            try cmd.encode(text, forKey: .text)
        }
    }
}

// MARK: - Pairing

struct PairRequest: Encodable { let code: String; let deviceName: String
    enum CodingKeys: String, CodingKey { case code, deviceName = "device_name" } }

struct PairResponse: Decodable { let deviceId: String; let token: String; let host: String
    enum CodingKeys: String, CodingKey { case token, host, deviceId = "device_id" } }
