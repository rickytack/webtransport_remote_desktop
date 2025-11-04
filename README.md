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
  - Performs naive reassembly of fragmented frames
  - Decodes H.264 frames and writes images to `images/`

## Status
- Works end‑to‑end on Linux/X11.
- Minimal error handling and backpressure. Intended as a clear starting point to extend.

## Build
```bash
cargo build --workspace
```

## Run
Open two terminals.

1) Server
```bash
cd server
RUST_LOG=info cargo run
```

2) Client
```bash
cd client
RUST_LOG=info cargo run
```
Images will be saved to the `images/` directory.
