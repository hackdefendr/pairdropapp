import Foundation

// Wire format for the PairDrop signaling server (server/ws-server.js).
// Every frame is a JSON object with a `type` discriminator.

// MARK: - Peer descriptions

public struct PeerName: Codable, Hashable, Sendable {
    public let model: String?
    public let os: String?
    public let browser: String?
    public let type: String?
    public let deviceName: String?
    public let displayName: String?
}

public struct PeerInfo: Codable, Hashable, Sendable {
    public let id: String
    public let name: PeerName
    public let rtcSupported: Bool
}

/// The `sender` stamped onto relayed messages by the server.
public struct MessageSender: Codable, Hashable, Sendable {
    public let id: String
    public let rtcSupported: Bool?
}

// MARK: - ICE configuration

/// `urls` arrives as either a string or an array of strings depending on the instance's rtc_config.
public struct IceServerConfig: Codable, Sendable {
    public let urls: [String]
    public let username: String?
    public let credential: String?

    private enum CodingKeys: String, CodingKey { case urls, username, credential }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let single = try? c.decode(String.self, forKey: .urls) {
            urls = [single]
        } else {
            urls = (try? c.decode([String].self, forKey: .urls)) ?? []
        }
        username = try? c.decode(String.self, forKey: .username)
        credential = try? c.decode(String.self, forKey: .credential)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(urls, forKey: .urls)
        try c.encodeIfPresent(username, forKey: .username)
        try c.encodeIfPresent(credential, forKey: .credential)
    }

    public init(urls: [String], username: String? = nil, credential: String? = nil) {
        self.urls = urls
        self.username = username
        self.credential = credential
    }
}

public struct RTCConfigPayload: Codable, Sendable {
    public let iceServers: [IceServerConfig]
    public let sdpSemantics: String?

    public static let fallback = RTCConfigPayload(
        iceServers: [IceServerConfig(urls: ["stun:stun.l.google.com:19302"])],
        sdpSemantics: "unified-plan"
    )

    public init(iceServers: [IceServerConfig], sdpSemantics: String?) {
        self.iceServers = iceServers
        self.sdpSemantics = sdpSemantics
    }
}

public struct WSConfigPayload: Codable, Sendable {
    public let rtcConfig: RTCConfigPayload?
    public let wsFallback: Bool?
}

// MARK: - SDP / ICE payloads

public struct SessionDescriptionPayload: Codable, Sendable {
    public let type: String   // "offer" | "answer" | "pranswer" | "rollback"
    public let sdp: String

    public init(type: String, sdp: String) {
        self.type = type
        self.sdp = sdp
    }
}

public struct IceCandidatePayload: Codable, Sendable {
    public let candidate: String
    public let sdpMid: String?
    public let sdpMLineIndex: Int32?
    public let usernameFragment: String?

    public init(candidate: String, sdpMid: String?, sdpMLineIndex: Int32?, usernameFragment: String? = nil) {
        self.candidate = candidate
        self.sdpMid = sdpMid
        self.sdpMLineIndex = sdpMLineIndex
        self.usernameFragment = usernameFragment
    }
}

// MARK: - Room identity

public enum RoomType: String, Codable, Sendable {
    case ip
    case secret
    case publicId = "public-id"
}

public struct RoomRef: Hashable, Sendable {
    public let type: RoomType
    public let id: String

    public init(type: RoomType, id: String) {
        self.type = type
        self.id = id
    }
}

// MARK: - Inbound

public enum ServerMessage: Sendable {
    case wsConfig(WSConfigPayload)
    case displayName(peerId: String, peerIdHash: String, displayName: String?, deviceName: String?)
    case peers(peers: [PeerInfo], room: RoomRef)
    case peerJoined(peer: PeerInfo, room: RoomRef)
    case peerLeft(peerId: String, room: RoomRef, disconnect: Bool)
    case signal(sender: MessageSender, room: RoomRef, sdp: SessionDescriptionPayload?, ice: IceCandidatePayload?, connected: Bool?)
    case ping
    case pairDeviceInitiated(roomSecret: String, pairKey: String)
    case pairDeviceJoined(roomSecret: String, peerId: String)
    case pairDeviceJoinKeyInvalid
    case pairDeviceCanceled(pairKey: String?)
    case joinKeyRateLimit
    case secretRoomDeleted(roomSecret: String)
    case roomSecretRegenerated(oldRoomSecret: String, newRoomSecret: String)
    case publicRoomCreated(roomId: String)
    case publicRoomIdInvalid(roomId: String?)
    case publicRoomLeft
    /// A transfer-protocol frame relayed over the WebSocket fallback, kept as raw JSON
    /// so it can be handed to a peer verbatim once the fallback is implemented.
    case relayed(type: String, sender: MessageSender, raw: Data)
    case unknown(type: String)

    /// `roomId` is absent from some frames; ip-room signalling ignores it server-side anyway.
    private static func room(_ dict: [String: Any]) -> RoomRef {
        let type = (dict["roomType"] as? String).flatMap(RoomType.init(rawValue:)) ?? .ip
        return RoomRef(type: type, id: dict["roomId"] as? String ?? "")
    }

    private static func decode<T: Decodable>(_ type: T.Type, from value: Any?) -> T? {
        guard let value, JSONSerialization.isValidJSONObject([value]) || value is [String: Any] else { return nil }
        guard let data = try? JSONSerialization.data(withJSONObject: value) else { return nil }
        return try? JSONDecoder().decode(T.self, from: data)
    }

    public static func parse(_ data: Data) -> ServerMessage? {
        guard let object = try? JSONSerialization.jsonObject(with: data),
              let dict = object as? [String: Any],
              let type = dict["type"] as? String else { return nil }

        switch type {
        case "ws-config":
            return .wsConfig(decode(WSConfigPayload.self, from: dict["wsConfig"])
                             ?? WSConfigPayload(rtcConfig: nil, wsFallback: nil))

        case "display-name":
            guard let peerId = dict["peerId"] as? String,
                  let hash = dict["peerIdHash"] as? String else { return .unknown(type: type) }
            return .displayName(peerId: peerId,
                                peerIdHash: hash,
                                displayName: dict["displayName"] as? String,
                                deviceName: dict["deviceName"] as? String)

        case "peers":
            let peers = decode([PeerInfo].self, from: dict["peers"]) ?? []
            return .peers(peers: peers, room: room(dict))

        case "peer-joined":
            guard let peer = decode(PeerInfo.self, from: dict["peer"]) else { return .unknown(type: type) }
            return .peerJoined(peer: peer, room: room(dict))

        case "peer-left":
            guard let peerId = dict["peerId"] as? String else { return .unknown(type: type) }
            return .peerLeft(peerId: peerId, room: room(dict), disconnect: dict["disconnect"] as? Bool ?? false)

        case "signal":
            guard let sender = decode(MessageSender.self, from: dict["sender"]) else { return .unknown(type: type) }
            return .signal(sender: sender,
                           room: room(dict),
                           sdp: decode(SessionDescriptionPayload.self, from: dict["sdp"]),
                           ice: decode(IceCandidatePayload.self, from: dict["ice"]),
                           connected: dict["connected"] as? Bool)

        case "ping":
            return .ping

        case "pair-device-initiated":
            guard let secret = dict["roomSecret"] as? String,
                  let key = dict["pairKey"] as? String else { return .unknown(type: type) }
            return .pairDeviceInitiated(roomSecret: secret, pairKey: key)

        case "pair-device-joined":
            guard let secret = dict["roomSecret"] as? String,
                  let peerId = dict["peerId"] as? String else { return .unknown(type: type) }
            return .pairDeviceJoined(roomSecret: secret, peerId: peerId)

        case "pair-device-join-key-invalid":
            return .pairDeviceJoinKeyInvalid

        case "pair-device-canceled":
            return .pairDeviceCanceled(pairKey: dict["pairKey"] as? String)

        case "join-key-rate-limit":
            return .joinKeyRateLimit

        case "secret-room-deleted":
            guard let secret = dict["roomSecret"] as? String else { return .unknown(type: type) }
            return .secretRoomDeleted(roomSecret: secret)

        case "room-secret-regenerated":
            guard let old = dict["oldRoomSecret"] as? String,
                  let new = dict["newRoomSecret"] as? String else { return .unknown(type: type) }
            return .roomSecretRegenerated(oldRoomSecret: old, newRoomSecret: new)

        case "public-room-created":
            guard let roomId = dict["roomId"] as? String else { return .unknown(type: type) }
            return .publicRoomCreated(roomId: roomId)

        case "public-room-id-invalid":
            return .publicRoomIdInvalid(roomId: dict["publicRoomId"] as? String)

        case "public-room-left":
            return .publicRoomLeft

        case "request", "header", "partition", "partition-received", "progress",
             "files-transfer-response", "file-transfer-complete", "message-transfer-complete",
             "text", "display-name-changed", "ws-chunk":
            guard let sender = decode(MessageSender.self, from: dict["sender"]) else { return .unknown(type: type) }
            return .relayed(type: type, sender: sender, raw: data)

        default:
            return .unknown(type: type)
        }
    }
}

// MARK: - Outbound

/// Frames this client sends to the signaling server.
public enum ClientMessage {
    case pong
    case disconnect
    case joinIpRoom
    case roomSecrets([String])
    case roomSecretsDeleted([String])
    case pairDeviceInitiate
    case pairDeviceJoin(pairKey: String)
    case pairDeviceCancel
    case regenerateRoomSecret(String)
    case createPublicRoom
    case joinPublicRoom(roomId: String, createIfInvalid: Bool)
    case leavePublicRoom
    case signalSDP(to: String, room: RoomRef, sdp: SessionDescriptionPayload)
    case signalICE(to: String, room: RoomRef, ice: IceCandidatePayload)

    public var json: [String: Any] {
        switch self {
        case .pong:
            return ["type": "pong"]
        case .disconnect:
            return ["type": "disconnect"]
        case .joinIpRoom:
            return ["type": "join-ip-room"]
        case .roomSecrets(let secrets):
            return ["type": "room-secrets", "roomSecrets": secrets]
        case .roomSecretsDeleted(let secrets):
            return ["type": "room-secrets-deleted", "roomSecrets": secrets]
        case .pairDeviceInitiate:
            return ["type": "pair-device-initiate"]
        case .pairDeviceJoin(let key):
            return ["type": "pair-device-join", "pairKey": key]
        case .pairDeviceCancel:
            return ["type": "pair-device-cancel"]
        case .regenerateRoomSecret(let secret):
            return ["type": "regenerate-room-secret", "roomSecret": secret]
        case .createPublicRoom:
            return ["type": "create-public-room"]
        case .joinPublicRoom(let roomId, let createIfInvalid):
            return ["type": "join-public-room", "publicRoomId": roomId, "createIfInvalid": createIfInvalid]
        case .leavePublicRoom:
            return ["type": "leave-public-room"]
        case .signalSDP(let to, let room, let sdp):
            return ["type": "signal", "to": to, "roomType": room.type.rawValue, "roomId": room.id,
                    "sdp": ["type": sdp.type, "sdp": sdp.sdp]]
        case .signalICE(let to, let room, let ice):
            var candidate: [String: Any] = ["candidate": ice.candidate]
            candidate["sdpMid"] = ice.sdpMid
            candidate["sdpMLineIndex"] = ice.sdpMLineIndex.map { Int($0) }
            candidate["usernameFragment"] = ice.usernameFragment
            return ["type": "signal", "to": to, "roomType": room.type.rawValue, "roomId": room.id,
                    "ice": candidate]
        }
    }
}
