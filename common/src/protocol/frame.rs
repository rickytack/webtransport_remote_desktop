use std::ops::{BitOr, BitOrAssign};

use bytes::Bytes;

/// Flags attached to an encoded frame.
///
/// Unknown bits are preserved when a header is decoded, so the wire format can
/// grow without breaking older peers. Use [`FrameFlags::contains`] when
/// interpreting a flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FrameFlags(u16);

impl FrameFlags {
    pub const KEYFRAME: Self = Self(1 << 0);
    pub const IDR: Self = Self(1 << 1);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl BitOr for FrameFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for FrameFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// A complete encoded frame ready to be fragmented or decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub id: u64,
    pub timestamp_us: u64,
    pub flags: FrameFlags,
    pub data: Bytes,
}

impl Frame {
    pub fn new(id: u64, timestamp_us: u64, flags: FrameFlags, data: impl Into<Bytes>) -> Self {
        Self { id, timestamp_us, flags, data: data.into() }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}
