//! Typed, length-prefixed control messages.
//!
//! Control runs on a reliable QUIC stream, separate from video datagrams.
//! Every message is a fixed 8-byte header followed by a bincode payload:
//!
//! ```text
//! 0..4   magic "WTRC"
//! 4      version
//! 5      message kind
//! 6..8   payload length, little-endian u16
//! 8..    bincode payload
//! ```

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Magic bytes at the start of every control frame.
pub const MAGIC: [u8; 4] = *b"WTRC";
/// Control protocol version.
pub const VERSION: u8 = 1;
/// Fixed control-frame header size.
pub const HEADER_LEN: usize = 8;
/// Upper bound for a bincode control payload.
pub const MAX_CONTROL_PAYLOAD: usize = u16::MAX as usize;

const KIND_HELLO: u8 = 1;
const KIND_WELCOME: u8 = 2;

/// Client-to-server handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u8,
}

/// Server-to-client handshake acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol_version: u8,
    pub width: u16,
    pub height: u16,
}

/// One typed control message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlMessage {
    Hello(Hello),
    Welcome(Welcome),
}

impl ControlMessage {
    fn kind(&self) -> u8 {
        match self {
            Self::Hello(_) => KIND_HELLO,
            Self::Welcome(_) => KIND_WELCOME,
        }
    }

    /// Encode this message, including its length-prefixed header.
    pub fn encode(&self) -> Result<Vec<u8>, ControlError> {
        let payload = bincode::serde::encode_to_vec(self, bincode::config::standard())
            .map_err(|error| ControlError::Encode(error.to_string()))?;

        if payload.len() > MAX_CONTROL_PAYLOAD {
            return Err(ControlError::PayloadTooLarge);
        }

        let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
        frame.extend_from_slice(&MAGIC);
        frame.push(VERSION);
        frame.push(self.kind());
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.extend_from_slice(&payload);
        Ok(frame)
    }

    /// Decode exactly one complete control frame.
    pub fn decode(frame: &[u8]) -> Result<Self, ControlError> {
        let (kind, declared_len) = parse_header(frame)?;
        let payload = &frame[HEADER_LEN..];

        if payload.len() > MAX_CONTROL_PAYLOAD {
            return Err(ControlError::PayloadTooLarge);
        }
        if payload.len() != declared_len {
            return Err(ControlError::LengthMismatch {
                declared: declared_len,
                actual: payload.len(),
            });
        }

        let (message, consumed) = bincode::serde::decode_from_slice(
            payload,
            bincode::config::standard().with_limit::<MAX_CONTROL_PAYLOAD>(),
        )
        .map_err(|error| ControlError::Decode(error.to_string()))?;

        if consumed != payload.len() {
            return Err(ControlError::TrailingBytes);
        }

        let message: Self = message;
        if message.kind() != kind {
            return Err(ControlError::KindMismatch { header: kind, payload: message.kind() });
        }

        Ok(message)
    }
}

/// Return the declared payload length, validating the fixed header.
pub fn payload_len(header: &[u8]) -> Result<usize, ControlError> {
    parse_header(header).map(|(_, len)| len)
}

fn parse_header(frame: &[u8]) -> Result<(u8, usize), ControlError> {
    if frame.len() < HEADER_LEN {
        return Err(ControlError::TooShort { actual: frame.len() });
    }
    if frame[..4] != MAGIC {
        return Err(ControlError::BadMagic);
    }
    if frame[4] != VERSION {
        return Err(ControlError::UnsupportedVersion(frame[4]));
    }

    let kind = frame[5];
    if kind != KIND_HELLO && kind != KIND_WELCOME {
        return Err(ControlError::UnknownKind(kind));
    }

    Ok((kind, u16::from_le_bytes([frame[6], frame[7]]) as usize))
}

/// Control-plane failures.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ControlError {
    #[error("control frame is too short: {actual} bytes")]
    TooShort { actual: usize },
    #[error("bad control magic")]
    BadMagic,
    #[error("unsupported control version {0}")]
    UnsupportedVersion(u8),
    #[error("unknown control message kind {0}")]
    UnknownKind(u8),
    #[error("control header kind {header} does not match payload kind {payload}")]
    KindMismatch { header: u8, payload: u8 },
    #[error("control payload is {actual} bytes, header says {declared}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("control payload exceeds {MAX_CONTROL_PAYLOAD} bytes")]
    PayloadTooLarge,
    #[error("trailing bytes after control payload")]
    TrailingBytes,
    #[error("control encode failed: {0}")]
    Encode(String),
    #[error("control decode failed: {0}")]
    Decode(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> ControlMessage {
        ControlMessage::Hello(Hello { protocol_version: VERSION })
    }

    #[test]
    fn hello_roundtrip() {
        let frame = hello().encode().unwrap();
        assert_eq!(frame.len(), HEADER_LEN + 2);
        assert_eq!(payload_len(&frame).unwrap(), 2);
        assert_eq!(ControlMessage::decode(&frame).unwrap(), hello());
    }

    #[test]
    fn welcome_roundtrip() {
        let message = ControlMessage::Welcome(Welcome {
            protocol_version: VERSION,
            width: 1280,
            height: 720,
        });
        let frame = message.encode().unwrap();
        assert_eq!(ControlMessage::decode(&frame).unwrap(), message);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut frame = hello().encode().unwrap();
        frame[0] = b'X';
        assert_eq!(ControlMessage::decode(&frame), Err(ControlError::BadMagic));
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut frame = hello().encode().unwrap();
        frame[4] = VERSION + 1;
        assert_eq!(
            ControlMessage::decode(&frame),
            Err(ControlError::UnsupportedVersion(VERSION + 1))
        );
    }

    #[test]
    fn rejects_unknown_kind() {
        let mut frame = hello().encode().unwrap();
        frame[5] = 99;
        assert_eq!(ControlMessage::decode(&frame), Err(ControlError::UnknownKind(99)));
    }

    #[test]
    fn rejects_short_frame() {
        assert_eq!(ControlMessage::decode(&[0, 0, 1]), Err(ControlError::TooShort { actual: 3 }));
    }

    #[test]
    fn rejects_length_mismatch() {
        let mut frame = hello().encode().unwrap();
        frame[6] = 1;
        frame[7] = 0;
        assert_eq!(
            ControlMessage::decode(&frame),
            Err(ControlError::LengthMismatch { declared: 1, actual: 2 })
        );
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut frame = hello().encode().unwrap();
        frame.push(0);
        frame[6..8].copy_from_slice(&3u16.to_le_bytes());
        assert_eq!(ControlMessage::decode(&frame), Err(ControlError::TrailingBytes));
    }

    #[test]
    fn rejects_header_kind_mismatch() {
        let mut frame = hello().encode().unwrap();
        frame[5] = KIND_WELCOME;
        assert_eq!(
            ControlMessage::decode(&frame),
            Err(ControlError::KindMismatch { header: KIND_WELCOME, payload: KIND_HELLO })
        );
    }
}
