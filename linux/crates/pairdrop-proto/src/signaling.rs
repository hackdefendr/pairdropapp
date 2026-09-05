//! Wire format for the PairDrop signaling server (`server/ws-server.js`).
//! Every frame is a JSON object with a `type` discriminator.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};

// MARK: peer descriptions

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerName {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub browser: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub device_name: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
}

impl PeerName {
    /// What to show a user: the peer's own display name, else whatever the server
    /// guessed from the User-Agent.
    pub fn best_label(&self) -> String {
        self.display_name
            .clone()
            .or_else(|| self.device_name.clone())
            .unwrap_or_else(|| "Unknown device".to_string())
    }
}

// The server sends camelCase; deriving rename_all on the struct would also rename
// `type`, which is already handled explicitly above.
impl PeerName {
    fn from_value(value: &Value) -> Self {
        let get = |k: &str| value.get(k).and_then(Value::as_str).map(str::to_string);
        Self {
            model: get("model"),
            os: get("os"),
            browser: get("browser"),
            kind: get("type"),
            device_name: get("deviceName"),
            display_name: get("displayName"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub id: String,
    pub name: PeerName,
    pub rtc_supported: bool,
}

impl PeerInfo {
    fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            id: value.get("id")?.as_str()?.to_string(),
            name: value.get("name").map(PeerName::from_value).unwrap_or_default(),
            rtc_supported: value.get("rtcSupported").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// The `sender` the server stamps onto relayed messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSender {
    pub id: String,
    pub rtc_supported: Option<bool>,
}

impl MessageSender {
    fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            id: value.get("id")?.as_str()?.to_string(),
            rtc_supported: value.get("rtcSupported").and_then(Value::as_bool),
        })
    }
}

// MARK: ICE configuration

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceServerConfig {
    #[serde(deserialize_with = "string_or_seq")]
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// `urls` is a bare string in the default `rtc_config` and an array in others.
fn string_or_seq<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(s) => vec![s],
        Value::Array(items) => items
            .into_iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RtcConfig {
    #[serde(default)]
    pub ice_servers: Vec<IceServerConfig>,
    #[serde(default)]
    pub sdp_semantics: Option<String>,
}

impl Default for RtcConfig {
    /// Used only when an instance sends no usable configuration of its own.
    fn default() -> Self {
        Self {
            ice_servers: vec![IceServerConfig {
                urls: vec!["stun:stun.l.google.com:19302".to_string()],
                username: None,
                credential: None,
            }],
            sdp_semantics: Some("unified-plan".to_string()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsConfig {
    #[serde(default)]
    pub rtc_config: Option<RtcConfig>,
    #[serde(default)]
    pub ws_fallback: Option<bool>,
}

// MARK: SDP / ICE payloads

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDescription {
    /// "offer" | "answer" | "pranswer" | "rollback"
    #[serde(rename = "type")]
    pub kind: String,
    pub sdp: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IceCandidate {
    pub candidate: String,
    #[serde(default)]
    pub sdp_mid: Option<String>,
    // Not derivable from `rename_all`: camelCase would produce `sdpMlineIndex`, and the
    // browser sends `sdpMLineIndex` with a capital L. Getting this wrong drops the
    // index silently and only some peers notice.
    #[serde(default, rename = "sdpMLineIndex")]
    pub sdp_mline_index: Option<u16>,
    #[serde(default)]
    pub username_fragment: Option<String>,
}

// MARK: room identity

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoomType {
    Ip,
    Secret,
    PublicId,
}

impl RoomType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ip => "ip",
            Self::Secret => "secret",
            Self::PublicId => "public-id",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ip" => Some(Self::Ip),
            "secret" => Some(Self::Secret),
            "public-id" => Some(Self::PublicId),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoomRef {
    pub kind: RoomType,
    pub id: String,
}

impl RoomRef {
    pub fn new(kind: RoomType, id: impl Into<String>) -> Self {
        Self { kind, id: id.into() }
    }

    /// `roomId` is absent from some frames; ip-room signaling ignores it server-side.
    fn from_value(value: &Value) -> Self {
        let kind = value
            .get("roomType")
            .and_then(Value::as_str)
            .and_then(RoomType::parse)
            .unwrap_or(RoomType::Ip);
        let id = value.get("roomId").and_then(Value::as_str).unwrap_or_default();
        Self::new(kind, id)
    }
}

// MARK: inbound

#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    WsConfig(WsConfig),
    DisplayName {
        peer_id: String,
        peer_id_hash: String,
        display_name: Option<String>,
        device_name: Option<String>,
    },
    Peers { peers: Vec<PeerInfo>, room: RoomRef },
    PeerJoined { peer: PeerInfo, room: RoomRef },
    PeerLeft { peer_id: String, room: RoomRef, disconnect: bool },
    Signal {
        sender: MessageSender,
        room: RoomRef,
        sdp: Option<SessionDescription>,
        ice: Option<IceCandidate>,
        connected: Option<bool>,
    },
    Ping,
    PairDeviceInitiated { room_secret: String, pair_key: String },
    PairDeviceJoined { room_secret: String, peer_id: String },
    PairDeviceJoinKeyInvalid,
    PairDeviceCanceled { pair_key: Option<String> },
    JoinKeyRateLimit,
    SecretRoomDeleted { room_secret: String },
    RoomSecretRegenerated { old_room_secret: String, new_room_secret: String },
    PublicRoomCreated { room_id: String },
    PublicRoomIdInvalid { room_id: Option<String> },
    PublicRoomLeft,
    /// A transfer frame relayed over the WebSocket fallback, kept as raw JSON so it can
    /// be handed to a peer verbatim once the fallback is implemented.
    Relayed { kind: String, sender: MessageSender, raw: Value },
    Unknown { kind: String },
}

/// Frame types the server relays between peers rather than acting on itself.
const RELAYED_TYPES: &[&str] = &[
    "request",
    "header",
    "partition",
    "partition-received",
    "progress",
    "files-transfer-response",
    "file-transfer-complete",
    "message-transfer-complete",
    "text",
    "display-name-changed",
    "ws-chunk",
];

impl ServerMessage {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        Self::from_json(&serde_json::from_slice::<Value>(bytes).ok()?)
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        let kind = value.get("type")?.as_str()?;
        let unknown = || Some(Self::Unknown { kind: kind.to_string() });

        match kind {
            "ws-config" => Some(Self::WsConfig(
                value
                    .get("wsConfig")
                    .and_then(|c| serde_json::from_value(c.clone()).ok())
                    .unwrap_or_default(),
            )),

            "display-name" => {
                let get = |k: &str| value.get(k).and_then(Value::as_str).map(str::to_string);
                match (get("peerId"), get("peerIdHash")) {
                    (Some(peer_id), Some(peer_id_hash)) => Some(Self::DisplayName {
                        peer_id,
                        peer_id_hash,
                        display_name: get("displayName"),
                        device_name: get("deviceName"),
                    }),
                    _ => unknown(),
                }
            }

            "peers" => Some(Self::Peers {
                peers: value
                    .get("peers")
                    .and_then(Value::as_array)
                    .map(|items| items.iter().filter_map(PeerInfo::from_value).collect())
                    .unwrap_or_default(),
                room: RoomRef::from_value(value),
            }),

            "peer-joined" => match value.get("peer").and_then(PeerInfo::from_value) {
                Some(peer) => Some(Self::PeerJoined { peer, room: RoomRef::from_value(value) }),
                None => unknown(),
            },

            "peer-left" => match value.get("peerId").and_then(Value::as_str) {
                Some(peer_id) => Some(Self::PeerLeft {
                    peer_id: peer_id.to_string(),
                    room: RoomRef::from_value(value),
                    disconnect: value.get("disconnect").and_then(Value::as_bool).unwrap_or(false),
                }),
                None => unknown(),
            },

            "signal" => match value.get("sender").and_then(MessageSender::from_value) {
                Some(sender) => Some(Self::Signal {
                    sender,
                    room: RoomRef::from_value(value),
                    sdp: value.get("sdp").and_then(|v| serde_json::from_value(v.clone()).ok()),
                    ice: value.get("ice").and_then(|v| serde_json::from_value(v.clone()).ok()),
                    connected: value.get("connected").and_then(Value::as_bool),
                }),
                None => unknown(),
            },

            "ping" => Some(Self::Ping),

            "pair-device-initiated" => {
                let get = |k: &str| value.get(k).and_then(Value::as_str).map(str::to_string);
                match (get("roomSecret"), get("pairKey")) {
                    (Some(room_secret), Some(pair_key)) => {
                        Some(Self::PairDeviceInitiated { room_secret, pair_key })
                    }
                    _ => unknown(),
                }
            }

            "pair-device-joined" => {
                let get = |k: &str| value.get(k).and_then(Value::as_str).map(str::to_string);
                match (get("roomSecret"), get("peerId")) {
                    (Some(room_secret), Some(peer_id)) => {
                        Some(Self::PairDeviceJoined { room_secret, peer_id })
                    }
                    _ => unknown(),
                }
            }

            "pair-device-join-key-invalid" => Some(Self::PairDeviceJoinKeyInvalid),

            "pair-device-canceled" => Some(Self::PairDeviceCanceled {
                pair_key: value.get("pairKey").and_then(Value::as_str).map(str::to_string),
            }),

            "join-key-rate-limit" => Some(Self::JoinKeyRateLimit),

            "secret-room-deleted" => match value.get("roomSecret").and_then(Value::as_str) {
                Some(secret) => Some(Self::SecretRoomDeleted { room_secret: secret.to_string() }),
                None => unknown(),
            },

            "room-secret-regenerated" => {
                let get = |k: &str| value.get(k).and_then(Value::as_str).map(str::to_string);
                match (get("oldRoomSecret"), get("newRoomSecret")) {
                    (Some(old_room_secret), Some(new_room_secret)) => {
                        Some(Self::RoomSecretRegenerated { old_room_secret, new_room_secret })
                    }
                    _ => unknown(),
                }
            }

            "public-room-created" => match value.get("roomId").and_then(Value::as_str) {
                Some(room_id) => Some(Self::PublicRoomCreated { room_id: room_id.to_string() }),
                None => unknown(),
            },

            "public-room-id-invalid" => Some(Self::PublicRoomIdInvalid {
                room_id: value.get("publicRoomId").and_then(Value::as_str).map(str::to_string),
            }),

            "public-room-left" => Some(Self::PublicRoomLeft),

            other if RELAYED_TYPES.contains(&other) => {
                match value.get("sender").and_then(MessageSender::from_value) {
                    Some(sender) => Some(Self::Relayed {
                        kind: other.to_string(),
                        sender,
                        raw: value.clone(),
                    }),
                    None => unknown(),
                }
            }

            _ => unknown(),
        }
    }
}

// MARK: outbound

#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    Pong,
    Disconnect,
    JoinIpRoom,
    RoomSecrets(Vec<String>),
    RoomSecretsDeleted(Vec<String>),
    PairDeviceInitiate,
    PairDeviceJoin { pair_key: String },
    PairDeviceCancel,
    RegenerateRoomSecret(String),
    CreatePublicRoom,
    JoinPublicRoom { room_id: String, create_if_invalid: bool },
    LeavePublicRoom,
    SignalSdp { to: String, room: RoomRef, sdp: SessionDescription },
    SignalIce { to: String, room: RoomRef, ice: IceCandidate },
}

impl ClientMessage {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Pong => json!({ "type": "pong" }),
            Self::Disconnect => json!({ "type": "disconnect" }),
            Self::JoinIpRoom => json!({ "type": "join-ip-room" }),
            Self::RoomSecrets(secrets) => json!({ "type": "room-secrets", "roomSecrets": secrets }),
            Self::RoomSecretsDeleted(secrets) => {
                json!({ "type": "room-secrets-deleted", "roomSecrets": secrets })
            }
            Self::PairDeviceInitiate => json!({ "type": "pair-device-initiate" }),
            Self::PairDeviceJoin { pair_key } => {
                json!({ "type": "pair-device-join", "pairKey": pair_key })
            }
            Self::PairDeviceCancel => json!({ "type": "pair-device-cancel" }),
            Self::RegenerateRoomSecret(secret) => {
                json!({ "type": "regenerate-room-secret", "roomSecret": secret })
            }
            Self::CreatePublicRoom => json!({ "type": "create-public-room" }),
            Self::JoinPublicRoom { room_id, create_if_invalid } => json!({
                "type": "join-public-room",
                "publicRoomId": room_id,
                "createIfInvalid": create_if_invalid,
            }),
            Self::LeavePublicRoom => json!({ "type": "leave-public-room" }),
            Self::SignalSdp { to, room, sdp } => json!({
                "type": "signal",
                "to": to,
                "roomType": room.kind.as_str(),
                "roomId": room.id,
                "sdp": sdp,
            }),
            Self::SignalIce { to, room, ice } => json!({
                "type": "signal",
                "to": to,
                "roomType": room.kind.as_str(),
                "roomId": room.id,
                "ice": ice,
            }),
        }
    }

    pub fn encode(&self) -> String {
        self.to_json().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_peers_frame() {
        let json = r#"
        {"type":"peers","roomType":"ip","roomId":"127.0.0.1","peers":[
          {"id":"1111","rtcSupported":true,
           "name":{"model":null,"os":"Mac OS","browser":"Safari","type":null,
                   "deviceName":"Mac Safari","displayName":"Amethyst Orca"}}]}"#;

        let Some(ServerMessage::Peers { peers, room }) = ServerMessage::parse(json.as_bytes()) else {
            panic!("did not parse");
        };
        assert_eq!(room.kind, RoomType::Ip);
        assert_eq!(room.id, "127.0.0.1");
        assert_eq!(peers[0].name.display_name.as_deref(), Some("Amethyst Orca"));
        assert_eq!(peers[0].name.best_label(), "Amethyst Orca");
        assert!(peers[0].rtc_supported);
    }

    /// `urls` is a bare string in the default rtc_config and an array in others.
    #[test]
    fn ice_server_urls_accept_either_shape() {
        let json = r#"
        {"type":"ws-config","wsConfig":{"wsFallback":true,"rtcConfig":{
          "sdpSemantics":"unified-plan",
          "iceServers":[{"urls":"stun:stun.l.google.com:19302"},
                        {"urls":["turns:a:5349","turn:a:3478"],"username":"u","credential":"c"}]}}}"#;

        let Some(ServerMessage::WsConfig(config)) = ServerMessage::parse(json.as_bytes()) else {
            panic!("did not parse");
        };
        assert_eq!(config.ws_fallback, Some(true));
        let rtc = config.rtc_config.unwrap();
        assert_eq!(rtc.ice_servers[0].urls, ["stun:stun.l.google.com:19302"]);
        assert_eq!(rtc.ice_servers[1].urls.len(), 2);
        assert_eq!(rtc.ice_servers[1].username.as_deref(), Some("u"));
    }

    #[test]
    fn signal_carries_sdp_and_sender() {
        let json = r#"
        {"type":"signal","roomType":"ip","roomId":"127.0.0.1",
         "sender":{"id":"abc","rtcSupported":true},
         "sdp":{"type":"offer","sdp":"v=0\r\n"}}"#;

        let Some(ServerMessage::Signal { sender, sdp, ice, .. }) =
            ServerMessage::parse(json.as_bytes())
        else {
            panic!("did not parse");
        };
        assert_eq!(sender.id, "abc");
        assert_eq!(sdp.unwrap().kind, "offer");
        assert!(ice.is_none());
    }

    /// The browser sends sdpMLineIndex as a number and sdpMid as a string; both are
    /// optional, and a trickle candidate may carry only the candidate line.
    #[test]
    fn signal_parses_ice_candidates() {
        let json = r#"
        {"type":"signal","roomType":"ip","sender":{"id":"abc"},
         "ice":{"candidate":"candidate:1 1 udp 2113937151 192.168.1.5 55555 typ host",
                "sdpMid":"0","sdpMLineIndex":0,"usernameFragment":"abcd"}}"#;

        let Some(ServerMessage::Signal { ice: Some(ice), .. }) = ServerMessage::parse(json.as_bytes())
        else {
            panic!("did not parse");
        };
        assert_eq!(ice.sdp_mid.as_deref(), Some("0"));
        assert_eq!(ice.sdp_mline_index, Some(0));
        assert!(ice.candidate.starts_with("candidate:1"));
    }

    /// The outgoing key has to match too — a candidate we send with the wrong casing
    /// is dropped by the far side just as silently.
    #[test]
    fn outgoing_ice_uses_the_browsers_key_casing() {
        let message = ClientMessage::SignalIce {
            to: "peer-1".into(),
            room: RoomRef::new(RoomType::Ip, "127.0.0.1"),
            ice: IceCandidate {
                candidate: "candidate:1 1 udp 1 1.2.3.4 5 typ host".into(),
                sdp_mid: Some("0".into()),
                sdp_mline_index: Some(7),
                username_fragment: None,
            },
        };

        let encoded = message.encode();
        assert!(encoded.contains(r#""sdpMLineIndex":7"#), "{encoded}");
        assert!(encoded.contains(r#""sdpMid":"0""#), "{encoded}");

        // And it survives a full round trip back through the parser.
        let relayed = format!(
            r#"{{"type":"signal","roomType":"ip","sender":{{"id":"x"}},"ice":{}}}"#,
            message.to_json()["ice"]
        );
        let Some(ServerMessage::Signal { ice: Some(ice), .. }) =
            ServerMessage::parse(relayed.as_bytes())
        else {
            panic!("did not parse back");
        };
        assert_eq!(ice.sdp_mline_index, Some(7));
    }

    #[test]
    fn unknown_type_does_not_panic() {
        let Some(ServerMessage::Unknown { kind }) =
            ServerMessage::parse(br#"{"type":"future-thing"}"#)
        else {
            panic!("expected unknown");
        };
        assert_eq!(kind, "future-thing");
    }

    /// A transfer frame arriving over the socket is kept whole so the fallback can
    /// forward it byte for byte.
    #[test]
    fn transfer_frames_are_kept_raw() {
        let json = r#"{"type":"text","text":"aGk=","sender":{"id":"abc","rtcSupported":false}}"#;
        let Some(ServerMessage::Relayed { kind, sender, raw }) =
            ServerMessage::parse(json.as_bytes())
        else {
            panic!("did not parse");
        };
        assert_eq!(kind, "text");
        assert_eq!(sender.id, "abc");
        assert_eq!(raw["text"], "aGk=");
    }

    #[test]
    fn ping_is_recognised() {
        assert_eq!(ServerMessage::parse(br#"{"type":"ping"}"#), Some(ServerMessage::Ping));
        assert_eq!(ClientMessage::Pong.encode(), r#"{"type":"pong"}"#);
    }

    /// The server strips `to` before relaying, but it must be present on the way out.
    #[test]
    fn signal_frames_address_a_peer() {
        let message = ClientMessage::SignalSdp {
            to: "peer-1".into(),
            room: RoomRef::new(RoomType::Ip, "127.0.0.1"),
            sdp: SessionDescription { kind: "offer".into(), sdp: "v=0\r\n".into() },
        };
        let json = message.to_json();
        assert_eq!(json["to"], "peer-1");
        assert_eq!(json["roomType"], "ip");
        assert_eq!(json["sdp"]["type"], "offer");

        // And it must survive a round trip through the server's relay shape.
        assert_eq!(json["type"], "signal");
    }

    #[test]
    fn room_types_match_the_wire() {
        assert_eq!(RoomType::PublicId.as_str(), "public-id");
        assert_eq!(RoomType::parse("public-id"), Some(RoomType::PublicId));
        assert_eq!(RoomType::parse("nonsense"), None);
    }
}
