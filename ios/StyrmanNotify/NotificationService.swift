// When an approval notification arrives, ask the Mac (directly, over Tailscale) what
// it's about, and show the actual command with Allow / Deny. The push itself never
// carries the command. If the Mac can't be reached in time, the notification stays
// as it was ("needs your approval") and tapping it opens the session.

import UserNotifications

final class NotificationService: UNNotificationServiceExtension, @unchecked Sendable {
    private var deliver: ((UNNotificationContent) -> Void)?
    private var original: UNNotificationContent?

    override func didReceive(_ request: UNNotificationRequest,
                             withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        deliver = contentHandler
        original = request.content
        let info = request.content.userInfo
        guard let session = info["session"] as? String, let req = info["request"] as? String,
              let creds = CredentialStore.load(),
              let mutable = request.content.mutableCopy() as? UNMutableNotificationContent else {
            contentHandler(request.content)
            return
        }
        // Only this extension touches these until it hands one back.
        nonisolated(unsafe) let content = mutable
        nonisolated(unsafe) let unchanged = request.content
        Task {
            if let d = try? await QuickAnswer.details(creds, session: session, request: req),
               d.open, let allow = d.allowOnce, let deny = d.deny {
                content.subtitle = "Needs your approval"
                content.body = d.text.count > 400 ? String(d.text.prefix(399)) + "…" : d.text
                content.categoryIdentifier = "PERMISSION_ACT"
                content.userInfo["allow"] = allow.id
                content.userInfo["deny"] = deny.id
                self.finish(content)
            } else {
                self.finish(unchanged)
            }
        }
    }

    override func serviceExtensionTimeWillExpire() {
        if let original { finish(original) }
    }

    private func finish(_ content: UNNotificationContent) {
        let d = deliver
        deliver = nil
        d?(content)
    }
}
