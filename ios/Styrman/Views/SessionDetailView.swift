import SwiftUI

struct SessionDetailView: View {
    @Environment(DaemonConnection.self) private var connection
    let sessionId: String

    var body: some View {
        let timeline = connection.timeline(for: sessionId)
        let summary = connection.session(sessionId)
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    ForEach(timeline.items) { item in
                        TimelineRow(item: item, cwd: summary?.cwd).id(item.id)
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
            .onChange(of: timeline.items.count) {
                withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
            }
        }
        .navigationTitle(summary?.repoName ?? "Session")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if let summary {
                ToolbarItem(placement: .topBarTrailing) { StatusChip(status: summary.status) }
            }
        }
        .safeAreaInset(edge: .bottom) {
            if summary == nil, connection.state == .connected {
                Text("This session is no longer shared.")
                    .font(.footnote).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity).padding(10).background(.bar)
            }
        }
        .onAppear { connection.watch(sessionId) }
        .onDisappear { connection.unwatch(sessionId) }
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
        case .assistant(_, let text, _):
            Text(markdown(text)).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
        case .tool(_, let tool):
            ToolCallRow(item: tool, title: displayTitle(tool.tool, cwd: cwd))
        case .permission(_, let p):
            PermissionCard(item: p, title: displayTitle(p.tool, cwd: cwd))
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
    let item: PermissionItem
    let title: String

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(item.isOpen ? "Permission needed" : "Permission", systemImage: "hand.raised.fill")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(item.isOpen ? Theme.accent : .secondary)
            Text(title).font(item.tool.kind == .execute ? Theme.mono : .body)
            if let reason = item.reason { Text(reason).font(.footnote).foregroundStyle(.secondary) }
            if item.isOpen {
                HStack(spacing: 8) {
                    ForEach(item.options) { opt in
                        Button(opt.label) {}
                            .buttonStyle(.bordered)
                            .tint(opt.kind.isAllow ? .primary : Theme.accent)
                            .disabled(true)
                    }
                }
                Text("Answer in the terminal for now; approving from the phone is coming next.")
                    .font(.caption).foregroundStyle(.secondary)
            } else if let text = item.resolutionText {
                Text(text).font(.footnote).foregroundStyle(.secondary)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 14)
                .fill(item.isOpen ? Theme.accent.opacity(0.08) : Color.clear)
        )
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(item.isOpen ? Theme.accent.opacity(0.6) : Color.secondary.opacity(0.3)))
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
