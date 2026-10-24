# Protocol v1 — media datagrams and control

This document describes the current wire format implemented in
`common/src/protocol/` and `common/src/control.rs`. It is intentionally small: one encoded video frame is
split into one or more unreliable WebTransport datagrams, and the receiver
returns a frame only when every fragment is present and the total length is
correct.

## Layers

```text
WebTransport (QUIC)
├── datagrams      video frame fragments (kind = 1)
└── control stream Hello/Welcome, input events            [partial, M2/M4]
```

## Control stream

The client opens one bidirectional stream. The first frame must be `Hello`; the
server validates it and replies with `Welcome` containing the encoded stream
size. Video datagrams follow on the same connection, and the client keeps the
stream open for low-rate input events.

Every control frame has an 8-byte header followed by a bincode payload:

| Offset | Size | Field | Notes |
|---:|---:|---|---|
| 0 | 4 | magic | `WTRC` |
| 4 | 1 | version | currently `1` |
| 5 | 1 | kind | `1` = Hello, `2` = Welcome, `3` = Input |
| 6 | 2 | payload_len | little-endian, maximum 65535 |
| 8 | … | payload | bincode |

The receiver reads the header, checks magic/version/kind, reads exactly
`payload_len` bytes, and rejects trailing bytes. A version mismatch ends the
handshake with an error rather than guessing compatibility.

## Input events (M4)

Input uses the same control stream after `Welcome`. Pointer coordinates are in
encoded-stream pixels (0..width-1, 0..height-1); the server scales them to the
root window before injecting them. The remaining events are:

- pointer move;
- mouse button press/release (left, middle, right, back, forward);
- key press/release using the portable `KeyCode` enum;
- scroll deltas in notches.

Client capture and server XTest injection are the next M4 slices.
Keyframe requests, statistics, and datagram pacing remain M2 work.

Datagram delivery is unreliable: fragments may be lost, duplicated, reordered,
or arrive after their frame is no longer useful. The protocol therefore aims
for *fast, bounded reassembly of complete frames*, not reliable delivery of
every fragment.

## Media datagram format

All integers are little-endian. Every datagram starts with a fixed 32-byte
header, followed by a fragment payload.

| Offset | Size | Field | Notes |
|---:|---:|---|---|
| 0 | 4 | magic | `WTRD` |
| 4 | 1 | version | currently `1` |
| 5 | 1 | kind | `1` = video fragment |
| 6 | 2 | flags | bit 0 = keyframe, bit 1 = IDR; unknown bits preserved |
| 8 | 8 | frame_id | monotonic per connection |
| 16 | 4 | frame_len | total encoded frame size in bytes |
| 20 | 2 | fragment_index | zero-based |
| 22 | 2 | fragment_count | number of fragments in this frame |
| 24 | 8 | timestamp_us | capture timestamp, microseconds |
| 32 | … | payload | fragment bytes |

The header is fixed-size so the fragmenter can compute exactly how much payload
fits into the peer's negotiated datagram limit:

```text
payload_capacity = max_datagram_size - HEADER_LEN
```

`max_datagram_size` comes from `Connection::max_datagram_size()` and already
includes the application payload limit. It must not be hard-coded.

## Limits

| Limit | Default | Enforced by |
|---|---:|---|
| Encoded frame size | 8 MiB | fragmenter and reassembler |
| Fragments per frame | 4096 | fragmenter and reassembler |
| In-flight partial frames | 4 | reassembler |
| Partial-frame payload bytes | 16 MiB | reassembler |
| Incomplete-frame timeout | 150 ms | reassembler, driven by a 50 ms timer |

These are safety limits, not quality targets. A frame larger than the limit is
rejected rather than silently truncated.

## Fragmenter rules

- An empty frame cannot be fragmented; the encoder may legally emit no output,
  in which case the server skips that frame.
- `fragment_count = ceil(frame_len / payload_capacity)`.
- Every fragment has at least one payload byte.
- `frame_len` must fit in `u32`; the configured hard limit makes that true.
- The keyframe/IDR flags are copied to every fragment of the frame.

## Reassembler rules

A frame is returned only when all of the following are true:

1. Every index in `0..fragment_count` is present exactly once.
2. The sum of all fragment payload lengths equals `frame_len`.
3. All fragments agree on `frame_len`, `fragment_count`, `timestamp_us`, and
   `flags`. The first accepted fragment fixes this metadata; later mismatches
   drop the whole frame.
4. The frame is not older than the newest completed frame.
5. The frame has not exceeded the timeout or the count/byte limits.

Expected network conditions are reported as `PushOutcome::Rejected` and must
not terminate the connection:

- malformed header, bad magic, wrong version, wrong kind;
- out-of-range fragment index, zero/too many fragments;
- empty payload, payload larger than the frame;
- duplicate fragment;
- inconsistent metadata, length overflow;
- stale frame, memory limit.

A frame that stops receiving fragments is evicted by `evict_expired(now)`. The
client calls it every 50 ms; otherwise a frame with no further arrivals would
never be cleaned up.

### Latency policy

Only the latest video matters for remote desktop. When a newer frame completes,
older partial frames are evicted. A partial frame older than 150 ms is dropped.
This trades a small chance of completing an old frame for bounded memory and
lower latency.

## Keyframes today

The server forces an IDR when a client connects and every 60 encoded frames
(about every two seconds at 30 fps). The client also skips P-frames until it
has seen the first keyframe, so a client that joins mid-stream does not feed
delta frames to a decoder that has no sequence parameter set.

This is a server-side safety net, not the final protocol. An explicit client
`KeyframeRequest` on decode error or incomplete frame belongs to M2.

## Compatibility

- `PROTOCOL_VERSION` is checked on every media datagram; a mismatch is a
  malformed datagram, not a connection error.
- Unknown flag bits are preserved and ignored.
- Future `kind` values can carry audio, cursor, or other media without changing
  the video fragment header.
- The control-plane handshake is versioned and length-prefixed; keyframe
  requests and pacing are the remaining M2 work.

## Not in v1

- retransmission of lost fragments (that belongs on a QUIC stream, not on
  datagrams);
- forward error correction;
- explicit client keyframe requests and fast loss recovery (M2);
- congestion/bitrate adaptation and datagram pacing (M3);
- encryption/authentication beyond WebTransport TLS (M5).
