use crate::remote_protocol::{BaseHeader, VideoFragmentHeader, VideoFragmentPacket};
use tracing::info;

/// Splits a video frame buffer into smaller fragments that can fit into datagrams.
pub struct Fragmentor {
    mtu: usize, // max payload size (bytes)
}

impl Fragmentor {
    /// Create a new fragmentor with a given MTU (e.g. 1200 for QUIC datagrams)
    pub fn new(mtu: usize) -> Self {
        Self { mtu }
    }

    /// Split a large frame buffer into fragments.
    pub fn fragment_frame(
        &self,
        frame_number: u32,
        timestamp: u64,
        base: BaseHeader,
        flags: u32,
        frame: Vec<u8>,
    ) -> Vec<VideoFragmentPacket> {
        let mut fragments = Vec::new();
        let total_size = frame.len();
        if total_size == 0 {
            return fragments;
        }

        let total_fragments = ((total_size + self.mtu - 1) / self.mtu) as u32;
        info!("total_fragments: {}", total_fragments);
        let mut offset = 0;
        let mut fragment_number = 0;

        while offset < total_size {
            let remaining = total_size - offset;
            let chunk_size = remaining.min(self.mtu);
            let data = frame[offset..offset + chunk_size].to_vec();

            let header = VideoFragmentHeader {
                base,
                data_size: data.len() as u32,
                frame_number,
                fragment_number,
                total_fragments,
                timestamp,
                flags,
            };

            fragments.push(VideoFragmentPacket { header, video_data: data });

            offset += chunk_size;
            fragment_number += 1;
        }

        info!("last fragment_number: {}", fragment_number);

        fragments
    }
}
