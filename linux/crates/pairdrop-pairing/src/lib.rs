//! Device pairing.
//!
//! Two devices that aren't on the same network can't see each other in the IP room. One
//! creates a six-digit key, the other enters it, and the server puts both in a shared
//! *secret room* they'll find each other in from anywhere.
//!
//! The secret is what persists — replayed as `room-secrets` on every reconnect — so it
//! lives in the platform credential store, not in a config file.

pub mod store;

use pairdrop_proto::{ClientMessage, ServerMessage};
pub use store::{best_available, KeyringStore, MemoryStore, PairedDevice, SecretStore, StoreError};

/// A pairing in progress on this device: we asked the server for a key and are waiting
/// for someone to enter it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivePairing {
    pub pair_key: String,
    pub room_secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingEvent {
    /// Show this key; the other device types it in.
    KeyReady { pair_key: String },
    /// Paired. `peer_id` is a placeholder name until the caller supplies a real one.
    Paired { secret: String, peer_id: String },
    KeyInvalid,
    /// Too many wrong keys; the server is refusing further attempts for a while.
    RateLimited,
    Canceled,
    /// The other device removed the pairing.
    Unpaired { display_name: String },
    /// The server rotated a secret; nothing to show, but it has been stored.
    SecretRotated,
    /// Saving failed, so this pairing won't survive a restart.
    NotPersisted(String),
}

/// What the caller should do next: show these events, and put these frames on the wire.
#[derive(Debug, Default)]
pub struct Outcome {
    pub events: Vec<PairingEvent>,
    pub send: Vec<ClientMessage>,
}

#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    #[error("a pairing key is six digits")]
    BadKey,
}

pub struct Pairing {
    store: Box<dyn SecretStore>,
    devices: Vec<PairedDevice>,
    active: Option<ActivePairing>,
}

impl Pairing {
    pub fn new(store: Box<dyn SecretStore>) -> Self {
        let devices = store.load();
        Self { store, devices, active: None }
    }

    pub fn devices(&self) -> &[PairedDevice] {
        &self.devices
    }

    pub fn active(&self) -> Option<&ActivePairing> {
        self.active.as_ref()
    }

    pub fn store_description(&self) -> String {
        self.store.description()
    }

    /// Sent right after connecting, so the server puts us back in every room we're
    /// paired into. Without this a paired device is invisible until the next pairing.
    pub fn room_secrets_message(&self) -> Option<ClientMessage> {
        if self.devices.is_empty() {
            return None;
        }
        Some(ClientMessage::RoomSecrets(
            self.devices.iter().map(|d| d.secret.clone()).collect(),
        ))
    }

    pub fn begin(&self) -> ClientMessage {
        ClientMessage::PairDeviceInitiate
    }

    pub fn cancel(&mut self) -> ClientMessage {
        self.active = None;
        ClientMessage::PairDeviceCancel
    }

    /// Non-digits are ignored so a key can be pasted with spaces or dashes.
    pub fn join(&self, key: &str) -> Result<ClientMessage, PairingError> {
        let digits: String = key.chars().filter(char::is_ascii_digit).collect();
        if digits.len() != 6 {
            return Err(PairingError::BadKey);
        }
        Ok(ClientMessage::PairDeviceJoin { pair_key: digits })
    }

    /// Forgets a device on this end and tells the server to tear the room down.
    pub fn unpair(&mut self, secret: &str) -> Outcome {
        self.devices.retain(|d| d.secret != secret);
        let mut outcome = self.persist();
        outcome.send.push(ClientMessage::RoomSecretsDeleted(vec![secret.to_string()]));
        // Re-send what's left, so the server's view matches ours exactly.
        outcome.send.push(ClientMessage::RoomSecrets(
            self.devices.iter().map(|d| d.secret.clone()).collect(),
        ));
        outcome
    }

    pub fn set_auto_accept(&mut self, secret: &str, auto_accept: bool) -> Outcome {
        if let Some(device) = self.devices.iter_mut().find(|d| d.secret == secret) {
            device.auto_accept = auto_accept;
        }
        self.persist()
    }

    /// Gives a paired device a real name once the caller knows it — the pairing frames
    /// only carry a peer id.
    pub fn set_display_name(&mut self, secret: &str, name: &str) -> Outcome {
        if let Some(device) = self.devices.iter_mut().find(|d| d.secret == secret) {
            if device.display_name == name {
                return Outcome::default();
            }
            device.display_name = name.to_string();
        }
        self.persist()
    }

    pub fn auto_accepts(&self, secret: &str) -> bool {
        self.devices
            .iter()
            .any(|d| d.secret == secret && d.auto_accept)
    }

    /// Feeds one server frame through. Anything unrelated to pairing is ignored.
    pub fn handle(&mut self, message: &ServerMessage) -> Outcome {
        match message {
            ServerMessage::PairDeviceInitiated { room_secret, pair_key } => {
                self.active = Some(ActivePairing {
                    pair_key: pair_key.clone(),
                    room_secret: room_secret.clone(),
                });
                Outcome {
                    events: vec![PairingEvent::KeyReady { pair_key: pair_key.clone() }],
                    send: Vec::new(),
                }
            }

            ServerMessage::PairDeviceJoined { room_secret, peer_id } => {
                self.active = None;
                // Both ends get this frame, and the initiator already knows the secret,
                // so adding it twice has to be harmless.
                if !self.devices.iter().any(|d| &d.secret == room_secret) {
                    self.devices.push(PairedDevice {
                        secret: room_secret.clone(),
                        display_name: peer_id.clone(),
                        auto_accept: false,
                    });
                }

                let mut outcome = self.persist();
                outcome.events.push(PairingEvent::Paired {
                    secret: room_secret.clone(),
                    peer_id: peer_id.clone(),
                });
                outcome.send.push(ClientMessage::RoomSecrets(vec![room_secret.clone()]));
                outcome
            }

            ServerMessage::PairDeviceJoinKeyInvalid => Outcome {
                events: vec![PairingEvent::KeyInvalid],
                send: Vec::new(),
            },

            ServerMessage::JoinKeyRateLimit => Outcome {
                events: vec![PairingEvent::RateLimited],
                send: Vec::new(),
            },

            ServerMessage::PairDeviceCanceled { .. } => {
                self.active = None;
                Outcome { events: vec![PairingEvent::Canceled], send: Vec::new() }
            }

            ServerMessage::SecretRoomDeleted { room_secret } => {
                let name = self
                    .devices
                    .iter()
                    .find(|d| &d.secret == room_secret)
                    .map(|d| d.display_name.clone())
                    .unwrap_or_else(|| "A paired device".to_string());
                self.devices.retain(|d| &d.secret != room_secret);

                let mut outcome = self.persist();
                outcome.events.push(PairingEvent::Unpaired { display_name: name });
                outcome
            }

            ServerMessage::RoomSecretRegenerated { old_room_secret, new_room_secret } => {
                let mut rotated = false;
                for device in &mut self.devices {
                    if &device.secret == old_room_secret {
                        device.secret = new_room_secret.clone();
                        rotated = true;
                    }
                }
                if !rotated {
                    return Outcome::default();
                }

                let mut outcome = self.persist();
                outcome.events.push(PairingEvent::SecretRotated);
                // Re-register under the new secret or the room is lost on reconnect.
                outcome.send.push(ClientMessage::RoomSecrets(
                    self.devices.iter().map(|d| d.secret.clone()).collect(),
                ));
                outcome
            }

            _ => Outcome::default(),
        }
    }

    /// Writes the list out, turning a storage failure into an event rather than losing
    /// it — the pairing still works for this session either way.
    fn persist(&self) -> Outcome {
        match self.store.save(&self.devices) {
            Ok(()) => Outcome::default(),
            Err(error) => Outcome {
                events: vec![PairingEvent::NotPersisted(error.to_string())],
                send: Vec::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairing() -> Pairing {
        Pairing::new(Box::new(MemoryStore::new()))
    }

    #[test]
    fn a_key_must_be_six_digits() {
        let p = pairing();
        assert!(p.join("123456").is_ok());
        // Pasted with punctuation, which people do.
        assert!(matches!(
            p.join("123 456").unwrap(),
            ClientMessage::PairDeviceJoin { pair_key } if pair_key == "123456"
        ));
        assert!(p.join("12345").is_err());
        assert!(p.join("1234567").is_err());
        assert!(p.join("abcdef").is_err());
        assert!(p.join("").is_err());
    }

    #[test]
    fn initiating_surfaces_the_key() {
        let mut p = pairing();
        let outcome = p.handle(&ServerMessage::PairDeviceInitiated {
            room_secret: "s3cret".into(),
            pair_key: "004321".into(),
        });
        assert_eq!(
            outcome.events,
            vec![PairingEvent::KeyReady { pair_key: "004321".into() }]
        );
        assert_eq!(p.active().unwrap().room_secret, "s3cret");
    }

    #[test]
    fn joining_stores_the_secret_and_registers_the_room() {
        let mut p = pairing();
        let outcome = p.handle(&ServerMessage::PairDeviceJoined {
            room_secret: "s3cret".into(),
            peer_id: "peer-1".into(),
        });

        assert!(outcome
            .events
            .iter()
            .any(|e| matches!(e, PairingEvent::Paired { secret, .. } if secret == "s3cret")));
        assert!(outcome
            .send
            .iter()
            .any(|m| matches!(m, ClientMessage::RoomSecrets(s) if s == &["s3cret"])));
        assert_eq!(p.devices().len(), 1);
        assert!(p.active().is_none());
    }

    /// Both ends receive `pair-device-joined`, and the initiator already holds the
    /// secret — adding it twice would show a duplicate device forever.
    #[test]
    fn pairing_twice_with_one_secret_stores_one_device() {
        let mut p = pairing();
        let joined = ServerMessage::PairDeviceJoined {
            room_secret: "s3cret".into(),
            peer_id: "peer-1".into(),
        };
        p.handle(&joined);
        p.handle(&joined);
        assert_eq!(p.devices().len(), 1);
    }

    #[test]
    fn secrets_are_replayed_on_reconnect() {
        let mut p = pairing();
        assert!(p.room_secrets_message().is_none(), "nothing to replay when unpaired");

        p.handle(&ServerMessage::PairDeviceJoined {
            room_secret: "a".into(),
            peer_id: "peer-1".into(),
        });
        p.handle(&ServerMessage::PairDeviceJoined {
            room_secret: "b".into(),
            peer_id: "peer-2".into(),
        });

        match p.room_secrets_message().unwrap() {
            ClientMessage::RoomSecrets(secrets) => assert_eq!(secrets, vec!["a", "b"]),
            other => panic!("expected room-secrets, got {other:?}"),
        }
    }

    #[test]
    fn unpairing_tells_the_server_what_is_left() {
        let mut p = pairing();
        for (secret, peer) in [("a", "p1"), ("b", "p2")] {
            p.handle(&ServerMessage::PairDeviceJoined {
                room_secret: secret.into(),
                peer_id: peer.into(),
            });
        }

        let outcome = p.unpair("a");
        assert_eq!(p.devices().len(), 1);
        assert!(outcome
            .send
            .iter()
            .any(|m| matches!(m, ClientMessage::RoomSecretsDeleted(s) if s == &["a"])));
        assert!(outcome
            .send
            .iter()
            .any(|m| matches!(m, ClientMessage::RoomSecrets(s) if s == &["b"])));
    }

    /// A rotated secret that isn't re-registered leaves the room silently unreachable
    /// after the next reconnect.
    #[test]
    fn a_rotated_secret_is_stored_and_re_registered() {
        let mut p = pairing();
        p.handle(&ServerMessage::PairDeviceJoined {
            room_secret: "old".into(),
            peer_id: "peer-1".into(),
        });

        let outcome = p.handle(&ServerMessage::RoomSecretRegenerated {
            old_room_secret: "old".into(),
            new_room_secret: "new".into(),
        });

        assert_eq!(p.devices()[0].secret, "new");
        assert!(outcome.events.contains(&PairingEvent::SecretRotated));
        assert!(outcome
            .send
            .iter()
            .any(|m| matches!(m, ClientMessage::RoomSecrets(s) if s == &["new"])));
    }

    #[test]
    fn a_rotation_we_do_not_know_about_is_ignored() {
        let mut p = pairing();
        let outcome = p.handle(&ServerMessage::RoomSecretRegenerated {
            old_room_secret: "unknown".into(),
            new_room_secret: "new".into(),
        });
        assert!(outcome.events.is_empty());
        assert!(outcome.send.is_empty());
    }

    #[test]
    fn the_other_side_unpairing_removes_the_device() {
        let mut p = pairing();
        p.handle(&ServerMessage::PairDeviceJoined {
            room_secret: "a".into(),
            peer_id: "peer-1".into(),
        });
        p.set_display_name("a", "Josh's Phone");

        let outcome = p.handle(&ServerMessage::SecretRoomDeleted { room_secret: "a".into() });
        assert!(p.devices().is_empty());
        assert!(outcome.events.contains(&PairingEvent::Unpaired {
            display_name: "Josh's Phone".into()
        }));
    }

    #[test]
    fn auto_accept_is_per_device_and_persists() {
        let shared = std::sync::Arc::new(MemoryStore::new());
        // Re-loading from the same backing store is what proves it was written.
        let mut p = Pairing::new(Box::new(SharedStore(shared.clone())));
        p.handle(&ServerMessage::PairDeviceJoined {
            room_secret: "a".into(),
            peer_id: "peer-1".into(),
        });
        p.set_auto_accept("a", true);
        assert!(p.auto_accepts("a"));

        let reloaded = Pairing::new(Box::new(SharedStore(shared)));
        assert!(reloaded.auto_accepts("a"), "auto-accept did not survive a reload");
        assert!(!reloaded.auto_accepts("nonexistent"));
    }

    /// Lets two `Pairing` instances share one backing store, to test reload behaviour.
    struct SharedStore(std::sync::Arc<MemoryStore>);

    impl SecretStore for SharedStore {
        fn load(&self) -> Vec<PairedDevice> {
            self.0.load()
        }
        fn save(&self, devices: &[PairedDevice]) -> Result<(), StoreError> {
            self.0.save(devices)
        }
        fn description(&self) -> String {
            self.0.description()
        }
    }

    #[test]
    fn a_storage_failure_is_reported_not_swallowed() {
        struct Failing;
        impl SecretStore for Failing {
            fn load(&self) -> Vec<PairedDevice> {
                Vec::new()
            }
            fn save(&self, _: &[PairedDevice]) -> Result<(), StoreError> {
                Err(StoreError::Backend("keyring is locked".into()))
            }
            fn description(&self) -> String {
                "failing".into()
            }
        }

        let mut p = Pairing::new(Box::new(Failing));
        let outcome = p.handle(&ServerMessage::PairDeviceJoined {
            room_secret: "a".into(),
            peer_id: "peer-1".into(),
        });

        assert!(outcome
            .events
            .iter()
            .any(|e| matches!(e, PairingEvent::NotPersisted(_))));
        // The pairing still works for this session.
        assert_eq!(p.devices().len(), 1);
    }
}
