//! Two sessions in one process, signaling to each other directly.
//!
//! This is the test the macOS port never had: it exercises the offer/answer dance,
//! trickle ICE, the data channel, and the verification hash without needing a browser
//! or a server. Interop with the real web client still has to be checked separately —
//! this only proves the two halves agree with *each other*.

use std::time::Duration;

use pairdrop_proto::RtcConfig;
use pairdrop_rtc::{RtcEvent, RtcSession};
use tokio::sync::mpsc;

/// No ICE servers: both peers are on this machine, so host candidates are enough and
/// the test never touches the network.
fn local_only() -> RtcConfig {
    RtcConfig { ice_servers: vec![], sdp_semantics: Some("unified-plan".into()) }
}

struct Connected {
    caller: RtcSession,
    answerer: RtcSession,
    caller_hash: String,
    answerer_hash: String,
    caller_rx: mpsc::UnboundedReceiver<RtcEvent>,
    answerer_rx: mpsc::UnboundedReceiver<RtcEvent>,
}

/// Brings up a connected pair, relaying signaling between them by hand.
async fn connect_pair() -> Connected {
    let (answerer, mut answerer_rx) = RtcSession::new(&local_only(), false).await.unwrap();
    let (caller, mut caller_rx) = RtcSession::new(&local_only(), true).await.unwrap();

    let mut caller_hash = None;
    let mut answerer_hash = None;

    // Pump both event streams until each side reports its channel open.
    while caller_hash.is_none() || answerer_hash.is_none() {
        tokio::select! {
            Some(event) = caller_rx.recv() => match event {
                RtcEvent::LocalDescription(sdp) => {
                    answerer.accept_remote_description(&sdp).await.unwrap();
                }
                RtcEvent::LocalCandidate(ice) => {
                    // A candidate can arrive before the remote description is set; the
                    // library queues it, so an error here is not fatal to the test.
                    let _ = answerer.add_ice_candidate(&ice).await;
                }
                RtcEvent::Open { connection_hash } => caller_hash = Some(connection_hash),
                RtcEvent::Failed => panic!("caller ICE failed"),
                _ => {}
            },
            Some(event) = answerer_rx.recv() => match event {
                RtcEvent::LocalDescription(sdp) => {
                    caller.accept_remote_description(&sdp).await.unwrap();
                }
                RtcEvent::LocalCandidate(ice) => {
                    let _ = caller.add_ice_candidate(&ice).await;
                }
                RtcEvent::Open { connection_hash } => answerer_hash = Some(connection_hash),
                RtcEvent::Failed => panic!("answerer ICE failed"),
                _ => {}
            },
        }
    }

    Connected {
        caller,
        answerer,
        caller_hash: caller_hash.unwrap(),
        answerer_hash: answerer_hash.unwrap(),
        caller_rx,
        answerer_rx,
    }
}

async fn next_message(rx: &mut mpsc::UnboundedReceiver<RtcEvent>) -> RtcEvent {
    loop {
        match rx.recv().await.expect("event stream ended") {
            event @ (RtcEvent::Text(_) | RtcEvent::Binary(_)) => return event,
            RtcEvent::Failed => panic!("connection failed while waiting for a message"),
            _ => continue,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_sessions_connect_and_exchange_data() {
    let pair = tokio::time::timeout(Duration::from_secs(30), connect_pair())
        .await
        .expect("the pair did not connect within 30s");

    let Connected { caller, answerer, caller_hash, answerer_hash, mut answerer_rx, mut caller_rx } =
        pair;

    // Both sides must derive the same 16-digit code, or the verification the UI shows
    // is meaningless. This is the assertion the ordering rule exists for.
    assert_eq!(caller_hash, answerer_hash, "peers disagree on the connection hash");
    assert_eq!(caller_hash.len(), 16);
    assert!(caller_hash.chars().all(|c| c.is_ascii_digit()));

    // Text, the shape every control frame uses.
    caller.send_text(r#"{"type":"ping-from-test"}"#).await.unwrap();
    match tokio::time::timeout(Duration::from_secs(10), next_message(&mut answerer_rx))
        .await
        .expect("no text arrived")
    {
        RtcEvent::Text(text) => assert_eq!(text, r#"{"type":"ping-from-test"}"#),
        other => panic!("expected text, got {other:?}"),
    }

    // A full-size chunk. 64,000 bytes is what PairDrop actually sends, and it is close
    // enough to the 65,536-byte SCTP limit to be worth proving rather than assuming.
    let chunk: Vec<u8> = (0..pairdrop_proto::CHUNK_SIZE).map(|i| (i % 251) as u8).collect();
    answerer.send_binary(&chunk).await.unwrap();
    match tokio::time::timeout(Duration::from_secs(10), next_message(&mut caller_rx))
        .await
        .expect("no chunk arrived")
    {
        RtcEvent::Binary(received) => {
            assert_eq!(received.len(), pairdrop_proto::CHUNK_SIZE, "chunk was truncated");
            assert_eq!(received, chunk, "chunk arrived corrupted");
        }
        other => panic!("expected binary, got {other:?}"),
    }

    caller.close().await;
    answerer.close().await;
}

/// Ordered delivery is part of the wire contract: the receiver writes chunks to disk in
/// arrival order and never reorders them, so out-of-order delivery would corrupt files
/// silently rather than fail.
#[tokio::test(flavor = "multi_thread")]
async fn chunks_arrive_in_order() {
    let pair = tokio::time::timeout(Duration::from_secs(30), connect_pair())
        .await
        .expect("the pair did not connect within 30s");

    let Connected { caller, answerer, mut answerer_rx, .. } = pair;

    const COUNT: usize = 24;
    for index in 0..COUNT {
        // Each chunk is filled with its own index, so a swap is detectable.
        caller.send_binary(&vec![index as u8; 4_096]).await.unwrap();
    }

    for expected in 0..COUNT {
        match tokio::time::timeout(Duration::from_secs(15), next_message(&mut answerer_rx))
            .await
            .unwrap_or_else(|_| panic!("chunk {expected} never arrived"))
        {
            RtcEvent::Binary(received) => {
                assert_eq!(received.len(), 4_096);
                assert!(
                    received.iter().all(|&byte| byte as usize == expected),
                    "chunk {expected} arrived out of order or corrupted"
                );
            }
            other => panic!("expected binary, got {other:?}"),
        }
    }

    caller.close().await;
    answerer.close().await;
}
