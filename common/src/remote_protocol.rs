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

// Video header (fixed size)
#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub struct VideoFragmentHeader {
    pub base: BaseHeader,
    pub data_size: u32,       // Number of bytes in video data fragment
    pub frame_number: u32,    // Sequence number
    pub fragment_number: u32, // number of fragment of current frame
    pub total_fragments: u32, // Total number of fragments
    pub timestamp: u64,       // Presentation timestamp
    pub flags: u32,           // Keyframe, etc.
}

// Complete video packet (header + variable data)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFragmentPacket {
    pub header: VideoFragmentHeader,
    pub video_data: Vec<u8>, // Variable length H.264 data
}

pub fn to_bytes<T: Serialize>(packet: &T) -> Result<Vec<u8>, EncodeError> {
    bincode::serde::encode_to_vec(packet, bincode::config::standard())
}

pub fn from_bytes<T: for<'a> Deserialize<'a>>(data: &[u8]) -> Result<T, DecodeError> {
    bincode::serde::decode_from_slice(data, bincode::config::standard()).map(|(result, _)| result)
}
