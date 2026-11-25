import SwiftUI

struct SessionDetailView: View {
    @Environment(DaemonConnection.self) private var connection
    let sessionId: String
    @State private var speaker = Speaker.shared
    /// Set once the history has loaded: the newest reply at that point. Only replies
    /// after it are read aloud automatically.
    @State private var baseline: String??

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
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    ForEach(timeline.items) { item in
                        TimelineRow(item: item, cwd: summary?.cwd, sessionId: sessionId, agent: agent).id(item.id)
                    }
                    ForEach(timeline.pending) { p in
                        PendingPromptRow(prompt: p, working: summary?.status == .working, agent: summary?.agent.displayName ?? "the agent")
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
                    if !timeline.isLive {
                        HStack { Spacer(); ProgressView(); Spacer() }.padding()
                    }
                    Color.clear.frame(height: 1).id("bottom")
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
            }
            .defaultScrollAnchor(.bottom)
            // Like the Claude app: drag the conversation down to pull the keyboard away,
            // or tap the conversation to dismiss it.
            .scrollDismissesKeyboard(.interactively)
            .simultaneousGesture(TapGesture().onEnded {
                UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
            })
            .onChange(of: timeline.items.count + timeline.pending.count) {
                withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
            }
        }
        .navigationTitle(summary?.repoName ?? "Session")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if let summary {
                ToolbarItem(placement: .principal) {
                    HStack(spacing: 6) {
                        AgentBadge(agent: summary.agent)
                        Text(summary.repoName).font(.headline).lineLimit(1)
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    HStack(spacing: 10) {
                        Button {
                            speaker.autoRead.toggle()
                            if !speaker.autoRead { speaker.stop() }
                        } label: {
                            Image(systemName: speaker.autoRead ? "speaker.wave.2.fill" : "speaker.slash")
                                .foregroundStyle(speaker.autoRead ? Theme.color(for: summary.agent) : .secondary)
                        }
                        .accessibilityLabel(speaker.autoRead ? "Stop reading replies aloud" : "Read replies aloud")
                        StatusChip(status: summary.status)
                    }
                }
            }
        }
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
            if summary == nil, connection.state == .connected {
                Text("This session is no longer shared.")
                    .font(.footnote).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity).padding(10).background(.bar)
            } else if let summary {
                if summary.acceptsPrompts {
                    Composer(sessionId: sessionId, status: summary.status, agent: summary.agent.displayName,
                             agentColor: Theme.color(for: summary.agent))
                        .onChange(of: speaker.autoRead) { _, on in
                            // Turning it on reads the latest reply, so you hear where things are.
                            if on, let r = latestReply(timeline) { speaker.speak(r.text, id: r.id, agent: summary.agent) }
                        }
                } else {
                    Label {
                        Text("Read-only: this \(summary.agent.displayName) session wasn't started through Styrman. Run `styr setup` on the Mac, then start it again in a new terminal tab to approve and send messages from here.")
                    } icon: { Image(systemName: "eye") }
                    .font(.footnote).foregroundStyle(.secondary)
                    .padding(12).frame(maxWidth: .infinity, alignment: .leading).background(.bar)
                }
            }
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
}

/// How a tool call reads in the timeline: the bare command for shell tools, paths
/// relative to the session's folder for everything else.
func displayTitle(_ tool: ToolInfo, cwd: String?) -> String {
    if tool.kind == .execute, let cmd = tool.input["command"]?.string {
        return cmd
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

    var body: some View {
        switch item {
        case .divider(_, let text, let at):
            HStack(spacing: 8) {
                line
                Text("\(text) · \(at.formatted(date: .omitted, time: .shortened))")
                    .font(.caption).foregroundStyle(.secondary).fixedSize()
                line
            }
            .padding(.vertical, 4)
        case .user(_, let text, let fromPhone, _):
            HStack {
                Spacer(minLength: 40)
                VStack(alignment: .trailing, spacing: 4) {
                    Text(text).textSelection(.enabled)
                        .padding(.horizontal, 14).padding(.vertical, 10)
                        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 16))
                    if fromPhone {
                        Label("From this phone", systemImage: "iphone").font(.caption2).foregroundStyle(.secondary)
                    }
                }
            }
        case .assistant(let id, let text, _):
            ReplyView(id: id, text: markdown(text), raw: text, agent: agent)
        case .tool(_, let tool):
            ToolCallRow(item: tool, title: displayTitle(tool.tool, cwd: cwd))
        case .permission(_, let p):
            PermissionCard(item: p, title: displayTitle(p.tool, cwd: cwd), sessionId: sessionId)
        case .plan(_, let entries):
            PlanCard(entries: entries)
        case .notice(_, let text, let warning):
            Label(text, systemImage: warning ? "exclamationmark.triangle" : "bell")
                .font(.footnote).foregroundStyle(warning ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
        }
    }

    private var line: some View { Rectangle().fill(.quaternary).frame(height: 1) }

    private func markdown(_ s: String) -> AttributedString {
        (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
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
    @State private var speaker = Speaker.shared
    @State private var copied = false

    var body: some View {
        let playing = speaker.speakingId == id
        let tint = Theme.color(for: agent)
        VStack(alignment: .leading, spacing: 6) {
            Text(text).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
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
        .padding(.leading, playing ? 10 : 0)
        .overlay(alignment: .leading) {
            if playing {
                Capsule().fill(tint).frame(width: 3).padding(.vertical, 2)
            }
        }
        .animation(.easeOut(duration: 0.2), value: playing)
        .sensoryFeedback(.selection, trigger: playing)
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
    @State private var showDetail = false

    var body: some View {
        Button {
            showDetail = true
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: Theme.symbol(for: item.tool.kind))
                    .foregroundStyle(.secondary).frame(width: 18)
                Text(title)
                    .font(item.tool.kind == .execute ? Theme.mono : .callout)
                    .lineLimit(2)
                    .multilineTextAlignment(.leading)
                Spacer(minLength: 6)
                statusIcon
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.background.secondary, in: RoundedRectangle(cornerRadius: 12))
            .contentShape(RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .accessibilityHint("Shows the full command and its output")
        // Output opens in a sheet: expanding rows inline inside a bottom-anchored
        // LazyVStack made SwiftUI lose its scroll position (blank screen).
        .sheet(isPresented: $showDetail) { ToolDetailView(item: item, title: title) }
    }

    @ViewBuilder private var statusIcon: some View {
        switch item.status {
        case .pending, .running: ProgressView().controlSize(.small)
        case .completed: Image(systemName: "checkmark").foregroundStyle(.green).font(.caption.bold())
        case .failed: Image(systemName: "xmark").foregroundStyle(.red).font(.caption.bold())
        }
    }
}

struct PermissionCard: View {
    @Environment(DaemonConnection.self) private var connection
    let item: PermissionItem
    let title: String
    let sessionId: String
    @State private var sent: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(item.isOpen ? "Permission needed" : "Permission", systemImage: "hand.raised.fill")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(item.isOpen ? Theme.accent : .secondary)
            Text(title).font(item.tool.kind == .execute ? Theme.mono : .body).textSelection(.enabled)
            if let reason = item.reason { Text(reason).font(.footnote).foregroundStyle(.secondary) }
            if item.isOpen {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) { buttons }
                    VStack(alignment: .leading, spacing: 8) { buttons }
                }
                countdown
            } else if let text = item.resolutionText {
                Text(text).font(.footnote).foregroundStyle(.secondary)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 14).fill(item.isOpen ? Theme.accent.opacity(0.08) : Color.clear))
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(item.isOpen ? Theme.accent.opacity(0.6) : Color.secondary.opacity(0.3)))
        .sensoryFeedback(.success, trigger: item.isOpen) { wasOpen, isOpen in wasOpen && !isOpen && sent != nil }
    }

    @ViewBuilder private var buttons: some View {
        ForEach(item.options) { opt in
            Button {
                if connection.resolvePermission(session: sessionId, request: item.request, option: opt.id) {
                    sent = opt.id
                }
            } label: {
                HStack(spacing: 6) {
                    if sent == opt.id { ProgressView().controlSize(.mini) }
                    Text(opt.label)
                }
                .frame(minHeight: 30)
            }
            .buttonStyle(.borderedProminent)
            .tint(opt.kind.isAllow ? (opt.kind == .allowOnce ? Theme.ink.opacity(0.85) : .gray) : Theme.accent)
            .disabled(sent != nil || connection.state != .connected)
        }
    }

    @ViewBuilder private var countdown: some View {
        if let expires = item.expiresAt {
            TimelineView(.periodic(from: .now, by: 1)) { ctx in
                let left = Int(expires.timeIntervalSince(ctx.date).rounded(.up))
                Text(left > 0 ? "Answer here or in the terminal" : "Answer in the terminal")
                    .font(.caption).foregroundStyle(.secondary).monospacedDigit()
            }
        }
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
    let agentColor: Color
    @State private var text = ""
    @FocusState private var focused: Bool
    @State private var dictation = Dictation()
    /// What was in the box when dictation started; the transcript is added to it.
    @State private var beforeDictation = ""

    private var canSend: Bool {
        connection.state == .connected && status != .done
            && !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if status == .working {
                Text("\(agent) is working. Your message is sent when it finishes.")
                    .font(.caption).foregroundStyle(.secondary)
            } else if status == .done {
                Text("This session has ended.").font(.caption).foregroundStyle(.secondary)
            }
            HStack(alignment: .bottom, spacing: 8) {
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
                .tint(dictation.isListening ? agentColor : .secondary)
                .disabled(status == .done)
                .accessibilityLabel(dictation.isListening ? "Stop dictation" : "Dictate")
                Button {
                    dictation.stop()
                    if connection.sendPrompt(session: sessionId, text: text) { text = "" }
                } label: {
                    Image(systemName: "arrow.up.circle.fill").font(.system(size: 34))
                }
                .tint(agentColor)
                .disabled(!canSend)
                .accessibilityLabel("Send")
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
    }
}

struct PendingPromptRow: View {
    let prompt: SessionTimeline.PendingPrompt
    let working: Bool
    let agent: String

    private var pendingLabel: String {
        if prompt.delivered {
            return working ? "Delivered; \(agent) picks it up after the current step" : "Delivered"
        }
        return prompt.queued || working ? "Queued, sent when \(agent) finishes" : "Sending…"
    }

    var body: some View {
        HStack {
            Spacer(minLength: 40)
            VStack(alignment: .trailing, spacing: 4) {
                Text(prompt.text)
                    .padding(.horizontal, 14).padding(.vertical, 10)
                    .background(Theme.accent.opacity(0.15), in: RoundedRectangle(cornerRadius: 16))
                Label(pendingLabel, systemImage: prompt.delivered ? "checkmark.circle" : prompt.queued || working ? "clock" : "arrow.up.circle")
                    .font(.caption2).foregroundStyle(.secondary)
            }
        }
    }
}
