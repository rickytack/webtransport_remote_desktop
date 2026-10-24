use bytes::{BufMut, Bytes, BytesMut};
use thiserror::Error;

use super::frame::Frame;
use super::wire::{FragmentHeader, HEADER_LEN};
use super::{MAX_FRAGMENTS, MAX_FRAME_BYTES};

/// Splits an encoded frame into datagram-sized fragments.
#[derive(Debug, Clone)]
pub struct Fragmenter {
    max_datagram_size: usize,
}

impl Fragmenter {
    /// `max_datagram_size` is the maximum total size of one datagram,
    /// including the 32-byte fragment header.
    pub fn new(max_datagram_size: usize) -> Result<Self, FragmentError> {
        let min = HEADER_LEN + 1;
        if max_datagram_size < min {
            return Err(FragmentError::DatagramTooSmall { size: max_datagram_size, min });
        }
        Ok(Self { max_datagram_size })
    }

    pub fn max_datagram_size(&self) -> usize {
        self.max_datagram_size
    }

    pub fn payload_capacity(&self) -> usize {
        self.max_datagram_size - HEADER_LEN
    }

    /// Fragment `frame` into one or more datagrams.
    ///
    /// The returned datagrams are ready to pass to `Connection::send_datagram`.
    pub fn fragment(&self, frame: &Frame) -> Result<Vec<Bytes>, FragmentError> {
        if frame.is_empty() {
            return Err(FragmentError::EmptyFrame);
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Err(FragmentError::FrameTooLarge { len: frame.len(), max: MAX_FRAME_BYTES });
        }

        let capacity = self.payload_capacity();
        let fragment_count = frame.len().div_ceil(capacity);
        if fragment_count > MAX_FRAGMENTS as usize {
            return Err(FragmentError::TooManyFragments {
                count: fragment_count,
                max: MAX_FRAGMENTS,
            });
        }

        let frame_len = u32::try_from(frame.len())
            .map_err(|_| FragmentError::FrameTooLarge { len: frame.len(), max: MAX_FRAME_BYTES })?;
        let fragment_count = fragment_count as u16;
        let mut datagrams = Vec::with_capacity(fragment_count as usize);

        for (index, chunk) in frame.data.chunks(capacity).enumerate() {
            let header = FragmentHeader {
                flags: frame.flags,
                frame_id: frame.id,
                frame_len,
                fragment_index: index as u16,
                fragment_count,
                timestamp_us: frame.timestamp_us,
            };

            let mut datagram = BytesMut::with_capacity(HEADER_LEN + chunk.len());
            datagram.put_slice(&header.encode());
            datagram.put_slice(chunk);
            datagrams.push(datagram.freeze());
        }

        Ok(datagrams)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FragmentError {
    #[error("max datagram size {size} is too small; need at least {min}")]
    DatagramTooSmall { size: usize, min: usize },

    #[error("cannot fragment an empty frame")]
    EmptyFrame,

    #[error("frame is {len} bytes, exceeds limit {max}")]
    FrameTooLarge { len: usize, max: usize },

    #[error("frame requires {count} fragments, exceeds limit {max}")]
    TooManyFragments { count: usize, max: u16 },
}
