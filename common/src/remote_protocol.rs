use bincode::error::{DecodeError, EncodeError};
use serde::{Deserialize, Serialize};

// Base channel info - common to all packets
#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub struct BaseHeader {
    pub channel_id: u8, // 0..255
    pub direction: u8,  // 0 = client→server, 1 = server→client
}

// Connection initiation packet (client → server)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionPacket {
    header: BaseHeader,
    username: [u8; 32], // Fixed size for network protocols
    password: [u8; 32],
    domain: [u8; 32],
    width: u32,
    height: u32,
    depth: u32,
}

// Input packet (client → server)
#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub struct InputPacket {
    header: BaseHeader,
    mouse_btn: i32,
    mouse_pos_x: i32,
    mouse_pos_y: i32,
    mouse_pos_wheel: i32,
    key_scancode: i32,
    key_modifier: i32,
    key_down: bool,
}

/// Upper bound for a decoded control message. Media datagrams use the fixed
/// header in [`crate::protocol`] and never go through bincode.
pub const MAX_CONTROL_MESSAGE: usize = 64 * 1024;

pub fn to_bytes<T: Serialize>(packet: &T) -> Result<Vec<u8>, EncodeError> {
    bincode::serde::encode_to_vec(packet, bincode::config::standard())
}

pub fn from_bytes<T: for<'a> Deserialize<'a>>(data: &[u8]) -> Result<T, DecodeError> {
    bincode::serde::decode_from_slice(
        data,
        bincode::config::standard().with_limit::<MAX_CONTROL_MESSAGE>(),
    )
    .map(|(result, _)| result)
}
