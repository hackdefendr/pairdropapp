//! Two state machines wired together by an in-memory pipe, moving real files on disk.
//!
//! No WebRTC and no server, so these run in milliseconds and pin the protocol rhythm
//! itself: the request/response handshake, per-file headers, the partition
//! acknowledgement, and the silence at end of file that the receiver breaks.

use std::path::PathBuf;
use std::time::Duration;

use pairdrop_proto::{FileHeader, TransferMessage};
use pairdrop_transfer::{Channel, Transfer, TransferError, TransferEvent};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
enum Frame {
    Text(String),
    Binary(Vec<u8>),
}

struct Pipe {
    tx: mpsc::UnboundedSender<Frame>,
}

#[async_trait::async_trait]
impl Channel for Pipe {
    async fn send_text(&self, text: &str) -> Result<(), TransferError> {
        self.tx
            .send(Frame::Text(text.to_string()))
            .map_err(|_| TransferError::NotOpen)
    }

    async fn send_binary(&self, bytes: &[u8]) -> Result<(), TransferError> {
        self.tx
            .send(Frame::Binary(bytes.to_vec()))
            .map_err(|_| TransferError::NotOpen)
    }
}

struct Fixture {
    dir: PathBuf,
    sender: Transfer,
    receiver: Transfer,
    from_sender: mpsc::UnboundedReceiver<Frame>,
    from_receiver: mpsc::UnboundedReceiver<Frame>,
    pub sender_events: Vec<TransferEvent>,
    pub receiver_events: Vec<TransferEvent>,
    /// Every frame the sender put on the wire, in order: the message `type` for text
    /// frames, `"bin"` for a chunk. Lets a test assert the rhythm, not just the result.
    pub sent_frames: Vec<String>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("pairdrop-rt-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("out")).unwrap();
        std::fs::create_dir_all(dir.join("in")).unwrap();

        let (sender_tx, from_sender) = mpsc::unbounded_channel();
        let (receiver_tx, from_receiver) = mpsc::unbounded_channel();

        let mut receiver = Transfer::new(Box::new(Pipe { tx: receiver_tx }), dir.join("in"));
        receiver.auto_accept = true;

        Self {
            sender: Transfer::new(Box::new(Pipe { tx: sender_tx }), dir.join("out")),
            receiver,
            from_sender,
            from_receiver,
            sender_events: Vec::new(),
            receiver_events: Vec::new(),
            sent_frames: Vec::new(),
            dir,
        }
    }

    fn write(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.dir.join("out").join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn inbox(&self) -> PathBuf {
        self.dir.join("in")
    }

    /// Relays frames until the side we're waiting on reaches a terminal event.
    ///
    /// Deliberately not an idle timeout: disk reads happen on a blocking pool, and under
    /// parallel test load a pause longer than any fixed idle window is normal. Stopping
    /// on "nothing arrived recently" made this flaky in exactly that situation.
    async fn run_until(&mut self, stop: Stop) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

        while !self.reached(stop) {
            tokio::select! {
                Some(frame) = self.from_sender.recv() => {
                    self.sent_frames.push(label(&frame));
                    deliver(&mut self.receiver, frame, &mut self.receiver_events).await;
                }
                Some(frame) = self.from_receiver.recv() => {
                    deliver(&mut self.sender, frame, &mut self.sender_events).await;
                }
                _ = tokio::time::sleep_until(deadline) => {
                    panic!(
                        "timed out waiting for {stop:?}\n  sender: {:?}\n  receiver: {:?}",
                        self.sender_events, self.receiver_events
                    );
                }
            }
        }
    }

    /// Waits for the sender to finish, which for a file transfer happens strictly after
    /// the receiver has saved everything.
    async fn run(&mut self) {
        self.run_until(Stop::SenderDone).await;
    }

    fn reached(&self, stop: Stop) -> bool {
        match stop {
            Stop::SenderDone => self.sender_events.iter().any(|e| {
                matches!(
                    e,
                    TransferEvent::SendingFinished { .. }
                        | TransferEvent::Declined { .. }
                        | TransferEvent::Failed(_)
                )
            }),
            Stop::ReceiverDone => self.receiver_events.iter().any(|e| {
                matches!(
                    e,
                    TransferEvent::FilesReceived(_)
                        | TransferEvent::TextReceived(_)
                        | TransferEvent::Failed(_)
                )
            }),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Stop {
    SenderDone,
    ReceiverDone,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn label(frame: &Frame) -> String {
    match frame {
        Frame::Binary(_) => "bin".to_string(),
        Frame::Text(text) => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
            .unwrap_or_else(|| "text?".to_string()),
    }
}

async fn deliver(transfer: &mut Transfer, frame: Frame, events: &mut Vec<TransferEvent>) {
    match frame {
        Frame::Text(text) => transfer.on_text(&text, events).await.unwrap(),
        Frame::Binary(bytes) => transfer.on_binary(&bytes, events).await.unwrap(),
    }
}

fn received_files(events: &[TransferEvent]) -> Vec<PathBuf> {
    events
        .iter()
        .find_map(|event| match event {
            TransferEvent::FilesReceived(files) => Some(files.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

// MARK: tests

#[tokio::test]
async fn sends_one_file_byte_for_byte() {
    let mut fixture = Fixture::new("one");
    let contents: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    let path = fixture.write("hello.bin", &contents);

    let mut events = Vec::new();
    fixture.sender.send_files(&[path], &mut events).await.unwrap();
    fixture.sender_events.append(&mut events);
    fixture.run().await;

    let files = received_files(&fixture.receiver_events);
    assert_eq!(files.len(), 1, "events: {:?}", fixture.receiver_events);
    assert_eq!(files[0].file_name().unwrap(), "hello.bin");
    assert_eq!(std::fs::read(&files[0]).unwrap(), contents, "file arrived corrupted");

    assert!(fixture
        .sender_events
        .iter()
        .any(|e| matches!(e, TransferEvent::SendingFinished { files: 1 })));
    assert!(!fixture.sender.is_busy());
    assert!(!fixture.receiver.is_busy());
}

/// The frame sequence itself, not just the result.
///
/// A sub-megabyte file must produce **no** `partition` frame at all, and a 2.5 MB file
/// exactly two — after the first and second full partitions, and never after the final
/// short one. At end of file the sender goes quiet and waits for the receiver's
/// `file-transfer-complete`.
///
/// Worth asserting explicitly: an extra `partition` at end of file happens to be
/// harmless against this implementation (the acknowledgement finds no chunker and is
/// ignored), so a round-trip test that only checks the delivered bytes passes either
/// way. It is still wrong on the wire, and this is what catches it.
#[tokio::test]
async fn the_sender_goes_quiet_at_end_of_file() {
    let mut small = Fixture::new("rhythm-small");
    let path = small.write("small.bin", &vec![1u8; 40_000]);
    let mut events = Vec::new();
    small.sender.send_files(&[path], &mut events).await.unwrap();
    small.run().await;

    assert_eq!(
        small.sent_frames.iter().filter(|f| *f == "partition").count(),
        0,
        "a single-chunk file needs no partition handshake: {:?}",
        small.sent_frames
    );
    assert_eq!(small.sent_frames.last().map(String::as_str), Some("bin"));

    let mut big = Fixture::new("rhythm-big");
    // 1,024,000 + 1,024,000 + 452,000: two full partitions then a short one.
    let path = big.write("big.bin", &vec![2u8; 2_500_000]);
    let mut events = Vec::new();
    big.sender.send_files(&[path], &mut events).await.unwrap();
    big.run().await;

    assert_eq!(
        big.sent_frames.iter().filter(|f| *f == "partition").count(),
        2,
        "expected exactly two partition frames: {:?}",
        big.sent_frames.iter().filter(|f| *f != "bin").collect::<Vec<_>>()
    );
    assert_eq!(
        big.sent_frames.last().map(String::as_str),
        Some("bin"),
        "the last thing on the wire must be a chunk, not a partition"
    );
}

/// The partition handshake only engages past a megabyte, so this is the case that
/// catches a sender which forgets to wait for `partition-received`.
#[tokio::test]
async fn sends_a_file_spanning_several_partitions() {
    let mut fixture = Fixture::new("big");
    let contents: Vec<u8> = (0..2_500_000u32).map(|i| (i % 253) as u8).collect();
    let path = fixture.write("big.bin", &contents);

    let mut events = Vec::new();
    fixture.sender.send_files(&[path], &mut events).await.unwrap();
    fixture.sender_events.append(&mut events);
    fixture.run().await;

    let files = received_files(&fixture.receiver_events);
    assert_eq!(files.len(), 1, "events: {:?}", fixture.receiver_events);
    let received = std::fs::read(&files[0]).unwrap();
    assert_eq!(received.len(), contents.len(), "wrong length");
    assert_eq!(received, contents, "file arrived corrupted");

    // Progress must actually reach the end, not stall at the last partition boundary.
    let last = fixture
        .sender_events
        .iter()
        .filter_map(|e| match e {
            TransferEvent::SendProgress(p) => Some(*p),
            _ => None,
        })
        .fold(0.0f64, f64::max);
    assert!((last - 1.0).abs() < 1e-9, "final send progress was {last}");
}

#[tokio::test]
async fn sends_a_batch_in_order() {
    let mut fixture = Fixture::new("batch");
    let paths = vec![
        fixture.write("a.txt", b"first"),
        fixture.write("b.txt", b"second"),
        fixture.write("c.txt", b"third"),
    ];

    let mut events = Vec::new();
    fixture.sender.send_files(&paths, &mut events).await.unwrap();
    fixture.sender_events.append(&mut events);
    fixture.run().await;

    let files = received_files(&fixture.receiver_events);
    assert_eq!(files.len(), 3, "events: {:?}", fixture.receiver_events);
    assert_eq!(std::fs::read(&files[0]).unwrap(), b"first");
    assert_eq!(std::fs::read(&files[1]).unwrap(), b"second");
    assert_eq!(std::fs::read(&files[2]).unwrap(), b"third");

    assert!(fixture
        .sender_events
        .iter()
        .any(|e| matches!(e, TransferEvent::SendingFinished { files: 3 })));
}

/// Upstream stalls forever on these: there is no chunk to carry and no completion
/// signal, so neither side advances. Dropping them before the request is the fix.
#[tokio::test]
async fn skips_empty_files_rather_than_hanging() {
    let mut fixture = Fixture::new("empty");
    let empty = fixture.write("empty.txt", b"");
    let real = fixture.write("real.txt", b"content");

    let mut events = Vec::new();
    let skipped = fixture.sender.send_files(&[empty, real], &mut events).await.unwrap();
    fixture.sender_events.append(&mut events);
    assert_eq!(skipped, vec!["empty.txt"]);

    fixture.run().await;

    let files = received_files(&fixture.receiver_events);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file_name().unwrap(), "real.txt");
}

/// A request for nothing but empty files must fail cleanly instead of sending an empty
/// manifest and waiting.
#[tokio::test]
async fn refuses_a_batch_of_only_empty_files() {
    let mut fixture = Fixture::new("allempty");
    let empty = fixture.write("empty.txt", b"");

    let mut events = Vec::new();
    fixture.sender.send_files(&[empty], &mut events).await.unwrap();

    assert!(events.iter().any(|e| matches!(e, TransferEvent::Failed(_))), "{events:?}");
    assert!(!fixture.sender.is_busy());
}

#[tokio::test]
async fn declining_leaves_both_sides_idle() {
    let mut fixture = Fixture::new("decline");
    fixture.receiver.auto_accept = false;
    let path = fixture.write("a.txt", b"nope");

    let mut events = Vec::new();
    fixture.sender.send_files(&[path], &mut events).await.unwrap();
    fixture.sender_events.append(&mut events);

    // Let the request land, then decline it.
    let frame = fixture.from_sender.recv().await.unwrap();
    deliver(&mut fixture.receiver, frame, &mut fixture.receiver_events).await;
    assert!(matches!(
        fixture.receiver_events.first(),
        Some(TransferEvent::RequestReceived(_))
    ));

    let mut events = Vec::new();
    fixture.receiver.respond(false, &mut events).await.unwrap();
    fixture.run().await;

    assert!(fixture
        .sender_events
        .iter()
        .any(|e| matches!(e, TransferEvent::Declined { .. })));
    assert!(!fixture.sender.is_busy());
    assert!(!fixture.receiver.is_busy());
}

/// The manifest is a contract. A peer that accepts a request for one file and then
/// sends a different one must be stopped, or "send me a photo" becomes "write whatever
/// you like into my downloads folder".
#[tokio::test]
async fn rejects_a_file_that_was_not_agreed() {
    let mut fixture = Fixture::new("manifest");
    let path = fixture.write("expected.txt", b"twelve bytes");

    let mut events = Vec::new();
    fixture.sender.send_files(&[path], &mut events).await.unwrap();

    // Deliver only the request, so the receiver accepts and waits for the header.
    let frame = fixture.from_sender.recv().await.unwrap();
    deliver(&mut fixture.receiver, frame, &mut fixture.receiver_events).await;

    // Now hand it a header for something else entirely.
    let mut events = Vec::new();
    fixture
        .receiver
        .on_message(
            TransferMessage::Header(FileHeader {
                name: "evil.sh".into(),
                mime: "text/plain".into(),
                size: 9,
            }),
            &mut events,
        )
        .await
        .unwrap();

    assert!(
        events.iter().any(|e| matches!(e, TransferEvent::Failed(_))),
        "an unagreed file was accepted: {events:?}"
    );
    assert!(!fixture.receiver.is_busy());
    assert!(!fixture.inbox().join("evil.sh").exists());
}

/// The sender picks the name, so a path traversal has to be neutralised on arrival.
#[tokio::test]
async fn sanitizes_a_hostile_filename_on_arrival() {
    let mut fixture = Fixture::new("hostile");
    let path = fixture.write("innocent.txt", b"payload");

    let mut events = Vec::new();
    fixture.sender.send_files(&[path], &mut events).await.unwrap();

    // Rewrite the manifest and header to claim a traversing path, the way a malicious
    // peer would. Both must agree or the manifest check fires first.
    let hostile = "../../../../tmp/pairdrop-escape.txt";
    let mut events = Vec::new();
    fixture
        .receiver
        .on_message(
            TransferMessage::Request(pairdrop_proto::TransferRequest {
                header: vec![FileHeader { name: hostile.into(), mime: String::new(), size: 7 }],
                total_size: 7,
                images_only: false,
                thumbnail_data_url: None,
            }),
            &mut events,
        )
        .await
        .unwrap();
    fixture
        .receiver
        .on_message(
            TransferMessage::Header(FileHeader {
                name: hostile.into(),
                mime: String::new(),
                size: 7,
            }),
            &mut events,
        )
        .await
        .unwrap();
    fixture.receiver.on_binary(b"payload", &mut events).await.unwrap();

    let files = received_files(&events);
    assert_eq!(files.len(), 1, "{events:?}");
    assert_eq!(files[0].file_name().unwrap(), "pairdrop-escape.txt");
    assert_eq!(
        files[0].parent().unwrap(),
        fixture.inbox(),
        "the file escaped the download directory"
    );
}

#[tokio::test]
async fn text_round_trips_and_is_acknowledged() {
    let mut fixture = Fixture::new("text");

    fixture.sender.send_text("héllo 🌍").await.unwrap();
    fixture.run_until(Stop::ReceiverDone).await;

    assert!(fixture
        .receiver_events
        .iter()
        .any(|e| matches!(e, TransferEvent::TextReceived(t) if t == "héllo 🌍")));
}

#[tokio::test]
async fn a_name_collision_does_not_overwrite() {
    let mut fixture = Fixture::new("collision");
    std::fs::write(fixture.inbox().join("note.txt"), b"original").unwrap();

    let path = fixture.write("note.txt", b"incoming");
    let mut events = Vec::new();
    fixture.sender.send_files(&[path], &mut events).await.unwrap();
    fixture.run().await;

    let files = received_files(&fixture.receiver_events);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file_name().unwrap(), "note (2).txt");
    assert_eq!(std::fs::read(fixture.inbox().join("note.txt")).unwrap(), b"original");
    assert_eq!(std::fs::read(&files[0]).unwrap(), b"incoming");
}
