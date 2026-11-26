// The session folder's uncommitted changes, file by file, with comments on lines
// that go back to the agent as one message (like reviewing a pull request).

import SwiftUI

/// A comment on one line of a change.
struct ReviewComment: Identifiable, Hashable {
    let id = UUID()
    let path: String
    let line: FileDiff.Line
    var text: String

    /// "src/upload.rs line 42" (the new file's line; a removed line says so).
    var place: String {
        if let n = line.new { return "`\(path)` line \(n)" }
        if let o = line.old { return "`\(path)` (removed line \(o))" }
        return "`\(path)`"
    }
}

enum ReviewMessage {
    /// All comments as one message for the agent.
    static func compose(_ comments: [ReviewComment]) -> String {
        var out = "Review comments on the uncommitted changes:\n"
        for (i, c) in comments.enumerated() {
            let code = c.line.text.trimmingCharacters(in: .whitespaces)
            out += "\n\(i + 1). \(c.place)"
            if !code.isEmpty { out += ": `\(code.prefix(120))`" }
            out += "\n   \(c.text.replacingOccurrences(of: "\n", with: "\n   "))\n"
        }
        return out
    }
}

struct ChangesSheet: View {
    @Environment(DaemonConnection.self) private var connection
    @Environment(\.dismiss) private var dismiss
    let sessionId: String
    let agent: AgentKind
    let cwd: String?
    @State private var comments: [ReviewComment] = []

    var body: some View {
        NavigationStack {
            Group {
                if let c = connection.changes[sessionId] {
                    if c.files.isEmpty {
                        ContentUnavailableView("No changes", systemImage: "checkmark.circle",
                                               description: Text("Nothing uncommitted in this folder."))
                    } else {
                        List {
                            Section {
                                ForEach(c.files, id: \.path) { f in
                                    NavigationLink {
                                        FileReviewView(file: f, comments: $comments)
                                    } label: {
                                        HStack {
                                            Image(systemName: f.isNew ? "doc.badge.plus" : f.isDeleted ? "trash" : "doc.text")
                                                .foregroundStyle(.secondary)
                                            VStack(alignment: .leading, spacing: 1) {
                                                Text((f.path as NSString).lastPathComponent).font(.body.weight(.medium))
                                                let dir = (f.path as NSString).deletingLastPathComponent
                                                if !dir.isEmpty { Text(dir).font(.caption).foregroundStyle(.secondary) }
                                            }
                                            Spacer()
                                            let n = comments.filter { $0.path == f.path }.count
                                            if n > 0 {
                                                Label("\(n)", systemImage: "text.bubble").font(.caption).foregroundStyle(Theme.color(for: agent))
                                            }
                                            DiffCounts(diffs: [f])
                                        }
                                    }
                                }
                            } header: {
                                Text("\(c.files.count) changed file\(c.files.count == 1 ? "" : "s")" + (c.branch.map { " on \($0)" } ?? ""))
                            } footer: {
                                if c.truncated { Text("Long diff: only the first part is shown.") }
                                else { Text("Tap a line in a file to comment on it. Comments go to the agent together.") }
                            }
                        }
                    }
                } else {
                    ProgressView("Reading changes…")
                }
            }
            .navigationTitle("Changes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
                ToolbarItem(placement: .primaryAction) {
                    Button { connection.getChanges(sessionId) } label: { Image(systemName: "arrow.clockwise") }
                        .accessibilityLabel("Refresh")
                }
            }
            .safeAreaInset(edge: .bottom) {
                if !comments.isEmpty {
                    Button {
                        if connection.sendPrompt(session: sessionId, text: ReviewMessage.compose(comments)) {
                            comments = []
                            dismiss()
                        }
                    } label: {
                        Text("Send \(comments.count) comment\(comments.count == 1 ? "" : "s") to \(agent.displayName)")
                            .font(.headline).frame(maxWidth: .infinity).padding(.vertical, 6)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.color(for: agent))
                    .padding(.horizontal, 16).padding(.vertical, 8)
                    .background(.bar)
                }
            }
        }
        .tint(Theme.color(for: agent))
        .onAppear { connection.getChanges(sessionId) }
    }
}

/// One file's diff; tap a line to comment on it.
struct FileReviewView: View {
    let file: FileDiff
    @Binding var comments: [ReviewComment]
    @State private var editing: EditTarget?

    struct EditTarget: Identifiable {
        let id = UUID()
        let line: FileDiff.Line
        let existing: ReviewComment?
    }

    var body: some View {
        ScrollView([.vertical, .horizontal]) {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(Array(file.lines.enumerated()), id: \.offset) { _, l in
                    row(l)
                    ForEach(comments.filter { $0.path == file.path && $0.line == l }) { c in
                        Button { editing = EditTarget(line: l, existing: c) } label: {
                            Label(c.text, systemImage: "text.bubble.fill")
                                .font(.footnote).multilineTextAlignment(.leading)
                                .padding(8)
                                .frame(maxWidth: 320, alignment: .leading)
                                .background(Color.accentColor.opacity(0.12), in: RoundedRectangle(cornerRadius: 8))
                        }
                        .buttonStyle(.plain)
                        .padding(.leading, 56).padding(.vertical, 4)
                    }
                }
            }
            .padding(.vertical, 8)
        }
        .navigationTitle((file.path as NSString).lastPathComponent)
        .navigationBarTitleDisplayMode(.inline)
        .sheet(item: $editing) { t in
            CommentEditor(place: ReviewComment(path: file.path, line: t.line, text: "").place,
                          code: t.line.text, initial: t.existing?.text ?? "") { text in
                if let e = t.existing, let i = comments.firstIndex(of: e) {
                    if text.isEmpty { comments.remove(at: i) } else { comments[i].text = text }
                } else if !text.isEmpty {
                    comments.append(ReviewComment(path: file.path, line: t.line, text: text))
                }
            }
            .presentationDetents([.medium])
        }
    }

    @ViewBuilder private func row(_ l: FileDiff.Line) -> some View {
        switch l.kind {
        case .gap:
            Text(l.text.isEmpty ? "⋯" : l.text).font(.caption).foregroundStyle(.tertiary)
                .padding(.horizontal, 12).padding(.vertical, 4)
        default:
            Button { editing = EditTarget(line: l, existing: nil) } label: {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(l.new.map(String.init) ?? l.old.map(String.init) ?? "")
                        .foregroundStyle(.tertiary).frame(width: 36, alignment: .trailing)
                    Text(l.kind == .added ? "+" : l.kind == .removed ? "−" : " ")
                        .foregroundStyle(l.kind == .added ? Theme.diffAdded : Theme.diffRemoved)
                    Text(l.text.isEmpty ? " " : l.text)
                        .foregroundStyle(l.kind == .context ? .secondary : .primary)
                        .fixedSize()
                }
                .font(.system(.caption, design: .monospaced))
                .padding(.horizontal, 8).padding(.vertical, 2)
                .frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
                .background(l.kind == .added ? Theme.diffAdded.opacity(0.14)
                            : l.kind == .removed ? Theme.diffRemoved.opacity(0.14) : .clear)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityHint("Comment on this line")
        }
    }
}

private struct CommentEditor: View {
    let place: String
    let code: String
    let initial: String
    let done: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 10) {
                Text(place.replacingOccurrences(of: "`", with: "")).font(.caption).foregroundStyle(.secondary)
                Text(code).font(.system(.caption, design: .monospaced)).lineLimit(3)
                    .padding(8).frame(maxWidth: .infinity, alignment: .leading)
                    .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
                TextField("What should change here?", text: $text, axis: .vertical)
                    .lineLimit(3...8)
                    .focused($focused)
                Spacer()
            }
            .padding()
            .navigationTitle(initial.isEmpty ? "Comment" : "Edit comment")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button(initial.isEmpty ? "Add" : "Save") {
                        done(text.trimmingCharacters(in: .whitespacesAndNewlines))
                        dismiss()
                    }
                    .disabled(initial.isEmpty && text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
            .onAppear { text = initial; focused = true }
        }
    }
}
