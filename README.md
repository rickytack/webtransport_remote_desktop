# WebTransport Remote Desktop

A small foundation for a remote desktop protocol over WebTransport.

Why WebTransport:
- Low‑latency, UDP‑like datagrams for video; reliable streams for control — same port/connection
- Browser‑ready: modern browsers expose the WebTransport API for in‑browser clients

## What this project does
- Server:
  - Captures frames from an X11 display
  - Encodes frames to H.264 (OpenH264)
  - Fragments and sends video over WebTransport datagrams
- Client:
  - Receives datagrams via WebTransport
  - Performs bounded, loss-aware reassembly of fragmented frames
  - Decodes H.264 frames and displays them in a native window

## Status
- Works end‑to‑end on Linux/X11 with a native GUI client.
- Media transport v1 is implemented in `common/src/protocol/`: fixed 32-byte
  fragment header, negotiated datagram sizing, duplicate/reorder/malformed
  handling, timeouts, memory limits, and a lossy randomized test suite.
- Video runs at 1280×720 / 30 fps in a local release build. Capture and
  encoding run on dedicated threads; the encoder is configured for low-complexity
  screen content.
- A new client receives an IDR on connect, and the server refreshes it every
  60 frames. There is no explicit client keyframe request yet.
- The control stream uses a typed, length-prefixed `Hello`/`Welcome`
  handshake. Explicit keyframe requests, pacing, and input are next (M2/M4).
- See [`docs/protocol.md`](docs/protocol.md) for the wire format and limits.

## Build and test
```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Run
Open two terminals. Use a release build: debug builds make X11 capture and
software H.264 encoding dramatically slower.

1) Server
```bash
RUST_LOG=info cargo run --release -p server
```

2) Client
```bash
RUST_LOG=info cargo run --release -p client
```
The client opens a native window. Press `Esc` or close the window to exit.

The stream defaults to 1280×720. Override it on the server:
```bash
WTR_WIDTH=1920 WTR_HEIGHT=1080 RUST_LOG=info cargo run --release -p server
```

Measured locally with a 2560×1440 X11 screen: 1280×720, ~30 fps with moving
content, 0 reassembly rejections, and no decode errors.
