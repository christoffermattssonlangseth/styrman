import SwiftUI

/// Start a new session on the Mac: pick an agent and a folder in your home directory.
/// It opens in a new cmux tab, already shared with this phone.
struct NewSessionView: View {
    @Environment(DaemonConnection.self) private var connection
    @Environment(\.dismiss) private var dismiss
    @State private var agent: AgentKind?
    @State private var folder: String?
    @State private var firstMessage = ""
    @State private var browsing: String?

    var body: some View {
        let listing = connection.folders
        NavigationStack {
            Form {
                Section("Agent") {
                    if let agents = listing?.agents, !agents.isEmpty {
                        Picker("Agent", selection: $agent) {
                            ForEach(agents, id: \.self) { a in Text(a.displayName).tag(Optional(a)) }
                        }
                        .pickerStyle(.segmented)
                        .listRowBackground(Color.clear)
                    } else if listing != nil {
                        Text("No agents are set up on the Mac. Run `styr setup` there.")
                            .font(.footnote).foregroundStyle(.secondary)
                    } else {
                        ProgressView()
                    }
                }

                Section("Folder") {
                    if let folder, let listing {
                        Label(listing.display(folder), systemImage: "folder.fill")
                            .foregroundStyle(agent.map { Theme.color(for: $0) } ?? .accentColor)
                    } else {
                        Text("Choose a folder below").foregroundStyle(.secondary)
                    }
                }

                if let listing, !listing.recent.isEmpty, browsing == nil || browsing == listing.home {
                    Section("Recent") {
                        ForEach(listing.recent, id: \.self) { p in
                            folderRow(name: listing.display(p), path: p, repo: true, canOpen: false)
                        }
                    }
                }

                if let listing {
                    Section {
                        if let parent = listing.parent {
                            Button {
                                open(parent)
                            } label: {
                                Label("Up to \(listing.display(parent))", systemImage: "arrow.turn.left.up")
                            }
                        }
                        Button {
                            folder = listing.path
                        } label: {
                            Label("Use \(listing.display(listing.path))", systemImage: folder == listing.path ? "checkmark.circle.fill" : "circle")
                        }
                        ForEach(listing.entries) { e in
                            folderRow(name: e.name, path: e.path, repo: e.isRepo, canOpen: true)
                        }
                    } header: {
                        Text(listing.display(listing.path))
                    }
                }

                Section {
                    TextField("First message (optional)", text: $firstMessage, axis: .vertical)
                        .lineLimit(1...5)
                } footer: {
                    Text("Opens a new cmux tab on your Mac running \(agent?.displayName ?? "the agent") in this folder, shared with this phone. Approvals work as usual.")
                }
            }
            .navigationTitle("New session")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Start") {
                        if let agent, let folder,
                           connection.startSession(agent: agent, cwd: folder, prompt: firstMessage) {
                            dismiss()
                        }
                    }
                    .disabled(agent == nil || folder == nil || connection.state != .connected)
                }
            }
            .onAppear { connection.listFolders(nil) }
            .onChange(of: listing?.agents, initial: true) { _, agents in
                if agent == nil { agent = agents?.first }
            }
        }
    }

    private func open(_ path: String) {
        browsing = path
        connection.listFolders(path)
    }

    @ViewBuilder
    private func folderRow(name: String, path: String, repo: Bool, canOpen: Bool) -> some View {
        HStack {
            Button {
                folder = path
            } label: {
                HStack {
                    Image(systemName: folder == path ? "checkmark.circle.fill" : repo ? "shippingbox" : "folder")
                        .foregroundStyle(folder == path ? (agent.map { Theme.color(for: $0) } ?? .accentColor) : .secondary)
                        .frame(width: 22)
                    Text(name).foregroundStyle(.primary).lineLimit(1).truncationMode(.middle)
                }
            }
            .buttonStyle(.plain)
            Spacer()
            if canOpen {
                Button {
                    open(path)
                } label: {
                    Image(systemName: "chevron.right").foregroundStyle(.tertiary)
                }
                .buttonStyle(.borderless)
                .accessibilityLabel("Open \(name)")
            }
        }
    }
}
