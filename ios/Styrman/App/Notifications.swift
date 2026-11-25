// Push notifications: permission prompt, APNs registration, and opening the
// session a notification is about.

import SwiftUI
import UserNotifications

@MainActor
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    /// Set by StyrmanApp so callbacks can reach the connection.
    var connection: DaemonConnection?

    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        // Approvals the extension could look up get Allow / Deny right on the notification.
        // Allowing needs the phone unlocked (Face ID); denying doesn't.
        let allow = UNNotificationAction(identifier: "ALLOW", title: "Allow once",
                                         options: [.authenticationRequired], icon: .init(systemImageName: "checkmark"))
        let deny = UNNotificationAction(identifier: "DENY", title: "Deny",
                                        options: [.destructive], icon: .init(systemImageName: "xmark"))
        center.setNotificationCategories([
            UNNotificationCategory(identifier: "PERMISSION_ACT", actions: [allow, deny], intentIdentifiers: [],
                                   hiddenPreviewsBodyPlaceholder: "Needs your approval", options: []),
            UNNotificationCategory(identifier: "PERMISSION", actions: [], intentIdentifiers: [], options: []),
        ])
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        connection?.pushToken = deviceToken.map { String(format: "%02x", $0) }.joined()
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        connection?.lastError = "Notifications unavailable: \(error.localizedDescription)"
    }

    /// Ask once (after pairing), then register with APNs on every launch.
    static func enablePush() async {
        #if DEBUG
        // Screenshots (simulator): no permission prompt over the app.
        if ProcessInfo.processInfo.environment["STYR_NO_PUSH"] == "1" { return }
        #endif
        let center = UNUserNotificationCenter.current()
        let settings = await center.notificationSettings()
        var allowed = settings.authorizationStatus == .authorized
        if settings.authorizationStatus == .notDetermined {
            allowed = (try? await center.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
        }
        if allowed { UIApplication.shared.registerForRemoteNotifications() }
    }

    // Foreground: styrd doesn't push to a connected app, but show anything that arrives.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification)
        async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
        let content = response.notification.request.content
        let info = content.userInfo
        let session = info["session"] as? String
        let action = response.actionIdentifier
        if action == "ALLOW" || action == "DENY" {
            // Answered from the notification: send it to the Mac without opening the app.
            guard let session, let request = info["request"] as? String,
                  let option = info[action == "ALLOW" ? "allow" : "deny"] as? String,
                  let creds = CredentialStore.load() else { return }
            do {
                try await QuickAnswer.answer(creds, session: session, request: request, option: option)
            } catch {
                // Say so, and let a tap open the session to answer there.
                let n = UNMutableNotificationContent()
                n.title = content.title
                n.body = "Couldn't answer from here: \(error.localizedDescription) Tap to open the session."
                n.userInfo = ["session": session]
                n.threadIdentifier = content.threadIdentifier
                try? await center.add(UNNotificationRequest(identifier: "answer-failed-\(request)", content: n, trigger: nil))
            }
            return
        }
        await MainActor.run {
            if let session { self.connection?.requestedSession = session }
        }
    }
}
