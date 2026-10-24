use thiserror::Error;

use super::frame::FrameFlags;

/// Fixed size of the video fragment header.
pub const HEADER_LEN: usize = 32;
pub const PROTOCOL_VERSION: u8 = 1;
pub const MAGIC: [u8; 4] = *b"WTRD";
pub const KIND_VIDEO: u8 = 1;

/// Header placed at the beginning of every media datagram.
///
/// Wire layout (little endian):
///
/// ```text
/// offset size field
/// 0      4    magic "WTRD"
/// 4      1    protocol version
/// 5      1    packet kind (1 = video fragment)
/// 6      2    frame flags
/// 8      8    frame id
/// 16     4    total frame length in bytes
/// 20     2    fragment index
/// 22     2    fragment count
/// 24     8    capture timestamp in microseconds
/// 32     ..   fragment payload
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentHeader {
    pub flags: FrameFlags,
    pub frame_id: u64,
    pub frame_len: u32,
    pub fragment_index: u16,
    pub fragment_count: u16,
    pub timestamp_us: u64,
}

impl FragmentHeader {
    /// Encode the header into its fixed-size wire representation.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0..4].copy_from_slice(&MAGIC);
        out[4] = PROTOCOL_VERSION;
        out[5] = KIND_VIDEO;
        out[6..8].copy_from_slice(&self.flags.bits().to_le_bytes());
        out[8..16].copy_from_slice(&self.frame_id.to_le_bytes());
        out[16..20].copy_from_slice(&self.frame_len.to_le_bytes());
        out[20..22].copy_from_slice(&self.fragment_index.to_le_bytes());
        out[22..24].copy_from_slice(&self.fragment_count.to_le_bytes());
        out[24..32].copy_from_slice(&self.timestamp_us.to_le_bytes());
        out
    }

    /// Decode a header from the beginning of a datagram.
    ///
    /// This validates the magic, version, and packet kind, but it does not
    /// validate field ranges. The reassembler performs those checks because
    /// they depend on its configuration.
    pub fn decode(datagram: &[u8]) -> Result<Self, WireError> {
        if datagram.len() < HEADER_LEN {
            return Err(WireError::TooShort { len: datagram.len(), min: HEADER_LEN });
        }

        if datagram[0..4] != MAGIC {
            return Err(WireError::BadMagic);
        }

        let version = datagram[4];
        if version != PROTOCOL_VERSION {
            return Err(WireError::UnsupportedVersion(version));
        }

        let kind = datagram[5];
        if kind != KIND_VIDEO {
            return Err(WireError::UnsupportedKind(kind));
        }

        Ok(Self {
            flags: FrameFlags::from_bits(read_u16(datagram, 6)),
            frame_id: read_u64(datagram, 8),
            frame_len: read_u32(datagram, 16),
            fragment_index: read_u16(datagram, 20),
            fragment_count: read_u16(datagram, 22),
            timestamp_us: read_u64(datagram, 24),
        })
    }
}

/// Returns the payload slice following a header.
///
/// Callers must validate the header first. This function returns an empty
/// slice for a header-only datagram rather than panicking.
pub fn payload(datagram: &[u8]) -> &[u8] {
    datagram.get(HEADER_LEN..).unwrap_or(&[])
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]])
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WireError {
    #[error("datagram is {len} bytes, need at least {min}")]
    TooShort { len: usize, min: usize },

    #[error("bad media datagram magic")]
    BadMagic,

    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u8),

    #[error("unsupported packet kind {0}")]
    UnsupportedKind(u8),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> FragmentHeader {
        FragmentHeader {
            flags: FrameFlags::KEYFRAME | FrameFlags::IDR,
            frame_id: 0x1122_3344_5566_7788,
            frame_len: 123_456,
            fragment_index: 7,
            fragment_count: 42,
            timestamp_us: 9_876_543_210,
        }
    }

    #[test]
    fn header_roundtrip() {
        let original = header();
        let encoded = original.encode();
        assert_eq!(encoded.len(), HEADER_LEN);
        assert_eq!(FragmentHeader::decode(&encoded), Ok(original));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut encoded = header().encode();
        encoded[0] = b'X';
        assert_eq!(FragmentHeader::decode(&encoded), Err(WireError::BadMagic));
    }

    #[test]
    fn rejects_short_header() {
        let encoded = header().encode();
        assert_eq!(
            FragmentHeader::decode(&encoded[..HEADER_LEN - 1]),
            Err(WireError::TooShort { len: HEADER_LEN - 1, min: HEADER_LEN })
        );
    }

    #[test]
    fn rejects_version_and_kind() {
        let mut encoded = header().encode();
        encoded[4] = 99;
        assert_eq!(FragmentHeader::decode(&encoded), Err(WireError::UnsupportedVersion(99)));

        let mut encoded = header().encode();
        encoded[5] = 99;
        assert_eq!(FragmentHeader::decode(&encoded), Err(WireError::UnsupportedKind(99)));
    }

    #[test]
    fn payload_after_header() {
        let encoded = header().encode();
        assert_eq!(payload(&encoded), &[]);

        let mut datagram = encoded.to_vec();
        datagram.extend_from_slice(b"abc");
        assert_eq!(payload(&datagram), b"abc");
    }
}
