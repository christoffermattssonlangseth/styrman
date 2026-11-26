// When an approval notification arrives, ask the Mac (directly, over Tailscale) what
// it's about, and show the actual command with Allow / Deny. The push itself never
// carries the command. If the Mac can't be reached in time, the notification stays
// as it was ("needs your approval") and tapping it opens the session.

import Foundation
import UserNotifications
import WidgetKit

final class NotificationService: UNNotificationServiceExtension, @unchecked Sendable {
    private var deliver: ((UNNotificationContent) -> Void)?
    private var original: UNNotificationContent?

    override func didReceive(_ request: UNNotificationRequest,
                             withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        deliver = contentHandler
        original = request.content
        let info = request.content.userInfo
        guard let session = info["session"] as? String,
              let creds = CredentialStore.load(),
              let mutable = request.content.mutableCopy() as? UNMutableNotificationContent else {
            contentHandler(request.content)
            return
        }
        // Only this extension touches these until it hands one back.
        let box = Box(service: self, content: mutable, unchanged: request.content)
        let req = info["request"] as? String
        Task.detached {
            async let refresh: Void = Self.refreshWidget(creds)
            let details: QuickAnswer.Details?
            if let req {
                details = try? await QuickAnswer.details(creds, session: session, request: req)
            } else {
                details = nil
            }
            await refresh
            box.finish(with: details)
        }
    }

    private static func fetchSnapshot(_ credentials: Credentials) async throws -> WidgetSnapshot {
        let started = Date.now
        var request = URLRequest(url: credentials.httpBase.appending(path: "v1/live"))
        request.timeoutInterval = 8
        request.setValue("Bearer \(credentials.token)", forHTTPHeaderField: "Authorization")
        let (data, response) = try await URLSession.shared.data(for: request)
        guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw URLError(.badServerResponse) }
        let live = try JSONDecoder().decode(LiveState.self, from: data)
        return WidgetSnapshot(state: live, updated: started, host: credentials.host)
    }

    private static func refreshWidget(_ credentials: Credentials) async {
        guard let snapshot = try? await fetchSnapshot(credentials),
              CredentialStore.load()?.token == credentials.token else { return }
        snapshot.save()
        WidgetCenter.shared.reloadTimelines(ofKind: "Agents")
    }

    /// Hands the notification and this service to the task (they're not Sendable).
    private final class Box: @unchecked Sendable {
        let service: NotificationService
        let content: UNMutableNotificationContent
        let unchanged: UNNotificationContent
        init(service: NotificationService, content: UNMutableNotificationContent, unchanged: UNNotificationContent) {
            self.service = service
            self.content = content
            self.unchanged = unchanged
        }

        func finish(with d: QuickAnswer.Details?) {
            guard let d, d.open, let allow = d.allowOnce, let deny = d.deny else {
                service.finish(unchanged)
                return
            }
            content.subtitle = "Needs your approval"
            content.body = d.text.count > 400 ? String(d.text.prefix(399)) + "…" : d.text
            content.categoryIdentifier = "PERMISSION_ACT"
            content.userInfo["allow"] = allow.id
            content.userInfo["deny"] = deny.id
            service.finish(content)
        }
    }

    override func serviceExtensionTimeWillExpire() {
        if let original { finish(original) }
    }

    fileprivate func finish(_ content: UNNotificationContent) {
        let d = deliver
        deliver = nil
        d?(content)
    }
}
