// Switch a session's model and reasoning effort. The agent restarts in its terminal
// and resumes the same conversation; the choice holds only for this session (the
// agent's own /model would make it the default everywhere).

import SwiftUI

enum ModelText {
    /// "Sonnet · low", "claude-opus-5-5", "Default".
    static func current(_ s: SessionSummary, in models: [ModelChoice]) -> String {
        let model = models.first { $0.matches(s.model) }?.label ?? s.model.map(short) ?? "Default"
        return [model, s.effort].compactMap { $0 }.joined(separator: " · ")
    }

    /// "claude-haiku-5-5" → "Haiku 5.5"; anything else as it is.
    static func short(_ model: String) -> String {
        let parts = model.split(separator: "-")
        guard parts.first == "claude", parts.count >= 2 else { return model }
        let version = parts.dropFirst(2).joined(separator: ".")
        return parts[1].capitalized + (version.isEmpty ? "" : " " + version)
    }
}

struct ModelSheet: View {
    @Environment(DaemonConnection.self) private var connection
    @Environment(\.dismiss) private var dismiss
    let sessionId: String
    let agent: AgentKind
    @State private var model: String?
    @State private var effort: String?

    private var summary: SessionSummary? { connection.session(sessionId) }
    private var models: [ModelChoice]? { connection.models[sessionId] }
    private var chosen: ModelChoice? { models?.first { $0.id == model } }
    private var busy: Bool { summary?.status == .working || summary?.status == .needsApproval }

    var body: some View {
        NavigationStack {
            Group {
                if let models, !models.isEmpty {
                    form(models)
                } else if models != nil {
                    ContentUnavailableView("No models", systemImage: "cpu",
                                           description: Text("\(agent.displayName) didn't list any models on the Mac."))
                } else {
                    ProgressView("Asking \(agent.displayName)…")
                }
            }
            .navigationTitle("Model")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Switch") {
                        connection.setModel(sessionId, model: model, effort: effort)
                        dismiss()
                    }
                    .disabled(model == nil || busy || !changed)
                }
            }
        }
        .tint(Theme.color(for: agent))
        .onAppear {
            connection.getModels(sessionId)
            effort = summary?.effort
        }
        .onChange(of: models) { _, list in
            if model == nil { model = list?.first { $0.matches(summary?.model) }?.id }
        }
    }

    private var changed: Bool {
        guard let summary else { return false }
        return !(chosen?.matches(summary.model) ?? false) || effort != summary.effort
    }

    private func form(_ models: [ModelChoice]) -> some View {
        Form {
            Section {
                Picker("Model", selection: $model) {
                    ForEach(models) { m in
                        Text(m.label).tag(Optional(m.id))
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
            }
            if let efforts = chosen?.efforts, !efforts.isEmpty {
                Section("Effort") {
                    Picker("Effort", selection: $effort) {
                        Text("Default").tag(String?.none)
                        ForEach(efforts, id: \.self) { e in
                            Text(e.capitalized).tag(Optional(e))
                        }
                    }
                    .pickerStyle(.menu)
                }
            }
            Section {
            } footer: {
                if busy {
                    Text("\(agent.displayName) is working. Switch once it's done, or stop it first.")
                } else {
                    Text("\(agent.displayName) restarts in its terminal and picks up this conversation. Only this session changes, not your default.")
                }
            }
        }
        .onChange(of: model) { _, _ in
            // An effort the new model doesn't take goes back to its default.
            if let e = effort, !(chosen?.efforts.contains(e) ?? false) { effort = nil }
        }
    }
}
