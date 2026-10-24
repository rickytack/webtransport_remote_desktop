//! Video transport framing for WebTransport datagrams.
//!
//! One encoded video frame is split into one or more datagrams. Every
//! datagram starts with a fixed [`HEADER_LEN`]-byte header. The receiver
//! reassembles a frame only when every fragment is present and the total
//! payload length matches the declared frame length.
//!
//! This module is deliberately independent of `wtransport` so it can be
//! tested with an in-memory lossy channel.

mod fragmenter;
mod frame;
mod reassembly;
mod wire;

pub use bytes::Bytes;
pub use fragmenter::{FragmentError, Fragmenter};
pub use frame::{Frame, FrameFlags};
pub use reassembly::{
    ConfigError, EvictedFrame, EvictionReason, PushOutcome, Reassembler, ReassemblerConfig,
    ReassemblerStats, RejectReason,
};
pub use wire::{
    payload, FragmentHeader, WireError, HEADER_LEN, KIND_VIDEO, MAGIC, PROTOCOL_VERSION,
};

/// Hard upper bound for a single encoded frame (8 MiB).
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Hard upper bound for the number of fragments in one frame.
pub const MAX_FRAGMENTS: u16 = 4096;
