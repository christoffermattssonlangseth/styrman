import SwiftUI

struct SessionListView: View {
    @Environment(DaemonConnection.self) private var connection
    @State private var showSettings = false
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
                section("Needs you", needsYou)
                section("Working", working)
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
                        Text("Type **/styr** in a Claude Code session on \(connection.host) and it shows up here.")
                    }
                }
            }
            .navigationTitle("Styrman")
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { WheelMark(size: 24) }
                ToolbarItem(placement: .topBarTrailing) {
                    Button { showSettings = true } label: { Image(systemName: "gearshape") }
                        .accessibilityLabel("Settings")
                }
            }
            .navigationDestination(for: String.self) { id in SessionDetailView(sessionId: id) }
            .sheet(isPresented: $showSettings) { SettingsView() }
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
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    AgentBadge(agent: session.agent)
                    Text(session.repoName).font(.headline).lineLimit(1)
                }
                if let title = session.title, !title.isEmpty {
                    Text(title).font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
                }
                Text(session.cwd.map(shortPath) ?? "")
                    .font(.caption).foregroundStyle(.tertiary).lineLimit(1).truncationMode(.middle)
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 4) {
                StatusChip(status: session.status)
                Text(session.updated, format: .relative(presentation: .named))
                    .font(.caption2).foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, 2)
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
