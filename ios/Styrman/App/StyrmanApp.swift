import SwiftUI

@main
struct StyrmanApp: App {
    @UIApplicationDelegateAdaptor private var appDelegate: AppDelegate
    @State private var connection = DaemonConnection()
    @State private var pendingLink: PairingLink?
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView(pendingLink: $pendingLink)
                .environment(connection)
                .tint(Theme.accent)
                .onOpenURL { url in
                    if let link = PairingLink(url: url) { pendingLink = link }
                }
                .onChange(of: connection.sessions, initial: true) { _, sessions in
                    LiveActivities.shared.connection = connection
                    if connection.state == .connected {
                        LiveActivities.shared.update(sessions, host: connection.host)
                    }
                }
                .onChange(of: scenePhase, initial: true) { _, phase in
                    switch phase {
                    case .active:
                        appDelegate.connection = connection
                        connection.connect()
                        if connection.credentials != nil { Task { await AppDelegate.enablePush() } }
                    case .background: connection.disconnect()
                    default: break
                    }
                }
        }
    }
}

struct RootView: View {
    @Environment(DaemonConnection.self) private var connection
    @Binding var pendingLink: PairingLink?

    var body: some View {
        Group {
            if connection.credentials == nil {
                PairingView()
            } else {
                SessionListView()
            }
        }
        .sheet(item: $pendingLink) { link in
            ConfirmPairingView(link: link)
        }
        #if DEBUG
        // UI automation (simulator): STYR_AUTOPAIR=<styrman://pair link> pairs without the sheet.
        .task {
            if let s = ProcessInfo.processInfo.environment["STYR_AUTOPAIR"], let url = URL(string: s),
               let link = PairingLink(url: url), connection.credentials?.wsURL != link.wsURL {
                try? await connection.pair(with: link, deviceName: "Simulator")
            }
        }
        #endif
    }
}

extension PairingLink: Identifiable {
    var id: String { wsURL.absoluteString + code }
}
