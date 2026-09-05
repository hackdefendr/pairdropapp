# PairDrop native clients

Small, native clients for [PairDrop](https://github.com/schlagmichdoch/PairDrop) — the
self-hosted "AirDrop for everything else". Pick a nearby device, drop a file on it, done.

Transfers go peer-to-peer over a WebRTC data channel, speaking the same protocol as the
web client, so these show up alongside browsers and the Android app on any PairDrop
instance — and never route file contents through the server.

| Platform | Status | |
|---|---|---|
| **macOS** | Menu bar app | [Download](../../releases/tag/v0.1.0) · [docs](macos/README.md) |
| **Linux** | GTK4 window, Flatpak | [Download](../../releases/tag/linux-v0.1.0) · [docs](linux/README.md) |
| iOS | Not started | |

You need a PairDrop instance to connect to. **Nothing is configured by default** — point
the app at your own server (`https://drop.example.com`) or at
[pairdrop.net](https://pairdrop.net).

## Installing

**macOS** — open the `.dmg` and drag PairDrop to Applications. macOS 14+, Apple Silicon
and Intel. Builds are ad-hoc signed, so macOS will say it "cannot verify the developer";
[`macos/README.md`](macos/README.md#install) has the two ways past that. There is no Dock
icon — look for the paper plane in the menu bar.

**Linux** — `flatpak install --user PairDrop-*.flatpak`, then
`flatpak run app.pairdrop.Linux`. x86_64 only; other architectures build from source.
Details, permissions, and the source-build route: [`linux/README.md`](linux/README.md).

Both open their settings on first launch so you can enter your instance's address.

## Building from source

```sh
cd macos && ./install.sh     # universal release → /Applications
cd linux && ./install.sh     # → ~/.local
```

macOS needs Xcode 15+; Linux needs Rust 1.85+, GTK 4 and libadwaita 1.5+. Each platform's
README covers its own toolchain, and [`docs/RELEASING.md`](docs/RELEASING.md) covers
cutting a release.

## Layout

```
shared/PairDropKit/   The protocol in Swift — no UI, shared with the future iOS client
macos/                Menu bar app: AppKit shell, SwiftUI panel
linux/                GTK4 app, and the protocol in Rust
  crates/pairdrop-proto      Wire format
  crates/pairdrop-net        Signalling over WebSocket
  crates/pairdrop-rtc        Peer connection and data channel
  crates/pairdrop-transfer   Transfer state machine and chunking
  crates/pairdrop-pairing    Six-digit pairing, secrets in the credential store
  crates/pairdrop-client     The engine — everything above, minus a UI
  crates/pairdrop-cli        `pairdrop-probe`, a headless peer and instance diagnostic
ios/                  Not started
docs/                 Release process and notes
```

The two platforms share a protocol, not code. Swift's half of it sits at the repo root so
iOS can import it unchanged; Linux needed its own implementation because
`URLSessionWebSocketTask`, the WebRTC xcframework and the Keychain are all Apple-only.
What keeps them honest is a set of test vectors checked against the original JavaScript,
used by both.

The wire protocol itself is written up in [`macos/README.md`](macos/README.md#how-it-talks-to-the-server),
with the details that are easy to get subtly wrong collected in
[`linux/README.md`](linux/README.md#notes-on-the-protocol).

## Credits and licence

PairDrop is by [schlagmichdoch](https://github.com/schlagmichdoch/PairDrop), itself based
on Snapdrop by RobinLinus. This repository is an independent set of native clients for
that protocol and is not affiliated with the upstream project.

Licensed under [GPL-3.0](LICENSE), matching upstream.
