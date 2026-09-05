//! The transfer state machine from PairDrop's `public/scripts/network.js`.
//!
//! It is deliberately independent of WebRTC: it talks to a [`Channel`], which the RTC
//! session implements and tests can fake. That makes a full send-and-receive round trip
//! testable with real files on disk and no network at all.
//!
//! The rhythm, from the sender's side:
//!
//! 1. `request` — the manifest, which the receiver accepts or declines
//! 2. `header` — one per file
//! 3. 64,000-byte binary chunks, stopping every ~1 MB for a `partition` /
//!    `partition-received` handshake so a fast sender can't bury a slow receiver
//! 4. at end of file the sender goes **quiet** — no `partition` — and waits for the
//!    receiver's `file-transfer-complete`

pub mod files;

use std::path::{Path, PathBuf};

use files::{FileChunker, FileReceiver};
use pairdrop_proto::{FileHeader, TransferMessage, TransferRequest};

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("the channel is not open")]
    NotOpen,
    #[error("{0}")]
    Channel(String),
}

/// Whatever carries frames to the peer. Implemented by the WebRTC data channel, and by
/// an in-memory pipe in the tests.
#[async_trait::async_trait]
pub trait Channel: Send + Sync {
    async fn send_text(&self, text: &str) -> Result<(), TransferError>;
    async fn send_binary(&self, bytes: &[u8]) -> Result<(), TransferError>;
}

/// What the state machine wants the surrounding app to know about.
#[derive(Debug, Clone, PartialEq)]
pub enum TransferEvent {
    /// The peer wants to send us something. Answer with [`Transfer::respond`].
    RequestReceived(TransferRequest),
    /// Fraction of the current batch, 0.0 to 1.0.
    SendProgress(f64),
    ReceiveProgress(f64),
    FilesReceived(Vec<PathBuf>),
    TextReceived(String),
    /// The peer accepted and everything went out.
    SendingFinished { files: usize },
    Declined { reason: Option<String> },
    Failed(String),
    /// The name the peer calls itself, which beats whatever the server guessed.
    PeerNameChanged(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Activity {
    Idle,
    AwaitingResponse,
    Sending,
    IncomingRequest,
    Receiving,
}

pub struct Transfer {
    channel: Box<dyn Channel>,
    download_directory: PathBuf,
    staging_directory: PathBuf,
    /// Accept incoming transfers without asking. Set for devices the user has paired.
    pub auto_accept: bool,

    activity: Activity,

    // Outgoing
    queue: Vec<PathBuf>,
    requested: Vec<PathBuf>,
    chunker: Option<FileChunker>,
    total_outgoing: i64,
    sent_before_current_file: i64,
    sent_in_current_file: i64,
    files_sent_in_batch: usize,

    // Incoming
    pending_request: Option<TransferRequest>,
    accepted_request: Option<TransferRequest>,
    remaining_headers: Vec<FileHeader>,
    receiver: Option<FileReceiver>,
    received_files: Vec<PathBuf>,
    total_incoming_received: i64,
    bytes_in_current_file: i64,
    last_reported_progress: f64,
}

impl Transfer {
    pub fn new(channel: Box<dyn Channel>, download_directory: PathBuf) -> Self {
        let staging_directory = std::env::temp_dir().join("pairdrop-incoming");
        Self {
            channel,
            download_directory,
            staging_directory,
            auto_accept: false,
            activity: Activity::Idle,
            queue: Vec::new(),
            requested: Vec::new(),
            chunker: None,
            total_outgoing: 0,
            sent_before_current_file: 0,
            sent_in_current_file: 0,
            files_sent_in_batch: 0,
            pending_request: None,
            accepted_request: None,
            remaining_headers: Vec::new(),
            receiver: None,
            received_files: Vec::new(),
            total_incoming_received: 0,
            bytes_in_current_file: 0,
            last_reported_progress: 0.0,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.activity != Activity::Idle
    }

    // MARK: sending

    /// Builds the manifest and asks the peer to accept it. Returns the names of any
    /// files that couldn't be sent, which the caller should surface.
    pub async fn send_files(
        &mut self,
        paths: &[PathBuf],
        events: &mut Vec<TransferEvent>,
    ) -> Result<Vec<String>, TransferError> {
        if self.activity != Activity::Idle {
            return Err(TransferError::Channel("already busy with another transfer".into()));
        }

        let mut headers = Vec::new();
        let mut accepted = Vec::new();
        let mut skipped = Vec::new();
        let mut total: i64 = 0;
        let mut images_only = true;

        for path in paths {
            let Ok(metadata) = std::fs::metadata(path) else {
                skipped.push(display_name(path));
                continue;
            };
            // A zero-byte file stalls the transfer: the protocol has no chunk to carry
            // and no completion signal, so neither side ever advances. The web client
            // has the same gap, so drop them rather than hang.
            if metadata.is_dir() || metadata.len() == 0 {
                skipped.push(display_name(path));
                continue;
            }

            let mime = mime_for(path);
            if !mime.starts_with("image/") {
                images_only = false;
            }
            total += metadata.len() as i64;
            headers.push(FileHeader {
                name: display_name(path),
                mime,
                size: metadata.len() as i64,
            });
            accepted.push(path.clone());
        }

        if accepted.is_empty() {
            events.push(TransferEvent::Failed(
                "Nothing to send — empty files and folders aren't supported.".into(),
            ));
            return Ok(skipped);
        }

        self.requested = accepted;
        self.total_outgoing = total;
        self.activity = Activity::AwaitingResponse;

        let request = TransferRequest {
            header: headers,
            total_size: total,
            images_only,
            thumbnail_data_url: None,
        };
        self.send(TransferMessage::Request(request)).await?;
        Ok(skipped)
    }

    pub async fn send_text(&self, text: &str) -> Result<(), TransferError> {
        self.send(TransferMessage::Text(text.to_string())).await
    }

    pub async fn announce_name(&self, name: &str) -> Result<(), TransferError> {
        self.send(TransferMessage::DisplayNameChanged(name.to_string())).await
    }

    async fn start_sending(&mut self, events: &mut Vec<TransferEvent>) -> Result<(), TransferError> {
        self.queue = std::mem::take(&mut self.requested);
        self.sent_before_current_file = 0;
        self.files_sent_in_batch = 0;
        self.activity = Activity::Sending;
        events.push(TransferEvent::SendProgress(0.0));
        self.next_file(events).await
    }

    async fn next_file(&mut self, events: &mut Vec<TransferEvent>) -> Result<(), TransferError> {
        if self.queue.is_empty() {
            return Ok(());
        }
        let path = self.queue.remove(0);
        self.sent_in_current_file = 0;

        let chunker = match FileChunker::open(&path) {
            Ok(chunker) => chunker,
            Err(error) => {
                let message = format!("Could not read {}: {error}", display_name(&path));
                self.fail(message.clone(), events);
                return Ok(());
            }
        };

        let header = FileHeader {
            name: display_name(&path),
            mime: mime_for(&path),
            size: chunker.size as i64,
        };
        self.chunker = Some(chunker);
        self.send(TransferMessage::Header(header)).await?;
        self.send_next_partition(events).await
    }

    /// Reads up to one partition off disk and puts it on the wire. Disk work happens on
    /// a blocking thread so a slow filesystem can't stall the runtime.
    async fn send_next_partition(
        &mut self,
        events: &mut Vec<TransferEvent>,
    ) -> Result<(), TransferError> {
        let Some(mut chunker) = self.chunker.take() else { return Ok(()) };

        let read = tokio::task::spawn_blocking(move || {
            let result = chunker.read_partition();
            (chunker, result)
        })
        .await;

        let (chunker, result) = match read {
            Ok(pair) => pair,
            Err(error) => {
                self.fail(format!("Read failed: {error}"), events);
                return Ok(());
            }
        };

        let (chunks, at_file_end) = match result {
            Ok(value) => value,
            Err(error) => {
                self.fail(format!("Read failed: {error}"), events);
                return Ok(());
            }
        };
        let offset = chunker.offset as i64;
        self.chunker = Some(chunker);

        for chunk in &chunks {
            self.channel.send_binary(chunk).await?;
            self.sent_in_current_file += chunk.len() as i64;
        }

        if self.total_outgoing > 0 && self.activity == Activity::Sending {
            let sent = self.sent_before_current_file + self.sent_in_current_file;
            events.push(TransferEvent::SendProgress(
                (sent as f64 / self.total_outgoing as f64).min(1.0),
            ));
        }

        // At end of file we go quiet and wait for the receiver's file-transfer-complete,
        // which is what the web client does.
        //
        // An extra `partition` here is tolerated rather than fatal — the acknowledgement
        // comes back, finds no chunker, and is ignored — so nothing in a round-trip test
        // notices. `the_sender_goes_quiet_at_end_of_file` asserts the frame sequence
        // directly for that reason.
        if !at_file_end {
            self.send(TransferMessage::Partition { offset }).await?;
        }
        Ok(())
    }

    // MARK: receiving

    /// Accept or decline the request the peer sent.
    pub async fn respond(
        &mut self,
        accepted: bool,
        events: &mut Vec<TransferEvent>,
    ) -> Result<(), TransferError> {
        let Some(request) = self.pending_request.take() else { return Ok(()) };

        self.send(TransferMessage::FilesTransferResponse { accepted, reason: None }).await?;

        if accepted {
            self.remaining_headers = request.header.clone();
            self.accepted_request = Some(request);
            self.received_files.clear();
            self.total_incoming_received = 0;
            self.bytes_in_current_file = 0;
            self.last_reported_progress = 0.0;
            self.activity = Activity::Receiving;
            events.push(TransferEvent::ReceiveProgress(0.0));
        } else {
            self.activity = Activity::Idle;
        }
        Ok(())
    }

    /// A binary chunk arrived.
    pub async fn on_binary(
        &mut self,
        data: &[u8],
        events: &mut Vec<TransferEvent>,
    ) -> Result<(), TransferError> {
        if data.is_empty() {
            return Ok(());
        }
        let Some(receiver) = self.receiver.as_mut() else { return Ok(()) };

        if let Err(error) = receiver.append(data) {
            self.fail(format!("Could not save the incoming file: {error}"), events);
            return Ok(());
        }
        self.bytes_in_current_file += data.len() as i64;
        let complete = receiver.is_complete();

        self.report_receive_progress(events).await?;

        if complete {
            self.finish_incoming_file(events).await?;
        }
        Ok(())
    }

    async fn finish_incoming_file(
        &mut self,
        events: &mut Vec<TransferEvent>,
    ) -> Result<(), TransferError> {
        let Some(receiver) = self.receiver.take() else { return Ok(()) };
        let size = receiver.header.size;
        let directory = self.download_directory.clone();

        let saved = tokio::task::spawn_blocking(move || receiver.finish(&directory)).await;
        let path = match saved {
            Ok(Ok(path)) => path,
            Ok(Err(error)) => {
                self.fail(format!("Could not save the incoming file: {error}"), events);
                return Ok(());
            }
            Err(error) => {
                self.fail(format!("Could not save the incoming file: {error}"), events);
                return Ok(());
            }
        };

        self.total_incoming_received += size;
        self.bytes_in_current_file = 0;
        self.received_files.push(path);

        self.send(TransferMessage::FileTransferComplete).await?;

        if !self.remaining_headers.is_empty() {
            self.remaining_headers.remove(0);
        }
        if self.remaining_headers.is_empty() {
            let files = std::mem::take(&mut self.received_files);
            self.accepted_request = None;
            self.activity = Activity::Idle;
            events.push(TransferEvent::FilesReceived(files));
        }
        Ok(())
    }

    async fn report_receive_progress(
        &mut self,
        events: &mut Vec<TransferEvent>,
    ) -> Result<(), TransferError> {
        let Some(request) = self.accepted_request.as_ref() else { return Ok(()) };
        if request.total_size <= 0 {
            return Ok(());
        }

        let done = self.total_incoming_received + self.bytes_in_current_file;
        let progress = (done as f64 / request.total_size as f64).min(1.0);
        events.push(TransferEvent::ReceiveProgress(progress));

        // The same throttle the web client uses, so the channel isn't flooded with
        // progress frames while chunks are trying to get through.
        if progress - self.last_reported_progress >= 0.005 || progress >= 1.0 {
            self.last_reported_progress = progress;
            self.send(TransferMessage::Progress(progress)).await?;
        }
        Ok(())
    }

    fn begin_incoming_file(&mut self, header: FileHeader, events: &mut Vec<TransferEvent>) -> bool {
        let Some(expected) = self.remaining_headers.first() else { return false };

        // The peer must deliver exactly what we agreed to accept — otherwise a request
        // for one small file could be answered with something else entirely.
        if expected.name != header.name || expected.size != header.size {
            self.fail("The peer sent a file we didn't agree to receive.".into(), events);
            return false;
        }

        self.bytes_in_current_file = 0;
        match FileReceiver::create(header, &self.staging_directory) {
            Ok(receiver) => {
                self.receiver = Some(receiver);
                true
            }
            Err(error) => {
                self.fail(format!("Could not stage the incoming file: {error}"), events);
                false
            }
        }
    }

    // MARK: inbound frames

    /// A text frame arrived on the channel.
    pub async fn on_text(
        &mut self,
        text: &str,
        events: &mut Vec<TransferEvent>,
    ) -> Result<(), TransferError> {
        let Some(message) = TransferMessage::parse(text.as_bytes()) else { return Ok(()) };
        self.on_message(message, events).await
    }

    pub async fn on_message(
        &mut self,
        message: TransferMessage,
        events: &mut Vec<TransferEvent>,
    ) -> Result<(), TransferError> {
        match message {
            TransferMessage::Request(request) => {
                // One request at a time per peer, same as the web client.
                if self.pending_request.is_some() || self.accepted_request.is_some() {
                    self.send(TransferMessage::FilesTransferResponse {
                        accepted: false,
                        reason: Some("busy".into()),
                    })
                    .await?;
                    return Ok(());
                }
                self.pending_request = Some(request.clone());
                self.activity = Activity::IncomingRequest;

                if self.auto_accept {
                    self.respond(true, events).await?;
                } else {
                    events.push(TransferEvent::RequestReceived(request));
                }
            }

            TransferMessage::FilesTransferResponse { accepted, reason } => {
                if self.activity != Activity::AwaitingResponse {
                    return Ok(());
                }
                if accepted {
                    self.start_sending(events).await?;
                } else {
                    self.requested.clear();
                    self.activity = Activity::Idle;
                    events.push(TransferEvent::Declined { reason });
                }
            }

            TransferMessage::Header(header) => {
                if self.accepted_request.is_none() {
                    return Ok(());
                }
                let size = header.size;
                if !self.begin_incoming_file(header, events) {
                    return Ok(());
                }
                // No chunks will ever arrive for an empty file, so complete it here
                // rather than waiting forever.
                if size == 0 {
                    self.finish_incoming_file(events).await?;
                }
            }

            // The sender paused for our acknowledgement.
            TransferMessage::Partition { .. } => {
                self.send(TransferMessage::PartitionReceived).await?;
            }

            TransferMessage::PartitionReceived => {
                self.send_next_partition(events).await?;
            }

            TransferMessage::Progress(progress) => {
                if self.activity == Activity::Sending {
                    events.push(TransferEvent::SendProgress(progress.min(1.0)));
                }
            }

            TransferMessage::FileTransferComplete => {
                self.sent_before_current_file += self.sent_in_current_file;
                self.sent_in_current_file = 0;
                self.files_sent_in_batch += 1;
                self.chunker = None;

                if self.queue.is_empty() {
                    let files = self.files_sent_in_batch;
                    self.files_sent_in_batch = 0;
                    self.activity = Activity::Idle;
                    events.push(TransferEvent::SendingFinished { files });
                } else {
                    self.next_file(events).await?;
                }
            }

            TransferMessage::MessageTransferComplete => {}

            TransferMessage::Text(text) => {
                events.push(TransferEvent::TextReceived(text));
                self.send(TransferMessage::MessageTransferComplete).await?;
            }

            TransferMessage::DisplayNameChanged(name) => {
                if !name.is_empty() {
                    events.push(TransferEvent::PeerNameChanged(name));
                }
            }
        }
        Ok(())
    }

    // MARK: teardown

    /// Drops everything in flight. Called when the channel closes or a transfer fails.
    pub fn reset(&mut self) {
        self.queue.clear();
        self.requested.clear();
        self.chunker = None;
        self.sent_in_current_file = 0;
        self.sent_before_current_file = 0;
        self.files_sent_in_batch = 0;

        if let Some(receiver) = self.receiver.take() {
            receiver.discard();
        }
        self.accepted_request = None;
        self.pending_request = None;
        self.remaining_headers.clear();
        self.received_files.clear();
        self.total_incoming_received = 0;
        self.bytes_in_current_file = 0;
        self.activity = Activity::Idle;
    }

    fn fail(&mut self, message: String, events: &mut Vec<TransferEvent>) {
        self.reset();
        events.push(TransferEvent::Failed(message));
    }

    async fn send(&self, message: TransferMessage) -> Result<(), TransferError> {
        self.channel.send_text(&message.to_json().to_string()).await
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Unnamed file")
        .to_string()
}

/// Enough of a MIME guess for PairDrop's purposes: it only uses this to decide whether a
/// batch is images-only and to label the download.
fn mime_for(path: &Path) -> String {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain",
        "json" => "application/json",
        "zip" => "application/zip",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
    .to_string()
}
