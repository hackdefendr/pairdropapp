import Foundation

/// Resolves a user-typed server address into the URLs PairDrop actually uses.
///
/// The web client derives its WebSocket endpoint from `location.host + location.pathname`,
/// unless `/config` names a separate `signalingServer`. We reproduce both.
public struct ServerEndpoint: Equatable, Sendable {
    /// Normalized base, always with a scheme and a trailing slash (e.g. `https://drop.example.com/`).
    public let base: URL

    public init?(address: String) {
        var text = address.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return nil }

        if !text.contains("://") {
            // Bare host: assume TLS unless it looks like a LAN address or names a port.
            text = ServerEndpoint.looksLocal(text) ? "http://\(text)" : "https://\(text)"
        }
        if !text.hasSuffix("/") { text += "/" }

        guard let url = URL(string: text),
              let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https",
              url.host != nil else { return nil }

        base = url
    }

    private static func looksLocal(_ host: String) -> Bool {
        let name = host.split(separator: "/").first.map(String.init) ?? host
        let bare = name.split(separator: ":").first.map(String.init) ?? name
        return bare == "localhost"
            || bare.hasSuffix(".local")
            || bare.hasPrefix("192.168.")
            || bare.hasPrefix("10.")
            || bare.hasPrefix("127.")
            || (bare.hasPrefix("172.") && (16...31).contains(Int(bare.split(separator: ".").dropFirst().first ?? "") ?? 0))
    }

    public var isSecure: Bool { base.scheme?.lowercased() == "https" }

    public var configURL: URL { base.appendingPathComponent("config") }

    /// `host + path` — what the web client passes as the WebSocket domain when the
    /// instance does not override it.
    public var wsDomain: String {
        var host = base.host ?? ""
        if let port = base.port { host += ":\(port)" }
        var path = base.path
        if path.isEmpty { path = "/" }
        if !path.hasSuffix("/") { path += "/" }
        return host + path
    }

    /// - Parameter signalingServer: the `signalingServer` value from `/config`, when the
    ///   instance delegates signalling elsewhere. Expected to end in `/`, as upstream does.
    public func webSocketURL(signalingServer: String?, peerId: String?, peerIdHash: String?) -> URL? {
        var domain = signalingServer?.trimmingCharacters(in: .whitespacesAndNewlines)
        if domain?.isEmpty ?? true { domain = wsDomain }
        guard var domain else { return nil }
        if !domain.hasSuffix("/") { domain += "/" }

        let scheme = isSecure ? "wss" : "ws"
        guard var components = URLComponents(string: "\(scheme)://\(domain)server") else { return nil }

        var query = [URLQueryItem(name: "webrtc_supported", value: "true")]
        if let peerId, let peerIdHash {
            query.append(URLQueryItem(name: "peer_id", value: peerId))
            query.append(URLQueryItem(name: "peer_id_hash", value: peerIdHash))
        }
        components.queryItems = query
        return components.url
    }
}

/// Response body of `GET /config`.
public struct InstanceConfig: Decodable, Sendable {
    public let signalingServer: String?

    public init(signalingServer: String?) {
        self.signalingServer = signalingServer
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        // The server sends `false` when unset, not a string.
        signalingServer = try? c.decode(String.self, forKey: .signalingServer)
    }

    private enum CodingKeys: String, CodingKey { case signalingServer }
}
