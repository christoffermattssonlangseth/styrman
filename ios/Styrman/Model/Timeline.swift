// Folds a session's event stream into display items. Tool calls update in place by
// call id, permission cards close when resolved, streamed text merges by message id.

import Foundation
import Observation

struct ToolItem: Hashable, Sendable {
    var call: String
    var tool: ToolInfo
    var status: ToolStatus
    var output: ToolOutput?
}

struct PermissionItem: Hashable, Sendable {
    var request: String
    var tool: ToolInfo
    var reason: String?
    var options: [PermissionOption]
    var expiresAt: Date?
    var resolution: (outcome: PermissionOutcome, by: Resolver)?

    static func == (a: Self, b: Self) -> Bool {
        a.request == b.request && a.isOpen == b.isOpen
    }
    func hash(into h: inout Hasher) { h.combine(request) }

    var isOpen: Bool { resolution == nil }

    var resolutionText: String? {
        guard let (outcome, by) = resolution else { return nil }
        let who: String = switch by {
        case .phone: "on the phone"
        case .terminal: "in the terminal"
        case .otherClient: "elsewhere"
        case .daemon: "by Styrman"
        }
        switch outcome {
        case .selected(_, let kind): return (kind.isAllow ? "Allowed " : "Denied ") + who
        case .timedOut: return "Not answered from the phone in time"
        case .cancelled: return "Answered " + who
        }
    }
}

enum TimelineItem: Identifiable, Hashable, Sendable {
    case divider(id: String, text: String, at: Date)
    case user(id: String, text: String, fromPhone: Bool, at: Date)
    case assistant(id: String, text: String, at: Date)
    case tool(id: String, ToolItem)
    case permission(id: String, PermissionItem)
    case plan(id: String, [PlanEntry])
    case notice(id: String, text: String, warning: Bool)

    var id: String {
        switch self {
        case .divider(let id, _, _), .user(let id, _, _, _), .assistant(let id, _, _), .tool(let id, _),
             .permission(let id, _), .plan(let id, _), .notice(let id, _, _): id
        }
    }
}

@MainActor
@Observable
final class SessionTimeline {
    let sessionId: String
    private(set) var items: [TimelineItem] = []
    private(set) var lastSeq: UInt64 = 0
    /// History caught up and live events flowing.
    var isLive = false {
        didSet { if isLive { loaded = true } }
    }
    /// The history has been fully loaded at least once (it stays cached after that).
    private(set) var loaded = false
    /// Prompts sent from this phone that the agent hasn't picked up yet.
    private(set) var pending: [PendingPrompt] = []

    struct PendingPrompt: Identifiable, Hashable, Sendable {
        let id = UUID()
        let text: String
        var queued = false
        /// The Mac handed it to the agent (typed into its terminal); the agent
        /// submits it when it gets to it.
        var delivered = false
        var unconfirmed = false
    }

    func markUnconfirmed(_ id: UUID) {
        if let i = pending.firstIndex(where: { $0.id == id && !$0.queued && !$0.delivered }) {
            pending[i].unconfirmed = true
        }
    }

    func markDelivered(_ text: String) {
        if let i = pending.firstIndex(where: { $0.text == text && !$0.delivered }) {
            pending[i].delivered = true
            pending[i].unconfirmed = false
        }
    }

    @discardableResult
    func addPending(_ text: String) -> UUID {
        let prompt = PendingPrompt(text: text)
        pending.append(prompt)
        return prompt.id
    }

    func markQueued(_ text: String) {
        if let i = pending.firstIndex(where: { $0.text == text && !$0.queued && !$0.delivered }) {
            pending[i].queued = true
            pending[i].unconfirmed = false
        }
    }

    func dismissUnconfirmed(_ id: UUID) {
        pending.removeAll { $0.id == id && $0.unconfirmed }
    }

    func dropPending(_ text: String) {
        if let i = pending.firstIndex(where: { $0.text == text }) { pending.remove(at: i) }
    }

    @ObservationIgnored private var toolIndex: [String: Int] = [:]
    @ObservationIgnored private var permissionIndex: [String: Int] = [:]
    @ObservationIgnored private var messageIndex: [String: Int] = [:]
    @ObservationIgnored private var planIndex: Int?

    init(sessionId: String) { self.sessionId = sessionId }

    /// What an edit approval would change. Codex approvals read off the terminal carry
    /// only the path, so they borrow the patch from the latest edit step for that file.
    func diffs(for permission: PermissionItem) -> [FileDiff]? {
        guard permission.tool.kind == .edit else { return nil }
        if let d = FileDiff.from(permission.tool) { return d }
        let paths = Set(permission.tool.locations ?? []).union([permission.tool.input["path"]?.string].compactMap { $0 })
        for item in items.reversed() {
            guard case .tool(_, let t) = item, t.tool.kind == .edit,
                  let d = FileDiff.from(t.tool) else { continue }
            let mine = Set(d.map(\.path)).union(t.tool.locations ?? [])
            if paths.isEmpty || !mine.isDisjoint(with: paths) || mine.contains(where: { m in paths.contains { $0.hasSuffix(m) || m.hasSuffix($0) } }) {
                return d
            }
        }
        return nil
    }

    /// Anything beyond start/share markers.
    var hasContent: Bool {
        items.contains { if case .divider = $0 { false } else { true } }
    }

    var openPermissions: [PermissionItem] {
        items.compactMap { if case .permission(_, let p) = $0, p.isOpen { p } else { nil } }
    }

    func apply(_ events: [Event]) {
        for e in events where e.seq > lastSeq {
            apply(e)
            lastSeq = e.seq
        }
    }

    private func append(_ item: TimelineItem) { items.append(item) }

    private func apply(_ e: Event) {
        let key = "\(e.seq)"
        switch e.kind {
        case .sessionStarted(_, _, let start):
            append(.divider(id: key, text: start == "resume" ? "Session resumed" : "Session started", at: e.at))
        case .sessionEnded:
            append(.divider(id: key, text: "Session ended", at: e.at))
        case .sharingChanged(let shared):
            append(.divider(id: key, text: shared ? "Shared with this phone" : "Sharing stopped", at: e.at))
        case .userMessage(let text, let fromPhone):
            // Match by text too: if the Mac lost track (restart), the prompt we
            // typed comes back looking like terminal input.
            dropPending(text)
            append(.user(id: key, text: text, fromPhone: fromPhone, at: e.at))
        case .assistantMessage(let id, let text, let delta):
            if let i = messageIndex[id], case .assistant(let iid, let old, let at) = items[i] {
                items[i] = .assistant(id: iid, text: delta ? old + text : text, at: at)
            } else {
                messageIndex[id] = items.count
                append(.assistant(id: key, text: text, at: e.at))
            }
        case .toolCall(let call, let tool, let status, let output):
            let item = ToolItem(call: call, tool: tool, status: status, output: output)
            if !call.isEmpty, let i = toolIndex[call], case .tool(let iid, _) = items[i] {
                items[i] = .tool(id: iid, item)
            } else {
                if !call.isEmpty { toolIndex[call] = items.count }
                append(.tool(id: key, item))
            }
        case .permissionRequested(let request, let tool, let reason, let options, let expires):
            permissionIndex[request] = items.count
            let exp = expires.map { Date(timeIntervalSince1970: Double($0) / 1000) }
            append(.permission(id: key, PermissionItem(request: request, tool: tool, reason: reason, options: options, expiresAt: exp)))
        case .permissionResolved(let request, let outcome, let by):
            if let i = permissionIndex[request], case .permission(let iid, var p) = items[i] {
                p.resolution = (outcome, by)
                items[i] = .permission(id: iid, p)
            }
        case .plan(let entries):
            if let i = planIndex, case .plan(let iid, _) = items[i] {
                items[i] = .plan(id: iid, entries)
            } else {
                planIndex = items.count
                append(.plan(id: key, entries))
            }
        case .notice(let text, let warning):
            append(.notice(id: key, text: text, warning: warning))
        case .error(let message, _):
            append(.notice(id: key, text: message, warning: true))
        case .turnStarted, .turnEnded, .reasoning, .unknown:
            break
        }
    }
}
