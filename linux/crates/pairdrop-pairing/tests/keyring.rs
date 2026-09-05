//! Exercises the real platform credential store — Secret Service on Linux, Keychain on
//! macOS.
//!
//! `#[ignore]` because it needs a live credential store and writes to it: a container,
//! a CI runner, or a headless session has none, and a normal `cargo test` shouldn't
//! touch the user's keyring. Run it deliberately:
//!
//! ```sh
//! cargo test -p pairdrop-pairing --test keyring -- --ignored --test-threads=1
//! ```

use pairdrop_pairing::{KeyringStore, PairedDevice, SecretStore};

/// Namespaced away from the real entry, and removed again at the end.
const TEST_SERVICE: &str = "app.pairdrop.roomsecrets.test";

fn device(secret: &str, name: &str, auto: bool) -> PairedDevice {
    PairedDevice {
        secret: secret.to_string(),
        display_name: name.to_string(),
        auto_accept: auto,
    }
}

#[test]
#[ignore = "writes to the real credential store"]
fn secrets_survive_a_round_trip_through_the_platform_store() {
    let store = KeyringStore::with_service(TEST_SERVICE)
        .expect("no credential store available — is gnome-keyring or KWallet running?");
    store.clear().unwrap();
    assert!(store.load().is_empty(), "the test entry was not clean");

    let devices = vec![
        device("secret-one", "Josh's Phone", true),
        device("secret-two", "Living Room Pi", false),
    ];
    store.save(&devices).unwrap();

    // A fresh handle, so this reads from the store rather than from memory.
    let reopened = KeyringStore::with_service(TEST_SERVICE).unwrap();
    let loaded = reopened.load();
    assert_eq!(loaded, devices, "what came back differs from what went in");
    assert!(loaded[0].auto_accept);
    assert!(!loaded[1].auto_accept);

    // Saving an empty list removes the entry rather than leaving an empty one behind.
    reopened.save(&[]).unwrap();
    assert!(KeyringStore::with_service(TEST_SERVICE).unwrap().load().is_empty());

    store.clear().unwrap();
}

/// A secret with characters that would break a naive encoding must survive intact —
/// the server generates these, and we never get to choose the alphabet.
#[test]
#[ignore = "writes to the real credential store"]
fn awkward_values_survive() {
    let store = KeyringStore::with_service(TEST_SERVICE).unwrap();
    store.clear().unwrap();

    let devices = vec![device(
        "aB3/+=\"\\{}[]:,\u{1F30D}",
        "Ünïcödé \"quoted\" name 🌍",
        true,
    )];
    store.save(&devices).unwrap();
    assert_eq!(KeyringStore::with_service(TEST_SERVICE).unwrap().load(), devices);

    store.clear().unwrap();
}
