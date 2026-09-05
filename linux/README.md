# PairDrop for Linux

A GTK4/libadwaita client for [PairDrop](https://github.com/schlagmichdoch/PairDrop),
in Rust. **In progress** — the protocol layer is done and tested; there is no app yet.

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
  pairdrop-cli/     `pairdrop-probe` — headless peer and instance diagnostic
```

Still to come: the WebRTC data channel, the transfer state machine, secret storage via the
Secret Service, and the GTK4 app.

## Building

Everything so far is pure Rust with rustls rather than the platform TLS stack, so it
builds and tests **on any platform** — including the Mac this is being developed on:

```sh
cd linux
cargo test
cargo run --bin pairdrop-probe -- https://drop.example.com --quit-after 20
```

Verified building and running on Linux too:

```sh
docker run --rm -v "$PWD":/w -w /w rust:1.94-slim cargo test
```

## `pairdrop-probe`

A headless peer: it connects, joins the IP room, and reports what the server says.
Transfers aren't wired up yet — that needs the data channel.

It doubles as an instance diagnostic. If peers show "couldn't connect", this says why:

```
ICE:      1 STUN, 0 TURN; ws fallback off
  ⚠ STUN only and no fallback: peers that can't reach each other directly
    have no path. Add a TURN server, or run the instance with --include-ws-fallback.
```

`--allow-untrusted-tls` for a self-signed certificate, `--quit-after N` to exit on a
timer, `--max-attempts N` to stop retrying and exit non-zero.

The UI itself needs a real desktop session — a container has no display server, no D-Bus
session bus, and no tray host, and X11 forwarding wouldn't exercise the tray or
drag-and-drop, which are the parts most worth watching.

## App shape

macOS puts this in the menu bar. That doesn't translate: GNOME removed the system tray in
2017, and `StatusNotifierItem` needs a shell extension there.

So the primary surface is a **small window**, with a tray icon *when the desktop provides
one* — KDE Plasma, Cinnamon, XFCE, or GNOME with AppIndicator installed. The app has to be
fully usable without it. Targets are GNOME and KDE Plasma.

## Notes on the protocol

Two things the Swift port learned the hard way, both load-bearing:

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
- **The server can't name a native client.** It builds the device label as
  `os.name + " " + (device.model ?? browser.name)` with no guard for the undefined case,
  and ua-parser-js has no generic-browser fallback — so any native client reads as
  "Linux undefined" (the macOS one is "Mac undefined"). No User-Agent avoids it without
  impersonating a browser. It only shows before a data channel opens; after that
  `display-name-changed` carries the real hostname.

The rest of the wire protocol is documented in [`../macos/README.md`](../macos/README.md).
