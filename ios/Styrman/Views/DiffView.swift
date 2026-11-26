// Colored diffs for file edits: built from what the agent asked to change (Claude's
// Edit / MultiEdit / Write input, Codex's apply_patch), shown in approval cards and
// in a step's detail sheet.

import SwiftUI

struct FileDiff: Hashable, Sendable {
    enum Kind: Hashable, Sendable { case added, removed, context, gap }
    struct Line: Hashable, Sendable {
        let kind: Kind
        let text: String
    }

    var path: String
    var lines: [Line]
    /// A new file (Write, or Codex's "Add File").
    var isNew = false
    var isDeleted = false

    var added: Int { lines.filter { $0.kind == .added }.count }
    var removed: Int { lines.filter { $0.kind == .removed }.count }

    /// The diffs an edit tool's input describes, or nil when it isn't one we can read.
    static func from(_ tool: ToolInfo) -> [FileDiff]? {
        let input = tool.input
        let path = input["file_path"]?.string ?? input["path"]?.string ?? tool.locations?.first ?? "file"
        // Codex: apply_patch carries a patch (as `patch`, `input`, `command`, or the whole input).
        if let patch = input["patch"]?.string ?? input["input"]?.string ?? input["command"]?.string ?? input.string,
           patch.contains("*** Begin Patch") || patch.contains("*** Update File:") || patch.contains("*** Add File:") {
            let diffs = codexPatch(patch)
            return diffs.isEmpty ? nil : diffs
        }
        // Claude: Edit, and MultiEdit's list of edits.
        if let old = input["old_string"]?.string, let new = input["new_string"]?.string {
            return [FileDiff(path: path, lines: lines(old: old, new: new))]
        }
        if case .array(let edits)? = input["edits"] {
            let lines = edits.compactMap { e -> [Line]? in
                guard let old = e["old_string"]?.string, let new = e["new_string"]?.string else { return nil }
                return Self.lines(old: old, new: new)
            }
            guard !lines.isEmpty else { return nil }
            return [FileDiff(path: path, lines: Array(lines.joined(separator: [Line(kind: .gap, text: "")])))]
        }
        // Claude: Write (the whole file, new or replaced).
        if tool.name == "Write", let content = input["content"]?.string {
            return [FileDiff(path: path, lines: split(content).map { Line(kind: .added, text: $0) }, isNew: true)]
        }
        // A unified diff, if an agent hands one over.
        if let d = input["diff"]?.string ?? input["patch"]?.string, d.contains("\n+") || d.contains("\n-") {
            return [FileDiff(path: path, lines: unified(d))]
        }
        return nil
    }

    /// The line-level difference between two snippets: shared lines as context.
    static func lines(old: String, new: String) -> [Line] {
        let a = split(old), b = split(new)
        // Snippets are small; past this, list the old lines then the new ones.
        guard a.count * b.count <= 160_000 else {
            return a.map { Line(kind: .removed, text: $0) } + b.map { Line(kind: .added, text: $0) }
        }
        // Longest common subsequence, then walk it.
        var lcs = Array(repeating: Array(repeating: 0, count: b.count + 1), count: a.count + 1)
        for i in stride(from: a.count - 1, through: 0, by: -1) {
            for j in stride(from: b.count - 1, through: 0, by: -1) {
                lcs[i][j] = a[i] == b[j] ? lcs[i + 1][j + 1] + 1 : max(lcs[i + 1][j], lcs[i][j + 1])
            }
        }
        var out: [Line] = []
        var i = 0, j = 0
        while i < a.count || j < b.count {
            if i < a.count, j < b.count, a[i] == b[j] {
                out.append(Line(kind: .context, text: a[i])); i += 1; j += 1
            } else if i < a.count, j == b.count || lcs[i + 1][j] >= lcs[i][j + 1] {
                out.append(Line(kind: .removed, text: a[i])); i += 1
            } else {
                out.append(Line(kind: .added, text: b[j])); j += 1
            }
        }
        return trimContext(out)
    }

    /// Codex's patch format: "*** Update File: path", then +/-/space lines and "@@" hunks.
    static func codexPatch(_ patch: String) -> [FileDiff] {
        var files: [FileDiff] = []
        var current: FileDiff?
        func flush() { if let c = current { files.append(c) }; current = nil }
        for raw in patch.split(separator: "\n", omittingEmptySubsequences: false) {
            let l = String(raw)
            if let p = l.stripping("*** Update File: ") {
                flush(); current = FileDiff(path: p, lines: [])
            } else if let p = l.stripping("*** Add File: ") {
                flush(); current = FileDiff(path: p, lines: [], isNew: true)
            } else if let p = l.stripping("*** Delete File: ") {
                flush(); files.append(FileDiff(path: p, lines: [], isDeleted: true))
            } else if let p = l.stripping("*** Move to: ") {
                current?.path = p
            } else if l.hasPrefix("*** ") {
                continue // Begin / End Patch, End of File
            } else if l.hasPrefix("@@") {
                if current?.lines.isEmpty == false { current?.lines.append(Line(kind: .gap, text: "")) }
            } else if l.hasPrefix("+") {
                current?.lines.append(Line(kind: .added, text: String(l.dropFirst())))
            } else if l.hasPrefix("-") {
                current?.lines.append(Line(kind: .removed, text: String(l.dropFirst())))
            } else if l.hasPrefix(" ") {
                current?.lines.append(Line(kind: .context, text: String(l.dropFirst())))
            }
        }
        flush()
        return files
    }

    /// A unified diff's body (headers dropped).
    static func unified(_ d: String) -> [Line] {
        d.split(separator: "\n", omittingEmptySubsequences: false).compactMap { raw in
            let l = String(raw)
            if l.hasPrefix("+++") || l.hasPrefix("---") || l.hasPrefix("diff ") || l.hasPrefix("index ") { return nil }
            if l.hasPrefix("@@") { return Line(kind: .gap, text: "") }
            if l.hasPrefix("+") { return Line(kind: .added, text: String(l.dropFirst())) }
            if l.hasPrefix("-") { return Line(kind: .removed, text: String(l.dropFirst())) }
            if l.hasPrefix(" ") { return Line(kind: .context, text: String(l.dropFirst())) }
            return nil
        }
    }

    private static func split(_ s: String) -> [String] {
        var lines = s.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        if lines.last == "" { lines.removeLast() }
        return lines
    }

    /// Long runs of unchanged lines shrink to two lines either side of a change.
    private static func trimContext(_ lines: [Line], keep: Int = 2) -> [Line] {
        let changed = lines.indices.filter { lines[$0].kind != .context }
        guard !changed.isEmpty else { return lines }
        var out: [Line] = []
        var lastKept = -1
        for i in lines.indices {
            let near = changed.contains { abs($0 - i) <= keep }
            if lines[i].kind != .context || near {
                if lastKept >= 0, i - lastKept > 1 { out.append(Line(kind: .gap, text: "")) }
                out.append(lines[i]); lastKept = i
            }
        }
        return out
    }
}

private extension String {
    func stripping(_ prefix: String) -> String? {
        hasPrefix(prefix) ? String(dropFirst(prefix.count)).trimmingCharacters(in: .whitespaces) : nil
    }
}

/// "+3 −1" in green and red.
struct DiffCounts: View {
    let diffs: [FileDiff]
    var body: some View {
        let added = diffs.reduce(0) { $0 + $1.added }
        let removed = diffs.reduce(0) { $0 + $1.removed }
        HStack(spacing: 4) {
            if added > 0 { Text("+\(added)").foregroundStyle(Theme.diffAdded) }
            if removed > 0 { Text("−\(removed)").foregroundStyle(Theme.diffRemoved) }
        }
        .font(.system(.caption2, design: .monospaced).weight(.semibold))
        .accessibilityLabel("\(added) lines added, \(removed) removed")
    }
}

/// The diffs, one block per file. `maxLines` folds long ones behind "Show all".
struct DiffPreview: View {
    let diffs: [FileDiff]
    var cwd: String?
    var maxLines: Int? = nil
    @State private var showAll = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(shown.enumerated()), id: \.offset) { _, pair in file(pair.shown, full: pair.full) }
            if hiddenCount > 0 {
                Button(showAll ? "Show less" : "Show all \(totalLines) lines") {
                    withAnimation(.snappy) { showAll.toggle() }
                }
                .font(.caption).buttonStyle(.borderless)
            }
        }
    }

    private var totalLines: Int { diffs.reduce(0) { $0 + $1.lines.count } }
    private var hiddenCount: Int { maxLines.map { max(0, totalLines - $0) } ?? 0 }

    /// The files, cut down to `maxLines` lines in all unless expanded (with the whole
    /// file alongside, for its counts).
    private var shown: [(shown: FileDiff, full: FileDiff)] {
        guard let max = maxLines, !showAll else { return diffs.map { ($0, $0) } }
        var left = max
        return diffs.compactMap { full in
            guard left > 0 || full.lines.isEmpty else { return nil }
            var f = full
            f.lines = Array(f.lines.prefix(left))
            left -= f.lines.count
            return (f, full)
        }
    }

    private func file(_ f: FileDiff, full: FileDiff) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: f.isNew ? "doc.badge.plus" : f.isDeleted ? "trash" : "doc.text")
                Text(relative(f.path)).lineLimit(1).truncationMode(.head)
                if f.isNew { Text("new").foregroundStyle(Theme.diffAdded) }
                if f.isDeleted { Text("deleted").foregroundStyle(Theme.diffRemoved) }
                Spacer(minLength: 4)
                DiffCounts(diffs: [full])
            }
            .font(.caption.weight(.medium))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 10).padding(.vertical, 6)
            if !f.lines.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(f.lines.enumerated()), id: \.offset) { _, l in line(l) }
                    }
                    .frame(minWidth: 0, alignment: .leading)
                }
            }
        }
        .background(Color(.tertiarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .textSelection(.enabled)
    }

    @ViewBuilder private func line(_ l: FileDiff.Line) -> some View {
        switch l.kind {
        case .gap:
            Text("⋯").font(.caption2).foregroundStyle(.tertiary)
                .padding(.horizontal, 10).padding(.vertical, 1)
        default:
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(l.kind == .added ? "+" : l.kind == .removed ? "−" : " ")
                    .foregroundStyle(l.kind == .added ? Theme.diffAdded : Theme.diffRemoved)
                    .frame(width: 8)
                Text(l.text.isEmpty ? " " : l.text)
                    .foregroundStyle(l.kind == .context ? .secondary : .primary)
                    .fixedSize(horizontal: true, vertical: false)
            }
            .font(.system(.caption, design: .monospaced))
            .padding(.horizontal, 10).padding(.vertical, 1)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(l.kind == .added ? Theme.diffAdded.opacity(0.14)
                        : l.kind == .removed ? Theme.diffRemoved.opacity(0.14) : .clear)
        }
    }

    private func relative(_ p: String) -> String {
        if let cwd, !cwd.isEmpty, p.hasPrefix(cwd.hasSuffix("/") ? cwd : cwd + "/") {
            return String(p.dropFirst(cwd.hasSuffix("/") ? cwd.count : cwd.count + 1))
        }
        return shortPath(p)
    }
}
