First Linux release — a GTK4/libadwaita client for
[PairDrop](https://github.com/schlagmichdoch/PairDrop).

## Install

Download the `.flatpak` and install it:

```sh
flatpak install --user PairDrop-0.1.0-linux-x86_64.flatpak
flatpak run app.pairdrop.Linux
```

Needs the GNOME 50 runtime, which Flatpak fetches automatically if you don't have it.
**x86_64 only** — on other architectures, build from source (below).

On first launch, open the menu → Preferences and enter your instance's address. Nothing
is configured by default; point it at your own server or a public one.

## What it does

- **Nearby devices** appear automatically. Drop files on one to send, or click it to
  pick files.
- **Incoming transfers** prompt before anything touches the disk, and land in
  `~/Downloads` by default.
- **Text** sent to you is copied to the clipboard, which is what the web client does.
- **Pairing** (menu → Pair a Device) connects two devices on different networks with a
  six-digit key. Paired devices stay visible from anywhere and can be set to accept
  files without asking.
- **Verification code** — each connected device shows the same 16-digit code on both
  ends, so you can confirm you're talking to who you think you are.

Transfers are peer-to-peer over a WebRTC data channel and never pass through the server.

## What the sandbox asks for

| Permission | Why |
|---|---|
| `network` | The signalling WebSocket, and the UDP sockets WebRTC binds for ICE |
| `xdg-download` | Received files are written without user interaction, so there's no portal request to hang them on |
| `org.freedesktop.secrets` | Pairing secrets. Without it the app still runs — pairings just don't outlive the session |

Files you *send* go through the file portal, so it never needs blanket read access.

Choosing a download folder outside `~/Downloads` works for the session, but the portal
path isn't stable across restarts — under Flatpak, leaving it at the default is the
reliable choice.

## Build from source

```sh
git clone https://github.com/hackdefendr/pairdropapp
cd pairdropapp/linux
./install.sh          # → ~/.local, no root needed
```

Needs Rust 1.85+, GTK 4, and **libadwaita 1.5 or newer**. Debian 13, Fedora and Arch all
clear that; older LTS releases don't, which is what the Flatpak is for.

## Not in this release

- **No tray icon.** The window is the only surface. This is deliberate for now: GNOME
  removed the system tray in 2017 and needs an extension for one, so the app has to be
  fully usable without it either way.
- **No dragging of selected text** onto a device — files only.
- **WebSocket fallback.** Instances run with `--include-ws-fallback` relay for peers
  without WebRTC; peers advertising `rtcSupported: false` are ignored here.
- **Folders.** Files only. Zero-byte files are skipped — the protocol has no way to
  signal their completion, and the web client stalls on them too.

If devices show "Couldn't connect", check your instance's ICE configuration: with STUN
only and no fallback, peers that can't reach each other directly have no path, and the
web client fails the same way. `pairdrop-probe` in this repo reports exactly that.

## Verified

Peer discovery, the transfer prompt, accepting, and pairing were all exercised against a
real instance on x86_64 — both natively and through the installed Flatpak — with received
files checked byte-for-byte by SHA-256.
