import PhotosUI
import SwiftUI

struct SessionDetailView: View {
    @Environment(DaemonConnection.self) private var connection
    let sessionId: String
    @State private var speaker = Speaker.shared
    /// Set once the history has loaded: the newest reply at that point. Only replies
    /// after it are read aloud automatically.
    @State private var baseline: String??
    @State private var showReadOnlyHelp = false
    @State private var confirmEnd = false
    @State private var showTerminal = false
    @State private var showChanges = false
    @State private var showModel = false
    @State private var renaming: SessionSummary?
    /// Scrolled to (near) the end: new text is followed. Scrolled up to read, the
    /// conversation stays where you are while the reply is written.
    @State private var atBottom = true
    /// Whether you're scrolling (dragging, or it's still gliding).
    @State private var scrollPhase: ScrollPhase = .idle
    /// How many of the latest rows to draw (more with "Show earlier").
    @State private var shown = 150
    @Environment(\.dismiss) private var dismiss

    /// The approval waiting for an answer, if any: it's pinned above the message box.
    private func openPermission(_ timeline: SessionTimeline) -> PermissionItem? {
        for item in timeline.items.reversed() {
            if case .permission(_, let p) = item, p.isOpen { return p }
        }
        return nil
    }

    /// Back-to-back dividers ("Session started", "Shared with this phone") read as one line.
    private func merged(_ items: [TimelineItem]) -> [TimelineItem] {
        var out: [TimelineItem] = []
        for item in items {
            if case .divider(_, let text, _) = item, case .divider(let id, let prev, let at)? = out.last {
                let both = prev == "Session started" && text.hasPrefix("Shared")
                    ? "Started and shared" : "\(prev) · \(text.prefix(1).lowercased() + text.dropFirst())"
                out[out.count - 1] = .divider(id: id, text: both, at: at)
            } else {
                out.append(item)
            }
        }
        return out
    }

    private func latestReply(_ timeline: SessionTimeline) -> (id: String, text: String)? {
        for item in timeline.items.reversed() {
            if case .assistant(let id, let text, _) = item { return (id, text) }
        }
        return nil
    }

    var body: some View {
        let timeline = connection.timeline(for: sessionId)
        let summary = connection.session(sessionId)
        let agent = summary?.agent ?? .unknown
        conversation(timeline, summary: summary, agent: agent)
        .navigationTitle(summary.map { $0.name ?? $0.repoName } ?? "Session")
        .renameSession($renaming)
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(Theme.canvas, for: .navigationBar)
        .sheet(isPresented: $showTerminal) { TerminalSheet(sessionId: sessionId, agent: agent) }
        .sheet(isPresented: $showModel) { ModelSheet(sessionId: sessionId, agent: agent) }
        .sheet(isPresented: $showChanges) { ChangesSheet(sessionId: sessionId, agent: agent, cwd: summary?.cwd) }
        #if DEBUG
        // Screenshots (simulator): STYR_SHOW=terminal opens the terminal of the opened session.
        .task {
            switch ProcessInfo.processInfo.environment["STYR_SHOW"] {
            case "terminal": try? await Task.sleep(for: .seconds(6)); showTerminal = true
            case "changes": try? await Task.sleep(for: .seconds(6)); showChanges = true
            default: break
            }
        }
        #endif
        .confirmationDialog("End this session?", isPresented: $confirmEnd, titleVisibility: .visible) {
            Button("End session", role: .destructive) {
                connection.endSession(sessionId)
                dismiss()
            }
        } message: {
            Text(EndSessionText.message(summary))
        }
        .toolbarBackground(.visible, for: .navigationBar)
        .toolbar { if let summary { toolbarItems(summary) } }
        .overlay(alignment: .top) {
            if let error = connection.lastError {
                Text(error)
                    .font(.footnote).padding(.horizontal, 12).padding(.vertical, 8)
                    .background(.thinMaterial, in: Capsule())
                    .padding(.top, 8)
                    .task {
                        try? await Task.sleep(for: .seconds(3))
                        connection.lastError = nil
                    }
            }
        }
        .safeAreaInset(edge: .bottom) {
            bottomBar(timeline, summary: summary)
            .animation(.snappy, value: openPermission(timeline)?.request)
        }

        .onAppear { connection.watch(sessionId) }
        .onChange(of: timeline.isLive, initial: true) { _, live in
            if live, baseline == nil { baseline = .some(latestReply(timeline)?.id) }
        }
        .onDisappear {
            connection.unwatch(sessionId)
            speaker.stop()
        }
        // Read new replies aloud as they arrive (when switched on).
        .onChange(of: latestReply(timeline)?.id) { _, id in
            guard let seen = baseline, let id, id != seen, let r = latestReply(timeline) else { return }
            baseline = .some(id)
            if speaker.autoRead { speaker.speak(r.text, id: r.id, agent: agent) }
        }
    }

    private func conversation(_ timeline: SessionTimeline, summary: SessionSummary?, agent: AgentKind) -> some View {
        ScrollViewReader { proxy in
            ScrollView {
                if !timeline.loaded {
                    // First load: a calm placeholder, not a black screen and a jumping list.
                    VStack(spacing: 10) {
                        ProgressView()
                        Text(connection.state == .connected ? "Loading conversation…" : "Waiting for your Mac…")
                            .font(.footnote).foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity).padding(.top, 160)
                } else {
                    messages(timeline, summary: summary, agent: agent)
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .transition(.opacity)
                }
            }
            // Open at the end. As content grows: at the end, stay at the end (smoothly,
            // no jump per update); scrolled up, don't move what you're reading.
            .defaultScrollAnchor(.bottom, for: .initialOffset)
            .defaultScrollAnchor(atBottom ? .bottom : .top, for: .sizeChanges)
            .onScrollPhaseChange { _, phase in scrollPhase = phase }
            // At the end or not, from where it's scrolled. Not while content grows:
            // a reply or tool card arriving makes the view look scrolled up for a
            // moment, and must not stop it following.
            .onScrollGeometryChange(for: ScrollSpot.self) { g in
                // The visible part ends above the input bar (the bottom inset); offsets
                // alone leave out the space under the navigation bar.
                ScrollSpot(near: g.visibleRect.maxY - g.contentInsets.bottom >= g.contentSize.height - 120,
                           height: g.contentSize.height)
            } action: { old, new in
                // Your scrolling counts; the jump's own animation (passing through
                // "not at the end") doesn't.
                let yours = scrollPhase == .interacting || scrollPhase == .decelerating
                if new.near || yours || (scrollPhase == .idle && new.height == old.height) { atBottom = new.near }
            }
            // Following: keep the end in view whenever the content grows.
            .onScrollGeometryChange(for: CGFloat.self) { $0.contentSize.height } action: { old, new in
                if atBottom, scrollPhase == .idle, new > old {
                    proxy.scrollTo("bottom", anchor: .bottom)
                }
            }
            .overlay(alignment: .bottomTrailing) {
                if !atBottom && timeline.loaded {
                    Button {
                        // Follow again from now on. While a reply grows, the animated
                        // jump can land short of the end: land there once it's done.
                        atBottom = true
                        withAnimation(.easeOut(duration: 0.25)) { proxy.scrollTo("bottom", anchor: .bottom) }
                        Task { @MainActor in
                            try? await Task.sleep(for: .milliseconds(350))
                            if atBottom { proxy.scrollTo("bottom", anchor: .bottom) }
                        }
                    } label: {
                        Image(systemName: "arrow.down")
                            .font(.body.weight(.semibold))
                            .padding(11)
                            .background(.thinMaterial, in: Circle())
                    }
                    .tint(Theme.color(for: agent))
                    .padding(14)
                    .transition(.opacity.combined(with: .scale(scale: 0.8)))
                    .accessibilityLabel("Jump to the latest")
                }
            }
            .animation(.easeOut(duration: 0.2), value: atBottom)
            .animation(.easeOut(duration: 0.2), value: timeline.loaded)
            .background(SessionBackground(agent: agent))
            // Like the Claude app: drag the conversation down to pull the keyboard away,
            // or tap the conversation to dismiss it.
            .scrollDismissesKeyboard(.interactively)
            .simultaneousGesture(TapGesture().onEnded {
                UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
            })
            // A message you send always brings you to the end.
            .onChange(of: timeline.pending.count) { old, new in
                guard new > old else { return }
                atBottom = true
                withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            // Land on the latest message when it opens and when the history arrives
            // (again once it's laid out: right away, the end may not be there yet).
            .onChange(of: timeline.loaded, initial: true) { _, loaded in
                guard loaded else { return }
                Task { @MainActor in
                    proxy.scrollTo("bottom", anchor: .bottom)
                    try? await Task.sleep(for: .milliseconds(150))
                    proxy.scrollTo("bottom", anchor: .bottom)
                    atBottom = true
                }
            }
        }
    }

    private func messages(_ timeline: SessionTimeline, summary: SessionSummary?, agent: AgentKind) -> some View {
            // A plain (not lazy) stack: a lazy one anchored to the bottom sometimes
            // left its scroll position past the content, a blank screen until you
            // scrolled. Only the latest rows are drawn, so long sessions stay quick.
            VStack(alignment: .leading, spacing: 12) {
                // The actions under the latest reply wait until the turn is over: moving them
                // from reply to reply while it works would shift everything each time.
                let busy = summary?.status == .working || summary?.status == .needsApproval
                let latest = busy ? nil : latestReply(timeline)?.id
                // An answered ask shows with its reply, where the reply came.
                let exchanges = PeerNote.exchanges(timeline.shownItems)
                let answered = Set(exchanges.map(\.askId))
                let rows = DisplayRow.rows(merged(timeline.shownItems.filter { !answered.contains($0.id) }))
                let openAsks = PeerNote.openAsks(timeline.shownItems)
                if rows.count > shown {
                    Button("Show earlier") { shown += 150 }
                        .font(.footnote.weight(.medium))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 4)
                }
                ForEach(rows.suffix(shown)) { row in
                    switch row {
                    case .item(let item):
                        TimelineRow(item: item, cwd: summary?.cwd, sessionId: sessionId, agent: agent,
                                    latestReplyId: latest, openAsks: openAsks, exchanges: exchanges).id(item.id)
                    case .tools(let id, let tools):
                        StepsRow(tools: tools, cwd: summary?.cwd).id(id)
                    }
                }
                ForEach(timeline.pending) { p in
                    PendingPromptRow(prompt: p) { timeline.dismissUnconfirmed(p.id) }
                }
                if summary?.status == .working, timeline.pending.isEmpty || timeline.preview != nil {
                    if let text = timeline.preview {
                        LivePreviewRow(text: text, agent: agent, sessionId: sessionId).id("preview")
                    }
                    // Under it, as in the terminal: what the agent is doing, and for how long.
                    if timeline.status != nil || timeline.preview == nil {
                        WorkingRow(status: timeline.status, since: timeline.statusAt,
                                   thinkingSince: timeline.thinkingSince, agent: agent).id("status")
                    }
                }
                if !timeline.runningTasks.isEmpty {
                    RunningTasksLink(timeline: timeline, cwd: summary?.cwd).id("tasks")
                }
                if timeline.isLive && !timeline.hasContent && timeline.pending.isEmpty {
                    VStack(spacing: 6) {
                        Image(systemName: "ellipsis.bubble").font(.title2).foregroundStyle(.tertiary)
                        Text("Nothing yet").font(.headline)
                        Text("Prompts, tool calls and replies show up here as they happen.")
                            .font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity).padding(.top, 40)
                }
                Color.clear.frame(height: 1).id("bottom")
            }
    }

    @ToolbarContentBuilder
    private func toolbarItems(_ summary: SessionSummary) -> some ToolbarContent {
            ToolbarItem(placement: .principal) {
                HStack(spacing: 8) {
                    VStack(alignment: .center, spacing: 0) {
                        Text(summary.name ?? summary.repoName).font(.headline).lineLimit(1)
                        HStack(spacing: 4) {
                            Image(systemName: "circle.fill").font(.system(size: 6))
                                .foregroundStyle(summary.status == .working ? Theme.accent
                                      : summary.status.needsUser ? Theme.attention
                                      : summary.status == .done ? Color.secondary : Theme.live)
                                .symbolEffect(.pulse, options: .repeating, isActive: summary.status == .working)
                            if summary.name != nil {
                                Text(summary.repoName).foregroundStyle(.secondary).lineLimit(1)
                                Text("·").foregroundStyle(.tertiary)
                            }
                            Text(summary.agent.displayName).foregroundStyle(Theme.textSecondary)
                            Text("·").foregroundStyle(.tertiary)
                            Text(Theme.label(for: summary.status))
                                .foregroundStyle(summary.status.needsUser ? AnyShapeStyle(Theme.attention) : AnyShapeStyle(.secondary))
                                .contentTransition(.opacity)
                            if let mode = summary.mode.flatMap(PermissionMode.init), mode != .manual {
                                Text("·").foregroundStyle(.tertiary)
                                Text(mode.short)
                                    .fixedSize()
                                    .foregroundStyle(mode == .bypassPermissions ? AnyShapeStyle(.red) : AnyShapeStyle(.secondary))
                                    .accessibilityLabel("\(mode.label) mode")
                            }
                        }
                        .font(.caption2.weight(.medium))
                        .animation(.easeOut(duration: 0.2), value: summary.status)
                    }
                }
                .accessibilityElement(children: .combine)
            }
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    if summary.status != .done, let mode = summary.mode.flatMap(PermissionMode.init) {
                        Picker(selection: Binding(get: { mode }, set: { connection.setMode(sessionId, to: $0) })) {
                            ForEach(PermissionMode.choices(current: mode)) { m in
                                Label {
                                    Text(m.label)
                                    Text(m.detail)
                                } icon: {
                                    Image(systemName: m.symbol)
                                }
                                .tag(m)
                            }
                        } label: {
                            Label("Mode: \(mode.label)", systemImage: mode.symbol)
                        }
                        .pickerStyle(.menu)
                        Divider()
                    }
                    if summary.hasLiveControls, summary.status != .done {
                        Button { showModel = true } label: {
                            Label("Model: \(ModelText.current(summary, in: connection.models[sessionId] ?? []))", systemImage: "cpu")
                        }
                    }
                    Button { renaming = summary } label: { Label("Rename…", systemImage: "pencil") }
                    Toggle(isOn: Binding(get: { speaker.autoRead }, set: { on in
                        speaker.autoRead = on
                        if !on { speaker.stop() }
                    })) {
                        Label("Read replies aloud", systemImage: "speaker.wave.2")
                    }
                    if summary.cwd != nil {
                        Button { showChanges = true } label: { Label("Changes", systemImage: "plusminus") }
                    }
                    if summary.hasLiveControls {
                        Button { showTerminal = true } label: { Label("Terminal", systemImage: "terminal") }
                    }
                    if summary.outdated && summary.inTerminalWrapper {
                        // Explains why Stop, Terminal and Model are missing here.
                        Button {} label: {
                            Label {
                                Text("Started before an update")
                                Text("End it and start it again for Stop, Terminal and Model.")
                            } icon: {
                                Image(systemName: "arrow.clockwise.circle")
                            }
                        }
                        .disabled(true)
                    }
                    Divider()
                    if summary.canEnd {
                        Button(role: .destructive) { confirmEnd = true } label: {
                            Label("End session", systemImage: "stop.circle")
                        }
                    }
                    Button(role: .destructive) {
                        connection.stopSharing(sessionId)
                        dismiss()
                    } label: {
                        Label(summary.status == .done ? "Remove" : "Stop sharing", systemImage: "eye.slash")
                    }
                } label: {
                    Image(systemName: speaker.autoRead ? "speaker.wave.2.circle" : "ellipsis.circle")
                }
                .accessibilityLabel("More")
            }
    }

    private func bottomBar(_ timeline: SessionTimeline, summary: SessionSummary?) -> some View {
        VStack(spacing: 0) {
            if connection.troubled && connection.state != .unauthorized {
                ConnectionPill()
                    .padding(8)
                    .frame(maxWidth: .infinity)
                    .background(Theme.canvas)
            }
            if summary == nil, connection.state == .connected {
                Text("This session is no longer shared.")
                    .font(.footnote).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity).padding(10).background(Theme.canvas)
            } else if let summary {
                HelperLinks(summary: summary)
                if let p = openPermission(timeline) {
                    PermissionCard(item: p, title: displayTitle(p.tool, cwd: summary.cwd), sessionId: sessionId, cwd: summary.cwd, pinned: true)
                        // A new request is a new card: "sending" mustn't carry over.
                        .id(p.request)
                        .padding(.horizontal, 12).padding(.top, 10).padding(.bottom, 4)
                        .frame(maxWidth: .infinity)
                        .background(Theme.canvas)
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                }
                if summary.acceptsPrompts {
                    Composer(sessionId: sessionId, status: summary.status, agent: summary.agent.displayName,
                             canStop: summary.hasLiveControls)
                        .onChange(of: speaker.autoRead) { _, on in
                            // Turning it on reads the latest reply, so you hear where things are.
                            if on, let r = latestReply(timeline) { speaker.speak(r.text, id: r.id, agent: summary.agent) }
                        }
                } else {
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Label("Read-only", systemImage: "eye")
                            Spacer()
                            Button(showReadOnlyHelp ? "Hide" : "How to reply from here") {
                                withAnimation(.snappy) { showReadOnlyHelp.toggle() }
                            }
                            .buttonStyle(.borderless)
                        }
                        if showReadOnlyHelp {
                            Text("This \(summary.agent.displayName) session wasn't started through Styrman. Run `styr setup` on the Mac, then start it again in a new terminal tab.")
                                .foregroundStyle(.secondary)
                                .transition(.opacity)
                        }
                    }
                    .font(.footnote).foregroundStyle(.secondary)
                    .padding(.horizontal, 14).padding(.vertical, 10)
                    .frame(maxWidth: .infinity, alignment: .leading).background(Theme.canvas)
                }
            }
        }
    }
}

/// `cd <session folder> && cargo test` → `cargo test`: agents often start each command
/// by going to the folder they're already in, which hides the part that matters. A
/// `cd` anywhere else stays: it matters (above all when approving).
func withoutCd(_ cmd: String, cwd: String?) -> String {
    guard let cwd, !cwd.isEmpty else { return cmd }
    let t = cmd.trimmingCharacters(in: .whitespaces)
    let home = cwd.hasSuffix("/") ? String(cwd.dropLast()) : cwd
    for form in [home, "\"\(home)\"", "'\(home)'", home + "/"] {
        for sep in [" && ", "; "] {
            let prefix = "cd \(form)\(sep)"
            if t.hasPrefix(prefix) {
                let rest = String(t.dropFirst(prefix.count)).trimmingCharacters(in: .whitespaces)
                return rest.isEmpty ? cmd : rest
            }
        }
    }
    return cmd
}

/// How a tool call reads in the timeline: the bare command for shell tools, paths
/// relative to the session's folder for everything else.
func displayTitle(_ tool: ToolInfo, cwd: String?) -> String {
    if tool.kind == .execute, let cmd = tool.input["command"]?.string {
        return withoutCd(cmd, cwd: cwd)
    }
    guard let cwd, !cwd.isEmpty else { return tool.title }
    let prefix = cwd.hasSuffix("/") ? cwd : cwd + "/"
    return tool.title.replacingOccurrences(of: prefix, with: "")
}

struct TimelineRow: View {
    let item: TimelineItem
    var cwd: String? = nil
    var sessionId: String = ""
    var agent: AgentKind = .unknown
    var latestReplyId: String? = nil
    /// Asks to other agents still waiting for their reply (notice ids).
    var openAsks: Set<String> = []
    /// Asks and their replies, paired.
    var exchanges: [PeerExchange] = []

    var body: some View {
        switch item {
        case .divider(_, let text, let at):
            HStack(spacing: 8) {
                line
                Text("\(text) · \(at.formatted(date: .omitted, time: .shortened))")
                    .font(.caption2).foregroundStyle(.tertiary).lineLimit(1)
                line
            }
            .padding(.vertical, 2)
        case .user(_, let text, _, _) where AgentMessage(text) != nil:
            AgentMessageRow(message: AgentMessage(text)!)
        case .user(_, let raw, let fromPhone, _):
            let parts = AttachedFiles.split(raw)
            let text = parts?.text ?? raw
            HStack {
                Spacer(minLength: 56)
                VStack(alignment: .trailing, spacing: 4) {
                    if let files = parts?.files, !files.isEmpty { AttachedFilesRow(files: files) }
                    if !text.isEmpty {
                    Text(text).textSelection(.enabled)
                        .foregroundStyle(Theme.textPrimary)
                        .padding(.horizontal, 16).padding(.vertical, 11)
                        .background(Theme.bubble, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
                    }
                    if fromPhone {
                        Image(systemName: "iphone").font(.caption2).foregroundStyle(.tertiary)
                            .accessibilityLabel("From this phone")
                    }
                }
            }
        case .assistant(let id, let text, _):
            ReplyView(id: id, text: markdown(text), raw: text, agent: agent, isLatest: id == latestReplyId)
        case .tool(_, let tool):
            // Runs of tool calls sit close together (the list's spacing is 12).
            ToolCallRow(item: tool, title: displayTitle(tool.tool, cwd: cwd))
                .padding(.vertical, -4)
        case .permission(_, let p) where p.isOpen:
            // The card itself is pinned above the message box.
            Label("Waiting for your answer below", systemImage: "hand.raised.fill")
                .font(.footnote.weight(.medium)).foregroundStyle(Theme.attention)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 12).padding(.vertical, 8)
                .background(Theme.attention.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
        case .permission(_, let p):
            PermissionCard(item: p, title: displayTitle(p.tool, cwd: cwd), sessionId: sessionId, cwd: cwd)
        case .plan(_, let entries):
            PlanCard(entries: entries)
        case .notice(let id, _, _) where exchanges.contains { $0.id == id }:
            ExchangeCard(exchange: exchanges.first { $0.id == id }!, asker: agent, sessionId: sessionId,
                         thread: exchanges)
        case .notice(let id, let text, _) where PeerNote(text) != nil:
            PeerCard(note: PeerNote(text)!, asker: agent, sessionId: sessionId, waiting: openAsks.contains(id))
        case .notice(_, let text, let warning):
            // Another agent's reply quoted here is Markdown: bold as bold, not `**`.
            Label { Text(ReplyText.inline(text)) } icon: { Image(systemName: warning ? "exclamationmark.triangle" : "bell") }
                .font(.footnote).foregroundStyle(warning ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
        }
    }

    private var line: some View { Rectangle().fill(.quaternary).frame(height: 1) }

    private func markdown(_ s: String) -> AttributedString {
        let s = ReplyMarkdown.readable(s)
        return (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(s)
    }
}

/// Play / stop reading one reply.
/// An agent's reply with Claude-app-style actions under it: copy, and play/stop.
/// While it's being read, a bar in the agent's colour marks it.
struct ReplyView: View {
    let id: String
    let text: AttributedString
    let raw: String
    let agent: AgentKind
    var isLatest = true
    @State private var speaker = Speaker.shared
    @State private var copied = false

    var body: some View {
        let playing = speaker.speakingId == id
        let tint = Theme.color(for: agent)
        VStack(alignment: .leading, spacing: 6) {
            ReplyText(raw: raw, selectable: true)
            // Under every reply, as in the Claude app (a long-press selects text).
            HStack(spacing: 2) {
                ActionIcon(
                    symbol: copied ? "checkmark" : "doc.on.doc",
                    label: copied ? "Copied" : "Copy",
                    tint: copied ? tint : .secondary
                ) {
                    UIPasteboard.general.string = raw
                    copied = true
                    Task { try? await Task.sleep(for: .seconds(1.5)); copied = false }
                }
                ActionIcon(
                    symbol: playing ? "stop.fill" : "speaker.wave.2",
                    label: playing ? "Stop reading" : "Read aloud",
                    tint: playing ? tint : .secondary,
                    animating: playing
                ) {
                    speaker.toggle(raw, id: id, agent: agent)
                }
            }
            .padding(.leading, -8)
        }
        .padding(.leading, playing ? 12 : 0)
        // An edge in the agent's colour while it's read aloud.
        .overlay(alignment: .leading) {
            if playing { Capsule().fill(tint).frame(width: 4).padding(.vertical, 2) }
        }
        .animation(.easeOut(duration: 0.2), value: playing)
        // A tap when it starts; none when it ends (a lone haptic tick sounds like a click).
        .sensoryFeedback(.selection, trigger: playing) { _, now in now }
    }
}

/// A read-only UITextView: drag the handles to select, then Copy / Look Up / Share.
struct SelectableText: UIViewRepresentable {
    let text: AttributedString

    func makeUIView(context: Context) -> UITextView {
        let v = UITextView()
        v.isEditable = false
        v.isSelectable = true
        v.backgroundColor = .clear
        v.adjustsFontForContentSizeCategory = true
        v.textContainerInset = UIEdgeInsets(top: 16, left: 12, bottom: 32, right: 12)
        v.dataDetectorTypes = [.link]
        v.attributedText = Self.render(text)
        return v
    }

    func updateUIView(_ v: UITextView, context: Context) {}

    /// Markdown's inline styles (bold, italic, code, links) as UIKit attributes.
    static func render(_ text: AttributedString) -> NSAttributedString {
        let sans = UIFont.preferredFont(forTextStyle: .body)
        // Replies read as prose: a serif at the body size (it scales with Dynamic Type).
        let body = sans.fontDescriptor.withDesign(.serif).map { UIFont(descriptor: $0, size: 0) } ?? sans
        let para = NSMutableParagraphStyle()
        para.lineSpacing = 3
        let out = NSMutableAttributedString()
        for run in text.runs {
            let piece = String(text[run.range].characters)
            var font = body
            let intent = run.inlinePresentationIntent ?? []
            if intent.contains(.code) {
                font = UIFont.monospacedSystemFont(ofSize: body.pointSize * 0.9, weight: .regular)
            } else {
                var traits: UIFontDescriptor.SymbolicTraits = []
                if intent.contains(.stronglyEmphasized) { traits.insert(.traitBold) }
                if intent.contains(.emphasized) { traits.insert(.traitItalic) }
                if !traits.isEmpty, let d = body.fontDescriptor.withSymbolicTraits(traits) {
                    font = UIFont(descriptor: d, size: 0)
                }
            }
            var attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: UIColor(Theme.textPrimary),
                                                         .paragraphStyle: para]
            if intent.contains(.code) { attrs[.backgroundColor] = UIColor(Theme.codeFill) }
            if let link = run.link { attrs[.link] = link }
            out.append(NSAttributedString(string: piece, attributes: attrs))
        }
        return out
    }
}

/// A small icon button with a comfortable tap target.
struct ActionIcon: View {
    let symbol: String
    let label: String
    let tint: Color
    var animating = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(tint)
                .symbolEffect(.variableColor.iterative, isActive: animating)
                .contentTransition(.symbolEffect(.replace))
                .frame(width: 36, height: 30)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }
}

struct ToolCallRow: View {
    let item: ToolItem
    let title: String
    /// Inside a run's card: no background of its own.
    var bare = false
    @State private var showDetail = false

    var body: some View {
        Button {
            showDetail = true
        } label: {
            // Compact: one line, small type. Tap for the full command and output.
            HStack(alignment: .center, spacing: 6) {
                Image(systemName: Theme.symbol(for: item.tool.kind))
                    .font(.caption)
                    .foregroundStyle(.secondary).frame(width: 14)
                // Claude says what a command is for ("Run the parser tests"): plainer
                // than the command (still in the details).
                let described = item.tool.kind == .execute ? item.tool.input["description"]?.string : nil
                Text(described ?? title)
                    .font(item.tool.kind == .execute && described == nil
                          ? .system(.caption, design: .monospaced) : .footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 4)
                if item.tool.kind == .edit, let diffs = FileDiff.from(item.tool) { DiffCounts(diffs: diffs) }
                statusIcon
            }
            .padding(.horizontal, 10).padding(.vertical, bare ? 7 : 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(bare ? AnyShapeStyle(Color.clear) : AnyShapeStyle(Theme.surface),
                        in: RoundedRectangle(cornerRadius: 8))
            .contentShape(RoundedRectangle(cornerRadius: 8))
        }
        .buttonStyle(.plain)
        .accessibilityHint("Shows the full command and its output")
        // Output opens in a sheet: expanding rows inline inside a bottom-anchored
        // LazyVStack made SwiftUI lose its scroll position (blank screen).
        .sheet(isPresented: $showDetail) { ToolDetailView(item: item, title: title) }
    }

    @ViewBuilder private var statusIcon: some View {
        switch item.status {
        case .pending, .running: ProgressView().controlSize(.mini)
        case .completed: Image(systemName: "checkmark").foregroundStyle(.green).font(.caption2.bold())
        case .failed: Image(systemName: "xmark").foregroundStyle(.red).font(.caption2.bold())
        }
    }
}

struct PermissionCard: View {
    @Environment(DaemonConnection.self) private var connection
    let item: PermissionItem
    let title: String
    let sessionId: String
    var cwd: String? = nil
    /// Pinned above the message box: tighter, so the conversation stays visible.
    var pinned = false
    @State private var sent: String?
    @State private var showAll = false
    @State private var fullHeight: CGFloat = 0
    @State private var shownHeight: CGFloat = 0

    private func titleText(limit: Int?) -> some View {
        Text(title)
            .font(item.tool.kind == .execute ? .system(.footnote, design: .monospaced) : .callout.weight(.medium))
            .lineLimit(limit)
    }

    private var canAnswer: Bool {
        connection.state == .connected && connection.timeline(for: sessionId).isLive
    }

    /// "Would you like to run the following command?" says nothing the card doesn't.
    private var reason: String? {
        guard let r = item.reason, !r.hasPrefix("Would you like to") else { return nil }
        return r
    }

    var body: some View {
        VStack(alignment: .leading, spacing: pinned ? 10 : 12) {
            HStack {
                HStack(spacing: 6) {
                    Image(systemName: "hand.raised.fill").font(.caption).foregroundStyle(Theme.attention)
                    Text(item.isOpen ? "Allow this?" : "Permission")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(item.isOpen ? Theme.textPrimary : Theme.textSecondary)
                }
                Spacer()
                if pinned {
                    Text("or answer in the terminal").font(.caption2).foregroundStyle(.tertiary)
                }
            }

            // What it's about, compact: commands in a small monospaced box, edits as a diff.
            if let diffs = connection.timeline(for: sessionId).diffs(for: item) {
                DiffPreview(diffs: diffs, cwd: cwd, maxLines: pinned ? 8 : 24)
            } else {
            let limit = item.tool.kind == .execute ? (pinned ? 3 : 6) : 4
            Group {
                if item.tool.kind == .execute {
                    titleText(limit: showAll ? nil : limit)
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Theme.bubble, in: RoundedRectangle(cornerRadius: 10))
                } else {
                    titleText(limit: showAll ? nil : limit)
                }
            }
            .textSelection(.enabled)
            // Measured, not guessed from its length: at a large text size a short
            // command can wrap past the limit, and you must see what you approve.
            .background(alignment: .topLeading) {
                titleText(limit: nil)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(item.tool.kind == .execute ? 10 : 0)
                    .hidden()
                    .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { fullHeight = $0 }
            }
            .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { shownHeight = $0 }
            if !showAll, fullHeight > shownHeight + 1 {
                Button("Show all") { showAll = true }.font(.caption).buttonStyle(.borderless)
            }
            }
            if let reason { Text(reason).font(.footnote).foregroundStyle(.secondary) }

            if item.isOpen {
                if !canAnswer {
                    Text(connection.state == .connected ? "Checking whether this request is still open…" : "Reconnect to answer from this phone.")
                        .font(.footnote).foregroundStyle(.secondary)
                }
                VStack(spacing: 8) {
                    ForEach(item.options) { opt in option(opt) }
                }
                if !pinned { countdown }
            } else if let text = item.resolutionText {
                Text(text).font(.footnote).foregroundStyle(.secondary)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(item.isOpen ? Theme.surface : Color.clear, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(Theme.hairline))
        // An amber edge while it waits for you.
        .overlay(alignment: .leading) {
            if item.isOpen {
                UnevenRoundedRectangle(topLeadingRadius: 16, bottomLeadingRadius: 16, style: .continuous)
                    .fill(Theme.attention).frame(width: 3)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .sensoryFeedback(.success, trigger: item.isOpen) { wasOpen, isOpen in wasOpen && !isOpen && sent != nil }
        .sensoryFeedback(.impact(weight: .medium), trigger: sent) { _, new in new != nil }
    }

    /// One option as a full-width row of the same shape as the others.
    private func option(_ opt: PermissionOption) -> some View {
        let (head, code) = OptionLabel.split(opt.label)
        let deny = opt.kind == .rejectOnce || opt.kind == .rejectAlways
        let primary = opt.kind == .allowOnce
        // Allow once is the answer most of the time: a solid button. Always-allow is quieter,
        // and Deny is red but outlined so it doesn't compete.
        return Button {
            if connection.resolvePermission(session: sessionId, request: item.request, option: opt.id,
                                            failed: { sent = nil }) {
                sent = opt.id
                // The card closes when the Mac reports the answer. If it hasn't after
                // 10 s, unlock the buttons so you can try again.
                Task {
                    try? await Task.sleep(for: .seconds(10))
                    // `item` is a copy from when the card was drawn: ask the timeline.
                    let open = connection.timeline(for: sessionId).openPermissions.contains { $0.request == item.request }
                    if open, sent == opt.id { sent = nil }
                }
            }
        } label: {
            HStack(alignment: .center, spacing: 10) {
                Group {
                    if sent == opt.id {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: deny ? "xmark" : opt.kind == .allowAlways ? "checkmark.seal" : opt.kind == .other ? "circle" : "checkmark")
                            .font(.callout.weight(.semibold))
                    }
                }
                .frame(width: 20)
                VStack(alignment: .leading, spacing: 2) {
                    Text(head).font(.callout.weight(.semibold)).lineLimit(2)
                    if let code {
                        Text(code)
                            .font(.system(.caption, design: .monospaced))
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .opacity(0.8)
                    }
                }
                Spacer(minLength: 0)
            }
            .multilineTextAlignment(.leading)
            .padding(.horizontal, 12).padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .foregroundStyle(primary ? Theme.onSolid : deny ? Theme.textSecondary : Theme.textPrimary)
            .background(primary ? AnyShapeStyle(Theme.solid) : AnyShapeStyle(deny ? Color.clear : Theme.bubble),
                        in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(deny ? Theme.hairline : .clear))
            .contentShape(RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .disabled(sent != nil || !canAnswer)
        .opacity(sent != nil && sent != opt.id ? 0.5 : 1)
    }

    @ViewBuilder private var countdown: some View {
        if let expires = item.expiresAt {
            TimelineView(.periodic(from: .now, by: 1)) { ctx in
                let left = Int(expires.timeIntervalSince(ctx.date).rounded(.up))
                Text(left > 0 ? "Answer here or in the terminal" : "Answer in the terminal")
                    .font(.caption).foregroundStyle(.secondary).monospacedDigit()
            }
        } else {
            Text("Answer here or in the terminal").font(.caption).foregroundStyle(.secondary)
        }
    }
}

/// Option labels for the phone: the agent's long wording, made short.
enum OptionLabel {
    /// "Yes, and don't ask again for commands that start with `xcodebuild …`"
    /// → ("Always allow commands starting with", "xcodebuild …").
    static func split(_ label: String) -> (String, String?) {
        var head = label
        var code: String?
        if let open = label.firstIndex(of: "`"),
           let close = label[label.index(after: open)...].firstIndex(of: "`") {
            code = String(label[label.index(after: open)..<close])
            head = String(label[..<open]).trimmingCharacters(in: .whitespaces)
        }
        let rewrites: [(String, String)] = [
            ("Yes, and don't ask again for commands that start with", "Always allow commands starting with"),
            ("Yes, and don't ask again for these files", "Always allow edits to these files"),
            ("Yes, and don't ask again", "Always allow"),
            ("Yes, proceed", "Allow once"),
            ("No, and tell Codex what to do differently", "Deny and tell Codex what to do"),
        ]
        for (from, to) in rewrites where head.hasPrefix(from) {
            head = to + head.dropFirst(from.count)
            break
        }
        return (head.isEmpty ? label : head, code)
    }
}

struct PlanCard: View {
    let entries: [PlanEntry]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("Plan", systemImage: "list.bullet").font(.subheadline.weight(.semibold))
            ForEach(Array(entries.enumerated()), id: \.offset) { _, e in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Image(systemName: e.status == "completed" ? "checkmark.circle.fill" : e.status == "in_progress" ? "circle.dotted" : "circle")
                        .foregroundStyle(e.status == "completed" ? AnyShapeStyle(.green) : AnyShapeStyle(.secondary))
                    Text(e.content).strikethrough(e.status == "completed").foregroundStyle(e.status == "completed" ? .secondary : .primary)
                }
                .font(.callout)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.background.secondary, in: RoundedRectangle(cornerRadius: 12))
    }
}

/// One step, as the Claude app shows it when you tap it: the tool's name on top,
/// what the step was for, then its command and its output, each with a copy button.
struct ToolDetailView: View {
    let item: ToolItem
    let title: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                StepDetail(item: item, title: title).padding(16)
            }
            .navigationTitle(item.tool.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { dismiss() } label: { Image(systemName: "xmark") }.accessibilityLabel("Close")
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }
}

/// A step's details: what it was for (unless `heading` is off), its command or
/// input, an edit's changes, and its output.
struct StepDetail: View {
    let item: ToolItem
    let title: String
    var heading = true

    /// Long output shows its first part (a whole log would make scrolling crawl).
    static let maxOutput = 20_000

    private var outputText: String {
        guard let output = item.output else {
            return item.status == .completed || item.status == .failed ? "(no output)" : "Still running…"
        }
        let text = output.displayText
        if text.isEmpty { return "(no output)" }
        return text.count > Self.maxOutput ? String(text.prefix(Self.maxOutput)) + "\n…" : text
    }

    var body: some View {
        let shell = item.tool.kind == .execute
        VStack(alignment: .leading, spacing: 18) {
            if heading, shell, let d = item.tool.input["description"]?.string {
                Text(d).font(.title3)
            }
            DetailCard(label: shell ? "Command" : "Input", copy: title) {
                Text(shell ? ShellHighlight.attributed(title) : AttributedString(title))
            }
            if item.tool.kind == .edit, let diffs = FileDiff.from(item.tool) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Changes").font(.subheadline).foregroundStyle(.secondary)
                    DiffPreview(diffs: diffs)
                }
            }
            DetailCard(label: "Output", copy: outputText) {
                Text(outputText)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// "Command" / "Output": a label with a copy button, the text in a rounded card.
struct DetailCard<Content: View>: View {
    let label: String
    let copy: String
    @ViewBuilder let content: Content
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(label).font(.subheadline).foregroundStyle(.secondary)
                Spacer()
                Button {
                    UIPasteboard.general.string = copy
                    copied = true
                    Task { try? await Task.sleep(for: .seconds(1.5)); copied = false }
                } label: {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc").font(.footnote)
                }
                .foregroundStyle(.secondary)
                .accessibilityLabel("Copy \(label.lowercased())")
            }
            content
                .font(.system(.footnote, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(14)
                .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        }
    }
}

/// A shell command coloured the way the Claude app does: commands in orange,
/// quoted strings in green, the rest plain.
enum ShellHighlight {
    static let command = Color(red: 0.95, green: 0.55, blue: 0.35)
    static let string = Color(red: 0.55, green: 0.78, blue: 0.45)

    static func attributed(_ cmd: String) -> AttributedString {
        var out = AttributedString()
        var word = ""
        var atStart = true   // the next word is a command
        var quote: Character? = nil
        var str = ""
        func flushWord() {
            guard !word.isEmpty else { return }
            var a = AttributedString(word)
            // `VAR=value` before a command isn't the command.
            if atStart, !word.contains("=") { a.foregroundColor = command; atStart = false }
            else if atStart, word.contains("=") { /* still waiting for the command */ }
            out += a
            word = ""
        }
        var chars = Array(cmd)[...]
        while let c = chars.first {
            chars = chars.dropFirst()
            if let q = quote {
                str.append(c)
                if c == q { var a = AttributedString(str); a.foregroundColor = string; out += a; str = ""; quote = nil }
                continue
            }
            switch c {
            case "'", "\"":
                flushWord(); atStart = false; quote = c; str = String(c)
            case " ", "\t":
                flushWord(); out += AttributedString(String(c))
            case "\n", ";", "|", "&", "(":
                flushWord(); out += AttributedString(String(c)); atStart = true
            default:
                word.append(c)
            }
        }
        flushWord()
        if !str.isEmpty { var a = AttributedString(str); a.foregroundColor = string; out += a }
        return out
    }
}


/// Message box at the bottom of a session.
struct Composer: View {
    @Environment(DaemonConnection.self) private var connection
    let sessionId: String
    let status: SessionStatus
    let agent: String
    /// Runs in `styr`: its turn can be stopped from here.
    var canStop = false
    @State private var text = ""
    @FocusState private var focused: Bool
    @State private var dictation = Dictation()
    /// What was in the box when dictation started; the transcript is added to it.
    @State private var beforeDictation = ""
    @State private var attachments: [Attachment] = []
    @State private var photoItems: [PhotosPickerItem] = []
    @State private var pickingFiles = false
    @State private var pickingPhotos = false
    @State private var sending = false

    private var canSend: Bool {
        connection.state == .connected && status != .done && !sending
            && (!text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty)
    }

    private var showsStop: Bool {
        canStop && (status == .working || status == .needsApproval) && !sending
            && text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && attachments.isEmpty
    }

    /// Upload the attachments to the Mac, then send the message with their paths.
    private func send() {
        dictation.stop()
        guard !attachments.isEmpty else {
            if connection.sendPrompt(session: sessionId, text: text) { text = "" }
            return
        }
        sending = true
        let files = attachments
        let message = text
        Task {
            defer { sending = false }
            var paths: [String] = []
            for f in files {
                do {
                    paths.append(try await connection.upload(session: sessionId, name: f.name, data: f.data))
                } catch {
                    connection.lastError = "Couldn't send \(f.name): \(error.localizedDescription)"
                    return
                }
            }
            if connection.sendPrompt(session: sessionId, text: AttachedFiles.compose(message, paths: paths)) {
                text = ""
                attachments = []
            }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if status == .done {
                Text("This session has ended.").font(.caption).foregroundStyle(.secondary)
            }
            if !attachments.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(attachments) { a in
                            AttachmentChip(attachment: a) { attachments.removeAll { $0.id == a.id } }
                        }
                    }
                    .padding(.vertical, 2)
                }
            }
            // One rounded card, like Claude Code's prompt box: the text on top, the
            // controls in a row beneath it.
            VStack(alignment: .leading, spacing: 4) {
                TextField("Reply to \(agent)…", text: $text, axis: .vertical)
                    .lineLimit(1...8)
                    .focused($focused)
                    .foregroundStyle(Theme.textPrimary)
                    .padding(.horizontal, 14).padding(.top, 12)
                    .disabled(status == .done)
                HStack(spacing: 6) {
                    Menu {
                        // A PhotosPicker inside a Menu often never opens (the menu closes
                        // first): open it from outside instead.
                        Button { pickingPhotos = true } label: { Label("Photos", systemImage: "photo.on.rectangle") }
                        Button { pickingFiles = true } label: { Label("Files", systemImage: "folder") }
                    } label: {
                        Image(systemName: "plus")
                            .font(.system(size: 17, weight: .medium))
                            .frame(width: 34, height: 34)
                            .contentShape(Rectangle())
                    }
                    .tint(Theme.textSecondary)
                    .disabled(status == .done || sending)
                    .accessibilityLabel("Attach images or files")
                    Spacer()
                    Button {
                        if !dictation.isListening {
                            beforeDictation = text.isEmpty || text.hasSuffix(" ") ? text : text + " "
                            focused = false
                        }
                        Task { await dictation.toggle() }
                    } label: {
                        Image(systemName: dictation.isListening ? "stop.circle.fill" : "mic")
                            .font(.system(size: 17, weight: .medium))
                            .symbolEffect(.pulse, isActive: dictation.isListening)
                            .frame(width: 34, height: 34)
                            .contentShape(Rectangle())
                    }
                    .tint(dictation.isListening ? Theme.accent : Theme.textSecondary)
                    .disabled(status == .done)
                    .accessibilityLabel(dictation.isListening ? "Stop dictation" : "Dictate")
                    if showsStop {
                        // Nothing typed while it works: stop its turn instead (Esc on the Mac).
                        Button { connection.interrupt(sessionId) } label: {
                            Image(systemName: "stop.fill")
                                .font(.system(size: 12, weight: .bold))
                                .foregroundStyle(Theme.onSolid)
                                .frame(width: 32, height: 32)
                                .background(Theme.solid, in: Circle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Stop \(agent)")
                        .sensoryFeedback(.impact(weight: .medium), trigger: status)
                    } else {
                        Button(action: send) {
                            Group {
                                if sending {
                                    ProgressView().tint(.white)
                                } else {
                                    Image(systemName: "arrow.up").font(.system(size: 15, weight: .bold))
                                }
                            }
                            .foregroundStyle(.white)
                            .frame(width: 32, height: 32)
                            .background(canSend || sending ? Theme.accent : Theme.accent.opacity(0.35), in: Circle())
                        }
                        .buttonStyle(.plain)
                        .disabled(!canSend)
                        .accessibilityLabel("Send")
                    }
                }
                .padding(.horizontal, 6).padding(.bottom, 6)
            }
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: 24, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 24, style: .continuous).strokeBorder(Theme.hairline))
            .shadow(color: .black.opacity(0.04), radius: 8, y: 2)
        }
        .padding(.horizontal, 12).padding(.top, 6).padding(.bottom, 6)
        .background(Theme.canvas)
        .onChange(of: dictation.transcript) { _, t in
            if dictation.isListening || !t.isEmpty { text = beforeDictation + t }
        }
        .onChange(of: dictation.state) { _, s in
            switch s {
            case .denied: connection.lastError = "Allow Microphone and Speech Recognition for Styrman in Settings to dictate."
            case .unavailable: connection.lastError = "Dictation isn't available right now."
            default: break
            }
        }
        .onDisappear { dictation.stop() }
        .photosPicker(isPresented: $pickingPhotos, selection: $photoItems, maxSelectionCount: 5, matching: .images)
        .onChange(of: photoItems) { _, items in
            guard !items.isEmpty else { return }
            photoItems = []
            Task {
                var failed = 0
                for item in items {
                    guard let data = try? await item.loadTransferable(type: Data.self),
                          let jpeg = Self.photoJPEG(data) else { failed += 1; continue }
                    attachments.append(Attachment(name: Self.unique("photo.jpg", in: attachments), data: jpeg, isImage: true))
                }
                if failed > 0 {
                    connection.lastError = failed == 1 ? "Couldn't load that photo (is it still downloading from iCloud?)."
                        : "Couldn't load \(failed) of the photos (are they still downloading from iCloud?)."
                }
            }
        }
        .fileImporter(isPresented: $pickingFiles, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            guard case .success(let urls) = result else { return }
            for url in urls.prefix(5) {
                let scoped = url.startAccessingSecurityScopedResource()
                defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                guard let data = try? Data(contentsOf: url) else { continue }
                let image = ["png", "jpg", "jpeg", "gif", "webp", "heic"].contains(url.pathExtension.lowercased())
                attachments.append(Attachment(name: Self.unique(url.lastPathComponent, in: attachments), data: data, isImage: image))
            }
        }
    }
}

extension Composer {
    /// A name no other attachment has (they're saved side by side on the Mac):
    /// "photo.jpg", "photo-2.jpg", …
    static func unique(_ name: String, in attachments: [Attachment]) -> String {
        let taken = Set(attachments.map(\.name))
        guard taken.contains(name) || name == "photo.jpg" else { return name }
        let ext = (name as NSString).pathExtension
        let base = (name as NSString).deletingPathExtension
        var n = name == "photo.jpg" && !taken.contains(name) ? 1 : 2
        while true {
            let candidate = n == 1 ? name : "\(base)-\(n)" + (ext.isEmpty ? "" : ".\(ext)")
            if !taken.contains(candidate) { return candidate }
            n += 1
        }
    }

    /// A photo as JPEG (HEIC isn't readable by every agent), at most 2048 px on its
    /// longest side: plenty for an agent, and quick to send.
    static func photoJPEG(_ data: Data) -> Data? {
        guard let image = UIImage(data: data) else { return nil }
        let longest = max(image.size.width, image.size.height) * image.scale
        guard longest > 2048 else { return image.jpegData(compressionQuality: 0.85) }
        let factor = 2048 / longest
        let size = CGSize(width: image.size.width * image.scale * factor, height: image.size.height * image.scale * factor)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let scaled = UIGraphicsImageRenderer(size: size, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: size))
        }
        return scaled.jpegData(compressionQuality: 0.85)
    }
}

/// An image or file waiting to go with the next message.
struct Attachment: Identifiable {
    let id = UUID()
    let name: String
    let data: Data
    let isImage: Bool
}

private struct AttachmentChip: View {
    let attachment: Attachment
    let remove: () -> Void
    var body: some View {
        HStack(spacing: 6) {
            if attachment.isImage, let img = UIImage(data: attachment.data) {
                Image(uiImage: img).resizable().scaledToFill()
                    .frame(width: 30, height: 30).clipShape(RoundedRectangle(cornerRadius: 6))
            } else {
                Image(systemName: "doc").frame(width: 30, height: 30)
                    .background(Color(.tertiarySystemFill), in: RoundedRectangle(cornerRadius: 6))
            }
            Text(attachment.name).font(.caption).lineLimit(1).frame(maxWidth: 120)
            Button(action: remove) { Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary) }
                .buttonStyle(.plain)
                .accessibilityLabel("Remove \(attachment.name)")
        }
        .padding(.leading, 4).padding(.trailing, 8).padding(.vertical, 4)
        .background(Theme.surface, in: Capsule())
    }
}

/// How attachments ride along in a message: their paths on the Mac, after the text.
enum AttachedFiles {
    static let marker = "Attached (saved on this Mac):"

    static func compose(_ text: String, paths: [String]) -> String {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let list = paths.map { "- \($0)" }.joined(separator: "\n")
        return (t.isEmpty ? "" : t + "\n\n") + marker + "\n" + list
    }

    /// The text and the files' names (without the Mac's time stamp), if it has any.
    static func split(_ message: String) -> (text: String, files: [String])? {
        guard let r = message.range(of: marker) else { return nil }
        let text = message[..<r.lowerBound].trimmingCharacters(in: .whitespacesAndNewlines)
        let files = message[r.upperBound...].split(separator: "\n").compactMap { line -> String? in
            let p = line.trimmingCharacters(in: .whitespaces)
            guard p.hasPrefix("- ") else { return nil }
            let name = (String(p.dropFirst(2)) as NSString).lastPathComponent
            // "20261125-223812-photo-1.jpg" → "photo-1.jpg"
            if let m = name.range(of: #"^\d{8}-\d{6}-"#, options: .regularExpression) { return String(name[m.upperBound...]) }
            return name
        }
        return (text, files)
    }
}

/// Files that went with a message, as small chips.
struct AttachedFilesRow: View {
    let files: [String]
    var body: some View {
        HStack(spacing: 6) {
            ForEach(Array(files.enumerated()), id: \.offset) { _, f in
                let image = ["png", "jpg", "jpeg", "gif", "webp", "heic"].contains((f as NSString).pathExtension.lowercased())
                Label(f, systemImage: image ? "photo" : "doc")
                    .font(.caption)
                    .lineLimit(1)
                    .padding(.horizontal, 8).padding(.vertical, 4)
                    .background(Color(.tertiarySystemFill), in: Capsule())
            }
        }
    }
}

struct PendingPromptRow: View {
    let prompt: SessionTimeline.PendingPrompt
    let dismiss: () -> Void

    private var label: String {
        if prompt.delivered { return "Handed to agent" }
        if prompt.queued { return "Queued on Mac" }
        if prompt.unconfirmed { return "Delivery unconfirmed" }
        return "Sending…"
    }

    var body: some View {
        HStack {
            Spacer(minLength: 56)
            VStack(alignment: .trailing, spacing: 4) {
                if let files = AttachedFiles.split(prompt.text)?.files, !files.isEmpty { AttachedFilesRow(files: files) }
                let shown = AttachedFiles.split(prompt.text)?.text ?? prompt.text
                if !shown.isEmpty {
                    Text(shown)
                        .textSelection(.enabled)
                        .foregroundStyle(Theme.textPrimary.opacity(0.75))
                        .padding(.horizontal, 16).padding(.vertical, 11)
                        .background(Theme.bubble.opacity(0.6), in: RoundedRectangle(cornerRadius: 20, style: .continuous))
                        .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous)
                            .strokeBorder(Theme.hairline, style: StrokeStyle(lineWidth: 1, dash: [4, 3])))
                }
                Text(label).font(.caption2).foregroundStyle(.secondary)
                if prompt.unconfirmed {
                    HStack {
                        Button("Copy") { UIPasteboard.general.string = prompt.text }
                        Button("Dismiss", action: dismiss)
                    }
                    .font(.footnote)
                }
            }
        }
    }
}

/// A message another agent sent through Styrman (`styr ask`), as the receiving agent
/// read it: "Message from Claude (repo), sent through Styrman:\n\n<body>\n\n(Reply …)".
struct AgentMessage: Equatable {
    let sender: String   // "Claude (styrman)"
    let agent: AgentKind
    let body: String

    init?(_ text: String) {
        let prefix = "Message from "
        guard text.hasPrefix(prefix), let head = text.range(of: ", sent through Styrman:") else { return nil }
        sender = String(text[text.index(text.startIndex, offsetBy: prefix.count)..<head.lowerBound])
        var rest = String(text[head.upperBound...]).trimmingCharacters(in: .whitespacesAndNewlines)
        if let tail = rest.range(of: "\n\n(Reply as you normally would", options: .backwards) {
            rest = String(rest[..<tail.lowerBound])
        }
        body = rest.trimmingCharacters(in: .whitespacesAndNewlines)
        let lower = sender.lowercased()
        agent = lower.hasPrefix("claude") ? .claude : lower.hasPrefix("codex") ? .codex
            : lower.hasPrefix("agy") ? .antigravity : .unknown
    }
}

/// Shown on the left like a reply would be, with the sending agent's colour.
struct AgentMessageRow: View {
    let message: AgentMessage
    var body: some View {
        let c = Theme.color(for: message.agent)
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                AgentAvatar(agent: message.agent, size: 18)
                Text("From \(message.sender)").font(.caption.weight(.semibold)).foregroundStyle(c)
                Image(systemName: "arrow.right").font(.caption2).foregroundStyle(.secondary)
            }
            ReplyText(raw: message.body, selectable: true)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(c.opacity(0.10), in: RoundedRectangle(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(c.opacity(0.35)))
    }
}


/// What the conversation shows: timeline items, with runs of tool calls grouped.
enum DisplayRow: Identifiable {
    case item(TimelineItem)
    case tools(id: String, [ToolItem])

    var id: String {
        switch self {
        case .item(let i): i.id
        case .tools(let id, _): id
        }
    }

    static func rows(_ items: [TimelineItem]) -> [DisplayRow] {
        var out: [DisplayRow] = []
        var run: [(String, ToolItem)] = []
        func flush() {
            if let first = run.first {
                out.append(.tools(id: first.0, run.map(\.1)))
            }
            run = []
        }
        for item in items {
            if case .tool(let id, let t) = item {
                run.append((id, t))
            } else {
                flush()
                out.append(.item(item))
            }
        }
        flush()
        return out
    }
}

/// A run of steps, as the Claude app shows it: one quiet line. "Ran 3 commands,
/// read a file", a single step's description, or while one runs, "Running". Tap
/// for the steps, their commands and output.
struct StepsRow: View {
    let tools: [ToolItem]
    let cwd: String?
    @State private var open = false

    var body: some View {
        let running = tools.last.map { $0.status == .pending || $0.status == .running } ?? false
        let failed = tools.filter { $0.status == .failed }.count
        Button { open = true } label: {
            HStack(spacing: 8) {
                Group {
                    if tools.count > 1 && !running {
                        Image(systemName: "list.bullet").font(.footnote)
                    } else {
                        StepIcon(kind: (tools.last ?? tools[0]).tool.kind)
                    }
                }
                .frame(width: 18)
                if running {
                    Shimmer(text: Self.doing((tools.last ?? tools[0]).tool.kind), color: .secondary)
                } else {
                    Text(tools.count == 1 ? Self.title(tools[0], cwd: cwd) : Self.summary(tools))
                        .lineLimit(1).truncationMode(.tail)
                    if tools.count == 1, tools[0].tool.kind == .edit, let diffs = FileDiff.from(tools[0].tool) {
                        DiffCounts(diffs: diffs)
                    }
                    if failed > 0 {
                        Image(systemName: "exclamationmark.triangle").font(.caption).foregroundStyle(.orange)
                    }
                }
                Image(systemName: "chevron.right").font(.system(size: 9, weight: .semibold)).foregroundStyle(.tertiary)
                Spacer(minLength: 0)
            }
            .font(.footnote)
            .foregroundStyle(Theme.textSecondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityHint("Shows the steps, their commands and output")
        .sheet(isPresented: $open) {
            if tools.count == 1 {
                ToolDetailView(item: tools[0], title: displayTitle(tools[0].tool, cwd: cwd))
            } else {
                StepsSheet(tools: tools, cwd: cwd)
            }
        }
    }

    /// What one step says: Claude's description of a command ("Run the parser
    /// tests"), else the command or the tool's title.
    static func title(_ t: ToolItem, cwd: String?) -> String {
        (t.tool.kind == .execute ? t.tool.input["description"]?.string : nil) ?? displayTitle(t.tool, cwd: cwd)
    }

    /// A step in progress: "Running", "Reading", …
    static func doing(_ kind: ToolKind) -> String {
        switch kind {
        case .execute: "Running"
        case .read: "Reading"
        case .edit: "Editing"
        case .delete: "Deleting"
        case .move: "Moving"
        case .search: "Searching"
        case .fetch: "Fetching"
        case .think: "Planning"
        case .mcp, .other: "Working"
        }
    }

    /// "Ran 3 commands, read a file (1 failed)", kinds in the order they came.
    static func summary(_ tools: [ToolItem]) -> String {
        var order: [String] = []
        var counts: [String: Int] = [:]
        for t in tools {
            let key: String = switch t.tool.kind {
            case .execute: "ran|command|commands"
            case .read: "read|file|files"
            case .edit: "edited|file|files"
            case .delete: "deleted|file|files"
            case .move: "moved|file|files"
            case .search: "searched|time|times"
            case .fetch: "fetched|page|pages"
            case .think: "updated|plan|plans"
            case .mcp, .other: "used|tool|tools"
            }
            if counts[key] == nil { order.append(key) }
            counts[key, default: 0] += 1
        }
        var text = order.map { key in
            let w = key.split(separator: "|").map(String.init)
            let n = counts[key]!
            if w[1] == "time" { return n == 1 ? "searched once" : "searched \(n) times" }
            if w[1] == "plan" { return "updated the plan" }
            return n == 1 ? "\(w[0]) a \(w[1])" : "\(w[0]) \(n) \(w[2])"
        }.joined(separator: ", ")
        let failed = tools.filter { $0.status == .failed }.count
        if failed > 0 { text += " (\(failed) failed)" }
        return text.prefix(1).uppercased() + text.dropFirst()
    }
}

/// The steps of a run, as the Claude app lists them: one under another, joined by a
/// thin line, each opening in place to its command and output.
struct StepsSheet: View {
    let tools: [ToolItem]
    let cwd: String?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 0) {
                    ForEach(Array(tools.enumerated()), id: \.offset) { i, t in
                        StepEntry(item: t, cwd: cwd, last: i == tools.count - 1)
                    }
                }
                .padding(16)
            }
            .navigationTitle(StepsRow.summary(tools))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { dismiss() } label: { Image(systemName: "xmark") }.accessibilityLabel("Close")
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }
}

/// One step in a run's list: its icon on the line, what it did, and a chevron that
/// opens it.
struct StepEntry: View {
    let item: ToolItem
    let cwd: String?
    let last: Bool
    @State private var open = false

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(spacing: 6) {
                StepIcon(kind: item.tool.kind).frame(height: 22)
                if !last {
                    Rectangle().fill(Color.secondary.opacity(0.3)).frame(width: 1).frame(maxHeight: .infinity)
                }
            }
            .frame(width: 22)
            VStack(alignment: .leading, spacing: 12) {
                Button { withAnimation(.snappy) { open.toggle() } } label: {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(StepsRow.title(item, cwd: cwd))
                            .lineLimit(open ? nil : 1)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        if item.status == .failed {
                            Image(systemName: "exclamationmark.triangle").font(.caption).foregroundStyle(.orange)
                        } else if item.status == .running || item.status == .pending {
                            ProgressView().controlSize(.mini)
                        }
                        Image(systemName: "chevron.right").font(.caption.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .rotationEffect(.degrees(open ? 90 : 0))
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                if open {
                    StepDetail(item: item, title: displayTitle(item.tool, cwd: cwd), heading: false)
                        .transition(.opacity)
                }
            }
            .padding(.bottom, last ? 0 : 22)
        }
    }
}

/// A step's mark: `>_` for a command, the tool's symbol otherwise.
struct StepIcon: View {
    let kind: ToolKind
    var body: some View {
        Group {
            if kind == .execute {
                Text(">_").font(.system(.footnote, design: .monospaced).weight(.semibold))
                    .lineLimit(1).fixedSize()
            } else {
                Image(systemName: Theme.symbol(for: kind)).font(.footnote)
            }
        }
        .foregroundStyle(.secondary)
    }
}

/// Text with a lighter band passing over it, as the Claude app draws what's in progress.
struct Shimmer: View {
    let text: String
    var color: Color
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        if reduceMotion {
            Text(text).foregroundStyle(color)
        } else {
            TimelineView(.animation(minimumInterval: 1.0 / 20)) { ctx in
                let n = text.count
                // One pass about every two seconds, with a pause off the end.
                let head = (ctx.date.timeIntervalSinceReferenceDate / 0.07)
                    .truncatingRemainder(dividingBy: Double(n + 14)) - 3
                var out = AttributedString()
                for (i, ch) in text.enumerated() {
                    var a = AttributedString(String(ch))
                    let d = abs(Double(i) - head)
                    a.foregroundColor = color.opacity(d < 2.5 ? 0.45 + 0.2 * d : 1)
                    out += a
                }
                return Text(out)
            }
        }
    }
}

/// The grouped background with a faint wash of the agent's colour at the top.
struct SessionBackground: View {
    let agent: AgentKind
    var body: some View {
        ZStack(alignment: .top) {
            Theme.canvas
        }
        .ignoresSafeArea()
    }
}

/// The reply the agent is writing right now, as its terminal shows it: plain text
/// that grows, with a blinking cursor. The real message replaces it when it's done.
/// Where the conversation is scrolled: near its end or not, and how tall it is.
struct ScrollSpot: Equatable {
    var near: Bool
    var height: CGFloat
}

struct LivePreviewRow: View {
    let text: String
    let agent: AgentKind
    var sessionId = ""
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// How many characters of `text` show so far.
    @State private var revealed = 0

    var body: some View {
        // Like the Claude app: the terminal's text arrives in chunks, about twice a
        // second, but shows at a steady pace, newest words dimmed. A chunk shown all
        // at once is a jump; spread out, it reads as writing.
        let full = Self.formatted(text)
        let count = full.characters.count
        Group {
            if hasTable {
                ReplyText(raw: String(text.prefix(reduceMotion ? text.count : revealed)))
            } else {
                Text(Self.reveal(full, upTo: reduceMotion ? count : revealed))
                    .font(ReplyText.prose).lineSpacing(3)
            }
        }
        .foregroundStyle(Theme.textPrimary.opacity(0.85))
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentTransition(.identity)
        .accessibilityLabel("\(agent.displayName) is writing: \(String(full.characters))")
        .onAppear {
            // Back after a moment away (a tool step, a reading that missed): carry on
            // where it was, rather than write the whole reply out again.
            revealed = Self.resume(Self.shown[sessionId], now: Self.plain(text))
        }
        .onChange(of: text) { _, new in
            // Earlier text read differently (Claude bolds a word once its closing
            // `**` arrives): it changes in place; the reveal keeps its place.
            revealed = min(revealed, Self.plain(new).count)
        }
        .onChange(of: revealed) { _, n in Self.shown[sessionId] = (Self.plain(text), n) }
        .task(id: text) { await flow() }
    }

    /// How far each session's preview got: (its text, characters shown).
    static var shown: [String: (text: String, count: Int)] = [:]

    /// Where to pick up `now` after showing `before`: as far as before if it's the
    /// same reply (its opening matches), from the start if it's another one.
    static func resume(_ before: (text: String, count: Int)?, now: String) -> Int {
        guard let before else { return 0 }
        let same = before.text.commonPrefix(with: now).count
        return same >= min(20, now.count, before.text.count) && same > 0 ? min(before.count, now.count) : 0
    }

    /// The preview's Markdown (bold, italic, code: what the Mac read off the
    /// terminal's formatting), or the text as it is.
    static func formatted(_ text: String) -> AttributedString {
        (try? AttributedString(markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(text)
    }

    static func plain(_ text: String) -> String { String(formatted(text).characters) }

    /// The first `n` characters of `full`, its last few words dimmed.
    static func reveal(_ full: AttributedString, upTo n: Int) -> AttributedString {
        let end = full.characters.index(full.startIndex, offsetBy: min(n, full.characters.count))
        var shown = AttributedString(full[full.startIndex..<end])
        let fresh = split(String(shown.characters)).1.count
        let from = shown.characters.index(shown.endIndex, offsetBy: -fresh)
        shown[from..<shown.endIndex].foregroundColor = .secondary
        return shown
    }

    /// Reveal the rest at a pace that catches up in about half a second (the time
    /// to the next chunk), never slower than a quick reader.
    private func flow() async {
        let tick = 1.0 / 30
        let total = hasTable ? text.count : Self.plain(text).count
        while !Task.isCancelled, revealed < total {
            let behind = Double(total - revealed)
            let step = max(1, Int((max(60, behind / 0.5) * tick).rounded()))
            revealed = min(total, revealed + step)
            try? await Task.sleep(for: .seconds(tick))
        }
    }

    /// The text up to its last few words, and those words (shown dimmed).
    static func split(_ s: String) -> (String, String) {
        var words = 0
        var i = s.endIndex
        while i > s.startIndex {
            let j = s.index(before: i)
            if s[j] == " " || s[j] == "\n" {
                words += 1
                if words == 4 { break }
            }
            i = j
        }
        return (String(s[..<i]), String(s[i...]))
    }

    /// Tables the terminal drew come as `| a | b |` rows: drawn as a table.
    private var hasTable: Bool {
        ReplyMarkdown.blocks(text).contains { if case .table = $0 { true } else { false } }
    }
}

/// While the agent works, as the Claude app shows it: a little spinner and the
/// word Claude picked ("Brewing…") in its colour; while it thinks, or just after,
/// how long it's been ("26s · still thinking…", "22s · thought for 1s").
struct WorkingRow: View {
    var status: WorkStatus? = nil
    var since = Date()
    var thinkingSince: Date? = nil
    let agent: AgentKind

    var body: some View {
        let tint = Theme.color(for: agent)
        HStack(spacing: 8) {
            WorkingDots(color: tint).frame(width: 18)
            TimelineView(.periodic(from: .now, by: 1)) { ctx in
                if let time = Self.timeLine(status, extra: Int(ctx.date.timeIntervalSince(since)),
                                            thinkingFor: thinkingSince.map { ctx.date.timeIntervalSince($0) }) {
                    Text(time).foregroundStyle(.secondary)
                } else {
                    HStack(spacing: 0) {
                        Shimmer(text: status?.label ?? "Working…", color: tint)
                        if let secs = status?.elapsedSecs {
                            Text(" · \(WorkStatus.duration(secs + max(0, Int(ctx.date.timeIntervalSince(since)))))")
                                .foregroundStyle(Theme.textSecondary).monospacedDigit()
                        }
                    }
                }
            }
        }
        .font(.subheadline)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(agent.displayName) is working")
    }

    /// While it thinks or right after: "26s · still thinking…", "22s · thought for 1s".
    /// Nil otherwise (the word shows).
    static func timeLine(_ s: WorkStatus?, extra: Int, thinkingFor: TimeInterval?) -> String? {
        guard let s, let secs = s.elapsedSecs else { return nil }
        let time = WorkStatus.duration(secs + max(0, extra))
        if s.thinking { return "\(time) · \((thinkingFor ?? 0) >= 8 ? "still thinking…" : "thinking…")" }
        if let note = s.note, note.hasPrefix("thought for") { return "\(time) · \(note)" }
        return nil
    }
}

/// A star that grows and shrinks, like a coding agent's terminal spinner.
struct WorkingDots: View {
    let color: Color
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// Out and back: · ✢ ✳ ✶ ✻ ✽ ✻ ✶ ✳ ✢
    static let frames = ["·", "✢", "✳", "✶", "✻", "✽", "✻", "✶", "✳", "✢"]

    var body: some View {
        TimelineView(.periodic(from: .now, by: 0.12)) { ctx in
            let i = reduceMotion ? 4 : Int(ctx.date.timeIntervalSinceReferenceDate / 0.12) % Self.frames.count
            Text(Self.frames[i])
                .font(.system(size: 15, weight: .bold))
                .foregroundStyle(color)
                .frame(width: 16, height: 16)
        }
        .accessibilityHidden(true)
    }
}

/// The agent's terminal on the Mac, live and read-only: what `styr` sees on screen.
/// Sized to fit the width; pinch to zoom.
struct TerminalSheet: View {
    @Environment(DaemonConnection.self) private var connection
    @Environment(\.dismiss) private var dismiss
    let sessionId: String
    let agent: AgentKind
    @State private var zoom: CGFloat = 1
    @GestureState private var pinch: CGFloat = 1

    var body: some View {
        NavigationStack {
            GeometryReader { geo in
                let screen = connection.screens[sessionId]
                let cols = CGFloat(max(screen?.cols ?? 80, 20))
                // A monospaced character is about 0.6 of the font size wide.
                let fit = max(5, (geo.size.width - 16) / (cols * 0.6))
                ScrollView([.vertical, .horizontal]) {
                    Text(screen?.text.trimmingCharacters(in: .newlines) ?? "")
                        .font(.system(size: fit * zoom * pinch, design: .monospaced))
                        .foregroundStyle(Color(white: 0.9))
                        .fixedSize()
                        .padding(8)
                        .textSelection(.enabled)
                }
                .defaultScrollAnchor(.bottomLeading)
                .overlay {
                    if screen == nil {
                        ProgressView("Asking the Mac for the screen…").tint(.white).foregroundStyle(.white)
                    }
                }
                .simultaneousGesture(
                    MagnifyGesture()
                        .updating($pinch) { v, s, _ in s = v.magnification }
                        .onEnded { v in zoom = min(4, max(0.6, zoom * v.magnification)) }
                )
            }
            .background(Color(red: 0.07, green: 0.07, blue: 0.08))
            .navigationTitle("Terminal")
            .navigationBarTitleDisplayMode(.inline)
            .toolbarColorScheme(.dark, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
        // Dark inside the sheet only (preferredColorScheme would darken the app behind it).
        .environment(\.colorScheme, .dark)
        .tint(Theme.color(for: agent))
        // Keep asking while it's open: the Mac stops sending 20 s after the last ask.
        .task {
            while !Task.isCancelled {
                connection.watchScreen(sessionId)
                try? await Task.sleep(for: .seconds(8))
            }
        }
    }
}

/// Agents working together (`styr ask`): links from a session to the helpers it
/// started, and from a helper back to the session it's helping.
struct HelperLinks: View {
    @Environment(DaemonConnection.self) private var connection
    let summary: SessionSummary

    var body: some View {
        let asker = summary.helperOf.flatMap { connection.session($0) }
        let helpers = connection.sessions.filter { $0.helperOf == summary.id }
        if asker != nil || !helpers.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    if let asker {
                        chip(asker, "Helping \(asker.agent.displayName) · \(asker.repoName)", "arrow.turn.up.left")
                    }
                    ForEach(helpers) { h in
                        chip(h, "\(h.agent.displayName) is helping", "arrow.turn.down.right")
                    }
                }
                .padding(.horizontal, 12).padding(.vertical, 6)
            }
            .background(Theme.canvas)
        }
    }

    private func chip(_ s: SessionSummary, _ title: String, _ symbol: String) -> some View {
        NavigationLink(value: s.id) {
            HStack(spacing: 6) {
                AgentAvatar(agent: s.agent, size: 20)
                Text(title).lineLimit(1)
                // What it's doing now: working, waiting on you, or done.
                switch s.status {
                case .working: WorkingDots(color: Theme.color(for: s.agent))
                case .needsApproval: Image(systemName: "hand.raised.fill").foregroundStyle(Theme.attention)
                default: Image(systemName: "checkmark").font(.caption2.weight(.bold)).foregroundStyle(.secondary)
                }
            }
            .font(.footnote.weight(.medium))
            .padding(.horizontal, 10).padding(.vertical, 6)
            .background(Theme.color(for: s.agent).opacity(0.12), in: Capsule())
        }
        .buttonStyle(.plain)
    }
}


/// "3 running tasks", as the Claude app has it under the conversation: shell
/// commands and agents left running in the background. Tap for the list.
struct RunningTasksLink: View {
    let timeline: SessionTimeline
    let cwd: String?
    @State private var open = false

    var body: some View {
        let n = timeline.runningTasks.count
        Button { open = true } label: {
            Text(n == 1 ? "1 running task" : "\(n) running tasks").underline()
        }
        .buttonStyle(.plain)
        .foregroundStyle(.blue)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.top, 4)
        .sheet(isPresented: $open) { BackgroundTasksSheet(timeline: timeline, cwd: cwd) }
    }
}

/// The background tasks: running ones first, then those that finished, each a card
/// with what it does, whether it's a shell command or an agent, and for how long.
struct BackgroundTasksSheet: View {
    let timeline: SessionTimeline
    let cwd: String?
    @Environment(\.dismiss) private var dismiss
    @State private var showRunning = true
    @State private var showDone = false

    var body: some View {
        let running = timeline.runningTasks
        let done = Array(timeline.tasks.filter { $0.ended != nil }.reversed().prefix(30))
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    section("Running", count: running.count, open: $showRunning)
                    if showRunning {
                        if running.isEmpty {
                            Text("Nothing running now.").font(.subheadline).foregroundStyle(.secondary)
                        }
                        ForEach(running) { TaskCard(task: $0, cwd: cwd) }
                    }
                    if !done.isEmpty {
                        section("Finished", count: done.count, open: $showDone).padding(.top, 8)
                        if showDone { ForEach(done) { TaskCard(task: $0, cwd: cwd) } }
                    }
                }
                .padding(16)
            }
            .navigationTitle("Background tasks")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { dismiss() } label: { Image(systemName: "xmark") }.accessibilityLabel("Close")
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }

    private func section(_ title: String, count: Int, open: Binding<Bool>) -> some View {
        Button { withAnimation(.snappy) { open.wrappedValue.toggle() } } label: {
            HStack(spacing: 6) {
                Text(title)
                Image(systemName: "chevron.down").font(.caption.weight(.semibold))
                    .rotationEffect(.degrees(open.wrappedValue ? 0 : -90))
            }
            .font(.subheadline).foregroundStyle(.secondary)
        }
        .buttonStyle(.plain)
    }
}

/// One background task: its mark, what it does (shimmering while it runs), "Shell"
/// or "Agent" and how long it has run or ran. Tap for the command and output.
struct TaskCard: View {
    let task: AgentTask
    let cwd: String?
    @State private var open = false

    var body: some View {
        Button { open = true } label: {
            HStack(alignment: .top, spacing: 12) {
                Group {
                    if task.isAgent { Image(systemName: "diamond").font(.footnote) }
                    else { StepIcon(kind: .execute) }
                }
                .foregroundStyle(.secondary)
                .frame(width: 22, height: 22)
                VStack(alignment: .leading, spacing: 4) {
                    if task.ended == nil {
                        Shimmer(text: task.title, color: .primary).lineLimit(2)
                    } else {
                        Text(task.title).lineLimit(2)
                    }
                    TimelineView(.periodic(from: .now, by: 1)) { ctx in
                        HStack(spacing: 8) {
                            Text(task.isAgent ? "Agent" : "Shell").fontWeight(.medium)
                            Text(Self.duration((task.ended?.at ?? ctx.date).timeIntervalSince(task.started)))
                                .foregroundStyle(.secondary)
                            if let end = task.ended, end.status != "completed" {
                                Text(end.status).foregroundStyle(.orange)
                            }
                        }
                        .font(.footnote)
                    }
                    if let summary = task.ended?.summary {
                        Text(summary).font(.footnote).foregroundStyle(.secondary).lineLimit(2)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .sheet(isPresented: $open) {
            ToolDetailView(item: ToolItem(call: task.id, tool: task.tool,
                                          status: task.ended == nil ? .running : .completed, output: nil),
                           title: displayTitle(task.tool, cwd: cwd))
        }
    }

    /// "28s", "4m 2s", "1h 11m", like the Claude app's.
    static func duration(_ t: TimeInterval) -> String {
        let s = max(0, Int(t))
        if s >= 3600 { return "\(s / 3600)h \(s % 3600 / 60)m" }
        return WorkStatus.duration(s)
    }
}


/// What the Mac notes in a session when its agent talks to another one:
/// "Asked Codex (reCORD): …", "Started Codex (reCORD) for this and asked: …",
/// "Codex (reCORD) replied: …", "No reply from Codex (reCORD) in time".
struct PeerNote: Equatable {
    enum Kind: Equatable { case asked(started: Bool), replied, noReply }
    let kind: Kind
    let agent: AgentKind
    /// "reCORD": the other agent's folder.
    let repo: String
    let text: String

    init(kind: Kind, agent: AgentKind, repo: String, text: String) {
        (self.kind, self.agent, self.repo, self.text) = (kind, agent, repo, text)
    }

    /// "Codex (reCORD)".
    var who: String { "\(agent.displayName) (\(repo))" }

    init?(_ note: String) {
        func split(_ s: Substring) -> (AgentKind, String)? {
            guard let open = s.lastIndex(of: "("), s.hasSuffix(")") else { return nil }
            let name = s[..<open].trimmingCharacters(in: .whitespaces).lowercased()
            let agent: AgentKind = name.hasPrefix("claude") ? .claude : name.hasPrefix("codex") ? .codex
                : name.hasPrefix("agy") || name.hasPrefix("antigravity") ? .antigravity : .unknown
            guard agent != .unknown else { return nil }
            return (agent, String(s[s.index(after: open)..<s.index(before: s.endIndex)]))
        }
        let n = note[...]
        if let r = n.range(of: ") replied: "), let who = split(n[..<n.index(after: r.lowerBound)]) {
            (kind, agent, repo, text) = (.replied, who.0, who.1, String(n[r.upperBound...]))
        } else if n.hasPrefix("Asked "), let r = n.range(of: "): "),
                  let who = split(n[n.index(n.startIndex, offsetBy: 6)..<n.index(after: r.lowerBound)]) {
            (kind, agent, repo, text) = (.asked(started: false), who.0, who.1, String(n[r.upperBound...]))
        } else if n.hasPrefix("Started "), let r = n.range(of: ") for this and asked: "),
                  let who = split(n[n.index(n.startIndex, offsetBy: 8)..<n.index(after: r.lowerBound)]) {
            (kind, agent, repo, text) = (.asked(started: true), who.0, who.1, String(n[r.upperBound...]))
        } else if n.hasPrefix("No reply from "), n.hasSuffix(" in time"),
                  let who = split(n.dropFirst(14).dropLast(8)) {
            (kind, agent, repo, text) = (.noReply, who.0, who.1, "")
        } else {
            return nil
        }
    }

    /// Each reply with the ask it answers (the latest ask to that agent before it).
    static func exchanges(_ items: [TimelineItem]) -> [PeerExchange] {
        var asks: [String: (id: String, text: String)] = [:]
        var out: [PeerExchange] = []
        for item in items {
            guard case .notice(let id, let text, _) = item, let note = PeerNote(text) else { continue }
            switch note.kind {
            case .asked: asks[note.who] = (id, note.text)
            case .replied:
                if let ask = asks.removeValue(forKey: note.who) {
                    out.append(PeerExchange(id: id, askId: ask.id, agent: note.agent, repo: note.repo,
                                            question: ask.text, answer: note.text))
                }
            case .noReply: asks[note.who] = nil
            }
        }
        return out
    }

    /// The asks no reply (or "no reply in time") has come for yet, by notice id.
    static func openAsks(_ items: [TimelineItem]) -> Set<String> {
        var open: [String: String] = [:]   // who → notice id of the latest ask
        for item in items {
            guard case .notice(let id, let text, _) = item, let note = PeerNote(text) else { continue }
            switch note.kind {
            case .asked: open[note.who] = id
            case .replied, .noReply: open[note.who] = nil
            }
        }
        return Set(open.values)
    }
}

/// Agents talking to each other, as cards. An ask: who asked whom, the question,
/// and while it's out, what the other agent is doing. A reply: in the other agent's
/// colour, its Markdown, folded when long, with a way into its session.
struct PeerCard: View {
    @Environment(DaemonConnection.self) private var connection
    let note: PeerNote
    /// The agent of this session (the one asking).
    let asker: AgentKind
    let sessionId: String
    var waiting = false
    @State private var expanded = false

    /// The other agent's session, if it's one helping this one.
    private var helper: SessionSummary? {
        connection.sessions.first { $0.agent == note.agent && $0.repoName == note.repo && $0.helperOf == sessionId }
            ?? connection.sessions.first { $0.agent == note.agent && $0.repoName == note.repo && $0.status != .done }
    }

    var body: some View {
        let c = Theme.color(for: note.agent)
        switch note.kind {
        case .asked(let started):
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 6) {
                    AgentAvatar(agent: asker, size: 20)
                    Image(systemName: "arrow.right").font(.caption2.weight(.bold)).foregroundStyle(.secondary)
                    AgentAvatar(agent: note.agent, size: 20)
                    Text(started ? "Started \(note.agent.displayName) and asked" : "Asked \(note.agent.displayName)")
                        .font(.footnote.weight(.semibold))
                    Text(note.repo).font(.footnote).foregroundStyle(.secondary).lineLimit(1)
                }
                Text(ReplyText.inline(note.text))
                    .font(.subheadline).foregroundStyle(.secondary)
                    .lineLimit(expanded ? nil : 3)
                    .onTapGesture { withAnimation(.snappy) { expanded.toggle() } }
                if waiting { status(c) }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
            .background(Color.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(Color.primary.opacity(0.08)))
        case .replied:
            let long = note.text.count > 420
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 8) {
                    AgentAvatar(agent: note.agent, size: 24)
                    Text("\(note.agent.displayName) replied").font(.subheadline.weight(.semibold)).foregroundStyle(c)
                    Text(note.repo).font(.footnote).foregroundStyle(.secondary).lineLimit(1)
                    Spacer(minLength: 4)
                    if let helper { open(helper, c) }
                }
                ReplyText(raw: note.text, selectable: true)
                    .frame(maxHeight: long && !expanded ? 170 : nil, alignment: .top)
                    .clipped()
                    .overlay(alignment: .bottom) {
                        if long && !expanded {
                            LinearGradient(colors: [.clear, Color(.systemBackground).opacity(0.9)],
                                           startPoint: .top, endPoint: .bottom)
                                .frame(height: 50).allowsHitTesting(false)
                        }
                    }
                if long {
                    Button(expanded ? "Show less" : "Show more") { withAnimation(.snappy) { expanded.toggle() } }
                        .font(.footnote.weight(.semibold)).foregroundStyle(c)
                }
            }
            .padding(14)
            .background(c.opacity(0.08), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(c.opacity(0.3)))
        case .noReply:
            Label("No reply from \(note.agent.displayName) in time", systemImage: "clock.badge.exclamationmark")
                .font(.footnote).foregroundStyle(.secondary)
        }
    }

    /// While the other agent works on it: its dots and state, live.
    @ViewBuilder private func status(_ c: Color) -> some View {
        let name = note.agent.displayName
        HStack(spacing: 8) {
            switch helper?.status {
            case .needsApproval:
                Image(systemName: "hand.raised.fill").foregroundStyle(Theme.attention)
                Text("\(name) needs you").foregroundStyle(Theme.attention)
            case .working, nil:
                WorkingDots(color: c)
                Shimmer(text: "\(name) is on it…", color: c)
            default:
                WorkingDots(color: c)
                Text("Waiting for \(name)'s reply").foregroundStyle(.secondary)
            }
            Spacer(minLength: 4)
            if let helper { open(helper, c) }
        }
        .font(.footnote)
    }

    private func open(_ s: SessionSummary, _ c: Color) -> some View {
        NavigationLink(value: s.id) {
            HStack(spacing: 2) {
                Text("Open")
                Image(systemName: "chevron.right").font(.caption2.weight(.bold))
            }
            .font(.footnote.weight(.medium)).foregroundStyle(c)
        }
        .buttonStyle(.plain)
    }
}


/// One question to another agent and its answer.
struct PeerExchange: Identifiable, Equatable {
    /// The reply's notice id.
    let id: String
    let askId: String
    let agent: AgentKind
    let repo: String
    let question: String
    let answer: String

    /// Replies stored before they came whole were cut at 400 characters, ending
    /// in "…". The helper's own conversation still has the reply in full.
    var cutShort: Bool { answer.hasSuffix("…") }

    /// The whole reply, from the helper's conversation, for one that was cut short.
    static func full(_ answer: String, in items: [TimelineItem]) -> String? {
        guard answer.hasSuffix("…") else { return nil }
        let head = String(answer.dropLast()).trimmingCharacters(in: .whitespacesAndNewlines)
        guard head.count >= 20 else { return nil }
        for case let .assistant(_, text, _) in items.reversed() {
            let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if t.count > head.count, t.hasPrefix(head) { return t }
        }
        return nil
    }
}

/// An ask and its reply as one card, where the reply came: the question in the
/// asking agent's bubble, the answer in the helper's. Tap for the whole thread
/// between the two.
struct ExchangeCard: View {
    let exchange: PeerExchange
    let asker: AgentKind
    let sessionId: String
    /// Every exchange in this session (the thread is those with the same agent).
    let thread: [PeerExchange]
    @State private var open = false

    var body: some View {
        let c = Theme.color(for: exchange.agent)
        let mine = Theme.color(for: asker)
        Button { open = true } label: {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 6) {
                    AgentAvatar(agent: asker, size: 20)
                    Image(systemName: "arrow.left.arrow.right").font(.caption2.weight(.bold)).foregroundStyle(.secondary)
                    AgentAvatar(agent: exchange.agent, size: 20)
                    (Text(exchange.agent.displayName).fontWeight(.semibold)
                     + Text(" · \(exchange.repo)").foregroundColor(.secondary))
                        .font(.footnote).lineLimit(1)
                    Spacer(minLength: 4)
                    let n = thread.filter { $0.agent == exchange.agent && $0.repo == exchange.repo }.count
                    if n > 1 { Text("\(n) exchanges").font(.caption).foregroundStyle(.secondary).lineLimit(1).fixedSize() }
                    Image(systemName: "chevron.right").font(.caption2.weight(.bold)).foregroundStyle(.secondary)
                }
                // The question: the asker's, on the right.
                HStack {
                    Spacer(minLength: 48)
                    Text(ReplyText.inline(exchange.question))
                        .font(.subheadline).lineLimit(3)
                        .padding(.horizontal, 12).padding(.vertical, 8)
                        .background(mine.opacity(0.16), in: BubbleShape(mine: true))
                }
                // The answer: the helper's, on the left.
                HStack(alignment: .bottom, spacing: 8) {
                    AgentAvatar(agent: exchange.agent, size: 22)
                    Text(ReplyText.inline(exchange.answer))
                        .font(.subheadline).lineLimit(6)
                        .padding(.horizontal, 12).padding(.vertical, 8)
                        .background(c.opacity(0.14), in: BubbleShape(mine: false))
                    Spacer(minLength: 24)
                }
            }
            .multilineTextAlignment(.leading)
            .padding(12)
            .background(Color.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(Color.primary.opacity(0.08)))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityHint("Shows the whole conversation between the two agents")
        .sheet(isPresented: $open) {
            ExchangeThread(asker: asker, agent: exchange.agent, repo: exchange.repo, sessionId: sessionId,
                           exchanges: thread.filter { $0.agent == exchange.agent && $0.repo == exchange.repo })
        }
    }
}

/// Everything two agents said to each other, as a chat: the asking agent's
/// messages on the right, the helper's on the left, in full and selectable.
struct ExchangeThread: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(DaemonConnection.self) private var connection
    let asker: AgentKind
    let agent: AgentKind
    let repo: String
    let sessionId: String
    let exchanges: [PeerExchange]

    /// The helper's session: the one started for this session, else any of that
    /// agent in that repo.
    private var helper: SessionSummary? {
        let same = connection.sessions.filter { $0.agent == agent && $0.repoName == repo && $0.id != sessionId }
        return same.first { $0.helperOf == sessionId } ?? same.first
    }

    var body: some View {
        let c = Theme.color(for: agent)
        let mine = Theme.color(for: asker)
        let helper = helper
        let helperItems = helper.map { connection.timeline(for: $0.id).items } ?? []
        NavigationStack {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(spacing: 14) {
                        ForEach(exchanges) { x in
                            HStack(alignment: .bottom, spacing: 8) {
                                Spacer(minLength: 40)
                                ReplyText(raw: x.question, selectable: true)
                                    .padding(.horizontal, 14).padding(.vertical, 10)
                                    .background(mine.opacity(0.16), in: BubbleShape(mine: true))
                                AgentAvatar(agent: asker, size: 26)
                            }
                            HStack(alignment: .bottom, spacing: 8) {
                                AgentAvatar(agent: agent, size: 26)
                                VStack(alignment: .leading, spacing: 6) {
                                    let full = PeerExchange.full(x.answer, in: helperItems)
                                    ReplyText(raw: full ?? x.answer, selectable: true)
                                    if x.cutShort, full == nil {
                                        Text(helper == nil
                                             ? "Cut short: this reply came before replies were kept whole."
                                             : "Cut short. Fetching the rest from \(agent.displayName)…")
                                            .font(.caption).foregroundStyle(.secondary)
                                    }
                                }
                                .padding(.horizontal, 14).padding(.vertical, 10)
                                .background(c.opacity(0.14), in: BubbleShape(mine: false))
                                Spacer(minLength: 40)
                            }
                            .id(x.id)
                        }
                    }
                    .padding(16)
                }
                .onAppear { if let last = exchanges.last { proxy.scrollTo(last.id, anchor: .top) } }
            }
            .navigationTitle("\(asker.displayName) and \(agent.displayName)")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { dismiss() } label: { Image(systemName: "xmark") }.accessibilityLabel("Close")
                }
                if let helper {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button {
                            dismiss()
                            // After the sheet has gone, or the push is lost under it.
                            Task {
                                try? await Task.sleep(for: .milliseconds(350))
                                connection.pushedSession = helper.id
                            }
                        } label: {
                            Label("Open \(agent.displayName)", systemImage: "arrow.up.right")
                                .labelStyle(.titleAndIcon)
                        }
                    }
                }
            }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        // For replies cut short: the helper's conversation has them whole.
        .onAppear { if let h = helper, exchanges.contains(where: \.cutShort) { connection.watch(h.id) } }
        .onDisappear { if let h = helper, exchanges.contains(where: \.cutShort) { connection.unwatch(h.id) } }
    }
}

/// A chat bubble: round, with a smaller corner at the bottom on the speaker's side.
struct BubbleShape: Shape {
    let mine: Bool
    func path(in r: CGRect) -> Path {
        let big: CGFloat = 18, small: CGFloat = 6
        return UnevenRoundedRectangle(
            topLeadingRadius: big, bottomLeadingRadius: mine ? big : small,
            bottomTrailingRadius: mine ? small : big, topTrailingRadius: big,
            style: .continuous
        ).path(in: r)
    }
}
