//! The PairDrop wire protocol, independent of any transport or toolkit.
//!
//! This is a port of `shared/PairDropKit`'s protocol layer. The Swift original couldn't
//! be reused on Linux — it builds on `URLSessionWebSocketTask`, the WebRTC xcframework,
//! and the Keychain, none of which exist here — but the format it speaks is the same,
//! and the test vectors are shared between the two so they can't drift apart.
//!
//! Chunking constants and the partition handshake come from `public/scripts/network.js`.

pub mod cyrb53;
pub mod endpoint;
pub mod transfer;

pub use cyrb53::{connection_hash, hash};
pub use endpoint::{InstanceConfig, ServerEndpoint};
pub use transfer::{FileHeader, TransferMessage, TransferRequest};

/// Bytes per binary message on the data channel.
pub const CHUNK_SIZE: usize = 64_000;

/// The threshold at which the sender stops and waits for `partition-received`, which is
/// what keeps a fast sender from burying a slow receiver.
///
/// Note this is a *threshold*, not a partition size. Neither the web client nor the
/// Swift port truncates a chunk to land on it: they emit whole 64,000-byte chunks and
/// declare the partition over once the running total reaches this value. A partition is
/// therefore [`CHUNKS_PER_PARTITION`] chunks — 1,024,000 bytes, slightly over the
/// nominal megabyte. Clamping the last chunk to hit 1,000,000 exactly would desynchronise
/// the handshake against every other client.
pub const MAX_PARTITION_SIZE: usize = 1_000_000;

/// Whole chunks emitted before the sender waits for an acknowledgement.
// Written out rather than using `div_ceil`, which isn't const-stable at our MSRV.
pub const CHUNKS_PER_PARTITION: usize = (MAX_PARTITION_SIZE + CHUNK_SIZE - 1) / CHUNK_SIZE;

/// The name the caller gives its data channel; the answering side matches on it.
pub const DATA_CHANNEL_LABEL: &str = "data-channel";

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the overshoot, since the obvious "a partition is one megabyte" reading is
    /// wrong and would break interop in a way only a >1 MB transfer reveals.
    #[test]
    fn a_partition_overshoots_the_threshold() {
        assert_eq!(CHUNKS_PER_PARTITION, 16);
        assert_eq!(CHUNKS_PER_PARTITION * CHUNK_SIZE, 1_024_000);

        // 15 chunks is still short of the threshold; the 16th crosses it.
        assert!(15 * CHUNK_SIZE < MAX_PARTITION_SIZE);
        assert!(CHUNKS_PER_PARTITION * CHUNK_SIZE >= MAX_PARTITION_SIZE);
    }
}
