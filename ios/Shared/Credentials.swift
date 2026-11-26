// Shared by the app and its notification extension: the device's credentials for
// the Mac, and the two small HTTP calls that answer an approval from a notification.

import Foundation
import Security

struct Credentials: Codable, Equatable, Sendable {
    let wsURL: URL
    let token: String
    let deviceId: String
    let host: String

    /// http(s)://host:port/ of the Mac's phone API.
    var httpBase: URL {
        var c = URLComponents(url: wsURL, resolvingAgainstBaseURL: false)!
        c.scheme = c.scheme == "wss" ? "https" : "http"
        c.path = "/"
        return c.url!
    }
}

/// Device credentials in the Keychain (this device only, available after first unlock).
/// Stored in the app's own keychain group, which the notification extension shares.
enum CredentialStore {
    private static func info(_ key: String) -> String? {
        (Bundle.main.object(forInfoDictionaryKey: key) as? String).flatMap { $0.isEmpty || $0.hasPrefix("$(") ? nil : $0 }
    }
    /// The app's bundle id, also from the extension (same value as before, so existing pairings stay).
    private static var service: String { info("StyrmanAppBundleID") ?? Bundle.main.bundleIdentifier ?? "styrman" }
    private static var group: String? { info("StyrmanKeychainGroup") }
    private static let account = "daemon"

    private static func base() -> [String: Any] {
        var q: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        if let group { q[kSecAttrAccessGroup as String] = group }
        return q
    }

    static func load() -> Credentials? {
        var q = base()
        q[kSecReturnData as String] = true
        var out: AnyObject?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let data = out as? Data else { return nil }
        return try? JSONDecoder().decode(Credentials.self, from: data)
    }

    static func save(_ c: Credentials) throws {
        delete()
        var add = base()
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        add[kSecValueData as String] = try JSONEncoder().encode(c)
        let status = SecItemAdd(add as CFDictionary, nil)
        guard status == errSecSuccess else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(status)) }
    }

    static func delete() {
        SecItemDelete(base() as CFDictionary)
    }
}

/// Answering an approval straight from its notification (styrd's `/v1/permission/…`).
enum QuickAnswer {
    struct Option: Decodable, Sendable {
        let id: String
        let kind: String
        let label: String
    }

    struct Details: Decodable, Sendable {
        let open: Bool
        let agent: String
        let repo: String
        /// The command for shell tools, else the tool's one-line title.
        let text: String
        let options: [Option]

        var allowOnce: Option? { options.first { $0.kind == "allow_once" } }
        var deny: Option? { options.first { $0.kind == "reject_once" } ?? options.first { $0.kind == "reject_always" } }
    }

    enum Failure: LocalizedError {
        case status(Int, String)
        var errorDescription: String? {
            switch self {
            case .status(_, let message): message
            }
        }
    }

    private static func request(_ c: Credentials, session: String, request: String) -> URLRequest {
        var r = URLRequest(url: c.httpBase.appending(path: "v1/permission").appending(path: session).appending(path: request))
        r.setValue("Bearer \(c.token)", forHTTPHeaderField: "Authorization")
        r.timeoutInterval = 8
        return r
    }

    private static func check(_ data: Data, _ resp: URLResponse) throws {
        let status = (resp as? HTTPURLResponse)?.statusCode ?? 0
        guard status == 200 else {
            let message = (try? JSONDecoder().decode([String: String].self, from: data))?["error"]
                ?? "The Mac answered with an error (\(status))."
            throw Failure.status(status, message)
        }
    }

    /// A session in a few words (styrd's `/v1/brief/{session}`), to make a notification
    /// say which session it's about and what it said.
    struct Brief: Decodable, Sendable {
        let agent: String
        let repo: String
        /// Your latest message to it: tells sessions in the same folder apart.
        let asked: String?
        /// What it's doing or last said, first line.
        let line: String?
        let status: String
    }

    static func brief(_ c: Credentials, session: String) async throws -> Brief {
        var r = URLRequest(url: c.httpBase.appending(path: "v1/brief").appending(path: session))
        r.setValue("Bearer \(c.token)", forHTTPHeaderField: "Authorization")
        r.timeoutInterval = 8
        let (data, resp) = try await URLSession.shared.data(for: r)
        try check(data, resp)
        return try JSONDecoder().decode(Brief.self, from: data)
    }

    static func details(_ c: Credentials, session: String, request: String) async throws -> Details {
        let (data, resp) = try await URLSession.shared.data(for: Self.request(c, session: session, request: request))
        try check(data, resp)
        return try JSONDecoder().decode(Details.self, from: data)
    }

    static func answer(_ c: Credentials, session: String, request: String, option: String) async throws {
        var r = Self.request(c, session: session, request: request)
        r.httpMethod = "POST"
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.httpBody = try JSONEncoder().encode(["option": option])
        let (data, resp) = try await URLSession.shared.data(for: r)
        try check(data, resp)
    }
}
