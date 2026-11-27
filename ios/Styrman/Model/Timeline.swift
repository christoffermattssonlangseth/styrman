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
        didSet {
            if isLive { loaded = true } else { preview = nil; status = nil }
        }
    }
    /// The reply the agent is writing right now, as its terminal shows it. Gone when
    /// the real message (or anything else from the agent) arrives.
    private(set) var preview: String?

    /// The agent's status line while it works ("Crunching… (3s · thinking)"), and
    /// when it was read (its clock runs on from there).
    private(set) var status: WorkStatus?
    private(set) var statusAt = Date()

    /// Since when it's been thinking (for "still thinking…").
    private(set) var thinkingSince: Date?

    func showStatus(_ s: WorkStatus?) {
        guard s != status else { return }
        if s?.thinking == true { if status?.thinking != true { thinkingSince = .now } } else { thinkingSince = nil }
        status = s
        statusAt = .now
    }

    /// A shorter preview waiting to be shown (see `showPreview`).
    @ObservationIgnored private var shrinkTask: Task<Void, Never>?

    func showPreview(_ text: String?) {
        // A preview that gets shorter (or goes) waits a moment: a terminal redrawing
        // can flip between two readings, and the conversation would jump with each.
        // If a reading at least as long comes back meanwhile, the shorter one is
        // never shown. (Real messages and the turn ending clear it at once.)
        let now = (text ?? "").count
        if let current = preview, now < current.count {
            shrinkTask?.cancel()
            shrinkTask = Task { [weak self] in
                try? await Task.sleep(for: .seconds(1))
                guard !Task.isCancelled else { return }
                self?.setPreview(text)
            }
            return
        }
        shrinkTask?.cancel()
        setPreview(text)
    }

    /// Whether `preview` is a reply already among `items` (as a message). The
    /// preview is read off the terminal (bold as `**`, a heading as bold, code as
    /// backticks only where coloured) and the message is Claude's Markdown: compare
    /// the words, not the markup.
    nonisolated static func alreadySaid(_ preview: String, in items: some Collection<TimelineItem>) -> Bool {
        let words = { (s: String) in
            s.filter { !"*`#>".contains($0) }.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        }
        let body = String(words(preview.hasPrefix("…") ? String(preview.dropFirst()) : preview).prefix(200))
        guard !body.isEmpty else { return false }
        return items.contains { item in
            if case .assistant(_, let said, _) = item { words(said).contains(body) } else { false }
        }
    }

    private func setPreview(_ text: String?) {
        guard let text, !text.isEmpty else { preview = nil; return }
        // Already here as a message (the screen redrew a finished reply): no echo.
        if Self.alreadySaid(text, in: items.suffix(12)) {
            preview = nil
            return
        }
        preview = text
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
    /// When each tool call first showed up (to place text read from the transcript).
    @ObservationIgnored private var toolSeen: [String: Date] = [:]
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

    /// Tool calls that just arrived live, shown a moment later: the text the agent
    /// wrote before a tool often arrives just after it (Claude writes it to its
    /// transcript late), and would otherwise land above a card already on screen,
    /// splitting a group of steps apart: the conversation would jump.
    @ObservationIgnored private var holdUntil: [String: Date] = [:]
    private var reveals = 0
    static let toolHold: Duration = .milliseconds(1300)

    /// `items` as shown: without tool calls still held back.
    var shownItems: [TimelineItem] {
        _ = reveals // re-read when a held tool is let through
        guard !holdUntil.isEmpty else { return items }
        let now = Date()
        return items.filter { item in
            guard case .tool(let id, _) = item, let until = holdUntil[id] else { return true }
            return until <= now
        }
    }

    private func hold(_ id: String) {
        holdUntil[id] = Date().addingTimeInterval(1.3)
        let before = preview
        Task { [weak self] in
            try? await Task.sleep(for: Self.toolHold + .milliseconds(20))
            guard let self else { return }
            // The step shows now: the text before it is done, if it's still here.
            if before != nil, self.preview == before { self.shrinkTask?.cancel(); self.preview = nil }
            self.holdUntil[id] = nil
            self.reveals &+= 1
        }
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

    /// Insert at `i`, keeping the position indexes right.
    private func insert(_ item: TimelineItem, at i: Int) {
        guard i < items.count else { return append(item) }
        items.insert(item, at: i)
        func shift(_ d: inout [String: Int]) { for (k, v) in d where v >= i { d[k] = v + 1 } }
        shift(&toolIndex); shift(&permissionIndex); shift(&messageIndex)
        if let p = planIndex, p >= i { planIndex = p + 1 }
    }

    /// Where text the agent wrote goes when it's read from the transcript: Claude
    /// announces a tool (by hook) just before it writes the text that came first, so
    /// it goes above the tool calls that showed up in the last two seconds.
    private func placeForTranscriptText(at: Date) -> Int {
        var i = items.count
        while i > 0, case .tool(_, let t) = items[i - 1],
              let seen = toolSeen[t.call], seen >= at.addingTimeInterval(-2) {
            i -= 1
        }
        return i
    }

    private func apply(_ e: Event) {
        let key = "\(e.seq)"
        switch e.kind {
        case .assistantMessage, .userMessage, .permissionRequested, .turnEnded, .sessionEnded:
            shrinkTask?.cancel()
            preview = nil
            if case .turnEnded = e.kind { status = nil }
            if case .sessionEnded = e.kind { status = nil }
        // Live, a new step is held back a moment (see `hold`) for the text written
        // before it: that text, still showing as the preview, becomes the message in
        // place instead of blinking out first.
        case .toolCall where !isLive:
            preview = nil
        default:
            break
        }
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
            } else if e.source == "transcript" {
                let i = placeForTranscriptText(at: e.at)
                insert(.assistant(id: key, text: text, at: e.at), at: i)
                messageIndex[id] = i
            } else {
                messageIndex[id] = items.count
                append(.assistant(id: key, text: text, at: e.at))
            }
        case .toolCall(let call, let tool, let status, let output):
            let item = ToolItem(call: call, tool: tool, status: status, output: output)
            if !call.isEmpty, let i = toolIndex[call], case .tool(let iid, _) = items[i] {
                items[i] = .tool(id: iid, item)
            } else {
                if !call.isEmpty {
                    toolIndex[call] = items.count
                    toolSeen[call] = e.at
                }
                if isLive { hold(key) }
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
