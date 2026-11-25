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
    @State private var newFolderName = ""
    @State private var askNewFolder = false
    @State private var showFiles = false
    /// Select the folder that the next listing is about (just created).
    @State private var selectNext = false

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
                        Button {
                            newFolderName = ""
                            askNewFolder = true
                        } label: {
                            Label("New folder…", systemImage: "folder.badge.plus")
                        }
                    } header: {
                        HStack(spacing: 6) {
                            Text(listing.display(listing.path))
                            if let branch = listing.gitBranch {
                                Label(branch, systemImage: "arrow.triangle.branch")
                                    .labelStyle(.titleAndIcon)
                                    .font(.caption2.weight(.semibold))
                                    .padding(.horizontal, 6).padding(.vertical, 2)
                                    .background(.quaternary, in: Capsule())
                            }
                        }
                    }

                    // What's in this folder, to check it's the right one.
                    if !listing.files.isEmpty {
                        Section {
                            DisclosureGroup(isExpanded: $showFiles) {
                                ForEach(listing.files, id: \.self) { f in
                                    Label(f, systemImage: "doc")
                                        .font(.footnote).foregroundStyle(.secondary).lineLimit(1)
                                }
                                if listing.moreFiles {
                                    Text("…and more").font(.footnote).foregroundStyle(.tertiary)
                                }
                            } label: {
                                Text("\(listing.files.count)\(listing.moreFiles ? "+" : "") files in \(listing.display(listing.path))")
                                    .font(.footnote)
                            }
                        }
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
            .onChange(of: listing?.path) { _, path in
                showFiles = false
                if selectNext, let path { folder = path; selectNext = false }
            }
            .alert("New folder", isPresented: $askNewFolder) {
                TextField("Name", text: $newFolderName)
                    .textInputAutocapitalization(.never)
                Button("Create") {
                    if let parent = listing?.path, !newFolderName.trimmingCharacters(in: .whitespaces).isEmpty {
                        selectNext = true
                        connection.createFolder(in: parent, named: newFolderName)
                    }
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Inside \(listing.map { $0.display($0.path) } ?? "~")")
            }
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
