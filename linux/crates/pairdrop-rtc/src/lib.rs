//! One peer connection and its `data-channel`, matching what the web client builds in
//! `public/scripts/network.js`.
//!
//! Roles are decided by the signaling server, not negotiated here: peers already in the
//! room when we arrive are ones *we* call; peers that arrive after us call us. Only the
//! caller creates the data channel — the answerer picks it up from `on_data_channel`.
//!
//! This crate never talks to the signaling server. It emits the SDP and ICE payloads
//! that need sending and accepts the ones that arrive.

use std::sync::Arc;

use bytes::BytesMut;
use pairdrop_proto::{IceCandidate, RtcConfig, SessionDescription};
use tokio::sync::{mpsc, Mutex};
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceServer, RTCPeerConnectionIceEvent, RTCPeerConnectionState,
    RTCSessionDescription, Registry, register_default_interceptors,
};

/// The label both sides agree on. The answerer accepts whatever channel arrives, but the
/// caller must use this name for the web client to recognise it.
pub const DATA_CHANNEL_LABEL: &str = "data-channel";

#[derive(Debug, thiserror::Error)]
pub enum RtcError {
    #[error("webrtc: {0}")]
    Webrtc(#[from] webrtc::error::Error),
    #[error("the data channel is not open")]
    NotOpen,
}

/// What a session reports.
#[derive(Debug, Clone)]
pub enum RtcEvent {
    /// Send to the peer as `{sdp: …}`.
    LocalDescription(SessionDescription),
    /// Send to the peer as `{ice: …}`.
    LocalCandidate(IceCandidate),
    /// The channel is open, carrying the 16-digit code both sides display.
    Open { connection_hash: String },
    Text(String),
    Binary(Vec<u8>),
    /// ICE gave up. The caller should rebuild the session and offer again.
    Failed,
    Closed,
}

type SharedConnection = Arc<Mutex<Option<Arc<dyn PeerConnection>>>>;
type SharedChannel = Arc<Mutex<Option<Arc<dyn DataChannel>>>>;

pub struct RtcSession {
    connection: Arc<dyn PeerConnection>,
    channel: SharedChannel,
    events: mpsc::UnboundedSender<RtcEvent>,
}

impl RtcSession {
    /// Builds the connection, and for the caller the data channel and opening offer.
    pub async fn new(
        config: &RtcConfig,
        is_caller: bool,
    ) -> Result<(Self, mpsc::UnboundedReceiver<RtcEvent>), RtcError> {
        let (events, receiver) = mpsc::unbounded_channel();
        let channel: SharedChannel = Arc::new(Mutex::new(None));
        let shared_connection: SharedConnection = Arc::new(Mutex::new(None));

        let handler = Arc::new(Handler {
            events: events.clone(),
            channel: Arc::clone(&channel),
            connection: Arc::clone(&shared_connection),
            is_caller,
        });

        let mut media_engine = MediaEngine::default();
        let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;

        let connection = PeerConnectionBuilder::new()
            .with_configuration(
                RTCConfigurationBuilder::new()
                    .with_ice_servers(ice_servers(config))
                    .build(),
            )
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .with_handler(handler)
            // Any port: PairDrop is a client, so nothing has to reach us on a known one.
            .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
            .build()
            .await?;

        let connection: Arc<dyn PeerConnection> = Arc::new(connection);
        // The handler needs the connection to compute the verification hash, but the
        // connection is built *with* the handler. Filling it in here is safe: nothing
        // can fire before a remote description arrives, which is strictly later.
        *shared_connection.lock().await = Some(Arc::clone(&connection));

        let session = Self { connection, channel, events: events.clone() };

        if is_caller {
            let data_channel = session
                .connection
                .create_data_channel(
                    DATA_CHANNEL_LABEL,
                    Some(RTCDataChannelInit { ordered: true, ..Default::default() }),
                )
                .await?;

            *session.channel.lock().await = Some(Arc::clone(&data_channel));
            spawn_channel_pump(
                data_channel,
                Arc::clone(&session.connection),
                is_caller,
                events.clone(),
            );

            let offer = session.connection.create_offer(None).await?;
            session.connection.set_local_description(offer).await?;
            session.emit_local_description().await;
        }

        Ok((session, receiver))
    }

    async fn emit_local_description(&self) {
        if let Some(description) = self.connection.local_description().await {
            let _ = self.events.send(RtcEvent::LocalDescription(SessionDescription {
                kind: description.sdp_type.to_string(),
                sdp: description.sdp,
            }));
        }
    }

    /// Applies a peer's offer or answer. An offer is answered automatically, and the
    /// answer arrives on the event channel as a `LocalDescription`.
    pub async fn accept_remote_description(
        &self,
        description: &SessionDescription,
    ) -> Result<(), RtcError> {
        let remote = match description.kind.as_str() {
            "offer" => RTCSessionDescription::offer(description.sdp.clone())?,
            "answer" => RTCSessionDescription::answer(description.sdp.clone())?,
            _ => RTCSessionDescription::pranswer(description.sdp.clone())?,
        };
        self.connection.set_remote_description(remote).await?;

        if description.kind == "offer" {
            let answer = self.connection.create_answer(None).await?;
            self.connection.set_local_description(answer).await?;
            self.emit_local_description().await;
        }
        Ok(())
    }

    pub async fn add_ice_candidate(&self, ice: &IceCandidate) -> Result<(), RtcError> {
        self.connection
            .add_ice_candidate(webrtc::peer_connection::RTCIceCandidateInit {
                candidate: ice.candidate.clone(),
                sdp_mid: ice.sdp_mid.clone(),
                sdp_mline_index: ice.sdp_mline_index,
                username_fragment: ice.username_fragment.clone(),
                // Only meaningful for local candidates we gathered ourselves.
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    pub async fn send_text(&self, text: &str) -> Result<(), RtcError> {
        let guard = self.channel.lock().await;
        let channel = guard.as_ref().ok_or(RtcError::NotOpen)?;
        channel.send_text(text).await?;
        Ok(())
    }

    /// Sends one binary chunk. PairDrop's chunks are 64,000 bytes, comfortably under
    /// the 65,536-byte SCTP message size both ends negotiate by default.
    pub async fn send_binary(&self, bytes: &[u8]) -> Result<(), RtcError> {
        let guard = self.channel.lock().await;
        let channel = guard.as_ref().ok_or(RtcError::NotOpen)?;
        channel.send(BytesMut::from(bytes)).await?;
        Ok(())
    }

    /// Resolves once the send buffer has drained enough to accept more. The transfer
    /// loop awaits this so a fast reader can't outrun what SCTP will take.
    pub async fn writable(&self) -> Result<(), RtcError> {
        let guard = self.channel.lock().await;
        let channel = guard.as_ref().ok_or(RtcError::NotOpen)?;
        channel.writable().await?;
        Ok(())
    }

    pub async fn close(&self) {
        let _ = self.connection.close().await;
    }
}

// MARK: events from the connection

#[derive(Clone)]
struct Handler {
    events: mpsc::UnboundedSender<RtcEvent>,
    channel: SharedChannel,
    connection: SharedConnection,
    is_caller: bool,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        // Trickle ICE: send each candidate as it turns up rather than waiting for
        // gathering to finish, which is what the web client expects.
        let Ok(init) = event.candidate.to_json() else { return };
        let _ = self.events.send(RtcEvent::LocalCandidate(IceCandidate {
            candidate: init.candidate,
            sdp_mid: init.sdp_mid,
            sdp_mline_index: init.sdp_mline_index,
            username_fragment: init.username_fragment,
        }));
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        match state {
            // `Disconnected` is usually transient — ICE often recovers on its own — so
            // only `Failed` asks the caller to rebuild the session.
            RTCPeerConnectionState::Failed => {
                let _ = self.events.send(RtcEvent::Failed);
            }
            RTCPeerConnectionState::Closed => {
                let _ = self.events.send(RtcEvent::Closed);
            }
            _ => {}
        }
    }

    async fn on_data_channel(&self, data_channel: Arc<dyn DataChannel>) {
        // The answerer's side: the caller opened it, we take whatever arrives.
        *self.channel.lock().await = Some(Arc::clone(&data_channel));

        let Some(connection) = self.connection.lock().await.clone() else { return };
        spawn_channel_pump(data_channel, connection, self.is_caller, self.events.clone());
    }
}

/// Messages are polled rather than pushed, so each channel gets a task that drains it
/// into our event stream.
fn spawn_channel_pump(
    channel: Arc<dyn DataChannel>,
    connection: Arc<dyn PeerConnection>,
    is_caller: bool,
    events: mpsc::UnboundedSender<RtcEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = channel.poll().await {
            let forwarded = match event {
                DataChannelEvent::OnOpen => {
                    let hash = connection_hash(&connection, is_caller).await.unwrap_or_default();
                    RtcEvent::Open { connection_hash: hash }
                }
                DataChannelEvent::OnMessage(message) => {
                    // Control frames are JSON text, file contents are binary, and the
                    // receiver has to keep them apart — so carry the distinction.
                    if message.is_string {
                        match String::from_utf8(message.data.to_vec()) {
                            Ok(text) => RtcEvent::Text(text),
                            Err(_) => continue,
                        }
                    } else {
                        RtcEvent::Binary(message.data.to_vec())
                    }
                }
                DataChannelEvent::OnClose => {
                    let _ = events.send(RtcEvent::Closed);
                    break;
                }
                _ => continue,
            };

            if events.send(forwarded).is_err() {
                break; // nobody is listening any more
            }
        }
    });
}

/// The 16-digit code both peers display to confirm they're talking to each other.
///
/// It is `cyrb53` of the two DTLS fingerprints concatenated **in call order** — the
/// caller's first — so the two ends must agree on who called, or they show different
/// codes and the check is worse than useless.
async fn connection_hash(connection: &Arc<dyn PeerConnection>, is_caller: bool) -> Option<String> {
    let local = fingerprint(&connection.local_description().await?.sdp)?;
    let remote = fingerprint(&connection.remote_description().await?.sdp)?;

    let combined = if is_caller {
        format!("{local}{remote}")
    } else {
        format!("{remote}{local}")
    };
    Some(pairdrop_proto::connection_hash(&combined))
}

/// Everything after `a=fingerprint:` on the first such line, e.g. `sha-256 AB:CD:…`.
fn fingerprint(sdp: &str) -> Option<String> {
    sdp.split("\r\n")
        .find_map(|line| line.strip_prefix("a=fingerprint:"))
        .map(str::to_string)
}

fn ice_servers(config: &RtcConfig) -> Vec<RTCIceServer> {
    config
        .ice_servers
        .iter()
        .map(|server| RTCIceServer {
            urls: server.urls.clone(),
            username: server.username.clone().unwrap_or_default(),
            credential: server.credential.clone().unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_fingerprint_line() {
        let sdp = "v=0\r\na=group:BUNDLE 0\r\na=fingerprint:sha-256 AB:CD:EF\r\na=setup:actpass\r\n";
        assert_eq!(fingerprint(sdp).as_deref(), Some("sha-256 AB:CD:EF"));
    }

    #[test]
    fn missing_fingerprint_is_none() {
        assert!(fingerprint("v=0\r\na=setup:actpass\r\n").is_none());
    }

    /// The order is not symmetric: caller's fingerprint first. Reversing it shows the
    /// two peers different codes, which reads as a failed verification rather than a bug.
    #[test]
    fn hash_order_depends_on_who_called() {
        let local = "sha-256 AB:CD:EF:01:23:45:67:89";
        let remote = "sha-256 98:76:54:32:10:FE:DC:BA";

        let as_caller = pairdrop_proto::connection_hash(&format!("{local}{remote}"));
        let as_answerer = pairdrop_proto::connection_hash(&format!("{remote}{local}"));
        assert_ne!(as_caller, as_answerer);

        // Pinned to the value the web client's cyrb53 gives for this pair, so the
        // concatenation format can't drift.
        assert_eq!(as_caller, "8763102360577714");
        assert_eq!(as_caller.len(), 16);
    }
}
