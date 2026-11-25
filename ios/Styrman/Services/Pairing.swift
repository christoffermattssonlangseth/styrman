// Pairing links (`styrman://pair?v=1&url=…&code=…&name=…`), the pairing request,
// and Keychain storage of the resulting device credentials.

import Foundation
import Security
import UIKit

struct PairingLink: Equatable, Sendable {
    let wsURL: URL
    let code: String
    let host: String

    /// Parse a `styrman://pair` link (from the QR code or the camera app).
    init?(url: URL) {
        guard url.scheme == "styrman", url.host() == "pair",
              let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems else { return nil }
        func q(_ k: String) -> String? { items.first { $0.name == k }?.value }
        guard let u = q("url"), let ws = URL(string: u), ["ws", "wss"].contains(ws.scheme ?? ""),
              let code = q("code"), !code.isEmpty else { return nil }
        self.init(wsURL: ws, code: code, host: q("name") ?? ws.host() ?? "Mac")
    }

    init(wsURL: URL, code: String, host: String) {
        self.wsURL = wsURL
        self.code = code
        self.host = host
    }

    /// Manual entry: address like `100.1.2.3:4719`, `ws://…/v1/ws`, or a MagicDNS name.
    init?(address: String, code: String) {
        var a = address.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !a.isEmpty, !code.isEmpty else { return nil }
        if !a.contains("://") { a = "ws://" + a }
        guard var comps = URLComponents(string: a), comps.host != nil else { return nil }
        if comps.port == nil { comps.port = 4719 }
        if comps.path.isEmpty || comps.path == "/" { comps.path = "/v1/ws" }
        guard let url = comps.url else { return nil }
        self.init(wsURL: url, code: code, host: comps.host ?? "Mac")
    }

    /// `http(s)://host:port` of the daemon.
    var httpBase: URL {
        var c = URLComponents(url: wsURL, resolvingAgainstBaseURL: false)!
        c.scheme = wsURL.scheme == "wss" ? "https" : "http"
        c.path = ""
        return c.url!
    }
}


enum PairingError: LocalizedError {
    case rejected, server(Int), unreachable(String)
    var errorDescription: String? {
        switch self {
        case .rejected: "That pairing code is wrong or has expired. Run `styr pair` again."
        case .server(let code): "The Mac answered with an error (\(code))."
        case .unreachable(let why): "Couldn't reach the Mac: \(why). Is Tailscale on and styrd running?"
        }
    }
}

enum Pairing {
    static func pair(_ link: PairingLink, deviceName: String) async throws -> Credentials {
        var req = URLRequest(url: link.httpBase.appending(path: "v1/pair"))
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.timeoutInterval = 15
        req.httpBody = try JSONEncoder().encode(PairRequest(code: link.code, deviceName: deviceName))
        let (data, resp): (Data, URLResponse)
        do {
            (data, resp) = try await URLSession.shared.data(for: req)
        } catch {
            throw PairingError.unreachable(error.localizedDescription)
        }
        let status = (resp as? HTTPURLResponse)?.statusCode ?? 0
        if status == 401 { throw PairingError.rejected }
        guard status == 200 else { throw PairingError.server(status) }
        let r = try JSONDecoder().decode(PairResponse.self, from: data)
        return Credentials(wsURL: link.wsURL, token: r.token, deviceId: r.deviceId, host: r.host)
    }
}
