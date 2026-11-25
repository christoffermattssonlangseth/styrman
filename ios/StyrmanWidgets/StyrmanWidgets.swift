// The Live Activity (Lock Screen + Dynamic Island) and the "Agents" widget.

import ActivityKit
import SwiftUI
import WidgetKit

@main
struct StyrmanWidgets: WidgetBundle {
    var body: some Widget {
        AgentsWidget()
        StyrmanLiveActivity()
    }
}

// MARK: - Pieces

private struct AgentDot: View {
    let agent: String
    var size: CGFloat = 22
    var body: some View {
        Image(systemName: AgentStyle.symbol(agent))
            .font(.system(size: size * 0.45, weight: .bold))
            .foregroundStyle(AgentStyle.color(agent))
            .frame(width: size, height: size)
            .background(AgentStyle.color(agent).opacity(0.16), in: Circle())
    }
}

private struct StatusText: View {
    let status: String
    var body: some View {
        let needs = AgentStyle.needsYou(status)
        HStack(spacing: 4) {
            Circle().fill(needs ? Color.orange : Color.red.opacity(0.8)).frame(width: 6, height: 6)
            Text(AgentStyle.label(status))
        }
        .font(.caption2.weight(.semibold))
        .foregroundStyle(needs ? Color.orange : Color.secondary)
    }
}

private struct RowView: View {
    let row: LiveRow
    var body: some View {
        HStack(spacing: 8) {
            AgentDot(agent: row.agent)
            VStack(alignment: .leading, spacing: 0) {
                Text(row.repo).font(.subheadline.weight(.semibold)).lineLimit(1)
                Text(row.agent).font(.caption2).foregroundStyle(.secondary)
            }
            Spacer(minLength: 4)
            StatusText(status: row.status)
        }
    }
}

private func summary(_ s: LiveState) -> String {
    switch (s.needs, s.working) {
    case (0, 0): "All quiet"
    case (0, let w): w == 1 ? "1 agent working" : "\(w) agents working"
    case (let n, 0): n == 1 ? "1 needs you" : "\(n) need you"
    case (let n, let w): "\(n) need\(n == 1 ? "s" : "") you · \(w) working"
    }
}

// MARK: - Live Activity

struct StyrmanLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: StyrmanActivityAttributes.self) { context in
            let s = context.state
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 6) {
                    Image("Wheel").resizable().frame(width: 18, height: 18)
                    Text("Styrman").font(.footnote.weight(.semibold))
                    Spacer()
                    Text(summary(s)).font(.footnote.weight(.medium))
                        .foregroundStyle(s.needs > 0 ? Color.orange : Color.secondary)
                }
                ForEach(s.rows, id: \.self) { RowView(row: $0) }
            }
            .padding(16)
            .activityBackgroundTint(Color(.systemBackground).opacity(0.85))
        } dynamicIsland: { context in
            let s = context.state
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    HStack(spacing: 6) {
                        Image("Wheel").resizable().frame(width: 20, height: 20)
                        Text("Styrman").font(.caption.weight(.semibold))
                    }
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Text(summary(s)).font(.caption.weight(.medium))
                        .foregroundStyle(s.needs > 0 ? Color.orange : Color.secondary)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    VStack(spacing: 8) {
                        ForEach(s.rows, id: \.self) { RowView(row: $0) }
                    }
                    .padding(.top, 4)
                }
            } compactLeading: {
                if let r = s.rows.first {
                    AgentDot(agent: r.agent, size: 20)
                } else {
                    Image("Wheel").resizable().frame(width: 18, height: 18)
                }
            } compactTrailing: {
                if s.needs > 0 {
                    Text("\(s.needs)").font(.caption.weight(.bold)).foregroundStyle(.orange)
                } else {
                    Image(systemName: "circle.fill").font(.system(size: 7)).foregroundStyle(.red)
                        .symbolEffect(.pulse, options: .repeating)
                }
            } minimal: {
                if s.needs > 0 {
                    Text("\(s.needs)").font(.caption.weight(.bold)).foregroundStyle(.orange)
                } else {
                    Image("Wheel").resizable().frame(width: 16, height: 16)
                }
            }
            .keylineTint(s.needs > 0 ? .orange : .red)
        }
    }
}

// MARK: - Home and Lock Screen widget

struct AgentsEntry: TimelineEntry {
    let date: Date
    let snapshot: WidgetSnapshot?
}

struct AgentsProvider: TimelineProvider {
    static let sample = WidgetSnapshot(
        state: LiveState(working: 1, needs: 1, rows: [
            LiveRow(agent: "Claude", repo: "harbor", status: "needs_approval"),
            LiveRow(agent: "agy", repo: "beacon", status: "working"),
        ]),
        updated: .now, host: "Mac")

    func placeholder(in context: Context) -> AgentsEntry { AgentsEntry(date: .now, snapshot: Self.sample) }

    func getSnapshot(in context: Context, completion: @escaping (AgentsEntry) -> Void) {
        completion(AgentsEntry(date: .now, snapshot: context.isPreview ? Self.sample : WidgetSnapshot.load()))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<AgentsEntry>) -> Void) {
        // The app reloads this whenever it sees a change; the policy just keeps "updated …" fresh.
        completion(Timeline(entries: [AgentsEntry(date: .now, snapshot: WidgetSnapshot.load())],
                            policy: .after(.now.addingTimeInterval(15 * 60))))
    }
}

struct AgentsWidgetView: View {
    @Environment(\.widgetFamily) private var family
    let entry: AgentsEntry

    var body: some View {
        let s = entry.snapshot?.state ?? .idle
        switch family {
        case .accessoryRectangular:
            VStack(alignment: .leading, spacing: 2) {
                Text("Styrman").font(.caption2.weight(.semibold))
                Text(summary(s)).font(.headline).widgetAccentable()
                if let r = s.rows.first { Text("\(r.agent) · \(r.repo)").font(.caption2).lineLimit(1) }
            }
        case .accessoryCircular:
            ZStack {
                AccessoryWidgetBackground()
                VStack(spacing: 0) {
                    Text("\(s.needs)").font(.title3.weight(.bold))
                    Text("need you").font(.system(size: 8))
                }
            }
        default:
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 6) {
                    Image("Wheel").resizable().frame(width: 18, height: 18)
                    Text("Styrman").font(.caption.weight(.semibold))
                    Spacer()
                }
                Text(summary(s))
                    .font(.headline)
                    .foregroundStyle(s.needs > 0 ? Color.orange : Color.primary)
                ForEach(s.rows.prefix(family == .systemSmall ? 2 : 3), id: \.self) { r in
                    if family == .systemSmall {
                        HStack(spacing: 6) {
                            AgentDot(agent: r.agent, size: 18)
                            Text(r.repo).font(.caption.weight(.medium)).lineLimit(1)
                        }
                    } else {
                        RowView(row: r)
                    }
                }
                Spacer(minLength: 0)
                if let u = entry.snapshot?.updated {
                    Text("Updated \(u, style: .relative) ago").font(.caption2).foregroundStyle(.tertiary)
                }
            }
            .containerBackground(.fill.tertiary, for: .widget)
        }
    }
}

struct AgentsWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "Agents", provider: AgentsProvider()) { entry in
            AgentsWidgetView(entry: entry)
        }
        .configurationDisplayName("Agents")
        .description("Which agents are working and which need you.")
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryRectangular, .accessoryCircular])
    }
}
