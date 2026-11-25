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

/// The app's amber: bright on dark, deeper on light.
private let amber = Color(UIColor { $0.userInterfaceStyle == .dark ? UIColor(red: 1, green: 0.81, blue: 0.51, alpha: 1)
                                                                 : UIColor(red: 0.66, green: 0.39, blue: 0, alpha: 1) })

/// A solid circle in the agent's colour with its glyph in white.
private struct AgentDot: View {
    let agent: String
    var size: CGFloat = 20
    var body: some View {
        Image(systemName: AgentStyle.symbol(agent))
            .font(.system(size: size * 0.48, weight: .bold))
            .foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(AgentStyle.color(agent), in: Circle())
    }
}

/// "Approve", "Your turn", "Working": what each row wants, in a word.
private func shortStatus(_ status: String) -> String {
    switch status {
    case "needs_approval": "Approve"
    case "waiting": "Your turn"
    case "error": "Error"
    default: "Working"
    }
}

/// One line: agent (in its colour) · folder, and what it wants.
private struct RowView: View {
    let row: LiveRow
    var body: some View {
        let needs = AgentStyle.needsYou(row.status)
        HStack(spacing: 8) {
            AgentDot(agent: row.agent)
            (Text(row.agent).fontWeight(.semibold).foregroundColor(AgentStyle.color(row.agent))
                + Text("  \(row.repo)").foregroundColor(.secondary))
                .font(.footnote)
                .lineLimit(1)
            Spacer(minLength: 6)
            Text(shortStatus(row.status))
                .font(.caption.weight(.semibold))
                .foregroundStyle(needs ? amber : Color.secondary)
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
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 6) {
                    Image("Wheel").resizable().frame(width: 16, height: 16)
                    Text("Styrman").font(.footnote.weight(.semibold))
                    Spacer()
                    Text(summary(s)).font(.footnote.weight(.semibold))
                        .foregroundStyle(s.needs > 0 ? amber : Color.secondary)
                }
                ForEach(s.rows, id: \.self) { RowView(row: $0) }
            }
            .padding(.horizontal, 16).padding(.vertical, 14)
            // No tint: the system's own Lock Screen material, like the other activities.
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
                        .foregroundStyle(s.needs > 0 ? amber : Color.secondary)
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
                    Text("\(s.needs)").font(.caption.weight(.bold)).foregroundStyle(amber)
                } else {
                    Image(systemName: "circle.fill").font(.system(size: 7)).foregroundStyle(.red)
                        .symbolEffect(.pulse, options: .repeating)
                }
            } minimal: {
                if s.needs > 0 {
                    Text("\(s.needs)").font(.caption.weight(.bold)).foregroundStyle(amber)
                } else {
                    Image("Wheel").resizable().frame(width: 16, height: 16)
                }
            }
            .keylineTint(s.needs > 0 ? amber : .red)
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
                    .foregroundStyle(s.needs > 0 ? amber : Color.primary)
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
