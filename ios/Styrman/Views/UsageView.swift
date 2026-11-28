import SwiftUI

struct UsageView: View {
    @Environment(DaemonConnection.self) private var connection
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            TimelineView(.periodic(from: .now, by: 30)) { context in
                ScrollView {
                    VStack(spacing: 10) {
                        if connection.state != .connected {
                            Text("Disconnected from Mac").foregroundStyle(.secondary)
                        }
                        if let providers = connection.usage, !providers.isEmpty {
                            HeadroomCard(providers: providers, now: context.date)
                            ForEach(providers) { ProviderCard(provider: $0, now: context.date) }
                            Text("From Karv on your Mac.")
                                .font(.caption).foregroundStyle(.tertiary)
                        } else if connection.state == .connected {
                            Group {
                                if !connection.usageSupported {
                                    Text("Update Styrman on your Mac to view usage.")
                                } else if connection.usage == nil {
                                    ProgressView("Loading usage…")
                                } else {
                                    Text("No usage available. Open Karv on your Mac and sync it.")
                                }
                            }
                            .foregroundStyle(.secondary).padding(.top, 40)
                        }
                    }
                    .padding(16)
                }
                .background(Color(.systemGroupedBackground))
                .refreshable { connection.refreshUsage() }
            }
            .navigationTitle("Usage")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
        }
        .task(id: connection.state) {
            guard connection.state == .connected else { return }
            while !Task.isCancelled {
                connection.refreshUsage()
                do { try await Task.sleep(for: .seconds(60)) } catch { return }
            }
        }
    }
}

extension UsageProvider {
    /// The agent whose colour it wears.
    var agent: AgentKind {
        switch id {
        case "claude": .claude
        case "codex": .codex
        default: .antigravity
        }
    }

    /// Room left in its first (shortest) window, as Karv's headroom counts it.
    var headroom: Double { max(0, 1 - (windows.first?.usedFraction ?? 0)) }
}

/// "Overall headroom": the average room left across the agents, the soonest reset,
/// and a bar for each.
struct HeadroomCard: View {
    let providers: [UsageProvider]
    let now: Date

    var body: some View {
        let overall = providers.map(\.headroom).reduce(0, +) / Double(max(providers.count, 1))
        let next = providers.compactMap { p in p.windows.first.map { (p, $0.resetsAt) } }
            .filter { $0.1 > now }.min { $0.1 < $1.1 }
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("OVERALL HEADROOM").font(.caption.weight(.semibold)).tracking(1.2).foregroundStyle(.secondary)
                    Text(UsageText.percent(overall))
                        .font(.system(size: 40, weight: .bold, design: .rounded)).monospacedDigit()
                }
                Spacer()
                if let (p, at) = next {
                    VStack(alignment: .trailing, spacing: 6) {
                        Text("NEXT RESET").font(.caption.weight(.semibold)).tracking(1.2).foregroundStyle(.secondary)
                        HStack(spacing: 6) {
                            Circle().fill(Theme.color(for: p.agent)).frame(width: 7, height: 7)
                            Text("\(p.shortName) · \(UsageText.compact(at.timeIntervalSince(now)))")
                                .font(.caption.monospaced()).lineLimit(1)
                        }
                        .fixedSize()
                        .padding(.horizontal, 8).padding(.vertical, 4)
                        .overlay(Capsule().strokeBorder(Color.primary.opacity(0.12)))
                    }
                }
            }
            HStack(spacing: 14) {
                ForEach(providers) { p in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Text(p.shortName).font(.caption)
                            Spacer(minLength: 2)
                            Text(UsageText.percent(p.headroom))
                                .font(.caption.weight(.semibold)).monospacedDigit()
                        }
                        Bar(fraction: p.headroom, color: Theme.color(for: p.agent))
                    }
                }
            }
        }
        .padding(14)
        .background(Color(.secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
    }
}

extension UsageProvider {
    var shortName: String {
        switch id {
        case "claude": "Claude"
        case "codex": "Codex"
        case "gemini": "Gemini"
        default: id
        }
    }
}

/// One agent: its plan, where the numbers come from, its week as a sparkline, and
/// a tile for each window.
struct ProviderCard: View {
    let provider: UsageProvider
    let now: Date

    var body: some View {
        let c = Theme.color(for: provider.agent)
        let observed = Date(timeIntervalSince1970: Double(provider.observedAtMs) / 1_000)
        let stale = now.timeIntervalSince(observed) > 180
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .center, spacing: 10) {
                AgentAvatar(agent: provider.agent, size: 30)
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 8) {
                        Text(provider.shortName).font(.headline).lineLimit(1)
                        if let plan = provider.planName, !plan.isEmpty {
                            Text(plan.uppercased())
                                .font(.caption2.weight(.bold).monospaced()).foregroundStyle(c)
                                .padding(.horizontal, 7).padding(.vertical, 3)
                                .background(c.opacity(0.15), in: Capsule())
                                .fixedSize()
                        }
                    }
                    Text([provider.statusNote ?? (provider.estimated ? "Estimated from activity" : nil),
                          (stale ? "stale · " : "") + "\(UsageText.compact(now.timeIntervalSince(observed))) ago"]
                            .compactMap { $0 }.joined(separator: " · "))
                        .font(.caption).foregroundStyle(stale ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                if let history = provider.history, history.count > 1 {
                    Sparkline(values: history.map(\.usedFraction), color: c)
                        .frame(width: 48, height: 20)
                        .accessibilityHidden(true)
                }
            }
            LazyVGrid(columns: [GridItem(.flexible(), spacing: 8), GridItem(.flexible())], spacing: 8) {
                ForEach(Array(provider.windows.enumerated()), id: \.offset) { _, w in
                    WindowTile(window: w, color: c, now: now)
                }
            }
        }
        .padding(12)
        .background {
            RoundedRectangle(cornerRadius: 18, style: .continuous)
                .fill(Color(.secondarySystemGroupedBackground))
                .overlay(alignment: .topLeading) {
                    // The agent's colour, faintly, from the corner.
                    RadialGradient(colors: [c.opacity(0.14), .clear], center: .topLeading, startRadius: 0, endRadius: 260)
                        .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
                }
        }
        .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(c.opacity(0.18)))
    }
}

/// A window: room left, how it's pacing, when it resets.
struct WindowTile: View {
    let window: UsageWindow
    let color: Color
    let now: Date

    var body: some View {
        let left = max(0, 1 - window.usedFraction)
        VStack(alignment: .leading, spacing: 5) {
            // The window and when it resets, on one line.
            HStack(spacing: 4) {
                Text(window.title).font(.caption.weight(.medium)).foregroundStyle(.secondary).lineLimit(1)
                Spacer(minLength: 2)
                if !window.rolling, window.resetsAt > now {
                    Label(UsageText.compact(window.resetsAt.timeIntervalSince(now)), systemImage: "clock")
                        .labelStyle(.titleAndIcon).font(.caption2.monospaced()).foregroundStyle(.tertiary)
                        .lineLimit(1).fixedSize()
                        .accessibilityLabel("Resets in \(UsageText.compact(window.resetsAt.timeIntervalSince(now)))")
                }
            }
            HStack(alignment: .firstTextBaseline, spacing: 3) {
                Text(UsageText.percent(left))
                    .font(.system(.title2, design: .rounded).weight(.bold)).monospacedDigit()
                    .foregroundStyle(left < 0.1 ? AnyShapeStyle(.orange) : AnyShapeStyle(.primary))
                Text("left").font(.caption2).foregroundStyle(.secondary)
            }
            Bar(fraction: left, color: color)
            let pace = UsageText.pace(window, now: now)
            if !pace.text.isEmpty {
                Label {
                    Text(pace.text).fixedSize(horizontal: false, vertical: true)
                } icon: {
                    Image(systemName: pace.symbol)
                }
                .font(.caption2.monospaced())
                .foregroundStyle(pace.tone == .good ? AnyShapeStyle(Color.green)
                                 : pace.tone == .bad ? AnyShapeStyle(Color.red) : AnyShapeStyle(.secondary))
            }
            if let detail = window.detailText, window.rolling {
                Text(detail).font(.caption2).foregroundStyle(.tertiary).lineLimit(1)
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .accessibilityElement(children: .combine)
    }
}

/// A thin rounded progress bar.
struct Bar: View {
    let fraction: Double
    let color: Color
    var body: some View {
        GeometryReader { g in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.primary.opacity(0.08))
                Capsule().fill(color).frame(width: max(6, g.size.width * min(max(fraction, 0), 1)))
            }
        }
        .frame(height: 6)
    }
}

/// The week's activity as a line.
struct Sparkline: View {
    let values: [Double]
    let color: Color
    var body: some View {
        GeometryReader { g in
            let top = max(values.max() ?? 1, 0.0001)
            Path { p in
                for (i, v) in values.enumerated() {
                    let x = g.size.width * Double(i) / Double(max(values.count - 1, 1))
                    let y = g.size.height * (1 - v / top)
                    if i == 0 { p.move(to: CGPoint(x: x, y: y)) } else { p.addLine(to: CGPoint(x: x, y: y)) }
                }
            }
            .stroke(color, style: StrokeStyle(lineWidth: 2, lineCap: .round, lineJoin: .round))
        }
    }
}

enum UsageText {
    enum Tone { case good, neutral, bad }

    /// Karv's one-line pace, e.g. "15% reserve · 45m", "8% over · out in 1h 10m",
    /// "Rolling · frees up in 23m". Nothing to say without Karv's pace.
    static func pace(_ w: UsageWindow, now: Date) -> (text: String, symbol: String, tone: Tone) {
        if w.rolling {
            if w.usedFraction > 0, w.resetsAt > now {
                return ("Rolling · frees up in \(compact(w.resetsAt.timeIntervalSince(now)))", "arrow.triangle.2.circlepath", .neutral)
            }
            return ("Rolling · no recent use", "arrow.triangle.2.circlepath", .neutral)
        }
        guard let p = w.pace else { return ("", "", .neutral) }
        let pct = "\(Int((abs(p.reserve) * 100).rounded()))%"
        switch p.state {
        case "under": return ("\(pct) reserve · \(compact(p.reserveSeconds))", "arrow.up.right", .good)
        case "over":
            if let ms = p.exhaustsAtMs {
                let at = Date(timeIntervalSince1970: Double(ms) / 1_000)
                if at > now { return ("\(pct) over · out in \(compact(at.timeIntervalSince(now)))", "arrow.down.right", .bad) }
            }
            return ("\(pct) over pace", "arrow.down.right", .bad)
        default: return ("On pace", "equal", .neutral)
        }
    }

    /// "92%", as Karv writes it (no space, whatever the region).
    static func percent(_ f: Double) -> String { "\(Int((f * 100).rounded()))%" }

    /// Karv's compact durations: "45m", "2h 10m", "3d 4h", "<1m".
    static func compact(_ seconds: TimeInterval) -> String {
        let s = Int(abs(seconds))
        let (d, h, m) = (s / 86_400, s % 86_400 / 3_600, s % 3_600 / 60)
        if d > 0 { return h > 0 ? "\(d)d \(h)h" : "\(d)d" }
        if h > 0 { return m > 0 ? "\(h)h \(m)m" : "\(h)h" }
        return m > 0 ? "\(m)m" : "<1m"
    }

    /// "45 s", "58 min", "1 h 58 min", "5 d 9 h".
    static func short(_ seconds: TimeInterval) -> String {
        let s = Int(max(0, seconds))
        let (d, h, m) = (s / 86_400, s % 86_400 / 3_600, s % 3_600 / 60)
        if d > 0 { return h > 0 ? "\(d) d \(h) h" : "\(d) d" }
        if h > 0 { return m > 0 ? "\(h) h \(m) min" : "\(h) h" }
        if m > 0 { return "\(m) min" }
        return "\(s) s"
    }
}
