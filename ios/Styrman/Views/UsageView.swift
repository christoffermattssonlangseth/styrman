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
        return Section {
            ForEach(Array(provider.windows.enumerated()), id: \.offset) { _, window in
                let reset = Date(timeIntervalSince1970: Double(window.resetsAtMs) / 1_000)
                HStack(spacing: 16) {
                    Gauge(value: window.usedFraction) {
                        Text(window.title)
                    } currentValueLabel: {
                        Text(window.usedFraction, format: .percent.precision(.fractionLength(0)))
                    }
                    .gaugeStyle(.accessoryCircularCapacity)
                    .tint(window.usedFraction >= 0.9 ? Color.orange : Theme.accent)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(window.title)
                        if reset > now {
                            Text(window.rolling ? "Next relief \(reset, style: .relative)" : "Resets in \(reset, style: .relative)")
                                .font(.footnote).foregroundStyle(.secondary)
                        } else {
                            Text("Awaiting a fresh reading").font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                }
                .padding(.vertical, 4)
            }
        } header: {
            Text(provider.name)
        } footer: {
            let source = provider.estimated ? "Activity estimate" : "Reported quota"
            let freshness = now.timeIntervalSince(observed) > 180 ? "Stale · " : ""
            Text("\(source) · \(freshness)Updated \(observed, style: .relative) ago")
        }
    }
}
