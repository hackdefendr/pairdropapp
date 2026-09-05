//! Where pairing secrets live.
//!
//! A room secret is a bearer credential: anyone holding it joins the paired room and can
//! send to the device. It does not belong in a config file, so the real implementation
//! puts it in the platform credential store — Secret Service on Linux, Keychain on macOS.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    pub secret: String,
    pub display_name: String,
    #[serde(default)]
    pub auto_accept: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("no credential store is available: {0}")]
    Unavailable(String),
    #[error("credential store error: {0}")]
    Backend(String),
}

pub trait SecretStore: Send + Sync {
    fn load(&self) -> Vec<PairedDevice>;
    fn save(&self, devices: &[PairedDevice]) -> Result<(), StoreError>;
    /// Human-readable description of where secrets are going, for the UI to show.
    fn description(&self) -> String;
}

// MARK: platform credential store

/// The whole list is stored as one JSON blob under a single entry, rather than an entry
/// per device. That keeps the number of credential-store prompts to one, and means a
/// removal can't leave an orphan behind.
pub struct KeyringStore {
    service: String,
    account: String,
}

impl KeyringStore {
    /// Fails when there is no Secret Service to talk to — a headless box, a container,
    /// or a session without gnome-keyring or KWallet running.
    pub fn new() -> Result<Self, StoreError> {
        Self::with_service("app.pairdrop.roomsecrets")
    }

    /// A separate namespace, so a test never touches the real entry.
    pub fn with_service(service: &str) -> Result<Self, StoreError> {
        if let Err(error) = keyring::Entry::store_status() {
            return Err(StoreError::Unavailable(error.to_string()));
        }
        Ok(Self {
            service: service.to_string(),
            account: "room-secrets".to_string(),
        })
    }

    /// Removes the stored entry entirely.
    pub fn clear(&self) -> Result<(), StoreError> {
        let _ = self.entry()?.delete_credential();
        Ok(())
    }

    fn entry(&self) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(&self.service, &self.account)
            .map_err(|e| StoreError::Backend(e.to_string()))
    }
}

impl SecretStore for KeyringStore {
    fn load(&self) -> Vec<PairedDevice> {
        let Ok(entry) = self.entry() else { return Vec::new() };
        match entry.get_password() {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            // A missing entry is the normal state before the first pairing.
            Err(_) => Vec::new(),
        }
    }

    fn save(&self, devices: &[PairedDevice]) -> Result<(), StoreError> {
        let entry = self.entry()?;

        // Storing an empty list would leave a useless entry behind; remove it instead.
        if devices.is_empty() {
            let _ = entry.delete_credential();
            return Ok(());
        }

        let json = serde_json::to_string(devices).map_err(|e| StoreError::Backend(e.to_string()))?;
        entry.set_password(&json).map_err(|e| StoreError::Backend(e.to_string()))
    }

    fn description(&self) -> String {
        if cfg!(target_os = "macos") {
            "the login keychain".to_string()
        } else {
            "the desktop keyring (Secret Service)".to_string()
        }
    }
}

// MARK: in-memory

/// Used by tests, and as the fallback when there is no credential store.
///
/// Falling back to this rather than to a file is deliberate: pairings last only for the
/// session, which is a visible and explainable limitation. Silently writing bearer
/// credentials to disk in plain text would be a worse trade the user never agreed to.
#[derive(Default)]
pub struct MemoryStore {
    devices: Mutex<Vec<PairedDevice>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemoryStore {
    fn load(&self) -> Vec<PairedDevice> {
        self.devices.lock().map(|d| d.clone()).unwrap_or_default()
    }

    fn save(&self, devices: &[PairedDevice]) -> Result<(), StoreError> {
        match self.devices.lock() {
            Ok(mut guard) => {
                *guard = devices.to_vec();
                Ok(())
            }
            Err(_) => Err(StoreError::Backend("lock poisoned".into())),
        }
    }

    fn description(&self) -> String {
        "memory only — pairings are lost when PairDrop quits".to_string()
    }
}

/// The platform store when one is available, memory otherwise. The error is returned
/// alongside so the caller can tell the user why pairings won't persist.
pub fn best_available() -> (Box<dyn SecretStore>, Option<StoreError>) {
    match KeyringStore::new() {
        Ok(store) => (Box::new(store), None),
        Err(error) => (Box::new(MemoryStore::new()), Some(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(secret: &str) -> PairedDevice {
        PairedDevice {
            secret: secret.to_string(),
            display_name: format!("Device {secret}"),
            auto_accept: false,
        }
    }

    #[test]
    fn memory_store_round_trips() {
        let store = MemoryStore::new();
        assert!(store.load().is_empty());

        store.save(&[device("a"), device("b")]).unwrap();
        assert_eq!(store.load().len(), 2);
        assert_eq!(store.load()[0].secret, "a");

        store.save(&[]).unwrap();
        assert!(store.load().is_empty());
    }

    /// Older entries were written before `auto_accept` existed; they must still load.
    #[test]
    fn devices_decode_without_auto_accept() {
        let json = r#"[{"secret":"s","display_name":"Laptop"}]"#;
        let devices: Vec<PairedDevice> = serde_json::from_str(json).unwrap();
        assert_eq!(devices[0].display_name, "Laptop");
        assert!(!devices[0].auto_accept);
    }

    #[test]
    fn best_available_always_returns_a_store() {
        let (store, problem) = best_available();
        // Either works; what matters is that a caller always gets somewhere to write,
        // and is told when that place is only memory.
        if problem.is_some() {
            assert!(store.description().contains("memory"));
        }
        assert!(!store.description().is_empty());
    }
}
