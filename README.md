# PairDrop native clients

Small, native clients for [PairDrop](https://github.com/schlagmichdoch/PairDrop) — the
self-hosted "AirDrop for everything else". Drag a file onto the menu bar, drop it on a
nearby device, done. Transfers go peer-to-peer over a WebRTC data channel, speaking the
same protocol as the web client, so a native client shows up alongside browsers and the
Android app on any PairDrop instance.

| Platform | Status |
|---|---|
| **macOS** | Menu bar app — [install below](#install-macos) |
| iOS | Planned |
| Linux | Planned |

You need a PairDrop instance to connect to. Nothing is configured by default: point the
app at your own server (`https://drop.example.com`) or at
[pairdrop.net](https://pairdrop.net).

## Install (macOS)

Download `PairDrop-<version>-macOS-universal.dmg` from
[Releases](../../releases/latest), open it, and drag PairDrop to Applications.

macOS 14 or later, Apple Silicon and Intel.

### First launch

Builds are **ad-hoc signed** — there is no Apple Developer ID behind this yet — so macOS
will say it "cannot verify the developer". Two ways past it:

- Open the app, then go to **System Settings → Privacy & Security** and click
  **Open Anyway** next to the message about PairDrop.
- Or clear the download flag yourself:

  ```sh
  find /Applications/PairDrop.app -exec xattr -d com.apple.quarantine {} \; 2>/dev/null
  ```

PairDrop has no Dock icon — look for the paper plane in the menu bar. It opens Settings
on first launch so you can enter your instance's address.

Full usage, protocol notes, and what isn't built yet: [`macos/README.md`](macos/README.md).

## Build from source

```sh
cd macos
./install.sh          # universal release → /Applications, then launches it
```

Needs Xcode 15+ (Swift 5.9). `./build.sh` builds without installing;
`./package.sh` produces the release artifacts. Cutting a release:
[`docs/RELEASING.md`](docs/RELEASING.md).

## Layout

```
shared/PairDropKit/   Protocol implementation in Swift — no UI, shared across platforms
macos/                The menu bar app
ios/  linux/          Not started
docs/                 Release process
```

The protocol layer deliberately sits at the repo root rather than inside `macos/`, so the
iOS client can import it unchanged.

## Credits and licence

PairDrop is by [schlagmichdoch](https://github.com/schlagmichdoch/PairDrop), itself based
on Snapdrop by RobinLinus. This repository is an independent native client for that
protocol and is not affiliated with the upstream project.

Licensed under [GPL-3.0](LICENSE), matching upstream.
