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
        UNUserNotificationCenter.current().delegate = self
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
        let session = response.notification.request.content.userInfo["session"] as? String
        await MainActor.run {
            if let session { self.connection?.requestedSession = session }
        }
    }
}
