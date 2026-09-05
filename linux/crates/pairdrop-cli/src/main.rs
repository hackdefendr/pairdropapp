//! A headless PairDrop peer, for testing the client against a real instance without a
//! GUI — and for diagnosing an instance that peers can't connect through.
//!
//! It connects, joins the IP room, and opens a data channel to every peer it finds,
//! reporting the verification hash for each. With `--send` it transfers files; incoming
//! transfers are always accepted, which is what makes it useful as a test receiver.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use pairdrop_net::{connect, SignalingConfig, SignalingEvent, SignalingHandle, SignalingState};
use pairdrop_proto::{
    ClientMessage, RoomRef, RoomType, RtcConfig, ServerEndpoint, ServerMessage, WsConfig,
};
use pairdrop_rtc::{RtcEvent, RtcSession};
use pairdrop_transfer::{Channel, Transfer, TransferError, TransferEvent};
use tokio::sync::mpsc;

/// Bridges the data channel to the transfer state machine. A newtype because both the
/// trait and `RtcSession` live in other crates.
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

#[derive(Parser, Debug)]
#[command(name = "pairdrop-probe", about = "Headless PairDrop peer and instance diagnostic")]
struct Args {
    /// Instance address, e.g. https://drop.example.com or 192.168.1.50:3000
    server: String,

    /// The name other devices see, announced once a data channel opens.
    #[arg(long, default_value = "pairdrop-probe")]
    name: String,

    /// Connect to peers rather than only listing them.
    #[arg(long)]
    dial: bool,

    /// Files to send once connected. Implies --dial.
    #[arg(long, value_name = "FILE", num_args = 1..)]
    send: Vec<PathBuf>,

    /// Send only to the peer whose name contains this, rather than to everyone.
    #[arg(long, value_name = "NAME")]
    to: Option<String>,

    /// Where received files land.
    #[arg(long, value_name = "DIR", default_value = "./pairdrop-received")]
    out: PathBuf,

    /// Send this as a text message once connected.
    #[arg(long, value_name = "TEXT")]
    text: Option<String>,

    /// Trust a self-signed certificate. Only for an instance on your own network.
    #[arg(long)]
    allow_untrusted_tls: bool,

    /// Exit after this many seconds. Runs until interrupted when omitted.
    #[arg(long, value_name = "SECONDS")]
    quit_after: Option<u64>,

    /// Give up after this many consecutive connection failures.
    #[arg(long, value_name = "N")]
    max_attempts: Option<u32>,
}

/// One peer we're talking to, plus the role we took. The role is not negotiable: it
/// decides who creates the data channel and the order the verification hash is built in.
struct Peer {
    session: Arc<RtcSession>,
    transfer: Transfer,
    label: String,
    is_caller: bool,
    /// Set once we've sent to this peer, so a reconnect doesn't send everything twice.
    sent: bool,
    /// Last percentage printed, so a burst of progress events doesn't repeat a line.
    last_percent: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let endpoint = ServerEndpoint::parse(&args.server)
        .with_context(|| format!("{:?} is not a usable server address", args.server))?;

    // rustls needs a provider installed before any TLS happens, and choosing it here
    // rather than relying on a default keeps it explicit.
    rustls::crypto::aws_lc_rs::default_provider().install_default().ok();

    println!("Server:   {}", endpoint.base());
    println!("Config:   {}", endpoint.config_url());

    let mut config = SignalingConfig::new(endpoint);
    config.allow_untrusted_tls = args.allow_untrusted_tls;
    config.max_attempts = args.max_attempts;

    let (handle, mut events) = connect(config);

    // Every peer session funnels its events here, tagged with whose they are.
    let (rtc_tx, mut rtc_rx) = mpsc::unbounded_channel::<(String, RtcEvent)>();

    let mut peers: HashMap<String, Peer> = HashMap::new();
    let mut rtc_config = RtcConfig::default();
    let mut room = RoomRef::new(RoomType::Ip, String::new());
    let mut joined = false;
    let mut failure: Option<String> = None;

    let deadline = args.quit_after.map(|s| tokio::time::Instant::now() + Duration::from_secs(s));
    // Both quit paths are edge-triggered: an elapsed deadline stays elapsed, so without
    // this the timer arm would fire on every pass and spin.
    let mut stopping = false;

    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { break };
                match event {
                    SignalingEvent::State(state) => match state {
                        SignalingState::Connecting => println!("… connecting"),
                        SignalingState::Connected => println!("✓ connected"),
                        SignalingState::WaitingToRetry { seconds } => {
                            println!("… dropped, retrying in {seconds}s");
                            // Sessions built against the old socket can't be signalled
                            // any more; the peers list is resent on reconnect.
                            peers.clear();
                        }
                        SignalingState::Failed(reason) => {
                            println!("✗ {reason}");
                            failure = Some(reason);
                            break;
                        }
                        SignalingState::Closed => break,
                    },

                    SignalingEvent::Identity { peer_id, display_name, device_name, .. } => {
                        println!("Identity: {peer_id}");
                        println!("  the server calls us {display_name:?} / {device_name:?}");
                        // The web client joins the IP room once it has an identity, and
                        // so must we — peers are scoped to a room, not to the socket.
                        if !joined {
                            handle.send(ClientMessage::JoinIpRoom);
                            joined = true;
                        }
                    }

                    SignalingEvent::WsConfig(ws_config) => {
                        if let Some(rtc) = ws_config.rtc_config.clone() {
                            if !rtc.ice_servers.is_empty() {
                                rtc_config = rtc;
                            }
                        }
                        report_connectivity(&ws_config);
                    }

                    SignalingEvent::Message(message) => {
                        handle_server_message(
                            message, &args, &rtc_tx, &mut peers,
                            &rtc_config, &mut room,
                        ).await;
                    }
                }
            }

            Some((peer_id, event)) = rtc_rx.recv() => {
                handle_rtc_event(peer_id, event, &args, &handle, &mut peers, &room).await;
            }

            _ = tokio::signal::ctrl_c(), if !stopping => {
                println!("\nInterrupted.");
                stopping = true;
                handle.shutdown();
            }

            _ = tokio::time::sleep_until(deadline.unwrap_or_else(tokio::time::Instant::now)),
                if deadline.is_some() && !stopping =>
            {
                println!("\nTime's up.");
                stopping = true;
                handle.shutdown();
            }
        }
    }

    for peer in peers.values() {
        peer.session.close().await;
    }

    if let Some(reason) = failure {
        bail!(reason);
    }
    Ok(())
}

async fn handle_server_message(
    message: ServerMessage,
    args: &Args,
    rtc_tx: &mpsc::UnboundedSender<(String, RtcEvent)>,
    peers: &mut HashMap<String, Peer>,
    rtc_config: &RtcConfig,
    room: &mut RoomRef,
) {
    match message {
        ServerMessage::Peers { peers: list, room: peers_room } => {
            *room = peers_room;
            println!("Room {} ({}): {} peer(s)", room.id, room.kind.as_str(), list.len());
            for peer in list {
                let note = if peer.rtc_supported { "" } else { "  (no WebRTC — needs the ws fallback)" };
                println!("  • {} [{}]{note}", peer.name.best_label(), peer.id);

                // Peers already in the room when we arrive are the ones *we* call.
                if args.dial && peer.rtc_supported && !peers.contains_key(&peer.id) {
                    dial(peer.id.clone(), peer.name.best_label(), true, rtc_config, rtc_tx, peers, &args.out).await;
                }
            }
        }

        // A peer that arrives after us calls *us*, so there is nothing to do until
        // their offer shows up.
        ServerMessage::PeerJoined { peer, .. } => {
            println!("+ {} [{}]", peer.name.best_label(), peer.id);
        }

        ServerMessage::PeerLeft { peer_id, disconnect, .. } => {
            println!("- {peer_id}{}", if disconnect { " (disconnected)" } else { "" });
            if let Some(peer) = peers.remove(&peer_id) {
                peer.session.close().await;
            }
        }

        ServerMessage::Signal { sender, sdp, ice, .. } => {
            if !args.dial {
                println!("~ signal from {} (ignored — pass --dial to answer)", sender.id);
                return;
            }

            // No session yet means they called us, so we answer.
            if !peers.contains_key(&sender.id) {
                dial(sender.id.clone(), sender.id.clone(), false, rtc_config, rtc_tx, peers, &args.out).await;
            }
            let Some(peer) = peers.get(&sender.id) else { return };

            if let Some(sdp) = sdp {
                if let Err(error) = peer.session.accept_remote_description(&sdp).await {
                    println!("✗ {} rejected our {}: {error}", sender.id, sdp.kind);
                }
            } else if let Some(ice) = ice {
                // A candidate can arrive before the description it belongs to; the
                // library queues those, so a failure here is not worth reporting loudly.
                let _ = peer.session.add_ice_candidate(&ice).await;
            }
        }

        ServerMessage::Unknown { kind } => println!("? unhandled frame {kind:?}"),
        // Already reported through the Identity and WsConfig events.
        ServerMessage::DisplayName { .. } | ServerMessage::WsConfig(_) => {}
        other => println!("· {other:?}"),
    }
}

/// Builds a session for one peer and starts forwarding its events.
#[allow(clippy::too_many_arguments)]
async fn dial(
    peer_id: String,
    label: String,
    is_caller: bool,
    rtc_config: &RtcConfig,
    rtc_tx: &mpsc::UnboundedSender<(String, RtcEvent)>,
    peers: &mut HashMap<String, Peer>,
    out: &std::path::Path,
) {
    let (session, mut receiver) = match RtcSession::new(rtc_config, is_caller).await {
        Ok(pair) => pair,
        Err(error) => {
            println!("✗ couldn't set up a connection to {label}: {error}");
            return;
        }
    };

    let tx = rtc_tx.clone();
    let id = peer_id.clone();
    tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            if tx.send((id.clone(), event)).is_err() {
                break;
            }
        }
    });

    let session = Arc::new(session);
    let mut transfer = Transfer::new(Box::new(RtcChannel(Arc::clone(&session))), out.to_path_buf());
    // A probe with no UI has no way to prompt, and being a willing receiver is the
    // point of it.
    transfer.auto_accept = true;

    println!("→ {label}: {} …", if is_caller { "calling" } else { "answering" });
    peers.insert(peer_id, Peer { session, transfer, label, is_caller, sent: false, last_percent: 0 });
}

async fn handle_rtc_event(
    peer_id: String,
    event: RtcEvent,
    args: &Args,
    handle: &SignalingHandle,
    peers: &mut HashMap<String, Peer>,
    room: &RoomRef,
) {
    let Some(peer) = peers.get_mut(&peer_id) else { return };

    match event {
        RtcEvent::LocalDescription(sdp) => {
            handle.send(ClientMessage::SignalSdp {
                to: peer_id.clone(),
                room: room.clone(),
                sdp,
            });
        }

        RtcEvent::LocalCandidate(ice) => {
            handle.send(ClientMessage::SignalIce { to: peer_id.clone(), room: room.clone(), ice });
        }

        RtcEvent::Open { connection_hash } => {
            println!(
                "✓ {} connected as {} — verification {connection_hash}",
                peer.label,
                if peer.is_caller { "caller" } else { "answerer" }
            );

            // The server can't derive a useful name for a native client, so this is what
            // actually makes us show up with a real name on the other side.
            if let Err(error) = peer.transfer.announce_name(&args.name).await {
                println!("  couldn't announce our name: {error}");
            }

            if let Some(text) = &args.text {
                if let Err(error) = peer.transfer.send_text(text).await {
                    println!("  couldn't send the message: {error}");
                }
            }

            maybe_send(peer, args).await;
        }

        RtcEvent::Text(text) => {
            let mut events = Vec::new();
            if let Err(error) = peer.transfer.on_text(&text, &mut events).await {
                println!("✗ {} transfer error: {error}", peer.label);
            }
            let label = peer.label.clone();
            report_transfer(&label, &mut peer.last_percent, &events);

            // A peer that renames itself should be shown under the new name — and
            // `--to` matches on that name, which only arrives after the channel opens.
            // Retrying the send here is what makes name-based targeting work at all.
            if let Some(name) = events.iter().find_map(|e| match e {
                TransferEvent::PeerNameChanged(name) => Some(name.clone()),
                _ => None,
            }) {
                if let Some(peer) = peers.get_mut(&peer_id) {
                    peer.label = name;
                    maybe_send(peer, args).await;
                }
            }
        }

        RtcEvent::Binary(bytes) => {
            let mut events = Vec::new();
            if let Err(error) = peer.transfer.on_binary(&bytes, &mut events).await {
                println!("✗ {} transfer error: {error}", peer.label);
            }
            report_transfer(&peer.label, &mut peer.last_percent, &events);
        }

        RtcEvent::Failed => {
            println!("✗ {} — ICE failed, no path between us", peer.label);
            if let Some(mut peer) = peers.remove(&peer_id) {
                peer.transfer.reset();
                peer.session.close().await;
            }
        }

        RtcEvent::Closed => {
            println!("· {} closed", peer.label);
            if let Some(mut peer) = peers.remove(&peer_id) {
                peer.transfer.reset();
            }
        }
    }
}

/// Sends the queued files if this is the peer we were asked for, at most once.
///
/// Called both when the channel opens and when the peer announces its name, because
/// `--to` matches the announced name and that arrives strictly after the channel is up.
async fn maybe_send(peer: &mut Peer, args: &Args) {
    if args.send.is_empty() || peer.sent {
        return;
    }
    let wanted = args
        .to
        .as_ref()
        .is_none_or(|want| peer.label.to_lowercase().contains(&want.to_lowercase()));
    if !wanted {
        return;
    }

    peer.sent = true;
    println!("→ {} sending {} file(s) …", peer.label, args.send.len());
    let mut events = Vec::new();
    match peer.transfer.send_files(&args.send, &mut events).await {
        Ok(skipped) if !skipped.is_empty() => {
            println!("  skipped {} (empty files and folders)", skipped.join(", "));
        }
        Ok(_) => {}
        Err(error) => println!("  {error}"),
    }
    report_transfer(&peer.label, &mut peer.last_percent, &events);
}

/// Prints the interesting transfer events, skipping the progress firehose except at
/// round percentages.
fn report_transfer(label: &str, last_percent: &mut u32, events: &[TransferEvent]) {
    for event in events {
        match event {
            TransferEvent::RequestReceived(request) => {
                println!("← {label} wants to send {} file(s)", request.header.len());
            }
            TransferEvent::SendProgress(p) | TransferEvent::ReceiveProgress(p) => {
                let percent = (p * 100.0).round() as u32;
                if percent % 25 == 0 && percent > *last_percent {
                    *last_percent = percent;
                    println!("  {label} {percent}%");
                }
            }
            TransferEvent::FilesReceived(files) => {
                *last_percent = 0;
                println!("✓ received {} file(s) from {label}:", files.len());
                for file in files {
                    let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
                    println!("    {} ({size} bytes)", file.display());
                }
            }
            TransferEvent::TextReceived(text) => println!("← {label}: {text}"),
            TransferEvent::SendingFinished { files } => {
                *last_percent = 0;
                println!("✓ sent {files} file(s) to {label}");
            }
            TransferEvent::Declined { reason } => {
                println!("✗ {label} declined{}", reason.as_deref().map(|r| format!(" ({r})")).unwrap_or_default());
            }
            TransferEvent::Failed(message) => println!("✗ {label}: {message}"),
            TransferEvent::PeerNameChanged(name) => println!("· {label} is called {name:?}"),
        }
    }
}

/// The diagnosis that matters when peers show "couldn't connect": with STUN only and no
/// fallback, two peers that can't reach each other directly have no path at all, and no
/// client can fix it — the web client fails the same way.
fn report_connectivity(config: &WsConfig) {
    let rtc = config.rtc_config.clone().unwrap_or_default();

    let mut stun = 0usize;
    let mut turn = 0usize;
    for server in &rtc.ice_servers {
        for url in &server.urls {
            if url.starts_with("turn:") || url.starts_with("turns:") {
                turn += 1;
            } else if url.starts_with("stun:") || url.starts_with("stuns:") {
                stun += 1;
            }
        }
    }

    let fallback = config.ws_fallback.unwrap_or(false);
    println!("ICE:      {stun} STUN, {turn} TURN; ws fallback {}", if fallback { "on" } else { "off" });
    for server in &rtc.ice_servers {
        println!("  • {}", server.urls.join(", "));
    }

    if turn == 0 && !fallback {
        println!(
            "  ⚠ STUN only and no fallback: peers that can't reach each other directly\n    \
             have no path. Add a TURN server, or run the instance with --include-ws-fallback."
        );
    }
}
