# Releasing the iPhone app (TestFlight)

One-time, in App Store Connect (appstoreconnect.apple.com › Apps › +):
create an app with bundle id `com.christoffermattssonlangseth.styrman` (or yours, see
`ios/Signing.xcconfig`), name "Styrman", SKU e.g. `styrman`.

Each release:

1. `ios/scripts/archive.sh` builds a Release archive with a fresh build number.
2. Xcode › Window › Organizer: select the archive, **Distribute App** › **TestFlight & App Store** › **Distribute**.
   Xcode re-signs it for distribution (production push included).
3. In App Store Connect › TestFlight, add testers (internal testers need no review; external
   testers need a short beta review).

Notes:
- Release builds report `production` to styrd, so pushes go through Apple's production
  servers. Development builds (Xcode › Run) keep using the sandbox.
- For testers' phones to get notifications, the Mac they pair with needs push set up
  (`styr push setup` with the APNs key of the team that publishes the app, or the push
  relay once it's deployed).
- `ITSAppUsesNonExemptEncryption` is `false` (only Apple's TLS/WebSocket), so no export
  compliance questions. `PrivacyInfo.xcprivacy` declares no tracking and no collected data.
