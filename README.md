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
  - Decodes H.264 frames and writes images to `images/`

## Status
- Works end‑to‑end on Linux/X11.
- Media transport v1 is implemented in `common/src/protocol/`: fixed 32-byte
  fragment header, negotiated datagram sizing, duplicate/reorder/malformed
  handling, timeouts, memory limits, and a lossy randomized test suite.
- Capture/encode is still on the async runtime and runs at full display
  resolution; 720p and a dedicated capture/encode thread are next (M3).
- Handshake, keyframe requests, and input are still placeholders (M2/M4).
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
Images will be saved to the `images/` directory.

A quick local measurement: release build, 2560×1440 X11 capture, ~10 fps.
The 1280×720 target and moving capture/encode off the async runtime are M3.
