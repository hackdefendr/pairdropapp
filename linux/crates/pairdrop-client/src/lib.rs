//! The whole client, minus a user interface.
//!
//! Owns the signaling connection, one WebRTC session and transfer state machine per
//! peer, and the pairing store. A UI drives it with [`Command`]s and renders
//! [`Event`]s — it never touches the network directly.
//!
//! Runs on its own tokio runtime in a background thread, so a GUI toolkit's main loop
//! keeps the main thread.

pub mod settings;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use pairdrop_net::{connect, SignalingConfig, SignalingEvent, SignalingHandle, SignalingState};
use pairdrop_pairing::{best_available, Pairing, PairedDevice, PairingEvent};
use pairdrop_proto::{
    ClientMessage, RoomRef, RoomType, RtcConfig, ServerEndpoint, ServerMessage, TransferRequest,
};
use pairdrop_rtc::{RtcEvent, RtcSession};
use pairdrop_transfer::{Channel, Transfer, TransferError, TransferEvent};
use tokio::sync::mpsc;

pub use settings::Settings;

// MARK: what a UI sends and receives

#[derive(Debug, Clone)]
pub enum Command {
    /// Connect, or reconnect with changed settings.
    Connect(Settings),
    SendFiles { peer_id: String, paths: Vec<PathBuf> },
    SendText { peer_id: String, text: String },
    RespondToRequest { peer_id: String, accept: bool },
    BeginPairing,
    CancelPairing,
    JoinPairing { key: String },
    Unpair { secret: String },
    SetAutoAccept { secret: String, enabled: bool },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    NotConfigured,
    Connecting,
    Connected,
    Retrying { seconds: u64 },
    Failed(String),
}

/// Everything a UI needs to draw one device row.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerView {
    pub id: String,
    pub name: String,
    /// What kind of device, when the server could tell — "Linux Firefox" and such.
    pub detail: String,
    pub connected: bool,
    /// The 16-digit code both ends show, once a channel is open.
    pub connection_hash: Option<String>,
    pub paired: bool,
    pub busy: bool,
    pub progress: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Connection(ConnectionState),
    /// A whole snapshot. Simpler for a UI to apply than a stream of deltas, and the
    /// lists involved are a handful of rows.
    Peers(Vec<PeerView>),
    IncomingRequest {
        peer_id: String,
        peer_name: String,
        files: Vec<String>,
        total_size: i64,
    },
    FilesReceived { peer_name: String, paths: Vec<PathBuf> },
    TextReceived { peer_name: String, text: String },
    SendingFinished { peer_name: String, files: usize },
    /// Something worth putting in an activity list.
    Notice(String),
    Problem(String),
    PairingKey(String),
    PairingEnded,
    PairedDevices(Vec<PairedDevice>),
    /// Where secrets are being stored, and why not the keyring if that's the case.
    SecretStorage { description: String, problem: Option<String> },
}

/// Handle to the running engine.
pub struct Engine {
    commands: mpsc::UnboundedSender<Command>,
}

impl Engine {
    /// Starts the engine on its own thread. Events arrive on the returned channel,
    /// which a GTK main loop can await directly.
    pub fn start(settings: Settings) -> (Self, async_channel::Receiver<Event>) {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        // Unbounded so the engine never blocks on a UI that is slow to redraw.
        let (event_tx, event_rx) = async_channel::unbounded();

        std::thread::Builder::new()
            .name("pairdrop-engine".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = event_tx.send_blocking(Event::Problem(format!(
                            "Could not start the network engine: {error}"
                        )));
                        return;
                    }
                };
                runtime.block_on(run(settings, command_rx, event_tx));
            })
            .expect("spawning a thread");

        (Self { commands: command_tx }, event_rx)
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

// MARK: internals

struct Peer {
    session: Arc<RtcSession>,
    transfer: Transfer,
    view: PeerView,
    secret: Option<String>,
}

struct RtcChannel(Arc<RtcSession>);

#[async_trait::async_trait]
impl Channel for RtcChannel {
    async fn send_text(&self, text: &str) -> Result<(), TransferError> {
        self.0.send_text(text).await.map_err(|e| TransferError::Channel(e.to_string()))
    }
    async fn send_binary(&self, bytes: &[u8]) -> Result<(), TransferError> {
        self.0.send_binary(bytes).await.map_err(|e| TransferError::Channel(e.to_string()))
    }
}

struct State {
    settings: Settings,
    pairing: Pairing,
    peers: HashMap<String, Peer>,
    secret_for_peer: HashMap<String, String>,
    pending_requests: HashMap<String, TransferRequest>,
    rtc_config: RtcConfig,
    room: RoomRef,
    signaling: Option<SignalingHandle>,
    events: async_channel::Sender<Event>,
}

impl State {
    fn emit(&self, event: Event) {
        let _ = self.events.send_blocking(event);
    }

    fn publish_peers(&self) {
        let mut views: Vec<PeerView> = self.peers.values().map(|p| p.view.clone()).collect();
        // Connected first, then alphabetical, so the list doesn't jump around as peers
        // come and go.
        views.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| a.name.cmp(&b.name)));
        self.emit(Event::Peers(views));
    }

    fn publish_paired(&self) {
        self.emit(Event::PairedDevices(self.pairing.devices().to_vec()));
    }

    fn send_signal(&self, message: ClientMessage) {
        if let Some(handle) = &self.signaling {
            handle.send(message);
        }
    }
}

async fn run(
    settings: Settings,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: async_channel::Sender<Event>,
) {
    // rustls needs a provider installed before any TLS happens.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let (store, storage_problem) = best_available();
    let pairing = Pairing::new(store);
    let _ = events.send(Event::SecretStorage {
        description: pairing.store_description(),
        problem: storage_problem.map(|e| e.to_string()),
    })
    .await;

    let mut state = State {
        settings,
        pairing,
        peers: HashMap::new(),
        secret_for_peer: HashMap::new(),
        pending_requests: HashMap::new(),
        rtc_config: RtcConfig::default(),
        room: RoomRef::new(RoomType::Ip, String::new()),
        signaling: None,
        events,
    };
    state.publish_paired();

    let (rtc_tx, mut rtc_rx) = mpsc::unbounded_channel::<(String, RtcEvent)>();
    let mut signaling_events: Option<mpsc::UnboundedReceiver<SignalingEvent>> = None;

    // Connect straight away when a server is already configured.
    if !state.settings.server.trim().is_empty() {
        signaling_events = start_signaling(&mut state);
    } else {
        state.emit(Event::Connection(ConnectionState::NotConfigured));
    }

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                if matches!(command, Command::Shutdown) {
                    break;
                }
                if let Some(receiver) = handle_command(command, &mut state).await {
                    signaling_events = Some(receiver);
                }
            }

            Some(event) = async {
                match signaling_events.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                handle_signaling(event, &mut state, &rtc_tx).await;
            }

            Some((peer_id, event)) = rtc_rx.recv() => {
                handle_rtc(peer_id, event, &mut state).await;
            }
        }
    }

    for peer in state.peers.values() {
        peer.session.close().await;
    }
    if let Some(handle) = &state.signaling {
        handle.shutdown();
    }
}

fn start_signaling(state: &mut State) -> Option<mpsc::UnboundedReceiver<SignalingEvent>> {
    let Some(endpoint) = ServerEndpoint::parse(&state.settings.server) else {
        state.emit(Event::Connection(ConnectionState::Failed(format!(
            "{:?} is not a usable server address.",
            state.settings.server
        ))));
        return None;
    };

    let mut config = SignalingConfig::new(endpoint);
    config.allow_untrusted_tls = state.settings.allow_untrusted_tls;

    let (handle, receiver) = connect(config);
    state.signaling = Some(handle);
    Some(receiver)
}

async fn handle_command(
    command: Command,
    state: &mut State,
) -> Option<mpsc::UnboundedReceiver<SignalingEvent>> {
    match command {
        Command::Connect(settings) => {
            // Tear the old connection down first, or peers linger under stale sessions.
            for peer in state.peers.values() {
                peer.session.close().await;
            }
            state.peers.clear();
            state.pending_requests.clear();
            if let Some(handle) = state.signaling.take() {
                handle.shutdown();
            }

            state.settings = settings;
            state.publish_peers();

            if state.settings.server.trim().is_empty() {
                state.emit(Event::Connection(ConnectionState::NotConfigured));
                return None;
            }
            return start_signaling(state);
        }

        Command::SendFiles { peer_id, paths } => {
            let peer = state.peers.get_mut(&peer_id)?;
            if !peer.view.connected {
                let name = peer.view.name.clone();
                state.emit(Event::Problem(format!("Not connected to {name} yet.")));
                return None;
            }
            let mut events = Vec::new();
            let outcome = peer.transfer.send_files(&paths, &mut events).await;
            let name = peer.view.name.clone();
            match outcome {
                Ok(skipped) if !skipped.is_empty() => state.emit(Event::Notice(format!(
                    "Skipped {} — empty files and folders aren't supported.",
                    skipped.join(", ")
                ))),
                Ok(_) => {}
                Err(error) => state.emit(Event::Problem(format!("{name}: {error}"))),
            }
            apply_transfer_events(&peer_id, events, state);
        }

        Command::SendText { peer_id, text } => {
            let peer = state.peers.get(&peer_id)?;
            let name = peer.view.name.clone();
            if let Err(error) = peer.transfer.send_text(&text).await {
                state.emit(Event::Problem(format!("{name}: {error}")));
            } else {
                state.emit(Event::Notice(format!("Message sent to {name}.")));
            }
        }

        Command::RespondToRequest { peer_id, accept } => {
            state.pending_requests.remove(&peer_id);
            let peer = state.peers.get_mut(&peer_id)?;
            let mut events = Vec::new();
            let _ = peer.transfer.respond(accept, &mut events).await;
            apply_transfer_events(&peer_id, events, state);
        }

        Command::BeginPairing => {
            let message = state.pairing.begin();
            state.send_signal(message);
        }

        Command::CancelPairing => {
            let message = state.pairing.cancel();
            state.send_signal(message);
            state.emit(Event::PairingEnded);
        }

        Command::JoinPairing { key } => match state.pairing.join(&key) {
            Ok(message) => state.send_signal(message),
            Err(error) => state.emit(Event::Problem(error.to_string())),
        },

        Command::Unpair { secret } => {
            let outcome = state.pairing.unpair(&secret);
            for message in outcome.send {
                state.send_signal(message);
            }
            apply_pairing_events(outcome.events, state);
            state.publish_paired();
        }

        Command::SetAutoAccept { secret, enabled } => {
            let outcome = state.pairing.set_auto_accept(&secret, enabled);
            apply_pairing_events(outcome.events, state);
            // A peer already connected under this secret should pick the change up now.
            for peer in state.peers.values_mut() {
                if peer.secret.as_deref() == Some(secret.as_str()) {
                    peer.transfer.auto_accept = enabled;
                }
            }
            state.publish_paired();
        }

        Command::Shutdown => {}
    }
    None
}

async fn handle_signaling(
    event: SignalingEvent,
    state: &mut State,
    rtc_tx: &mpsc::UnboundedSender<(String, RtcEvent)>,
) {
    match event {
        SignalingEvent::State(signaling_state) => {
            let mapped = match signaling_state {
                SignalingState::Connecting => ConnectionState::Connecting,
                SignalingState::Connected => ConnectionState::Connected,
                SignalingState::WaitingToRetry { seconds } => {
                    // Sessions built against the dead socket can't be signalled any more.
                    for peer in state.peers.values() {
                        peer.session.close().await;
                    }
                    state.peers.clear();
                    state.publish_peers();
                    ConnectionState::Retrying { seconds }
                }
                SignalingState::Failed(reason) => ConnectionState::Failed(reason),
                SignalingState::Closed => ConnectionState::NotConfigured,
            };
            state.emit(Event::Connection(mapped));
        }

        SignalingEvent::Identity { .. } => {
            state.send_signal(ClientMessage::JoinIpRoom);
            // Paired rooms must be re-registered on every connection, or a paired
            // device stays invisible until the next pairing.
            if let Some(message) = state.pairing.room_secrets_message() {
                state.send_signal(message);
            }
        }

        SignalingEvent::WsConfig(config) => {
            if let Some(rtc) = config.rtc_config {
                if !rtc.ice_servers.is_empty() {
                    state.rtc_config = rtc;
                }
            }
            let has_turn = state.rtc_config.ice_servers.iter().any(|s| {
                s.urls.iter().any(|u| u.starts_with("turn:") || u.starts_with("turns:"))
            });
            if !has_turn && !config.ws_fallback.unwrap_or(false) {
                state.emit(Event::Notice(
                    "This instance offers STUN only. Devices that can't reach each other \
                     directly won't connect — it needs a TURN server."
                        .into(),
                ));
            }
        }

        SignalingEvent::Message(message) => {
            let outcome = state.pairing.handle(&message);
            for message in outcome.send {
                state.send_signal(message);
            }
            if !outcome.events.is_empty() {
                apply_pairing_events(outcome.events, state);
                state.publish_paired();
            }
            handle_server_message(message, state, rtc_tx).await;
        }
    }
}

async fn handle_server_message(
    message: ServerMessage,
    state: &mut State,
    rtc_tx: &mpsc::UnboundedSender<(String, RtcEvent)>,
) {
    match message {
        ServerMessage::Peers { peers, room } => {
            // An ip room is the signalling address for unpaired peers; a secret room
            // belongs to a pairing and must not replace it.
            if room.kind == RoomType::Ip {
                state.room = room.clone();
            }
            for info in peers {
                if !info.rtc_supported || state.peers.contains_key(&info.id) {
                    continue;
                }
                // Peers already in the room when we arrive are the ones we call.
                dial(&info.id, info.name.best_label(), describe(&info), true, &room, state, rtc_tx)
                    .await;
            }
            state.publish_peers();
        }

        ServerMessage::PeerJoined { peer, room } => {
            if room.kind == RoomType::Ip {
                state.room = room.clone();
            }
            // A peer arriving after us calls us, so there's nothing to do until their
            // offer shows up — but show them in the list straight away.
            if !state.peers.contains_key(&peer.id) && peer.rtc_supported {
                dial(&peer.id, peer.name.best_label(), describe(&peer), false, &room, state, rtc_tx)
                    .await;
            }
            state.publish_peers();
        }

        ServerMessage::PeerLeft { peer_id, .. } => {
            if let Some(peer) = state.peers.remove(&peer_id) {
                peer.session.close().await;
            }
            state.pending_requests.remove(&peer_id);
            state.publish_peers();
        }

        ServerMessage::Signal { sender, sdp, ice, .. } => {
            let Some(peer) = state.peers.get(&sender.id) else { return };
            if let Some(sdp) = sdp {
                let _ = peer.session.accept_remote_description(&sdp).await;
            } else if let Some(ice) = ice {
                // A candidate can arrive before the description it belongs to; the
                // library queues those.
                let _ = peer.session.add_ice_candidate(&ice).await;
            }
        }

        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn dial(
    peer_id: &str,
    name: String,
    detail: String,
    is_caller: bool,
    room: &RoomRef,
    state: &mut State,
    rtc_tx: &mpsc::UnboundedSender<(String, RtcEvent)>,
) {
    let (session, mut receiver) = match RtcSession::new(&state.rtc_config, is_caller).await {
        Ok(pair) => pair,
        Err(error) => {
            state.emit(Event::Problem(format!("Couldn't set up a connection to {name}: {error}")));
            return;
        }
    };

    let tx = rtc_tx.clone();
    let id = peer_id.to_string();
    tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            if tx.send((id.clone(), event)).is_err() {
                break;
            }
        }
    });

    let session = Arc::new(session);
    let mut transfer = Transfer::new(
        Box::new(RtcChannel(Arc::clone(&session))),
        state.settings.download_directory.clone(),
    );

    // A peer we reached through a secret room is one we paired with; honour the
    // auto-accept the user set for it.
    let secret = (room.kind == RoomType::Secret).then(|| room.id.clone());
    if let Some(secret) = &secret {
        transfer.auto_accept = state.pairing.auto_accepts(secret);
        state.secret_for_peer.insert(peer_id.to_string(), secret.clone());
    }

    state.peers.insert(
        peer_id.to_string(),
        Peer {
            session,
            transfer,
            secret,
            view: PeerView {
                id: peer_id.to_string(),
                name,
                detail,
                connected: false,
                connection_hash: None,
                paired: room.kind == RoomType::Secret,
                busy: false,
                progress: None,
            },
        },
    );
}

async fn handle_rtc(peer_id: String, event: RtcEvent, state: &mut State) {
    match event {
        RtcEvent::LocalDescription(sdp) => {
            let room = signalling_room(state, &peer_id);
            state.send_signal(ClientMessage::SignalSdp { to: peer_id, room, sdp });
        }

        RtcEvent::LocalCandidate(ice) => {
            let room = signalling_room(state, &peer_id);
            state.send_signal(ClientMessage::SignalIce { to: peer_id, room, ice });
        }

        RtcEvent::Open { connection_hash } => {
            let name = state.settings.effective_display_name();
            let Some(peer) = state.peers.get_mut(&peer_id) else { return };
            peer.view.connected = true;
            peer.view.connection_hash = Some(connection_hash);
            let _ = peer.transfer.announce_name(&name).await;
            state.publish_peers();
        }

        RtcEvent::Text(text) => {
            let Some(peer) = state.peers.get_mut(&peer_id) else { return };
            let mut events = Vec::new();
            let _ = peer.transfer.on_text(&text, &mut events).await;
            apply_transfer_events(&peer_id, events, state);
        }

        RtcEvent::Binary(bytes) => {
            let Some(peer) = state.peers.get_mut(&peer_id) else { return };
            let mut events = Vec::new();
            let _ = peer.transfer.on_binary(&bytes, &mut events).await;
            apply_transfer_events(&peer_id, events, state);
        }

        RtcEvent::Failed => {
            if let Some(peer) = state.peers.get_mut(&peer_id) {
                peer.view.connected = false;
                peer.view.progress = None;
                peer.transfer.reset();
                let name = peer.view.name.clone();
                state.emit(Event::Problem(format!("Couldn't connect to {name}.")));
            }
            state.publish_peers();
        }

        RtcEvent::Closed => {
            if let Some(peer) = state.peers.get_mut(&peer_id) {
                peer.view.connected = false;
                peer.view.progress = None;
                peer.transfer.reset();
            }
            state.publish_peers();
        }
    }
}

/// Signal through the paired room when there is one, so a paired device stays reachable
/// off the local network.
fn signalling_room(state: &State, peer_id: &str) -> RoomRef {
    match state.secret_for_peer.get(peer_id) {
        Some(secret) => RoomRef::new(RoomType::Secret, secret.clone()),
        None => state.room.clone(),
    }
}

fn apply_transfer_events(peer_id: &str, events: Vec<TransferEvent>, state: &mut State) {
    let name = state
        .peers
        .get(peer_id)
        .map(|p| p.view.name.clone())
        .unwrap_or_else(|| peer_id.to_string());

    let mut peers_changed = false;

    for event in events {
        match event {
            TransferEvent::RequestReceived(request) => {
                let files = request.header.iter().map(|f| f.name.clone()).collect();
                state.pending_requests.insert(peer_id.to_string(), request.clone());
                state.emit(Event::IncomingRequest {
                    peer_id: peer_id.to_string(),
                    peer_name: name.clone(),
                    files,
                    total_size: request.total_size,
                });
            }

            TransferEvent::SendProgress(p) | TransferEvent::ReceiveProgress(p) => {
                if let Some(peer) = state.peers.get_mut(peer_id) {
                    peer.view.progress = Some(p);
                    peer.view.busy = p < 1.0;
                    peers_changed = true;
                }
            }

            TransferEvent::FilesReceived(paths) => {
                if let Some(peer) = state.peers.get_mut(peer_id) {
                    peer.view.progress = None;
                    peer.view.busy = false;
                    peers_changed = true;
                }
                state.emit(Event::FilesReceived { peer_name: name.clone(), paths });
            }

            TransferEvent::TextReceived(text) => {
                state.emit(Event::TextReceived { peer_name: name.clone(), text });
            }

            TransferEvent::SendingFinished { files } => {
                if let Some(peer) = state.peers.get_mut(peer_id) {
                    peer.view.progress = None;
                    peer.view.busy = false;
                    peers_changed = true;
                }
                state.emit(Event::SendingFinished { peer_name: name.clone(), files });
            }

            TransferEvent::Declined { reason } => {
                if let Some(peer) = state.peers.get_mut(peer_id) {
                    peer.view.progress = None;
                    peer.view.busy = false;
                    peers_changed = true;
                }
                let detail = reason.map(|r| format!(" ({r})")).unwrap_or_default();
                state.emit(Event::Problem(format!("{name} declined the transfer{detail}.")));
            }

            TransferEvent::Failed(message) => {
                if let Some(peer) = state.peers.get_mut(peer_id) {
                    peer.view.progress = None;
                    peer.view.busy = false;
                    peers_changed = true;
                }
                state.emit(Event::Problem(format!("{name}: {message}")));
            }

            TransferEvent::PeerNameChanged(new_name) => {
                if let Some(peer) = state.peers.get_mut(peer_id) {
                    peer.view.name = new_name.clone();
                    peers_changed = true;
                }
                // A pairing frame only carries a peer id, so this is where a paired
                // device finally gets the name it calls itself.
                if let Some(secret) = state.secret_for_peer.get(peer_id).cloned() {
                    let outcome = state.pairing.set_display_name(&secret, &new_name);
                    apply_pairing_events(outcome.events, state);
                    state.publish_paired();
                }
            }
        }
    }

    if peers_changed {
        state.publish_peers();
    }
}

fn apply_pairing_events(events: Vec<PairingEvent>, state: &mut State) {
    for event in events {
        match event {
            PairingEvent::KeyReady { pair_key } => state.emit(Event::PairingKey(pair_key)),
            PairingEvent::Paired { secret, peer_id } => {
                state.secret_for_peer.insert(peer_id, secret);
                state.emit(Event::PairingEnded);
                state.emit(Event::Notice("Device paired.".into()));
            }
            PairingEvent::KeyInvalid => {
                state.emit(Event::Problem("That pairing key isn't valid.".into()));
            }
            PairingEvent::RateLimited => state.emit(Event::Problem(
                "Too many wrong keys — the server is refusing more attempts for now.".into(),
            )),
            PairingEvent::Canceled => state.emit(Event::PairingEnded),
            PairingEvent::Unpaired { display_name } => {
                state.emit(Event::Notice(format!("{display_name} removed this pairing.")));
            }
            PairingEvent::SecretRotated => {}
            PairingEvent::NotPersisted(reason) => state.emit(Event::Problem(format!(
                "The pairing works for now but couldn't be saved: {reason}"
            ))),
        }
    }
}

fn describe(peer: &pairdrop_proto::PeerInfo) -> String {
    let parts: Vec<&str> = [peer.name.os.as_deref(), peer.name.browser.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        parts.join(" ")
    }
}
