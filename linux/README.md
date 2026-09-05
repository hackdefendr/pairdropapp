# PairDrop for Linux

A GTK4/libadwaita client for [PairDrop](https://github.com/schlagmichdoch/PairDrop),
in Rust. Drop files on a nearby device and they're there.

## Why a rewrite rather than a port

`shared/PairDropKit` is Swift, and about half of it is Apple-only:

| Ports | Doesn't |
|---|---|
| The wire format, `cyrb53`, address handling, the chunk/partition rhythm, the transfer state machine | `SignalingClient` — `URLSessionWebSocketTask` [fatalErrors on Linux](https://github.com/swiftlang/swift-corelibs-foundation/issues/4730), since corelibs builds `URLSession` on libcurl |
| | `RTCSession` — the WebRTC xcframework is Apple platforms only |
| | `RoomSecretStore` — Keychain; here it's the Secret Service / `libsecret` |
| | All UI — AppKit and SwiftUI |

Swift's GTK bindings are immature enough that reusing the remaining half would cost more
than rewriting it, so this is Rust: `gtk4-rs` and libadwaita for the UI, and a data-channel
stack that exists on Linux.

The wire format is what has to match, not the code — and it's pinned by test vectors
shared with the Swift implementation, both checked against the original JavaScript.

## Layout

```
crates/
  pairdrop-proto/   Wire protocol. No I/O, no async, no toolkit — builds anywhere.
    cyrb53          Connection-verification hash, matching the browser's
    endpoint        User-typed address → /config and WebSocket URLs
    signaling       Server frames in and out
    transfer        Data-channel control frames
  pairdrop-net/     WebSocket transport: connect, keepalive, reconnect, TLS
  pairdrop-rtc/       One peer connection and its data channel
  pairdrop-transfer/  The transfer state machine, and file chunking on both sides
  pairdrop-pairing/   Six-digit pairing, and secrets in the platform credential store
  pairdrop-client/    The engine: signaling, peers, transfers and pairing, no UI
  pairdrop-gtk/       The app — GTK4 and libadwaita
  pairdrop-cli/       `pairdrop-probe` — headless peer and instance diagnostic
```

The engine runs a tokio runtime on its own thread and talks to the UI over channels, so
the GTK main loop never waits on the network.

Still to come: a tray icon where the desktop has one, and drag-and-drop of selected text.

## Installing

### Flatpak

Brings its own GTK and libadwaita, so it doesn't care what the distribution ships:

```sh
cd linux
./flatpak/build.sh              # build and install for the current user
./flatpak/build.sh --bundle     # also writes pairdrop.flatpak, a single shareable file
flatpak run app.pairdrop.Linux
```

The first build downloads the GNOME runtime and SDK — a couple of gigabytes — and takes
a while. Later builds reuse them.

Dependencies are pre-declared in `flatpak/cargo-sources.json` because the build sandbox
has no network. Regenerate it whenever `Cargo.lock` changes, using
[flatpak-builder-tools](https://github.com/flatpak/flatpak-builder-tools):

```sh
python3 flatpak-cargo-generator.py Cargo.lock -o flatpak/cargo-sources.json
```

### From source

```sh
cd linux
./install.sh                    # → ~/.local, no root needed
./install.sh --prefix /usr/local
```

Needs GTK 4 and libadwaita development packages:

| | |
|---|---|
| Debian, Ubuntu, Kali | `sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev` |
| Fedora | `sudo dnf install gcc pkgconf gtk4-devel libadwaita-devel` |
| Arch | `sudo pacman -S base-devel gtk4 libadwaita` |

On first launch, open the menu → Preferences and enter your instance's address. Nothing
is configured by default.

### What the Flatpak asks for

| Permission | Why |
|---|---|
| `--share=network` | The signalling WebSocket, and the UDP sockets WebRTC binds for ICE |
| `--filesystem=xdg-download` | Received files are written without user interaction, so there is no portal request to hang them on |
| `--talk-name=org.freedesktop.secrets` | Pairing secrets. Without it the app still runs; pairings just don't outlive the session |

Files you *send* go through the file portal, so the app never needs blanket read access.

Choosing a download folder outside `~/Downloads` works for the session, but the portal
path it produces isn't stable across restarts — so under Flatpak, leaving it at the
default is the reliable choice. Building from source has no such limit.

## Building

Everything except the GUI is pure Rust with rustls rather than the platform TLS stack, so
it builds and tests **on any platform** — including the macOS machine this was developed
on. The GTK crate is excluded from the workspace's `default-members` for exactly that
reason:

```sh
cd linux
cargo test                      # everything but the GUI
cargo build -p pairdrop-gtk     # the GUI, on Linux
cargo run --bin pairdrop-probe -- https://drop.example.com --quit-after 20
```

Verified building and running on Linux too:

```sh
docker run --rm -v "$PWD":/w -w /w -e CARGO_TARGET_DIR=/w/target/docker \
    rust:1.94-slim cargo test
```

`CARGO_TARGET_DIR` matters: macOS and Linux on Apple silicon are both `aarch64` host
triples, so without it the container and the host fight over `target/debug` — each run
invalidates the other's build, and running a binary from the wrong one gives
`Exec format error`.

The credential-store tests are `#[ignore]`d, because they need a live Secret Service and
write to it — a container or CI runner has none, and a plain `cargo test` shouldn't touch
the user's keyring. Run them deliberately on a desktop session:

```sh
cargo test -p pairdrop-pairing --test keyring -- --ignored --test-threads=1
```

The GUI needs a real display. For automated checks it runs headless under Xvfb, which is
enough to drive it with `xdotool` and screenshot the result — that is how the transfer
prompt and the pairing flow below were verified.

## `pairdrop-probe`

A headless peer: it connects, joins the IP room, and with `--dial` opens a WebRTC data
channel to every peer it finds. It accepts every incoming transfer, which is what makes
it useful as a test receiver.

```
Room 127.0.0.1 (ip): 1 peer(s)
  • Blue Quokka [1ac6680e-…]
→ Blue Quokka: calling …
✓ Blush Lemming connected as caller — verification 4183170496344325
· Blush Lemming is called "Rust Receiver"
→ Rust Receiver sending 2 file(s) …
  Rust Receiver 100%
✓ sent 2 file(s) to Rust Receiver
```

Two probes against a real instance move files byte-for-byte, verified by SHA-256 over a
2.5 MB file spanning three partitions.

It doubles as an instance diagnostic. If peers show "couldn't connect", this says why:

```
ICE:      1 STUN, 0 TURN; ws fallback off
  ⚠ STUN only and no fallback: peers that can't reach each other directly
    have no path. Add a TURN server, or run the instance with --include-ws-fallback.
```

`--send FILE…` with `--to NAME` to transfer, `--text` to send a message, `--out DIR` for
where received files land, `--pair` / `--join KEY` / `--unpair-all` for pairing,
`--dial` to connect rather than only list, `--name` for the name peers see,
`--allow-untrusted-tls` for a self-signed certificate, `--quit-after N` to exit on a
timer, `--max-attempts N` to stop retrying and exit non-zero.


## App shape

macOS puts this in the menu bar. That doesn't translate: GNOME removed the system tray in
2017, and `StatusNotifierItem` needs a shell extension there.

So the primary surface is a **small window**: a list of nearby devices, each one a drop
target. Click a device to pick files instead. Incoming transfers prompt before anything
touches the disk, unless the sender is a paired device you've marked trusted.

A tray icon is still to come, and optional by design — the app must stay fully usable
without one. Targets are GNOME and KDE Plasma; development happened on Kali's XFCE.

### A note on GTK panics

A panic inside a GTK callback crosses the C boundary and **aborts** rather than unwinding,
so a `RefCell` misuse is fatal rather than recoverable. One shipped that way during
development — `ui.paired_devices.borrow().clone()` passed straight into a function that
takes `borrow_mut()`, where the temporary borrow outlives the call — and it killed the
process the first time Preferences was opened. Bind such borrows to a local first.

## Pairing

Two devices on different networks can't see each other in the IP room. One creates a
six-digit key, the other enters it, and the server puts both in a shared *secret room*
they find each other in from anywhere.

```sh
pairdrop-probe https://drop.example.com --pair          # prints a key
pairdrop-probe https://drop.example.com --join 823866   # on the other device
```

The **room secret is a bearer credential** — anyone holding it can join the room and send
to the device — so it goes in the platform credential store: Secret Service on Linux
(gnome-keyring, KWallet), Keychain on macOS. It is never written to a config file, and
never printed: secret-room ids are shown as `paired devices (secret)` precisely so a
terminal scrollback or a piped log can't leak one.

With no credential store available — a container, a headless box, a session without
gnome-keyring — pairing still works but lasts only until the process exits, and says so
on startup. Falling back to a plain file was the alternative and is a worse trade to make
on the user's behalf without asking.

Secrets are replayed as `room-secrets` on every reconnect; without that a paired device
stays invisible until the next pairing.

## Notes on the protocol

Details that are load-bearing for interop, each found by a test rather than by
reading the spec:

- **`cyrb53` walks UTF-16 code units**, matching JavaScript's `charCodeAt`. Iterating
  Unicode scalars gives a different verification code, but only for strings containing
  emoji or other astral characters — so it looks correct until someone names their laptop
  with an emoji.
- **A "1 MB partition" is really 1,024,000 bytes.** Neither the web client nor the Swift
  port truncates a chunk to land on the threshold; both emit whole 64,000-byte chunks and
  end the partition once the total *reaches or passes* 1,000,000. That's 16 chunks. Making
  the last chunk land exactly on 1,000,000 would desynchronise the handshake against every
  other client, and only on transfers over a megabyte.
- **`sdpMLineIndex` has a capital L.** serde's `rename_all = "camelCase"` produces
  `sdpMlineIndex`, which silently never matches, and an ICE candidate that loses its
  m-line index is dropped by some peers and accepted by others. Renamed explicitly, with
  a test on both the parse and the serialize side.
- **A 64,000-byte chunk is close to the ceiling.** SCTP negotiates a 65,536-byte maximum
  message size by default, so PairDrop's chunks fit with 1,536 bytes to spare. The
  `webrtc` crate's own docs claim a 16,384-byte limit on received messages; that is stale,
  and `two_sessions_connect_and_exchange_data` sends a full chunk to prove it.
- **Both peers must derive the same connection hash.** It is `cyrb53` of the two DTLS
  fingerprints concatenated *caller's first*, so the two ends have to agree on who
  called. Reverse it on one side and the codes differ, which reads to a user as a failed
  verification rather than as a bug.
- **The server can't name a native client.** It builds the device label as
  `os.name + " " + (device.model ?? browser.name)` with no guard for the undefined case,
  and ua-parser-js has no generic-browser fallback — so any native client reads as
  "Linux undefined" (the macOS one is "Mac undefined"). No User-Agent avoids it without
  impersonating a browser. It only shows before a data channel opens; after that
  `display-name-changed` carries the real hostname.

The rest of the wire protocol is documented in [`../macos/README.md`](../macos/README.md).
