# PairDrop for macOS

A menu bar client for [PairDrop](https://github.com/schlagmichdoch/PairDrop). Drag files
onto the menu bar icon, drop them on a nearby device, done. Transfers go peer-to-peer over
a WebRTC data channel — the same protocol the web client speaks, so this Mac shows up
alongside browsers and the Android app on any PairDrop instance.

## Install

Download the `.dmg` from [Releases](../../releases/latest), open it, and drag PairDrop to
Applications. macOS 14 or later, Apple Silicon and Intel. Releases are ad-hoc signed —
see [First launch](../README.md#first-launch) for getting past Gatekeeper.

From source instead:

```sh
./install.sh
```

Builds a universal release, installs to `/Applications`, and launches it. `--to
~/Applications` installs elsewhere, `--no-launch` skips starting it.

PairDrop has no Dock icon — look for the paper plane in the menu bar. The first launch
opens Settings. Enter your instance's address — the same URL you'd open in a browser
(`https://drop.example.com`, or `http://192.168.1.50:3000` on a LAN). Bare hostnames
default to `https`, LAN addresses to `http`. Nothing is configured by default.

Turn on **Open PairDrop at login** in Settings once it's installed; macOS won't register a
login item for an app running out of a build directory.

## Build without installing

```sh
./build.sh              # debug
./build.sh release      # release
./build.sh release universal
open .build/arm64-apple-macosx/release/PairDrop.app
```

`swift Scripts/make-icon.swift` regenerates `Resources/PairDrop.icns`; `build.sh` does it
automatically if the file is missing.

### Signing

`build.sh` ad-hoc signs, which is all a locally built app needs — Gatekeeper only assesses
code that arrives quarantined, so `spctl` reporting "rejected" is expected and doesn't stop
it launching. To hand the app to someone else it needs a real identity:

```sh
SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" ./install.sh
```

That switches on the hardened runtime, ready to notarize. Ad-hoc signatures and the
hardened runtime are mutually exclusive here: dyld refuses to load an embedded framework
whose Team ID doesn't match the app's, and ad-hoc signatures have no Team ID.

## Release artifacts

```sh
./package.sh 0.1.0
```

Builds a universal release and writes a `.dmg`, a `.zip`, and `SHA256SUMS` to `dist/`.
With `SIGN_IDENTITY` and `NOTARY_PROFILE` set it also notarizes and staples both the app
and the disk image. Full process: [`../docs/RELEASING.md`](../docs/RELEASING.md).

## Using it

| | |
|---|---|
| **Click the icon** | Show nearby devices |
| **Drag files onto the icon** | Opens the panel — then drop on the device you want |
| **Drop on a device** | Sends immediately; they get a request to accept |
| **Right-click a device** | Send Files… / Send Message… / see the connection hash |
| **Drag selected text** | Sends it as a message; the receiver gets it on their clipboard |

Incoming transfers show an Accept / Decline in the panel. Received files land in
`~/Downloads` unless you pick another folder in Settings.

**Pairing** (Settings → Devices) connects two devices that aren't on the same network.
One creates a six-digit key, the other enters it. Paired devices stay visible anywhere,
and can be set to accept files automatically. Pairing secrets are stored in the keychain.

## Layout

```
shared/PairDropKit/     Protocol implementation — no UI, shared with the iOS app
  SignalingMessages     Wire format for the signaling server
  SignalingClient       WebSocket connection, keepalive, reconnect
  RTCSession            One peer connection + the `data-channel`
  PairDropPeer          Transfer state machine (request/header/chunk/partition)
  FileTransfer          Chunking on send, streaming to disk on receive
  PairDropClient        Owns the socket and the set of peers; what the UI observes
  pairdrop-probe        Headless peer for testing without a GUI

macos/                  The menu bar app
  StatusItemController  NSStatusItem, spring-loaded drop target, floating panel
  AppModel              Preferences ↔ client
  LoginItem             SMAppService registration
  Views/                SwiftUI panel, peer rows, settings
  Scripts/make-icon     Draws the app icon; run it to change the artwork
  build.sh              Assembles and signs PairDrop.app
  install.sh            Universal build → /Applications
  package.sh            Release artifacts → dist/
```

## How it talks to the server

Reproduced from `public/scripts/network.js` and `server/ws-server.js` upstream:

1. `GET /config` — picks up a `signalingServer` override if the instance sets one.
2. WebSocket to `<host><path>server?webrtc_supported=true`, reusing `peer_id` and
   `peer_id_hash` across reconnects so peers see one stable device.
3. The server pings every second and drops anything silent for five, so the receive loop
   answers `pong` inline.
4. `join-ip-room` puts us in the room for our public IP. Stored pairing secrets are
   replayed as `room-secrets`.
5. Peers already present are ones we call; peers that arrive after us call us. The caller
   creates the data channel named `data-channel` and sends the offer.
6. Transfers: `request` → `files-transfer-response` → per file a `header` then 64 KB binary
   chunks, with the receiver acknowledging each 1 MB partition before the sender reads more.

The server derives the name under a device from the `User-Agent`, and there is no browser
for it to recognise in ours, so it labels this Mac plain "Mac". The name peers actually
see is sent over the data channel once connected — the machine name by default, or
whatever you set in Settings.

When ICE gives up on a peer, the caller rebuilds the session and offers again on a
2/4/8/16-second backoff before showing "Couldn't connect". Peers that are merely slow to
wake — a backgrounded browser tab, a phone rejoining the network — recover on their own.

### A note on ICE configuration

A PairDrop instance hands clients its `iceServers` list. With STUN only, two peers that
can't reach each other directly have no path at all, and no client can fix that — the web
client fails the same way. If you see devices stuck at "Couldn't connect", the instance
needs a TURN server (upstream ships `docker-compose-coturn.yml` and `turnserver_example.conf`
for exactly this), or `--include-ws-fallback` to relay through the server.

## Testing

```sh
cd ../shared/PairDropKit && swift test          # protocol unit tests
```

`pairdrop-probe` is a full headless peer, useful for testing against a real instance:

```sh
swift build
./.build/debug/pairdrop-probe http://localhost:3000 --name Receiver --out /tmp/inbox
./.build/debug/pairdrop-probe http://localhost:3000 --to Receiver --send ~/file.zip
```

Verified against an actual PairDrop web client (headless Chrome) in both directions,
single and multi-file, byte-exact by SHA-256.

## Not done yet

- **WebSocket fallback.** Instances run with `--include-ws-fallback` relay transfers
  through the server for peers without WebRTC. The message layer is in place; `WSPeer`
  isn't. Peers advertising `rtcSupported: false` are ignored.
- **Folders.** Only files. Zero-byte files are skipped — the protocol has no way to signal
  their completion, and the web client stalls on them too.
- **Public rooms.** The client speaks the messages; there's no UI.
- **Thumbnails.** Outgoing image transfers don't include the preview the web client shows.
- **Notifications.** Requested on first launch, but only delivered when macOS considers
  the bundle trustworthy; an ad-hoc signed build may be ignored. The panel's activity list
  always works regardless.
