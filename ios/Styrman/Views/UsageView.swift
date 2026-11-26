import SwiftUI

struct UsageView: View {
    @Environment(DaemonConnection.self) private var connection
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            TimelineView(.periodic(from: .now, by: 30)) { context in
                List {
                    if connection.state != .connected {
                        Text("Disconnected from Mac").foregroundStyle(.secondary)
                    }
                    if let providers = connection.usage, !providers.isEmpty {
                        ForEach(providers) { provider in
                            providerSection(provider, now: context.date)
                        }
                    } else if connection.state == .connected {
                        if !connection.usageSupported {
                            Text("Update Styrman on your Mac to view usage.")
                        } else if connection.usage == nil {
                            ProgressView("Loading usage…")
                        } else {
                            Text("No usage available. Open Karv on your Mac and sync it.")
                        }
                    }
                }
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

    private func providerSection(_ provider: UsageProvider, now: Date) -> some View {
        let observed = Date(timeIntervalSince1970: Double(provider.observedAtMs) / 1_000)
        let stale = now.timeIntervalSince(observed) > 180
        return Section {
            ForEach(Array(provider.windows.enumerated()), id: \.offset) { _, window in
                let reset = Date(timeIntervalSince1970: Double(window.resetsAtMs) / 1_000)
                let hot = window.usedFraction >= 0.9
                VStack(alignment: .leading, spacing: 5) {
                    HStack(alignment: .firstTextBaseline) {
                        Text(window.title).font(.subheadline.weight(.medium))
                        Spacer()
                        Text(window.usedFraction, format: .percent.precision(.fractionLength(0)))
                            .font(.subheadline.weight(.semibold)).monospacedDigit()
                            .foregroundStyle(hot ? Color.orange : .primary)
                    }
                    ProgressView(value: min(max(window.usedFraction, 0), 1))
                        .tint(hot ? Color.orange : Theme.accent)
                    Text(reset > now
                         ? "\(window.rolling ? "Eases" : "Resets") in \(UsageText.short(reset.timeIntervalSince(now)))"
                         : "Awaiting a fresh reading")
                        .font(.caption).foregroundStyle(.secondary)
                }
                .padding(.vertical, 2)
            }
        } header: {
            HStack {
                Text(provider.name)
                Spacer()
                Text("\(stale ? "Stale · " : "")\(UsageText.short(now.timeIntervalSince(observed))) ago")
                    .textCase(nil).font(.caption)
                    .foregroundStyle(stale ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
            }
        } footer: {
            if provider.estimated { Text("Estimated from activity") }
        }
    }
}

enum UsageText {
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
