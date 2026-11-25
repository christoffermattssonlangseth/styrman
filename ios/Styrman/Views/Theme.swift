import SwiftUI

/// Helm theme: ink wheel, one signal-red accent, quiet greys.
enum Theme {
    static let accent = Color("AccentColor")
    static let ink = Color(red: 0.067, green: 0.071, blue: 0.078)
    static let mono = Font.system(.callout, design: .monospaced)

    static func color(for status: SessionStatus) -> Color {
        switch status {
        case .working: .blue
        case .needsApproval: accent
        case .waiting: .orange
        case .error: .red
        case .done, .unknown: .secondary
        }
    }

    static func label(for status: SessionStatus) -> String {
        switch status {
        case .working: "Working"
        case .needsApproval: "Needs approval"
        case .waiting: "Needs you"
        case .error: "Error"
        case .done: "Done"
        case .unknown: "—"
        }
    }

    static func symbol(for kind: ToolKind) -> String {
        switch kind {
        case .execute: "terminal"
        case .read: "doc.text"
        case .edit: "pencil"
        case .delete: "trash"
        case .move: "arrow.right.doc.on.clipboard"
        case .search: "magnifyingglass"
        case .fetch: "globe"
        case .think: "list.bullet.clipboard"
        case .mcp: "puzzlepiece.extension"
        case .other: "wrench.and.screwdriver"
        }
    }
}

struct WheelMark: View {
    var size: CGFloat = 28
    var body: some View {
        Image("Wheel").resizable().scaledToFit().frame(width: size, height: size).accessibilityHidden(true)
    }
}

struct StatusChip: View {
    let status: SessionStatus
    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(Theme.color(for: status)).frame(width: 7, height: 7)
            Text(Theme.label(for: status)).font(.caption.weight(.medium))
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
        .background(Theme.color(for: status).opacity(0.12), in: Capsule())
        .foregroundStyle(status == .done ? .secondary : .primary)
        .accessibilityElement(children: .combine)
    }
}

struct AgentBadge: View {
    let agent: AgentKind
    var body: some View {
        Text(agent.displayName)
            .font(.caption2.weight(.semibold))
            .padding(.horizontal, 6).padding(.vertical, 3)
            .background(.quaternary, in: RoundedRectangle(cornerRadius: 5))
            .foregroundStyle(.secondary)
    }
}
