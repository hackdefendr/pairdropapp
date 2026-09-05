//! The WebSocket half of a PairDrop client: connect, stay connected, and turn frames
//! into events.
//!
//! The server pings every second and drops peers that go five seconds without a pong,
//! so the receive loop answers `ping` inline and never surfaces it. Everything else is
//! handed to the caller over a channel.

mod tls;

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use pairdrop_proto::{ClientMessage, InstanceConfig, ServerEndpoint, ServerMessage, WsConfig};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

pub use tls::TlsError;

/// How the connection is doing, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalingState {
    Connecting,
    Connected,
    WaitingToRetry { seconds: u64 },
    Failed(String),
    Closed,
}

/// What the caller receives. `Ping` is handled internally and never appears here.
#[derive(Debug, Clone)]
pub enum SignalingEvent {
    State(SignalingState),
    /// Identity assigned by the server, reused across reconnects so peers see one device.
    Identity {
        peer_id: String,
        peer_id_hash: String,
        display_name: Option<String>,
        device_name: Option<String>,
    },
    /// What the instance tells clients about connectivity: the ICE servers to use and
    /// whether it will relay transfers for peers that can't establish a data channel.
    WsConfig(WsConfig),
    Message(ServerMessage),
}

#[derive(Debug, Clone)]
pub struct SignalingConfig {
    pub endpoint: ServerEndpoint,
    pub user_agent: String,
    /// Opt-in escape hatch for a self-hosted instance behind a self-signed certificate.
    pub allow_untrusted_tls: bool,
    /// Stop after this many consecutive failures. `None` retries forever.
    pub max_attempts: Option<u32>,
}

impl SignalingConfig {
    pub fn new(endpoint: ServerEndpoint) -> Self {
        Self {
            endpoint,
            // The server labels us `os.name + " " + (device.model ?? browser.name)` with
            // no guard for the undefined case (server/peer.js), and ua-parser-js has no
            // generic-browser fallback — so this reads as "Linux undefined" server-side.
            // Nothing avoids that short of impersonating a real browser, and it barely
            // matters: peers see the *display* name, and we send `display-name-changed`
            // with the real hostname once the data channel is open.
            user_agent: format!("PairDrop/{} (X11; Linux x86_64)", env!("CARGO_PKG_VERSION")),
            allow_untrusted_tls: false,
            max_attempts: None,
        }
    }
}

/// Handle to a running connection. Dropping it closes the socket.
pub struct SignalingHandle {
    commands: mpsc::UnboundedSender<Command>,
}

enum Command {
    Send(ClientMessage),
    Shutdown,
}

impl SignalingHandle {
    pub fn send(&self, message: ClientMessage) {
        let _ = self.commands.send(Command::Send(message));
    }

    /// Sends the polite `disconnect` frame so the server tears our rooms down at once,
    /// then closes without retrying.
    pub fn shutdown(&self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

/// Starts the connection. Events arrive on the returned receiver until the socket is
/// closed for good.
pub fn connect(config: SignalingConfig) -> (SignalingHandle, mpsc::UnboundedReceiver<SignalingEvent>) {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();

    tokio::spawn(run(config, command_rx, event_tx));

    (SignalingHandle { commands: command_tx }, event_rx)
}

async fn run(
    config: SignalingConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<SignalingEvent>,
) {
    let http = match tls::http_client(&config) {
        Ok(client) => client,
        Err(error) => {
            let _ = events.send(SignalingEvent::State(SignalingState::Failed(error.to_string())));
            return;
        }
    };
    let connector = match tls::ws_connector(&config) {
        Ok(connector) => connector,
        Err(error) => {
            let _ = events.send(SignalingEvent::State(SignalingState::Failed(error.to_string())));
            return;
        }
    };

    // Kept across reconnects so the server hands back the same identity and peers see
    // one device rather than a new one each time the network blips.
    let mut peer_id: Option<String> = None;
    let mut peer_id_hash: Option<String> = None;
    let mut attempt: u32 = 0;

    loop {
        let _ = events.send(SignalingEvent::State(SignalingState::Connecting));

        // An unreachable /config is not fatal — it only means the instance doesn't
        // delegate signaling elsewhere. A real outage surfaces on the socket below.
        let signaling_server = fetch_config(&http, &config).await.and_then(|c| c.signaling_server);

        let url = match config.endpoint.websocket_url(
            signaling_server.as_deref(),
            peer_id.as_deref(),
            peer_id_hash.as_deref(),
        ) {
            Some(url) => url,
            None => {
                let _ = events.send(SignalingEvent::State(SignalingState::Failed(
                    "Could not build a WebSocket URL for this server address.".into(),
                )));
                return;
            }
        };

        let outcome = session(
            &url,
            &config,
            Arc::clone(&connector),
            &mut commands,
            &events,
            &mut peer_id,
            &mut peer_id_hash,
            &mut attempt,
        )
        .await;

        match outcome {
            Outcome::ShutdownRequested => {
                let _ = events.send(SignalingEvent::State(SignalingState::Closed));
                return;
            }
            Outcome::Dropped(reason) => {
                attempt = attempt.saturating_add(1);
                if config.max_attempts.is_some_and(|max| attempt >= max) {
                    let _ = events.send(SignalingEvent::State(SignalingState::Failed(reason)));
                    return;
                }
                // Capped exponential backoff, matching the macOS client.
                let seconds = 2u64.saturating_pow(attempt.min(5)).min(30);
                let _ = events.send(SignalingEvent::State(SignalingState::WaitingToRetry { seconds }));

                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(seconds)) => {}
                    command = commands.recv() => {
                        if matches!(command, Some(Command::Shutdown) | None) {
                            let _ = events.send(SignalingEvent::State(SignalingState::Closed));
                            return;
                        }
                    }
                }
            }
        }
    }
}

enum Outcome {
    ShutdownRequested,
    Dropped(String),
}

#[allow(clippy::too_many_arguments)]
async fn session(
    url: &url::Url,
    config: &SignalingConfig,
    connector: Arc<rustls::ClientConfig>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &mpsc::UnboundedSender<SignalingEvent>,
    peer_id: &mut Option<String>,
    peer_id_hash: &mut Option<String>,
    attempt: &mut u32,
) -> Outcome {
    let request = match tls::ws_request(url, &config.user_agent) {
        Ok(request) => request,
        Err(error) => return Outcome::Dropped(error.to_string()),
    };

    let stream = tokio_tungstenite::connect_async_tls_with_config(
        request,
        None,
        false,
        Some(tokio_tungstenite::Connector::Rustls(connector)),
    )
    .await;

    let (mut socket, _) = match stream {
        Ok(pair) => pair,
        Err(error) => return Outcome::Dropped(format!("Couldn't reach the server: {error}")),
    };

    *attempt = 0;
    let _ = events.send(SignalingEvent::State(SignalingState::Connected));

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Send(message)) => {
                    if socket.send(WsMessage::Text(message.encode().into())).await.is_err() {
                        return Outcome::Dropped("Send failed.".into());
                    }
                }
                // Tell the server before going, so it drops us from rooms immediately
                // rather than waiting out the five-second timeout.
                Some(Command::Shutdown) | None => {
                    let _ = socket.send(WsMessage::Text(ClientMessage::Disconnect.encode().into())).await;
                    let _ = socket.close(None).await;
                    return Outcome::ShutdownRequested;
                }
            },

            frame = socket.next() => {
                let Some(frame) = frame else {
                    return Outcome::Dropped("The server closed the connection.".into());
                };
                let payload = match frame {
                    Ok(WsMessage::Text(text)) => text.as_bytes().to_vec(),
                    Ok(WsMessage::Binary(bytes)) => bytes.to_vec(),
                    // tokio-tungstenite answers protocol-level pings for us; PairDrop's
                    // own keepalive is the JSON `ping` handled below.
                    Ok(WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_)) => continue,
                    Ok(WsMessage::Close(_)) => {
                        return Outcome::Dropped("The server closed the connection.".into());
                    }
                    Err(error) => return Outcome::Dropped(format!("Connection lost: {error}")),
                };

                let Some(message) = ServerMessage::parse(&payload) else { continue };

                match &message {
                    // Pure keepalive: answer inline and never surface it. Missing five
                    // of these in a row gets us dropped from every room.
                    ServerMessage::Ping => {
                        if socket.send(WsMessage::Text(ClientMessage::Pong.encode().into())).await.is_err() {
                            return Outcome::Dropped("Send failed.".into());
                        }
                        continue;
                    }
                    ServerMessage::DisplayName { peer_id: id, peer_id_hash: hash, display_name, device_name } => {
                        *peer_id = Some(id.clone());
                        *peer_id_hash = Some(hash.clone());
                        let _ = events.send(SignalingEvent::Identity {
                            peer_id: id.clone(),
                            peer_id_hash: hash.clone(),
                            display_name: display_name.clone(),
                            device_name: device_name.clone(),
                        });
                    }
                    ServerMessage::WsConfig(ws_config) => {
                        let _ = events.send(SignalingEvent::WsConfig(ws_config.clone()));
                    }
                    _ => {}
                }

                if events.send(SignalingEvent::Message(message)).is_err() {
                    // Nobody is listening any more.
                    let _ = socket.close(None).await;
                    return Outcome::ShutdownRequested;
                }
            }
        }
    }
}

async fn fetch_config(client: &reqwest::Client, config: &SignalingConfig) -> Option<InstanceConfig> {
    let response = client
        .get(config.endpoint.config_url())
        .header("User-Agent", &config.user_agent)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;

    if !response.status().is_success() {
        return None;
    }
    response.json::<InstanceConfig>().await.ok()
}
