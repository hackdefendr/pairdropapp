# PairDrop for Linux

Not started, and last in the queue behind iOS.

`shared/PairDropKit` is Swift on top of Apple's `URLSessionWebSocketTask`, `Network`, and
the WebRTC xcframework, none of which exist here — so this platform needs its own
transport layer rather than a straight port. The wire protocol notes in
[`../macos/README.md`](../macos/README.md) are the part that carries over.
