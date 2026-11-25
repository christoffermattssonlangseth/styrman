// Shared by the app and the widget extension: the Live Activity's model (must match
// styrd's `live::LiveState`), the home-screen widget's snapshot, and agent styling.

import ActivityKit
import Foundation
import SwiftUI

struct StyrmanActivityAttributes: ActivityAttributes {
    struct Row: Codable, Hashable, Sendable {
        var agent: String
        var repo: String
        /// "needs_approval", "waiting", "working" or "error".
        var status: String
    }

    struct ContentState: Codable, Hashable, Sendable {
        var working: Int
        var needs: Int
        var rows: [Row]

        var isIdle: Bool { working == 0 && needs == 0 }
        static let idle = ContentState(working: 0, needs: 0, rows: [])
    }
}

typealias LiveState = StyrmanActivityAttributes.ContentState
typealias LiveRow = StyrmanActivityAttributes.Row

/// What the home-screen widget shows: the last state the app saw, and when.
struct WidgetSnapshot: Codable, Sendable {
    var state: LiveState
    var updated: Date
    var host: String

    static var group: String? {
        (Bundle.main.object(forInfoDictionaryKey: "StyrmanAppGroup") as? String).flatMap { $0.isEmpty || $0.hasPrefix("$(") ? nil : $0 }
    }

    static func load() -> WidgetSnapshot? {
        guard let g = group, let data = UserDefaults(suiteName: g)?.data(forKey: "snapshot") else { return nil }
        return try? JSONDecoder().decode(WidgetSnapshot.self, from: data)
    }

    func save() {
        guard let g = Self.group, let data = try? JSONEncoder().encode(self) else { return }
        UserDefaults(suiteName: g)?.set(data, forKey: "snapshot")
    }
}

/// Agent colours and glyphs, by display name ("Claude", "Codex", "agy").
enum AgentStyle {
    static func color(_ agent: String) -> Color {
        switch agent {
        case "Claude": Color(red: 0.71, green: 0.33, blue: 0.16)
        case "Codex": Color(red: 0.06, green: 0.48, blue: 0.35)
        case "agy": Color(red: 0.12, green: 0.37, blue: 0.82)
        default: .gray
        }
    }

    static func symbol(_ agent: String) -> String {
        switch agent {
        case "Claude": "asterisk"
        case "Codex": "chevron.left.forwardslash.chevron.right"
        case "agy": "atom"
        default: "terminal"
        }
    }

    static func label(_ status: String) -> String {
        switch status {
        case "needs_approval": "Needs approval"
        case "waiting": "Needs you"
        case "error": "Error"
        default: "Working"
        }
    }

    static func needsYou(_ status: String) -> Bool { status != "working" }
}
