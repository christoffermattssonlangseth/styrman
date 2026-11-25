// Runs the Live Activity while the app is open, and hands styrd the tokens it needs
// to keep it current (and start one) while the app is closed. Also keeps the
// home-screen widget's snapshot fresh.

import ActivityKit
import Foundation
import WidgetKit

@MainActor
final class LiveActivities {
    static let shared = LiveActivities()
    weak var connection: DaemonConnection?
    private var observing = Set<String>()
    private var lastWidget: LiveState?

    var enabled: Bool {
        get { UserDefaults.standard.object(forKey: "liveActivity") as? Bool ?? true }
        set {
            UserDefaults.standard.set(newValue, forKey: "liveActivity")
            if !newValue { Task { await endAll() } }
        }
    }

    private init() {
        // Push-to-start: lets styrd start the activity while the app is closed.
        Task { [weak self] in
            for await data in Activity<StyrmanActivityAttributes>.pushToStartTokenUpdates {
                self?.register(kind: "start", data)
            }
        }
        // Activities started by a push get their own update token.
        Task { [weak self] in
            for await activity in Activity<StyrmanActivityAttributes>.activityUpdates {
                self?.observe(activity)
            }
        }
    }

    static func state(of sessions: [SessionSummary]) -> LiveState {
        let live = sessions
            .filter { $0.shared && $0.status != .done && $0.status != .unknown }
            .sorted { a, b in
                a.status.needsUser != b.status.needsUser ? a.status.needsUser : a.updatedMs > b.updatedMs
            }
        let rows = live
            .filter { $0.status.needsUser || $0.status == .working }
            .prefix(3)
            .map { s in
                let status: String = switch s.status {
                case .needsApproval: "needs_approval"
                case .waiting: "waiting"
                case .error: "error"
                default: "working"
                }
                return LiveRow(agent: s.agent.displayName, repo: s.repoName, status: status)
            }
        return LiveState(working: live.filter { $0.status == .working }.count,
                         needs: live.filter { $0.status.needsUser }.count,
                         rows: Array(rows))
    }

    /// Called whenever the session list changes (app in the foreground).
    func update(_ sessions: [SessionSummary], host: String) {
        let state = Self.state(of: sessions)
        if state != lastWidget {
            lastWidget = state
            WidgetSnapshot(state: state, updated: .now, host: host).save()
            WidgetCenter.shared.reloadAllTimelines()
        }
        guard enabled, ActivityAuthorizationInfo().areActivitiesEnabled else { return }
        // ActivityKit's handles are safe to use from here; Swift can't tell.
        nonisolated(unsafe) let current = Activity<StyrmanActivityAttributes>.activities
        if state.isIdle {
            if !current.isEmpty { Task { await Self.endEverything() } }
            register(kind: "update", nil)
        } else if let a = current.first {
            nonisolated(unsafe) let a = a
            Task { await a.update(ActivityContent(state: state, staleDate: nil)) }
            observe(a)
        } else if let a = try? Activity.request(attributes: StyrmanActivityAttributes(),
                                                 content: ActivityContent(state: state, staleDate: nil),
                                                 pushType: .token) {
            observe(a)
        }
    }

    private func endAll() async {
        await Self.endEverything()
        register(kind: "update", nil)
    }

    nonisolated private static func endEverything() async {
        for a in Activity<StyrmanActivityAttributes>.activities {
            await a.end(nil, dismissalPolicy: .immediate)
        }
    }

    private func observe(_ activity: Activity<StyrmanActivityAttributes>) {
        guard !observing.contains(activity.id) else { return }
        observing.insert(activity.id)
        Task { [weak self] in
            for await data in activity.pushTokenUpdates { self?.register(kind: "update", data) }
        }
    }

    private func register(kind: String, _ data: Data?) {
        connection?.registerActivityToken(kind: kind, token: data.map { $0.map { String(format: "%02x", $0) }.joined() })
    }
}
