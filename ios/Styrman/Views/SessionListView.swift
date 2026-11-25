import AVFoundation
import SwiftUI

struct SessionListView: View {
    @Environment(DaemonConnection.self) private var connection
    @State private var showSettings = false
    @State private var showNewSession = false
    @State private var showUsage = false
    @State private var path: [String] = []
    @State private var showAllDone = false

    private var needsYou: [SessionSummary] { connection.sessions.filter { $0.status.needsUser } }
    private var working: [SessionSummary] { connection.sessions.filter { $0.status == .working || $0.status == .error } }
    private var done: [SessionSummary] { connection.sessions.filter { $0.status == .done || $0.status == .unknown } }

    var body: some View {
        NavigationStack(path: $path) {
            List {
                if connection.state != .connected {
                    Section { ConnectionBanner() }
                }
                if !connection.warnings.isEmpty {
                    Section {
                        ForEach(connection.warnings, id: \.self) { w in
                            Label(w, systemImage: "exclamationmark.triangle").font(.footnote).foregroundStyle(.orange)
                        }
                    }
                }
                // Busy agents first, then the ones waiting for you.
                section("Working", working)
                section("Needs you", needsYou)
                section("Done", showAllDone ? done : Array(done.prefix(5)))
                if done.count > 5 {
                    Button(showAllDone ? "Show fewer" : "Show all \(done.count) finished sessions") {
                        withAnimation { showAllDone.toggle() }
                    }
                    .font(.footnote)
                }
            }
            .listStyle(.insetGrouped)
            .overlay {
                if connection.sessions.isEmpty && connection.state == .connected {
                    ContentUnavailableView {
                        Label { Text("No shared sessions") } icon: { WheelMark(size: 56) }
                    } description: {
                        Text("Type **/styr** in Claude Code or agy on \(connection.host) and the session shows up here.")
                    }
                }
            }
            .navigationTitle("Styrman")
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { WheelMark(size: 24) }
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
            .navigationDestination(for: String.self) { id in SessionDetailView(sessionId: id) }
            .sheet(isPresented: $showUsage) { UsageView() }
            .sheet(isPresented: $showSettings) { SettingsView() }
            .sheet(isPresented: $showNewSession) { NewSessionView() }
            .refreshable { connection.connect() }
            .onChange(of: connection.requestedSession, initial: true) { _, id in
                if let id {
                    path = [id]
                    connection.requestedSession = nil
                }
            }
            .task { await AppDelegate.enablePush() }
            #if DEBUG
            // UI automation (simulator): STYR_OPEN_FIRST=1 opens the first session once it appears.
            .onChange(of: connection.sessions.first?.id) { _, id in
                if let id, path.isEmpty, ProcessInfo.processInfo.environment["STYR_OPEN_FIRST"] == "1" {
                    path = [id]
                }
            }
            // Screenshots (simulator): STYR_OPEN=<part of a session's native id> opens it;
            // STYR_SHOW=new|usage|settings opens that sheet.
            .onChange(of: connection.sessions.count) { _, _ in
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

    @ViewBuilder
    private func section(_ title: String, _ items: [SessionSummary]) -> some View {
        if !items.isEmpty {
            Section(title) {
                ForEach(items) { s in
                    NavigationLink(value: s.id) { SessionRow(session: s) }
                        .swipeActions(edge: .trailing) {
                            Button(s.status == .done ? "Remove" : "Stop sharing", role: .destructive) {
                                connection.stopSharing(s.id)
                            }
                        }
                }
            }
        }
    }
}

struct SessionRow: View {
    let session: SessionSummary

    var body: some View {
        // Three calm lines; nothing important competes for width:
        //   repo                          [status]
        //   Agent · first message              time
        //   ~/folder
        HStack(alignment: .center, spacing: 12) {
            AgentAvatar(agent: session.agent)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(session.repoName)
                        .font(.headline)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 4)
                    StatusChip(status: session.status)
                        .fixedSize()
                }
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(session.agent.displayName)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .fixedSize()
                    if let title = session.title, !title.isEmpty {
                        Text("· \(title)")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 4)
                    Text(session.updated, format: .relative(presentation: .named))
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                        .fixedSize()
                }
                if let cwd = session.cwd, !cwd.isEmpty {
                    Text(shortPath(cwd))
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }
}

func shortPath(_ p: String) -> String {
    if let r = p.range(of: #"^/Users/[^/]+"#, options: .regularExpression) {
        return "~" + p[r.upperBound...]
    }
    return p
}

struct ConnectionBanner: View {
    @Environment(DaemonConnection.self) private var connection

    var body: some View {
        switch connection.state {
        case .connecting:
            Label { Text("Connecting to \(connection.host)…") } icon: { ProgressView() }
        case .offline(let why):
            Label("\(why). Retrying…", systemImage: "wifi.slash").foregroundStyle(.secondary)
        case .unauthorized:
            VStack(alignment: .leading, spacing: 8) {
                Label("This phone is no longer paired with \(connection.host).", systemImage: "lock.slash")
                Button("Pair again") { connection.forget() }
            }
        case .connected, .unpaired:
            EmptyView()
        }
    }
}

struct SettingsView: View {
    @Environment(DaemonConnection.self) private var connection
    @Environment(\.dismiss) private var dismiss
    @State private var confirmForget = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Mac") {
                    LabeledContent("Name", value: connection.host)
                    LabeledContent("Address", value: connection.credentials?.wsURL.host() ?? "—")
                    LabeledContent("Status", value: statusText)
                }
                VoiceSettings()
                Section {
                    Button("Forget this Mac", role: .destructive) { confirmForget = true }
                } footer: {
                    Text("To revoke this phone from the Mac side, run `styr devices revoke`.")
                }
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
