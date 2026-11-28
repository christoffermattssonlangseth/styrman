import AVFoundation
import SwiftUI

struct SessionListView: View {
    @Environment(DaemonConnection.self) private var connection
    @State private var showSettings = false
    @State private var showNewSession = false
    @State private var showUsage = false
    @State private var path: [String] = []
    /// Sections folded shut (Done starts so).
    @State private var folded: Set<String> = ["Done"]
    /// Sessions whose older helpers are unfolded.
    @State private var openHelpers: Set<String> = []
    @State private var ending: SessionSummary?
    @State private var renaming: SessionSummary?

    private var needsYou: [SessionSummary] { connection.sessions.filter { $0.status.needsUser } }
    private var working: [SessionSummary] { connection.sessions.filter { $0.status == .working || $0.status == .error } }
    private var done: [SessionSummary] { connection.sessions.filter { $0.status == .done || $0.status == .unknown } }

    var body: some View {
        NavigationStack(path: $path) {
            list
                .confirmationDialog("End this session?", isPresented: endingShown, titleVisibility: .visible,
                                    presenting: ending) { s in
                    Button("End session", role: .destructive) { connection.endSession(s.id) }
                } message: { s in
                    Text(EndSessionText.message(s))
                }
                .alert("Styrman", isPresented: errorShown) {
                    Button("OK") { connection.lastError = nil }
                } message: {
                    Text(connection.lastError ?? "")
                }
                .renameSession($renaming)
                .navigationTitle("Styrman")
                .toolbar { toolbar }
                .navigationDestination(for: String.self) { id in SessionDetailView(sessionId: id) }
                .sheet(isPresented: $showUsage) { UsageView() }
                .sheet(isPresented: $showSettings) { SettingsView() }
                .sheet(isPresented: $showNewSession) { NewSessionView() }
                .refreshable { connection.retryNow() }
                .onChange(of: connection.requestedSession, initial: true) { _, id in open(id) }
                .onChange(of: connection.pushedSession) { _, id in
                    guard let id else { return }
                    if path.last != id { path.append(id) }
                    connection.pushedSession = nil
                }
                .task { await AppDelegate.enablePush() }
                // Fetch the active sessions' conversations ahead, so opening one is instant.
                .onChange(of: activeIds, initial: true) { _, ids in connection.preload(ids) }
                #if DEBUG
                // UI automation (simulator): STYR_OPEN_FIRST=1 opens the first session once it appears.
                .onChange(of: connection.sessions.first?.id) { _, id in
                    if let id, path.isEmpty, ProcessInfo.processInfo.environment["STYR_OPEN_FIRST"] == "1" {
                        path = [id]
                    }
                }
                // Screenshots (simulator): STYR_OPEN=<part of a session's native id> opens it;
                // STYR_SHOW=new|usage|settings opens that sheet.
                .onChange(of: connection.sessions.count, initial: true) { _, _ in
                    let env = ProcessInfo.processInfo.environment
                    if let want = env["STYR_OPEN"], path.isEmpty,
                       let s = connection.sessions.first(where: { $0.nativeId.contains(want) }) {
                        path = [s.id]
                    }
                    switch env["STYR_SHOW"] {
                    case "new": showNewSession = true
                    case "usage": showUsage = true
                    case "settings": showSettings = true
                    default: break
                    }
                }
                #endif
        }
    }

    // Kept apart from `body`: in one expression it's too much for the type checker.
    private var list: some View {
        List {
            SummaryHeader()
                .plainRow(top: 2, bottom: 4)
            if connection.state == .unauthorized {
                UnpairedCard().plainRow()
            }
            ForEach(connection.warnings, id: \.self) { w in
                Label(w, systemImage: "exclamationmark.triangle").font(.footnote).foregroundStyle(.orange).plainRow()
            }
            group("Needs attention", needsYou)
            group("Working", working)
            group("Done", done)
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .background(HomeBackground())
        .animation(.snappy, value: connection.sessions.map { "\($0.id)\($0.status)" })
        .overlay {
            if connection.sessions.isEmpty && connection.state == .connected && connection.sessionsLoaded {
                ContentUnavailableView {
                    Label { Text("No shared sessions") } icon: { WheelMark(size: 56) }
                } description: {
                    Text("Type **/styr** in Claude Code, Codex or agy on \(connection.host), or start one with **+**.")
                }
                .padding(.top, 120)
            }
        }
        // Only a connection that stays down gets a word; quick reconnects (coming
        // back to the app) don't.
        .overlay(alignment: .bottom) {
            if connection.troubled, connection.state != .unauthorized {
                ConnectionPill().padding(.bottom, 12)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(.snappy, value: connection.troubled)
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .topBarLeading) { WheelMark(size: 26) }
        ToolbarItem(placement: .topBarTrailing) {
            Button { showUsage = true } label: { Image(systemName: "chart.bar") }
                .accessibilityLabel("Usage")
        }
        ToolbarItem(placement: .topBarTrailing) {
            Button { showSettings = true } label: { Image(systemName: "gearshape") }
                .accessibilityLabel("Settings")
        }
        ToolbarItem(placement: .topBarTrailing) {
            Button { showNewSession = true } label: { Image(systemName: "plus") }
                .accessibilityLabel("New session")
                .disabled(connection.state != .connected)
        }
    }

    private var endingShown: Binding<Bool> {
        Binding(get: { ending != nil }, set: { if !$0 { ending = nil } })
    }

    private var errorShown: Binding<Bool> {
        Binding(get: { connection.lastError != nil && path.isEmpty },
                set: { if !$0 { connection.lastError = nil } })
    }

    private var activeIds: [String] { (needsYou + working).prefix(8).map(\.id) }

    /// From a notification, the Live Activity or the widget: on top of everything.
    private func open(_ id: String?) {
        guard let id else { return }
        showSettings = false
        showUsage = false
        showNewSession = false
        path = [id]
        connection.requestedSession = nil
    }

    @ViewBuilder
    private func group(_ title: String, _ items: [SessionSummary]) -> some View {
        if !items.isEmpty {
            let shut = folded.contains(title)
            Button {
                withAnimation(.snappy) {
                    if shut { folded.remove(title) } else { folded.insert(title) }
                }
            } label: {
                HStack(spacing: 6) {
                    SectionTitle(title: title, count: items.count)
                    Image(systemName: "chevron.down")
                        .font(.caption2.weight(.bold)).foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(shut ? -90 : 0))
                    Spacer()
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityValue(shut ? "Folded" : "Open")
            .plainRow(top: 16, bottom: 6)
            if !shut {
                let lines = SessionLine.lines(items, open: openHelpers)
                ForEach(Array(lines.enumerated()), id: \.element.id) { i, line in
                    let first = i == 0, last = i == lines.count - 1
                    switch line {
                    case .session(let s, let parent): row(s, under: parent, first: first, last: last)
                    case .more(let parent, let hidden): moreRow(parent, hidden, last: last)
                    case .less(let parent): lessRow(parent, last: last)
                    }
                }
            }
        }
    }

    /// "6 more helpers": a session's older helpers, folded under its newest two.
    private func moreRow(_ parent: String, _ hidden: Int, last: Bool) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Divider().padding(.leading, 62)
            Button {
                withAnimation(.snappy) { _ = openHelpers.insert(parent) }
            } label: {
                HStack(spacing: 6) {
                    Text("\(hidden) more \(hidden == 1 ? "helper" : "helpers")")
                    Image(systemName: "chevron.down").font(.caption2.weight(.bold))
                }
                .font(.footnote).foregroundStyle(.secondary)
                .padding(.leading, 62).frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(RowPressStyle())
        }
        .rowBlock(first: false, last: last)
    }

    /// Folds a session's older helpers away again.
    private func lessRow(_ parent: String, last: Bool) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Divider().padding(.leading, 62)
            Button {
                withAnimation(.snappy) { _ = openHelpers.remove(parent) }
            } label: {
                HStack(spacing: 6) {
                    Text("Show fewer")
                    Image(systemName: "chevron.up").font(.caption2.weight(.bold))
                }
                .font(.footnote).foregroundStyle(.secondary)
                .padding(.leading, 62).frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(RowPressStyle())
        }
        .rowBlock(first: false, last: last)
    }

    /// One line a session, the section's rows sharing one rounded block.
    private func row(_ s: SessionSummary, under parent: SessionSummary?, first: Bool, last: Bool) -> some View {
        let timeline = connection.timeline(for: s.id)
        let asker = s.helperOf.flatMap { connection.session($0) }
        return VStack(alignment: .leading, spacing: 0) {
            if !first { Divider().padding(.leading, parent == nil ? 44 : 62) }
            Button { path.append(s.id) } label: {
                // The repo shows unless the parent right above already says it.
                SessionCard(session: s, helping: parent != nil,
                            showsRepo: parent?.repoName != s.repoName)
            }
            .buttonStyle(RowPressStyle())
            .accessibilityHint(asker.map { "Helping \($0.agent.displayName) in \($0.repoName)" } ?? "")
            if s.status == .needsApproval, timeline.isLive,
               let permission = timeline.openPermissions.last {
                SessionQuickApproval(session: s.id, cwd: s.cwd, permission: permission)
                    .id(permission.request)
                    .padding(.horizontal, 12).padding(.bottom, 10)
            }
        }
        .rowBlock(first: first, last: last)
        .swipeActions(edge: .trailing) {
            Button(s.status == .done ? "Remove" : "Stop sharing", role: .destructive) {
                connection.stopSharing(s.id)
            }
            if s.canEnd {
                Button("End") { ending = s }.tint(.orange)
            }
        }
        .swipeActions(edge: .leading) {
            Button("Rename") { renaming = s }.tint(.blue)
        }
        .contextMenu {
            Button { renaming = s } label: { Label("Rename…", systemImage: "pencil") }
            if s.canEnd {
                Button(role: .destructive) { ending = s } label: {
                    Label("End session", systemImage: "stop.circle")
                }
            }
            Button(role: .destructive) { connection.stopSharing(s.id) } label: {
                Label(s.status == .done ? "Remove" : "Stop sharing", systemImage: "eye.slash")
            }
        }
    }
}

private struct SessionQuickApproval: View {
    @Environment(DaemonConnection.self) private var connection
    let session: String
    var cwd: String?
    let permission: PermissionItem
    @State private var answering = false

    var body: some View {
        if let allow = permission.options.first(where: { $0.kind == .allowOnce }) {
            VStack(alignment: .leading, spacing: 8) {
                if let diffs = connection.timeline(for: session).diffs(for: permission) {
                    DiffPreview(diffs: diffs, cwd: cwd, maxLines: 6)
                } else {
                    Text(displayTitle(permission.tool, cwd: cwd))
                        .font(permission.tool.kind == .execute ? Theme.mono : .callout)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
                if let reason = permission.reason, !reason.isEmpty {
                    Text(reason).font(.footnote).foregroundStyle(.secondary)
                }
                HStack {
                    Button("Allow once") { answer(allow) }
                        .buttonStyle(.borderedProminent)
                        .tint(Theme.attention)
                    if let deny = permission.options.first(where: { $0.kind == .rejectOnce }) {
                        Menu {
                            Button("Deny", role: .destructive) { answer(deny) }
                        } label: {
                            Label("More approval options", systemImage: "ellipsis.circle")
                                .labelStyle(.iconOnly)
                        }
                    }
                    if answering { ProgressView().controlSize(.small) }
                }
                .disabled(answering || connection.state != .connected)
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.attention.opacity(0.10), in: RoundedRectangle(cornerRadius: 14))
        }
    }

    private func answer(_ option: PermissionOption) {
        guard connection.state == .connected else { return }
        // Over the open connection, whichever way it reaches the Mac (Tailscale or
        // the local network); the card goes when the Mac reports the answer.
        answering = connection.resolvePermission(session: session, request: permission.request, option: option.id,
                                                 failed: { answering = false })
        if answering {
            Task {
                try? await Task.sleep(for: .seconds(10))
                answering = false
            }
        }
    }
}

private extension View {
    /// A list row without the list's chrome: no separator, no background, our own insets.
    func plainRow(top: CGFloat = 4, bottom: CGFloat = 4) -> some View {
        listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
            .listRowInsets(EdgeInsets(top: top, leading: 16, bottom: bottom, trailing: 16))
    }
}

/// A faint wash of the helm's red at the top, over the grouped background.
private struct HomeBackground: View {
    var body: some View {
        ZStack(alignment: .top) {
            Theme.canvas
        }
        .ignoresSafeArea()
    }
}

/// The Mac and the connection, quietly, under the title.
private struct SummaryHeader: View {
    @Environment(DaemonConnection.self) private var connection

    var body: some View {
        HStack(spacing: 7) {
            ConnectionDot()
            Text(connection.host.isEmpty ? "Your Mac" : connection.host)
                .font(.footnote.weight(.medium)).foregroundStyle(.secondary).lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}

/// Green when connected; amber while (re)connecting; grey after a while offline.
private struct ConnectionDot: View {
    @Environment(DaemonConnection.self) private var connection
    var body: some View {
        let color: Color = connection.state == .connected ? .green : connection.troubled ? .gray : .orange
        Circle().fill(color).frame(width: 8, height: 8)
            .overlay(Circle().stroke(color.opacity(0.35), lineWidth: 4))
            .animation(.easeOut(duration: 0.3), value: color)
            .accessibilityLabel(connection.state == .connected ? "Connected" : "Not connected")
    }
}

private struct SectionTitle: View {
    let title: String
    let count: Int
    var body: some View {
        HStack(spacing: 6) {
            Text(title).font(.footnote.weight(.semibold)).foregroundStyle(Theme.textSecondary)
            Text("\(count)").font(.footnote).monospacedDigit().foregroundStyle(.tertiary)
        }
    }
}

extension View {
    /// A row of a section's shared rounded block.
    func rowBlock(first: Bool, last: Bool) -> some View {
        listRowSeparator(.hidden)
            .listRowInsets(EdgeInsets(top: 0, leading: 16, bottom: 0, trailing: 16))
            .listRowBackground(
                UnevenRoundedRectangle(topLeadingRadius: first ? 14 : 0, bottomLeadingRadius: last ? 14 : 0,
                                       bottomTrailingRadius: last ? 14 : 0, topTrailingRadius: first ? 14 : 0,
                                       style: .continuous)
                    .fill(Theme.surface)
                    .padding(.horizontal, 16)
            )
    }
}

/// A line of a section: a session (a helper under the session it helps, when
/// that's listed here too), or the fold over a session's older helpers.
enum SessionLine: Identifiable, Equatable {
    case session(SessionSummary, parent: SessionSummary?)
    case more(parent: String, hidden: Int)
    /// Under a session's unfolded helpers: fold them again.
    case less(parent: String)

    var id: String {
        switch self {
        case .session(let s, _): s.id
        case .more(let p, _): "more-\(p)"
        case .less(let p): "less-\(p)"
        }
    }

    /// Shown helpers a session keeps unfolded.
    static let helpersShown = 2

    static func == (a: SessionLine, b: SessionLine) -> Bool { a.id == b.id }

    static func lines(_ items: [SessionSummary], open: Set<String>) -> [SessionLine] {
        let ids = Set(items.map(\.id))
        var drawn = Set<String>()
        var out: [SessionLine] = []
        func add(_ t: SessionSummary) {
            drawn.insert(t.id)
            let helpers = items.filter { $0.helperOf == t.id && !drawn.contains($0.id) }
                .sorted { $0.updated > $1.updated }
            let many = helpers.count > helpersShown + 1
            let fold = many && !open.contains(t.id)
            let shown = fold ? Array(helpers.prefix(helpersShown)) : helpers
            // Folded helpers count as drawn: they're behind "N more".
            drawn.formUnion(helpers.map(\.id))
            out += [.session(t, parent: nil)] + shown.map { .session($0, parent: t) }
                + (fold ? [.more(parent: t.id, hidden: helpers.count - shown.count)]
                        : many ? [.less(parent: t.id)] : [])
        }
        for t in items where t.helperOf.map({ !ids.contains($0) }) ?? true { add(t) }
        // A helper's helper (A asks B, B asks C) or two that asked each other: no
        // top to hang under, so each stands on its own rather than vanishing.
        for t in items where !drawn.contains(t.id) { add(t) }
        return out
    }
}

/// A row greys a little under your finger.
private struct RowPressStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(Color.primary.opacity(configuration.isPressed ? 0.06 : 0))
    }
}

/// A session as a card: who, where, what it's doing now, and whether it needs you.
struct SessionCard: View {
    let session: SessionSummary
    var helping = false
    var showsRepo = true

    private var tint: Color { Theme.color(for: session.agent) }

    var body: some View {
        // One line, as Happier has it: the agent's mark, what the session is
        // about, and on the right where and how it stands.
        HStack(spacing: 10) {
            if helping {
                Image(systemName: "arrow.turn.down.right")
                    .font(.caption2.weight(.semibold)).foregroundStyle(.tertiary)
            }
            Image(systemName: Theme.symbol(for: session.agent))
                .font(.system(size: 13, weight: .bold))
                .foregroundStyle(tint)
                .frame(width: 22)
                .accessibilityLabel(session.agent.displayName)
            Text(name)
                .font(.subheadline)
                .foregroundStyle(Theme.textPrimary)
                .lineLimit(1)
            Spacer(minLength: 6)
            if showsRepo, label != session.repoName {
                Text(session.repoName)
                    .font(.caption2).foregroundStyle(.tertiary)
                    .lineLimit(1).truncationMode(.middle)
                    .frame(maxWidth: 90, alignment: .trailing)
                    .fixedSize(horizontal: true, vertical: false)
            }
            trailing
        }
        .padding(.horizontal, 12)
        .frame(minHeight: 46)
        .contentShape(Rectangle())
        .animation(.easeOut(duration: 0.25), value: session.status)
        .accessibilityElement(children: .combine)
        .accessibilityValue(Theme.label(for: session.status))
        .accessibilityAddTraits(.isButton)
    }

    /// What it's about: its title (first message), else what it last did (a
    /// helper's title is only the ask's envelope), else its repo.
    private var label: String {
        session.name ?? Self.topic(session.title) ?? Self.topic(session.activity) ?? session.repoName
    }

    /// What it's about, as shown: plain, so bold in one title doesn't stand out.
    private var name: String { Self.plain(label) }

    /// Markdown as its words: "**NOT YET.** `x`" → "NOT YET. x".
    static func plain(_ s: String) -> String {
        let raw = balanced(s)
        guard let a = try? AttributedString(markdown: raw, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))
        else { return raw }
        return String(a.characters)
    }

    @ViewBuilder private var trailing: some View {
        switch session.status {
        case .working:
            ProgressView().controlSize(.mini).frame(width: 30)
        case .needsApproval:
            HStack(spacing: 4) {
                Image(systemName: "hand.raised.fill").font(.system(size: 10)).foregroundStyle(Theme.attention)
                time
            }
        case .done, .unknown:
            time
        default:
            HStack(spacing: 5) {
                Circle().fill(dot).frame(width: 6, height: 6)
                time
            }
        }
    }

    private var time: some View {
        Text(Self.ago(session.updated))
            .font(.caption2).monospacedDigit().foregroundStyle(.tertiary)
            .lineLimit(1).fixedSize()
            .frame(minWidth: 24, alignment: .trailing)
    }

    private var dot: Color {
        session.status == .working ? Theme.accent : Theme.color(for: session.status)
    }

    /// A title fit for one line. A helper's first message is the ask, wrapped:
    /// "Message from Claude (reCORD), sent through Styrman:\n\n<the question>…";
    /// its topic is the question.
    static func topic(_ title: String?) -> String? {
        guard var t = title?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty else { return nil }
        if t.hasPrefix("Message from "), let r = t.range(of: "sent through Styrman:") {
            t = String(t[r.upperBound...])
            if let tail = t.range(of: "(Reply as you normally would") { t = String(t[..<tail.lowerBound]) }
            t = t.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        t = t.replacingOccurrences(of: #"\s+"#, with: " ", options: .regularExpression)
        return t.isEmpty ? nil : t
    }

    /// A line cut out of a reply can hold half of a bold pair ("NOT YET.**"):
    /// then drop the markers rather than show them.
    static func balanced(_ s: String) -> String {
        s.components(separatedBy: "**").count % 2 == 0 ? s.replacingOccurrences(of: "**", with: "") : s
    }

    /// "now", "4m", "22h", "3d": short, as a chat list has it.
    static func ago(_ date: Date, now: Date = .now) -> String {
        let s = max(0, Int(now.timeIntervalSince(date)))
        switch s {
        case ..<60: return "now"
        case ..<3600: return "\(s / 60)m"
        case ..<86_400: return "\(s / 3600)h"
        case ..<(86_400 * 7): return "\(s / 86_400)d"
        default: return "\(s / (86_400 * 7))w"
        }
    }

}

/// The agent's avatar; while it works, a ring in its colour turns around it.
struct WorkingAvatar: View {
    let agent: AgentKind
    let working: Bool
    var size: CGFloat = 40
    var body: some View {
        let c = Theme.color(for: agent)
        AgentAvatar(agent: agent, size: size)
            .overlay {
                if working {
                    // Turned by the clock: a repeating animation would also replay
                    // whatever else changed as it started (the row's size).
                    TimelineView(.animation) { ctx in
                        Circle()
                            .trim(from: 0, to: 0.28)
                            .stroke(c, style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
                            .padding(-4)
                            .rotationEffect(.degrees(ctx.date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: 1.4) / 1.4 * 360))
                    }
                }
            }
            .padding(working ? 4 : 0)
            .frame(width: size + 8, height: size + 8)
    }
}

/// Shown only when the connection has been down for a while.
struct ConnectionPill: View {
    @Environment(DaemonConnection.self) private var connection
    var body: some View {
        HStack(spacing: 8) {
            if case .connecting = connection.state {
                ProgressView().controlSize(.small)
            } else {
                Image(systemName: "wifi.slash")
            }
            Text(text).lineLimit(1)
            Button("Retry now") { connection.retryNow() }.buttonStyle(.borderless)
        }
        .font(.footnote.weight(.medium))
        .padding(.horizontal, 14).padding(.vertical, 9)
        .background(.regularMaterial, in: Capsule())
        .shadow(color: .black.opacity(0.12), radius: 10, y: 3)
    }

    private var text: String {
        switch connection.state {
        case .offline(let why): "\(why). Retrying…"
        default: "Reconnecting to \(connection.host)…"
        }
    }
}

private struct UnpairedCard: View {
    @Environment(DaemonConnection.self) private var connection
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("This phone is no longer paired with \(connection.host).", systemImage: "lock.slash")
                .font(.subheadline.weight(.medium))
            Button("Pair again") { connection.forget() }.buttonStyle(.borderedProminent)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
    }
}

func shortPath(_ p: String) -> String {
    if let r = p.range(of: #"^/Users/[^/]+"#, options: .regularExpression) {
        return "~" + p[r.upperBound...]
    }
    return p
}

struct SettingsView: View {
    @Environment(DaemonConnection.self) private var connection
    @Environment(\.dismiss) private var dismiss
    @State private var confirmForget = false
    @State private var liveActivity = LiveActivities.shared.enabled
    @State private var notify = true
    @State private var notifyFinished = true

    var body: some View {
        NavigationStack {
            Form {
                Section("Mac") {
                    LabeledContent("Name", value: connection.host)
                    LabeledContent("Address", value: connection.credentials?.wsURL.host() ?? "—")
                    if connection.route == .localNetwork {
                        LabeledContent("Connected", value: "Over Wi-Fi (Tailscale unreachable)")
                    }
                    LabeledContent("Status", value: statusText)
                }
                VoiceSettings()
                Section {
                    Toggle("Notifications", isOn: $notify)
                        .onChange(of: notify) { _, on in connection.notify = on }
                    Toggle("When a turn finishes", isOn: $notifyFinished)
                        .onChange(of: notifyFinished) { _, on in connection.notifyFinished = on }
                        .disabled(!notify)
                } footer: {
                    Text(notify
                         ? (notifyFinished
                            ? "Approvals, errors, finished turns and newly shared sessions. Not for turns one agent asked of another."
                            : "Only when an agent needs your approval or stops with an error.")
                         : "No notifications from this Mac. Open the app to see what your agents are doing.")
                }
                Section {
                    Toggle("Live Activity", isOn: $liveActivity)
                        .onChange(of: liveActivity) { _, on in
                            LiveActivities.shared.enabled = on
                            if on { LiveActivities.shared.update(connection.sessions, host: connection.host) }
                        }
                } footer: {
                    Text("Shows which agents are working or need you on the Lock Screen and in the Dynamic Island. When the app is closed, updates usually show agent, folder and status. To include command and reply previews in direct pushes, run `styr push details on` on your Mac. Previews may be visible on your Lock Screen.")
                }
                Section {
                    Button("Forget this Mac", role: .destructive) { confirmForget = true }
                } footer: {
                    Text("To revoke this phone from the Mac side, run `styr devices revoke`.")
                }
            }
            .onAppear {
                notify = connection.notify
                notifyFinished = connection.notifyFinished
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .confirmationDialog("Forget \(connection.host)?", isPresented: $confirmForget, titleVisibility: .visible) {
                Button("Forget", role: .destructive) { connection.forget(); dismiss() }
            }
        }
    }

    private var statusText: String {
        switch connection.state {
        case .connected: "Connected"
        case .connecting: "Connecting"
        case .offline: "Offline"
        case .unauthorized: "Not authorized"
        case .unpaired: "Not paired"
        }
    }
}

/// Read-aloud settings: auto-read, and which voice each agent speaks with.
struct VoiceSettings: View {
    @State private var speaker = Speaker.shared
    @State private var choices: [AgentKind: String] = [:]
    @State private var voices = VoiceChoice.available()
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        Section {
            Toggle("Read replies aloud", isOn: $speaker.autoRead)
            ForEach(VoiceChoice.agents, id: \.self) { agent in
                HStack {
                    AgentBadge(agent: agent)
                    Picker("Voice", selection: binding(agent)) {
                        Text(automaticLabel(agent)).tag("")
                        ForEach(voices, id: \.identifier) { v in
                            Text(label(v)).tag(v.identifier)
                        }
                    }
                    .labelsHidden()
                    Spacer()
                    Button {
                        speaker.preview(agent)
                    } label: {
                        Image(systemName: speaker.speakingId == "preview-\(agent.rawValue)" ? "stop.circle" : "play.circle")
                            .font(.title3)
                    }
                    .buttonStyle(.borderless)
                    .tint(Theme.color(for: agent))
                    .accessibilityLabel("Preview \(agent.displayName)'s voice")
                }
            }
            if !VoiceChoice.hasPremiumVoice {
                Label {
                    Text("For the most natural sound, download a **Premium** voice or two: iOS Settings › Accessibility › Read & Speak › Voices › English. Styrman picks them up automatically.")
                } icon: {
                    Image(systemName: "sparkles")
                }
                .font(.footnote)
            }
        } header: {
            Text("Voice")
        } footer: {
            Text("Each agent reads its replies in its own voice. Dictation and speech happen on this iPhone.")
        }
        .onAppear { reload() }
        // Back from iOS Settings after downloading a voice.
        .onChange(of: scenePhase) { _, phase in if phase == .active { reload() } }
    }

    private func reload() {
        voices = VoiceChoice.available()
        for a in VoiceChoice.agents { choices[a] = VoiceChoice.chosen(a) ?? "" }
    }

    private func automaticLabel(_ agent: AgentKind) -> String {
        guard VoiceChoice.chosen(agent) == nil || choices[agent] == "",
              let v = VoiceChoice.voice(for: agent) else { return "Automatic" }
        return "Automatic (\(label(v)))"
    }

    private func binding(_ agent: AgentKind) -> Binding<String> {
        Binding(
            get: { choices[agent] ?? "" },
            set: { id in
                choices[agent] = id
                VoiceChoice.choose(id.isEmpty ? nil : id, for: agent)
                speaker.preview(agent)
            }
        )
    }

    private func label(_ v: AVSpeechSynthesisVoice) -> String {
        let q: String = switch v.quality {
        case .premium: " · Premium"
        case .enhanced: " · Enhanced"
        default: ""
        }
        return "\(v.name)\(q)"
    }
}

enum EndSessionText {
    /// What ending does, for the confirmation.
    static func message(_ s: SessionSummary?) -> String {
        let agent = s?.agent.displayName ?? "The agent"
        return "\(agent) quits on your Mac, as if you pressed Ctrl+C in its terminal. If it was started from this phone, its cmux tab closes too."
    }
}
/// "Rename…": an alert with the session's name to edit. Empty gives it its own
/// title back. The name shows on every phone and on its cmux tab.
struct RenameSession: ViewModifier {
    @Environment(DaemonConnection.self) private var connection
    @Binding var session: SessionSummary?
    @State private var text = ""

    func body(content: Content) -> some View {
        content.alert("Rename session", isPresented: Binding(get: { session != nil }, set: { if !$0 { session = nil } }),
                      presenting: session) { s in
            TextField(SessionCard.topic(s.title) ?? s.repoName, text: $text)
                .textInputAutocapitalization(.sentences)
            Button("Save") { connection.rename(s.id, to: text) }
            if s.name != nil {
                Button("Clear name", role: .destructive) { connection.rename(s.id, to: "") }
            }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("Shown here and on its tab on the Mac.")
        }
        .onChange(of: session?.id) { _, _ in text = session?.name ?? "" }
    }
}

extension View {
    func renameSession(_ session: Binding<SessionSummary?>) -> some View {
        modifier(RenameSession(session: session))
    }
}
