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
        .navigationTitle(summary?.repoName ?? "Session")
        .navigationBarTitleDisplayMode(.inline)
        // The bar takes the agent's colour, so it's always clear who you're talking to.
        .toolbarBackground(Theme.color(for: agent).opacity(0.07), for: .navigationBar)
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
            // Only your own scrolling stops following: new content arriving (a reply,
            // a tool card) can make the view briefly look scrolled up, and must not.
            .onScrollGeometryChange(for: Bool.self) { g in
                g.contentOffset.y + g.containerSize.height >= g.contentSize.height - 120
            } action: { _, near in
                if near || scrollPhase != .idle { atBottom = near }
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
                        withAnimation(.easeOut(duration: 0.25)) { proxy.scrollTo("bottom", anchor: .bottom) }
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
                withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            // Land on the latest message when it opens and when the history arrives.
            .onChange(of: timeline.loaded, initial: true) { _, loaded in
                if loaded { Task { @MainActor in proxy.scrollTo("bottom", anchor: .bottom) } }
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
                let rows = DisplayRow.rows(merged(timeline.shownItems))
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
                                    latestReplyId: latest).id(item.id)
                    case .tools(let id, let tools):
                        ToolRunCard(tools: tools, cwd: summary?.cwd).id(id)
                    }
                }
                ForEach(timeline.pending) { p in
                    PendingPromptRow(prompt: p) { timeline.dismissUnconfirmed(p.id) }
                }
                if summary?.status == .working, timeline.pending.isEmpty || timeline.preview != nil {
                    if let text = timeline.preview {
                        LivePreviewRow(text: text, agent: agent, sessionId: sessionId).id("preview")
                    } else {
                        WorkingRow(agent: agent).id("preview")
                    }
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
                    AgentAvatar(agent: summary.agent, size: 28)
                    VStack(alignment: .leading, spacing: 0) {
                        Text(summary.repoName).font(.headline).lineLimit(1)
                        HStack(spacing: 4) {
                            Text(summary.agent.displayName).foregroundStyle(Theme.color(for: summary.agent))
                            Text("·").foregroundStyle(.tertiary)
                            Text(Theme.label(for: summary.status))
                                .foregroundStyle(summary.status.needsUser ? AnyShapeStyle(Theme.attention) : AnyShapeStyle(.secondary))
                                .contentTransition(.opacity)
                            if summary.status == .working {
                                Image(systemName: "circle.fill").font(.system(size: 5)).foregroundStyle(Theme.accent)
                                    .symbolEffect(.pulse, options: .repeating)
                            }
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
                    .background(.bar)
            }
            if summary == nil, connection.state == .connected {
                Text("This session is no longer shared.")
                    .font(.footnote).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity).padding(10).background(.bar)
            } else if let summary {
                HelperLinks(summary: summary)
                if let p = openPermission(timeline) {
                    PermissionCard(item: p, title: displayTitle(p.tool, cwd: summary.cwd), sessionId: sessionId, cwd: summary.cwd, pinned: true)
                        .padding(.horizontal, 12).padding(.top, 10).padding(.bottom, 4)
                        .frame(maxWidth: .infinity)
                        .background(.bar)
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
                    .frame(maxWidth: .infinity, alignment: .leading).background(.bar)
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
                Spacer(minLength: 40)
                VStack(alignment: .trailing, spacing: 4) {
                    if let files = parts?.files, !files.isEmpty { AttachedFilesRow(files: files) }
                    if !text.isEmpty {
                    Text(text).textSelection(.enabled)
                        .padding(.horizontal, 14).padding(.vertical, 10)
                        .background(Color(.secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                        .shadow(color: .black.opacity(0.05), radius: 4, y: 1)
                    }
                    if fromPhone {
                        Label("From this phone", systemImage: "iphone").font(.caption2).foregroundStyle(.secondary)
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
        case .notice(_, let text, let warning):
            Label(text, systemImage: warning ? "exclamationmark.triangle" : "bell")
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
    /// The reply in a sheet where any part of it can be selected.
    @State private var selecting = false

    var body: some View {
        let playing = speaker.speakingId == id
        let tint = Theme.color(for: agent)
        VStack(alignment: .leading, spacing: 6) {
            ReplyText(raw: raw).textSelection(.enabled)
            // Actions under the latest reply only (and the one being read); long-press any reply for them.
            if isLatest || playing {
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
                ActionIcon(symbol: "text.cursor", label: "Select text", tint: .secondary) {
                    selecting = true
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
        }
        .padding(.leading, playing ? 12 : 0)
        // An edge in the agent's colour while it's read aloud.
        .overlay(alignment: .leading) {
            if playing { Capsule().fill(tint).frame(width: 4).padding(.vertical, 2) }
        }
        .contextMenu {
            Button { UIPasteboard.general.string = raw } label: { Label("Copy", systemImage: "doc.on.doc") }
            Button { selecting = true } label: { Label("Select text", systemImage: "text.cursor") }
            Button { speaker.toggle(raw, id: id, agent: agent) } label: {
                Label(playing ? "Stop reading" : "Read aloud", systemImage: playing ? "stop.fill" : "speaker.wave.2")
            }
        }
        .animation(.easeOut(duration: 0.2), value: playing)
        // A tap when it starts; none when it ends (a lone haptic tick sounds like a click).
        .sensoryFeedback(.selection, trigger: playing) { _, now in now }
        .sheet(isPresented: $selecting) { SelectTextSheet(text: text, raw: raw, agent: agent) }
    }
}

/// A reply where any part can be selected and copied (SwiftUI's Text only copies
/// all of it on iOS).
struct SelectTextSheet: View {
    let text: AttributedString
    let raw: String
    let agent: AgentKind
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            SelectableText(text: text)
                .navigationTitle("Select text")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
                    ToolbarItem(placement: .topBarLeading) {
                        Button("Copy all") {
                            UIPasteboard.general.string = raw
                            dismiss()
                        }
                    }
                }
        }
        .tint(Theme.color(for: agent))
        .presentationDragIndicator(.visible)
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
        let body = UIFont.preferredFont(forTextStyle: .body)
        let out = NSMutableAttributedString()
        for run in text.runs {
            let piece = String(text[run.range].characters)
            var font = body
            let intent = run.inlinePresentationIntent ?? []
            if intent.contains(.code) {
                font = UIFont.monospacedSystemFont(ofSize: body.pointSize * 0.92, weight: .regular)
            } else {
                var traits: UIFontDescriptor.SymbolicTraits = []
                if intent.contains(.stronglyEmphasized) { traits.insert(.traitBold) }
                if intent.contains(.emphasized) { traits.insert(.traitItalic) }
                if !traits.isEmpty, let d = body.fontDescriptor.withSymbolicTraits(traits) {
                    font = UIFont(descriptor: d, size: 0)
                }
            }
            var attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: UIColor.label]
            if intent.contains(.code) { attrs[.backgroundColor] = UIColor.secondarySystemFill }
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
            .background(bare ? AnyShapeStyle(Color.clear) : AnyShapeStyle(Color(.secondarySystemGroupedBackground)),
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
                Label(item.isOpen ? "Permission needed" : "Permission", systemImage: "hand.raised.fill")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(item.isOpen ? Theme.attention : .secondary)
                Spacer()
                if pinned {
                    Text("or answer in the terminal").font(.caption2).foregroundStyle(.tertiary)
                }
            }

            // What it's about, compact: commands in a small monospaced box, edits as a diff.
            if let diffs = connection.timeline(for: sessionId).diffs(for: item) {
                DiffPreview(diffs: diffs, cwd: cwd, maxLines: pinned ? 8 : 24)
            } else {
            Group {
                if item.tool.kind == .execute {
                    Text(title)
                        .font(.system(.footnote, design: .monospaced))
                        .lineLimit(showAll ? nil : pinned ? 3 : 6)
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color(.tertiarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
                } else {
                    Text(title).font(.callout.weight(.medium)).lineLimit(showAll ? nil : 4)
                }
            }
            .textSelection(.enabled)
            if !showAll, title.count > (pinned ? 120 : 240) || title.split(separator: "\n").count > (pinned ? 3 : 6) {
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
        .background(RoundedRectangle(cornerRadius: 14).fill(item.isOpen ? Theme.attention.opacity(0.08) : Color.clear))
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(item.isOpen ? Theme.attention.opacity(0.6) : Color.secondary.opacity(0.3)))
        .sensoryFeedback(.success, trigger: item.isOpen) { wasOpen, isOpen in wasOpen && !isOpen && sent != nil }
        .sensoryFeedback(.impact(weight: .medium), trigger: sent) { _, new in new != nil }
    }

    /// One option as a full-width row of the same shape as the others.
    private func option(_ opt: PermissionOption) -> some View {
        let (head, code) = OptionLabel.split(opt.label)
        let deny = !opt.kind.isAllow
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
                    if item.isOpen, sent == opt.id { sent = nil }
                }
            }
        } label: {
            HStack(alignment: .center, spacing: 10) {
                Group {
                    if sent == opt.id {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: deny ? "xmark" : opt.kind == .allowAlways ? "checkmark.seal" : "checkmark")
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
            .foregroundStyle(primary ? Theme.onSolid : deny ? Color.red : Color.primary)
            .background(primary ? AnyShapeStyle(Theme.solid) : AnyShapeStyle(deny ? Color.clear : Color(.secondarySystemFill)),
                        in: RoundedRectangle(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(deny ? Color.red.opacity(0.55) : .clear))
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

struct ToolDetailView: View {
    let item: ToolItem
    let title: String
    @Environment(\.dismiss) private var dismiss

    private var outputText: String {
        guard let output = item.output else {
            return item.status == .completed || item.status == .failed ? "(no output)" : "Still running…"
        }
        let text = output.displayText
        return text.isEmpty ? "(no output)" : text
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text(title)
                        .font(item.tool.kind == .execute ? Theme.mono : .body)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(12)
                        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
                    if item.tool.kind == .edit, let diffs = FileDiff.from(item.tool) {
                        Text("Changes").font(.subheadline.weight(.semibold)).foregroundStyle(.secondary)
                        DiffPreview(diffs: diffs)
                    }
                    Text("Output").font(.subheadline.weight(.semibold)).foregroundStyle(.secondary)
                    ScrollView(.horizontal) {
                        Text(outputText)
                            .font(.system(.footnote, design: .monospaced))
                            .textSelection(.enabled)
                            .fixedSize(horizontal: true, vertical: false)
                            .padding(12)
                    }
                    .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
                }
                .padding(16)
            }
            .navigationTitle(item.tool.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .topBarLeading) {
                    Button { UIPasteboard.general.string = outputText } label: { Image(systemName: "doc.on.doc") }
                        .accessibilityLabel("Copy output")
                }
            }
        }
        .presentationDetents([.medium, .large])
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
            if status == .working {
                Text(canStop ? "\(agent) is working. A message goes when it finishes; ■ stops it."
                             : "\(agent) is working. Your message is sent when it finishes.")
                    .font(.caption).foregroundStyle(.secondary)
            } else if status == .done {
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
            HStack(alignment: .bottom, spacing: 8) {
                Menu {
                    // A PhotosPicker inside a Menu often never opens (the menu closes
                    // first): open it from outside instead.
                    Button { pickingPhotos = true } label: { Label("Photos", systemImage: "photo.on.rectangle") }
                    Button { pickingFiles = true } label: { Label("Files", systemImage: "folder") }
                } label: {
                    Image(systemName: "paperclip.circle")
                        .font(.system(size: 34))
                }
                .tint(.secondary)
                .disabled(status == .done || sending)
                .accessibilityLabel("Attach images or files")
                TextField("Message \(agent)", text: $text, axis: .vertical)
                    .lineLimit(1...6)
                    .focused($focused)
                    .padding(.horizontal, 14).padding(.vertical, 10)
                    .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 20))
                    .disabled(status == .done)
                Button {
                    if !dictation.isListening {
                        beforeDictation = text.isEmpty || text.hasSuffix(" ") ? text : text + " "
                        focused = false
                    }
                    Task { await dictation.toggle() }
                } label: {
                    Image(systemName: dictation.isListening ? "stop.circle.fill" : "mic.circle")
                        .font(.system(size: 34))
                        .symbolEffect(.pulse, isActive: dictation.isListening)
                }
                .tint(dictation.isListening ? Theme.accent : .secondary)
                .disabled(status == .done)
                .accessibilityLabel(dictation.isListening ? "Stop dictation" : "Dictate")
                if showsStop {
                    // Nothing typed while it works: stop its turn instead (Esc on the Mac).
                    Button { connection.interrupt(sessionId) } label: {
                        Image(systemName: "stop.circle.fill").font(.system(size: 34))
                    }
                    .tint(.primary)
                    .accessibilityLabel("Stop \(agent)")
                    .sensoryFeedback(.impact(weight: .medium), trigger: status)
                } else {
                    Button(action: send) {
                        if sending {
                            ProgressView().frame(width: 34, height: 34)
                        } else {
                            Image(systemName: "arrow.up.circle.fill").font(.system(size: 34))
                        }
                    }
                    .tint(Theme.accent)
                    .disabled(!canSend)
                    .accessibilityLabel("Send")
                }
            }
        }
        .padding(.horizontal, 12).padding(.top, 8).padding(.bottom, 6)
        .background(.bar)
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
                    attachments.append(Attachment(name: "photo-\(attachments.count + 1).jpg", data: jpeg, isImage: true))
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
                attachments.append(Attachment(name: url.lastPathComponent, data: data, isImage: image))
            }
        }
    }
}

extension Composer {
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
        .background(Color(.secondarySystemGroupedBackground), in: Capsule())
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
            ForEach(files, id: \.self) { f in
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
            Spacer(minLength: 40)
            VStack(alignment: .trailing, spacing: 4) {
                if let files = AttachedFiles.split(prompt.text)?.files, !files.isEmpty { AttachedFilesRow(files: files) }
                let shown = AttachedFiles.split(prompt.text)?.text ?? prompt.text
                if !shown.isEmpty {
                    Text(shown)
                        .textSelection(.enabled)
                        .padding(.horizontal, 14).padding(.vertical, 10)
                        .background(Theme.accent.opacity(0.15), in: RoundedRectangle(cornerRadius: 16))
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
            Text(message.body).textSelection(.enabled)
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
            if run.count >= 2 {
                out.append(.tools(id: run[0].0, run.map(\.1)))
            } else if let (id, t) = run.first {
                out.append(.item(.tool(id: id, t)))
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

/// Several tool calls in a row, as one card: the latest few, the rest a tap away.
struct ToolRunCard: View {
    let tools: [ToolItem]
    let cwd: String?
    @State private var expanded = false
    private let visible = 3

    var body: some View {
        let hidden = expanded ? 0 : max(0, tools.count - visible)
        VStack(alignment: .leading, spacing: 0) {
            Button {
                withAnimation(.snappy) { expanded.toggle() }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "square.stack.3d.up").font(.caption2)
                    Text("\(tools.count) steps").font(.caption.weight(.semibold))
                    if tools.contains(where: { $0.status == .pending || $0.status == .running }) {
                        ProgressView().controlSize(.mini)
                    }
                    Spacer()
                    if tools.count > visible {
                        Text(expanded ? "Show fewer" : "\(hidden) more").font(.caption2)
                        Image(systemName: "chevron.down").font(.caption2.weight(.semibold))
                            .rotationEffect(.degrees(expanded ? 180 : 0))
                    }
                }
                .foregroundStyle(.secondary)
                .padding(.horizontal, 10).padding(.top, 8).padding(.bottom, 2)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(tools.count <= visible)
            ForEach(Array(tools.enumerated().dropFirst(hidden)), id: \.offset) { _, t in
                ToolCallRow(item: t, title: displayTitle(t.tool, cwd: cwd), bare: true)
                    .transition(.opacity)
            }
        }
        .padding(.bottom, 3)
        .background(Color(.secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Color.primary.opacity(0.05)))
    }
}

/// The grouped background with a faint wash of the agent's colour at the top.
struct SessionBackground: View {
    let agent: AgentKind
    var body: some View {
        ZStack(alignment: .top) {
            Color(.systemGroupedBackground)
            LinearGradient(colors: [Theme.color(for: agent).opacity(0.08), .clear], startPoint: .top, endPoint: .bottom)
                .frame(height: 280)
        }
        .ignoresSafeArea()
    }
}

/// The reply the agent is writing right now, as its terminal shows it: plain text
/// that grows, with a blinking cursor. The real message replaces it when it's done.
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
            }
        }
        .foregroundStyle(.primary.opacity(0.85))
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

/// While the agent works and nothing it writes shows yet: one quiet line, like the
/// Claude app's, in place of a preview.
struct WorkingRow: View {
    let agent: AgentKind

    var body: some View {
        TimelineView(.periodic(from: .now, by: 0.4)) { ctx in
            let n = Int(ctx.date.timeIntervalSinceReferenceDate / 0.4) % 3
            HStack(spacing: 8) {
                HStack(spacing: 3) {
                    ForEach(0..<3) { i in
                        Circle().frame(width: 4, height: 4).opacity(i == n ? 1 : 0.3)
                    }
                }
                Text("Working…")
            }
            .foregroundStyle(Theme.color(for: agent))
            .font(.subheadline)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityLabel("\(agent.displayName) is working")
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
            .background(.bar)
        }
    }

    private func chip(_ s: SessionSummary, _ title: String, _ symbol: String) -> some View {
        NavigationLink(value: s.id) {
            HStack(spacing: 6) {
                Image(systemName: symbol)
                Text(title).lineLimit(1)
                Circle().fill(Theme.color(for: s.agent)).frame(width: 6, height: 6)
                    .opacity(s.status == .working ? 1 : 0)
            }
            .font(.footnote.weight(.medium))
            .padding(.horizontal, 10).padding(.vertical, 6)
            .background(Theme.color(for: s.agent).opacity(0.12), in: Capsule())
        }
        .buttonStyle(.plain)
    }
}
