//! A headless PairDrop peer, for testing the client against a real instance without a
//! GUI — and for diagnosing an instance that peers can't connect through.
//!
//! Transfers aren't wired up yet: this connects, joins the IP room, and reports what
//! the server says. The WebRTC data channel is the next piece.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use pairdrop_net::{connect, SignalingConfig, SignalingEvent, SignalingState};
use pairdrop_proto::{ClientMessage, ServerEndpoint, ServerMessage, WsConfig};

#[derive(Parser, Debug)]
#[command(name = "pairdrop-probe", about = "Headless PairDrop peer and instance diagnostic")]
struct Args {
    /// Instance address, e.g. https://drop.example.com or 192.168.1.50:3000
    server: String,

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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let endpoint = ServerEndpoint::parse(&args.server)
        .with_context(|| format!("{:?} is not a usable server address", args.server))?;

    // rustls needs a provider installed before any TLS happens, and picking it here
    // rather than relying on a default keeps the choice explicit.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();

    println!("Server:   {}", endpoint.base());
    println!("Config:   {}", endpoint.config_url());

    let mut config = SignalingConfig::new(endpoint);
    config.allow_untrusted_tls = args.allow_untrusted_tls;
    config.max_attempts = args.max_attempts;

    let (handle, mut events) = connect(config);

    let deadline = args.quit_after.map(|s| tokio::time::Instant::now() + Duration::from_secs(s));
    let mut joined = false;
    let mut failure: Option<String> = None;
    // Both quit paths are edge-triggered: an elapsed deadline stays elapsed, so without
    // this the timer arm would fire on every pass and spin.
    let mut stopping = false;

    loop {
        let event = tokio::select! {
            event = events.recv() => match event {
                Some(event) => event,
                None => break,
            },
            _ = tokio::signal::ctrl_c(), if !stopping => {
                println!("\nInterrupted.");
                stopping = true;
                handle.shutdown();
                continue;
            }
            _ = tokio::time::sleep_until(deadline.unwrap_or_else(tokio::time::Instant::now)),
                if deadline.is_some() && !stopping =>
            {
                println!("\nTime's up.");
                stopping = true;
                handle.shutdown();
                continue;
            }
        };

        match event {
            SignalingEvent::State(state) => match state {
                SignalingState::Connecting => println!("… connecting"),
                SignalingState::Connected => println!("✓ connected"),
                SignalingState::WaitingToRetry { seconds } => {
                    println!("… dropped, retrying in {seconds}s");
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
                println!("  the server calls us {:?} / {:?}", display_name, device_name);

                // The web client joins the IP room once it has an identity, and so
                // must we — peers are scoped to a room, not to the socket.
                if !joined {
                    handle.send(ClientMessage::JoinIpRoom);
                    joined = true;
                }
            }

            SignalingEvent::WsConfig(config) => report_connectivity(&config),

            SignalingEvent::Message(message) => match message {
                ServerMessage::Peers { peers, room } => {
                    println!("Room {} ({}): {} peer(s)", room.id, room.kind.as_str(), peers.len());
                    for peer in peers {
                        println!(
                            "  • {} [{}]{}",
                            peer.name.best_label(),
                            peer.id,
                            if peer.rtc_supported { "" } else { "  (no WebRTC — needs the ws fallback)" }
                        );
                    }
                }
                ServerMessage::PeerJoined { peer, .. } => {
                    println!("+ {} [{}]", peer.name.best_label(), peer.id);
                }
                ServerMessage::PeerLeft { peer_id, disconnect, .. } => {
                    println!("- {peer_id}{}", if disconnect { " (disconnected)" } else { "" });
                }
                ServerMessage::Signal { sender, sdp, ice, .. } => {
                    let what = match (&sdp, &ice) {
                        (Some(sdp), _) => sdp.kind.clone(),
                        (_, Some(_)) => "ice".to_string(),
                        _ => "empty".to_string(),
                    };
                    // Nothing answers these yet; log them so it's clear who is calling.
                    println!("~ signal ({what}) from {} — no data channel yet", sender.id);
                }
                ServerMessage::Unknown { kind } => println!("? unhandled frame {kind:?}"),
                // Already reported through the Identity and WsConfig events; the raw
                // frame arrives as well, and printing both is just noise.
                ServerMessage::DisplayName { .. } | ServerMessage::WsConfig(_) => {}
                other => println!("· {other:?}"),
            },
        }
    }

    if let Some(reason) = failure {
        bail!(reason);
    }
    Ok(())
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
