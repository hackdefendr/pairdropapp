//! Control frames exchanged over the peer-to-peer data channel.
//! Mirrors the `Peer` class in PairDrop's `public/scripts/network.js`.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHeader {
    pub name: String,
    #[serde(default)]
    pub mime: String,
    pub size: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransferRequest {
    pub header: Vec<FileHeader>,
    pub total_size: i64,
    pub images_only: bool,
    pub thumbnail_data_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransferMessage {
    Request(TransferRequest),
    FilesTransferResponse { accepted: bool, reason: Option<String> },
    Header(FileHeader),
    Partition { offset: i64 },
    PartitionReceived,
    Progress(f64),
    FileTransferComplete,
    MessageTransferComplete,
    Text(String),
    DisplayNameChanged(String),
}

impl TransferMessage {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Request(request) => json!({
                "type": "request",
                "header": request.header,
                "totalSize": request.total_size,
                "imagesOnly": request.images_only,
                // The web client always sends the key, using "" when there is no preview.
                "thumbnailDataUrl": request.thumbnail_data_url.clone().unwrap_or_default(),
            }),

            Self::FilesTransferResponse { accepted, reason } => {
                let mut payload = json!({ "type": "files-transfer-response", "accepted": accepted });
                if let Some(reason) = reason {
                    payload["reason"] = json!(reason);
                }
                payload
            }

            Self::Header(header) => json!({
                "type": "header",
                "size": header.size,
                "name": header.name,
                "mime": header.mime,
            }),

            Self::Partition { offset } => json!({ "type": "partition", "offset": offset }),

            // The web client echoes the whole `partition` frame back as `offset`; the
            // sender ignores the value, so a plain offset-less ack is equivalent.
            Self::PartitionReceived => json!({ "type": "partition-received", "offset": 0 }),

            Self::Progress(progress) => json!({ "type": "progress", "progress": progress }),
            Self::FileTransferComplete => json!({ "type": "file-transfer-complete" }),
            Self::MessageTransferComplete => json!({ "type": "message-transfer-complete" }),

            // btoa(unescape(encodeURIComponent(text))) === base64 of the UTF-8 bytes
            Self::Text(text) => json!({ "type": "text", "text": BASE64.encode(text.as_bytes()) }),

            Self::DisplayNameChanged(name) => {
                json!({ "type": "display-name-changed", "displayName": name })
            }
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(&self.to_json()).expect("serde_json::Value always serializes")
    }

    pub fn parse(bytes: &[u8]) -> Option<Self> {
        Self::from_json(&serde_json::from_slice::<Value>(bytes).ok()?)
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        match value.get("type")?.as_str()? {
            "request" => {
                let header: Vec<FileHeader> = value
                    .get("header")
                    .and_then(|h| serde_json::from_value(h.clone()).ok())
                    .unwrap_or_default();
                let total_size = value
                    .get("totalSize")
                    .and_then(Value::as_i64)
                    .unwrap_or_else(|| header.iter().map(|f| f.size).sum());
                let thumbnail = value
                    .get("thumbnailDataUrl")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                Some(Self::Request(TransferRequest {
                    header,
                    total_size,
                    images_only: value.get("imagesOnly").and_then(Value::as_bool).unwrap_or(false),
                    thumbnail_data_url: thumbnail,
                }))
            }

            "files-transfer-response" => Some(Self::FilesTransferResponse {
                accepted: value.get("accepted").and_then(Value::as_bool).unwrap_or(false),
                reason: value.get("reason").and_then(Value::as_str).map(str::to_string),
            }),

            "header" => Some(Self::Header(FileHeader {
                name: value.get("name")?.as_str()?.to_string(),
                mime: value.get("mime").and_then(Value::as_str).unwrap_or_default().to_string(),
                size: value.get("size").and_then(Value::as_i64).unwrap_or(0),
            })),

            "partition" => Some(Self::Partition {
                offset: value.get("offset").and_then(Value::as_i64).unwrap_or(0),
            }),

            "partition-received" => Some(Self::PartitionReceived),

            "progress" => Some(Self::Progress(
                value.get("progress").and_then(Value::as_f64).unwrap_or(0.0),
            )),

            "file-transfer-complete" => Some(Self::FileTransferComplete),
            "message-transfer-complete" => Some(Self::MessageTransferComplete),

            "text" => {
                let decoded = BASE64.decode(value.get("text")?.as_str()?).ok()?;
                Some(Self::Text(String::from_utf8(decoded).ok()?))
            }

            "display-name-changed" => Some(Self::DisplayNameChanged(
                value.get("displayName")?.as_str()?.to_string(),
            )),

            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_base64_of_utf8() {
        let message = TransferMessage::Text("héllo 🌍".to_string());
        assert_eq!(message.to_json()["text"], "aMOpbGxvIPCfjI0=");

        let round = TransferMessage::parse(&message.encode()).unwrap();
        assert_eq!(round, TransferMessage::Text("héllo 🌍".to_string()));
    }

    #[test]
    fn request_round_trips() {
        let request = TransferRequest {
            header: vec![
                FileHeader { name: "a.png".into(), mime: "image/png".into(), size: 12 },
                FileHeader { name: "b.png".into(), mime: "image/png".into(), size: 34 },
            ],
            total_size: 46,
            images_only: true,
            thumbnail_data_url: None,
        };
        let encoded = TransferMessage::Request(request.clone()).encode();

        let Some(TransferMessage::Request(parsed)) = TransferMessage::parse(&encoded) else {
            panic!("did not parse back as a request");
        };
        assert_eq!(parsed.header, request.header);
        assert_eq!(parsed.total_size, 46);
        assert!(parsed.images_only);
    }

    /// The web client always includes the key, using "" for no preview.
    #[test]
    fn request_always_carries_thumbnail_key() {
        let message = TransferMessage::Request(TransferRequest {
            header: vec![],
            total_size: 0,
            images_only: false,
            thumbnail_data_url: None,
        });
        assert_eq!(message.to_json()["thumbnailDataUrl"], "");
    }

    /// Missing totalSize falls back to the sum of the headers rather than zero,
    /// which would make a progress bar sit at 100% for the whole transfer.
    #[test]
    fn request_without_total_size_sums_the_headers() {
        let json = r#"{"type":"request","header":[{"name":"a","mime":"","size":10},
                                                  {"name":"b","mime":"","size":32}]}"#;
        let Some(TransferMessage::Request(parsed)) = TransferMessage::parse(json.as_bytes()) else {
            panic!("did not parse");
        };
        assert_eq!(parsed.total_size, 42);
    }

    #[test]
    fn parses_large_sizes_as_i64() {
        let json = r#"{"type":"header","name":"big.iso","mime":"","size":5368709120}"#;
        let Some(TransferMessage::Header(header)) = TransferMessage::parse(json.as_bytes()) else {
            panic!("did not parse");
        };
        assert_eq!(header.size, 5_368_709_120);
    }

    #[test]
    fn unknown_type_is_none_rather_than_a_panic() {
        assert!(TransferMessage::parse(br#"{"type":"future-thing"}"#).is_none());
        assert!(TransferMessage::parse(b"not json").is_none());
        assert!(TransferMessage::parse(br#"{"no":"type"}"#).is_none());
    }

    /// The frame the sender waits for at the end of every file.
    #[test]
    fn completion_frames_round_trip() {
        for message in [
            TransferMessage::FileTransferComplete,
            TransferMessage::MessageTransferComplete,
            TransferMessage::PartitionReceived,
        ] {
            assert_eq!(TransferMessage::parse(&message.encode()).unwrap(), message);
        }
    }
}
