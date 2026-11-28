import AVFoundation
import SwiftUI

struct SessionListView: View {
    @Environment(DaemonConnection.self) private var connection
    @State private var showSettings = false
    @State private var showNewSession = false
    @State private var showUsage = false
    @State private var path: [String] = []
    @State private var showDone = false
    @State private var ending: SessionSummary?

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
            group("Needs you", needsYou)
            group("Working", working)
            if !done.isEmpty {
                Button {
                    withAnimation(.snappy) { showDone.toggle() }
                } label: {
                    HStack {
                        SectionTitle(title: "Done", count: done.count)
                        Spacer()
                        Image(systemName: "chevron.right")
                            .font(.caption.weight(.semibold)).foregroundStyle(.tertiary)
                            .rotationEffect(.degrees(showDone ? 90 : 0))
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .plainRow(top: 14, bottom: 2)
                if showDone {
                    ForEach(done) { s in
                        card(s, compact: true)
                    }
                }
            }
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
            SectionTitle(title: title, count: items.count).plainRow(top: 14, bottom: 2)
            ForEach(withHelpersUnder(items)) { s in card(s, compact: false) }
        }
    }

    /// A helper right under the session that started it, when both are listed here.
    private func withHelpersUnder(_ items: [SessionSummary]) -> [SessionSummary] {
        let ids = Set(items.map(\.id))
        let tops = items.filter { s in s.helperOf.map { !ids.contains($0) } ?? true }
        return tops.flatMap { t in [t] + items.filter { $0.helperOf == t.id } }
    }

    private func card(_ s: SessionSummary, compact: Bool) -> some View {
        let timeline = connection.timeline(for: s.id)
        let asker = s.helperOf.flatMap { connection.session($0) }
        return VStack(alignment: .leading, spacing: 6) {
            if let asker {
                Label("Helping \(asker.agent.displayName) · \(asker.repoName)", systemImage: "arrow.turn.down.right")
                    .font(.caption.weight(.medium)).foregroundStyle(.secondary)
                    .padding(.leading, 4)
            }
            Button { path.append(s.id) } label: {
                SessionCard(session: s, compact: compact)
            }
            .buttonStyle(CardPressStyle())
            if s.status == .needsApproval, timeline.isLive,
               let permission = timeline.openPermissions.last {
                SessionQuickApproval(session: s.id, cwd: s.cwd, permission: permission)
                    .id(permission.request)
            }
        }
        .padding(.leading, asker == nil ? 0 : 18)
        .plainRow(top: 5, bottom: 5)
        .swipeActions(edge: .trailing) {
            Button(s.status == .done ? "Remove" : "Stop sharing", role: .destructive) {
                connection.stopSharing(s.id)
            }
            if s.canEnd {
                Button("End") { ending = s }.tint(.orange)
            }
        }
        .contextMenu {
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
        guard let credentials = connection.credentials, connection.state == .connected else { return }
        answering = true
        Task {
            do {
                try await QuickAnswer.answer(credentials, session: session, request: permission.request, option: option.id)
            } catch {
                connection.lastError = error.localizedDescription
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
            Color(.systemGroupedBackground)
            RadialGradient(colors: [Theme.accent.opacity(0.10), .clear], center: .topLeading, startRadius: 0, endRadius: 420)
                .frame(height: 420)
            RadialGradient(colors: [Color.blue.opacity(0.06), .clear], center: .topTrailing, startRadius: 0, endRadius: 360)
                .frame(height: 360)
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
            Text(title.uppercased()).font(.caption.weight(.bold)).tracking(1.2).foregroundStyle(.secondary)
            Text("\(count)").font(.caption.weight(.bold)).monospacedDigit().foregroundStyle(.tertiary)
        }
    }
}

/// Cards sink a little under your finger.
private struct CardPressStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.98 : 1)
            .animation(.easeOut(duration: 0.15), value: configuration.isPressed)
    }
}

/// A session as a card: who, where, what it's doing now, and whether it needs you.
struct SessionCard: View {
    let session: SessionSummary
    var compact = false

    private var needs: Bool { session.status.needsUser }
    private var tint: Color { Theme.color(for: session.agent) }

    var body: some View {
        VStack(alignment: .leading, spacing: compact ? 4 : 10) {
            HStack(alignment: .center, spacing: 12) {
                WorkingAvatar(agent: session.agent, working: session.status == .working, size: compact ? 30 : 40)
                VStack(alignment: .leading, spacing: 1) {
                    Text(session.repoName)
                        .font(compact ? .subheadline.weight(.semibold) : .headline)
                        .lineLimit(1).truncationMode(.middle)
                    HStack(spacing: 4) {
                        Text(session.agent.displayName).foregroundStyle(tint).fontWeight(.semibold)
                        if let cwd = session.cwd, !cwd.isEmpty {
                            Text("· \(shortPath(cwd))").foregroundStyle(.tertiary).lineLimit(1).truncationMode(.middle)
                        }
                    }
                    .font(.caption)
                }
                Spacer(minLength: 4)
                VStack(alignment: .trailing, spacing: 4) {
                    if !compact { StatusChip(status: session.status).fixedSize() }
                    Text(session.updated, format: .relative(presentation: .named))
                        .font(.caption2).foregroundStyle(.tertiary).lineLimit(1).fixedSize()
                }
            }
            if !compact, let line = activity {
                if session.status == .needsApproval {
                    // What it wants to run, as code.
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Image(systemName: "hand.raised.fill").font(.caption).foregroundStyle(Theme.attention)
                        Text(line)
                            .font(.system(.footnote, design: .monospaced))
                            .lineLimit(2)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(.horizontal, 10).padding(.vertical, 8)
                    .background(Theme.attention.opacity(0.10), in: RoundedRectangle(cornerRadius: 10))
                } else {
                    Text(line)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .contentTransition(.opacity)
                }
            }
        }
        .padding(compact ? 12 : 14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            RoundedRectangle(cornerRadius: 20, style: .continuous)
                .fill(Color(.secondarySystemGroupedBackground))
                .shadow(color: needs ? Theme.attention.opacity(0.22) : .black.opacity(0.05), radius: needs ? 14 : 6, y: needs ? 4 : 2)
        }
        .overlay {
            RoundedRectangle(cornerRadius: 20, style: .continuous)
                .strokeBorder(needs ? Theme.attention.opacity(0.55) : Color.primary.opacity(0.05), lineWidth: needs ? 1.5 : 1)
        }
        .opacity(compact ? 0.85 : 1)
        .animation(.easeOut(duration: 0.25), value: session.activity)
        .animation(.easeOut(duration: 0.25), value: session.status)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
    }

    /// What it's doing right now (paths relative to its folder), else its first message.
    private var activity: AttributedString? {
        var raw: String
        if var a = session.activity, !a.isEmpty, session.status != .done {
            if let cwd = session.cwd, !cwd.isEmpty {
                a = a.replacingOccurrences(of: cwd.hasSuffix("/") ? cwd : cwd + "/", with: "")
            }
            raw = a.replacingOccurrences(of: #"/Users/[^/\s`]+"#, with: "~", options: .regularExpression)
            if session.status == .needsApproval {
                // "Run `cargo test`" → "cargo test": the chip already says it's a request.
                if let open = raw.firstIndex(of: "`"), let close = raw[raw.index(after: open)...].firstIndex(of: "`") {
                    raw = String(raw[raw.index(after: open)..<close])
                }
                return AttributedString(raw)
            }
        } else if let t = session.title, !t.isEmpty {
            raw = t
        } else {
            return nil
        }
        return (try? AttributedString(markdown: raw, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(raw)
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
        .background(Color(.secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 20, style: .continuous))
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
