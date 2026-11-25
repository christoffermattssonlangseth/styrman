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
                        PendingPromptRow(prompt: p) { timeline.dismissUnconfirmed(p.id) }
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
        // The bar takes the agent's colour, so it's always clear who you're talking to.
        .toolbarBackground(Theme.color(for: agent).opacity(0.22), for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .toolbar {
            if let summary {
                ToolbarItem(placement: .principal) {
                    HStack(spacing: 8) {
                        AgentAvatar(agent: summary.agent, size: 26)
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
            VStack(spacing: 0) {
                if !timeline.isLive && (summary != nil || connection.state != .connected) {
                    Label(connection.state == .connected ? "Catching up…" : "Disconnected from Mac",
                          systemImage: connection.state == .connected ? "clock" : "wifi.slash")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity)
                        .padding(8)
                        .background(.bar)
                }
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
            // Runs of tool calls sit close together (the list's spacing is 12).
            ToolCallRow(item: tool, title: displayTitle(tool.tool, cwd: cwd))
                .padding(.vertical, -4)
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
        .padding(.leading, 12)
        // A thin edge in the agent's colour on every reply; solid while it's read aloud.
        .overlay(alignment: .leading) {
            Capsule().fill(tint.opacity(playing ? 1 : 0.45)).frame(width: playing ? 4 : 3).padding(.vertical, 2)
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
            // Compact: one line, small type. Tap for the full command and output.
            HStack(alignment: .center, spacing: 6) {
                Image(systemName: Theme.symbol(for: item.tool.kind))
                    .font(.caption)
                    .foregroundStyle(.secondary).frame(width: 14)
                Text(title)
                    .font(item.tool.kind == .execute
                          ? .system(.caption, design: .monospaced) : .footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 4)
                statusIcon
            }
            .padding(.horizontal, 10).padding(.vertical, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.background.secondary, in: RoundedRectangle(cornerRadius: 8))
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
    @State private var sent: String?
    @State private var showAll = false

    /// "Would you like to run the following command?" says nothing the card doesn't.
    private var reason: String? {
        guard let r = item.reason, !r.hasPrefix("Would you like to") else { return nil }
        return r
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label(item.isOpen ? "Permission needed" : "Permission", systemImage: "hand.raised.fill")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(item.isOpen ? Theme.accent : .secondary)

            // What it's about, compact: commands in a small monospaced box.
            Group {
                if item.tool.kind == .execute {
                    Text(title)
                        .font(.system(.footnote, design: .monospaced))
                        .lineLimit(showAll ? nil : 6)
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color(.tertiarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
                } else {
                    Text(title).font(.callout.weight(.medium)).lineLimit(showAll ? nil : 4)
                }
            }
            .textSelection(.enabled)
            if !showAll, title.count > 240 || title.split(separator: "\n").count > 6 {
                Button("Show all") { showAll = true }.font(.caption).buttonStyle(.borderless)
            }
            if let reason { Text(reason).font(.footnote).foregroundStyle(.secondary) }

            if item.isOpen {
                VStack(spacing: 8) {
                    ForEach(item.options) { opt in option(opt) }
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

    /// One option as a full-width row of the same shape as the others.
    private func option(_ opt: PermissionOption) -> some View {
        let (head, code) = OptionLabel.split(opt.label)
        let deny = !opt.kind.isAllow
        let primary = opt.kind == .allowOnce
        let tint: Color = deny ? .red : Theme.accent
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
            .foregroundStyle(primary ? Color.white : tint)
            .background(primary ? tint : tint.opacity(0.1), in: RoundedRectangle(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(primary ? .clear : tint.opacity(0.45)))
            .contentShape(RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .disabled(sent != nil || connection.state != .connected)
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
                Text(prompt.text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 14).padding(.vertical, 10)
                    .background(Theme.accent.opacity(0.15), in: RoundedRectangle(cornerRadius: 16))
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
