import SwiftUI

/// Helm theme: ink wheel, one signal-red accent, quiet greys.
enum Theme {
    static let accent = Color("AccentColor")
    static let attention = Color(UIColor {
        // Light: a clear amber that still reads as text (4.6:1 on white).
        UIColor(hex: $0.userInterfaceStyle == .dark ? 0xFFCF83 : 0xA86400)
    })
    static let ink = Color(red: 0.067, green: 0.071, blue: 0.078)
    /// A solid main button: near-black in light mode, near-white in dark.
    static let solid = Color(UIColor { $0.userInterfaceStyle == .dark ? UIColor(hex: 0xF2F2F4) : UIColor(hex: 0x15181C) })
    static let onSolid = Color(UIColor { $0.userInterfaceStyle == .dark ? UIColor(hex: 0x15181C) : .white })
    static let mono = Font.system(.callout, design: .monospaced)

    // Warm neutrals, like a terminal on paper.
    private static func dyn(_ light: UInt32, _ dark: UInt32) -> Color {
        Color(UIColor { UIColor(hex: $0.userInterfaceStyle == .dark ? dark : light) })
    }
    /// The page everything sits on.
    static let canvas = dyn(0xFAF9F5, 0x1C1C1B)
    /// Cards, list blocks, the composer.
    static let surface = dyn(0xFFFFFF, 0x262625)
    /// The user's messages and quiet fills.
    static let bubble = dyn(0xF0EEE6, 0x32312F)
    static let hairline = dyn(0xE5E3DA, 0x363633)
    static let textPrimary = dyn(0x141413, 0xF5F4EE)
    static let textSecondary = dyn(0x73726C, 0xA3A19A)
    static let codeFill = dyn(0xEDEBE3, 0x2E2E2B)
    static let live = dyn(0x2EA043, 0x3FB950)
    /// Diff lines: green added, red removed, legible as text in both modes.
    static let diffAdded = Color(UIColor { UIColor(hex: $0.userInterfaceStyle == .dark ? 0x56D364 : 0x1A7F37) })
    static let diffRemoved = Color(UIColor { UIColor(hex: $0.userInterfaceStyle == .dark ? 0xF47067 : 0xCF222E) })

    static func color(for status: SessionStatus) -> Color {
        switch status {
        case .needsApproval, .waiting: attention
        case .error: .red
        case .working, .done, .unknown: .secondary
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
        let tint = Theme.color(for: status)
        let highlighted = status == .needsApproval || status == .waiting || status == .error
        HStack(spacing: 5) {
            Image(systemName: "circle.fill")
                .font(.system(size: 7))
                .foregroundStyle(status == .working ? Theme.accent : tint)
                // A working agent's dot breathes.
                .symbolEffect(.pulse, options: .repeating, isActive: status == .working)
            Text(Theme.label(for: status)).font(.caption.weight(.medium))
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
        .background(highlighted ? tint.opacity(0.12) : Color.clear, in: Capsule())
        .foregroundStyle(highlighted ? .primary : .secondary)
        .contentTransition(.opacity)
        .animation(.easeOut(duration: 0.2), value: status)
        .accessibilityElement(children: .combine)
    }
}

extension Theme {
    /// One color per agent, used for its badge and controls. Light/dark variants are
    /// tuned so the badge text stays readable (4.5:1) on its own tint.
    static func color(for agent: AgentKind) -> Color {
        func dyn(_ light: UInt32, _ dark: UInt32) -> Color {
            Color(UIColor { $0.userInterfaceStyle == .dark ? UIColor(hex: dark) : UIColor(hex: light) })
        }
        switch agent {
        case .claude: return dyn(0xB4532A, 0xF0936B)       // warm orange
        case .antigravity: return dyn(0x1F5FD1, 0x7AA7FF)  // blue
        case .codex: return dyn(0x0F7B5A, 0x4CC79A)        // green
        case .pty, .unknown: return dyn(0x5E6168, 0xA6A9B0) // grey
        }
    }
}

extension UIColor {
    convenience init(hex: UInt32) {
        self.init(red: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
                  blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
    }
}

extension Theme {
    /// A glyph per agent, for its avatar.
    static func symbol(for agent: AgentKind) -> String {
        switch agent {
        case .claude: "asterisk"
        case .codex: "chevron.left.forwardslash.chevron.right"
        case .antigravity: "atom"
        case .pty, .unknown: "terminal"
        }
    }
}

/// A tinted avatar identifies the agent in lists and headers.
struct AgentAvatar: View {
    let agent: AgentKind
    var size: CGFloat = 34
    var body: some View {
        let c = Theme.color(for: agent)
        Image(systemName: Theme.symbol(for: agent))
            .font(.system(size: size * 0.42, weight: .bold))
            .foregroundStyle(c)
            .frame(width: size, height: size)
            .background(c.opacity(0.12), in: Circle())
            .accessibilityLabel(agent.displayName)
    }
}

struct AgentBadge: View {
    let agent: AgentKind
    var body: some View {
        let c = Theme.color(for: agent)
        Text(agent.displayName)
            .font(.caption2.weight(.bold))
            .padding(.horizontal, 7).padding(.vertical, 3)
            .background(c.opacity(0.16), in: RoundedRectangle(cornerRadius: 5))
            .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(c.opacity(0.35)))
            .foregroundStyle(c)
            .accessibilityLabel("Agent: \(agent.displayName)")
    }
}
