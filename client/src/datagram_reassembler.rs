use crate::remote_protocol::VideoFragmentPacket;
use std::collections::HashMap;

use tracing::info;

// TODO: Stale frames must be dropped.
// TODO: Partial frames which were not constructed within 100 ms must be dropped
/// Collects incoming fragments and reassembles complete frames.
pub struct Reassembler {
    /// frame_number → list of fragments
    partials: HashMap<u32, Vec<VideoFragmentPacket>>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self { partials: HashMap::new() }
    }

    pub fn push_fragment(&mut self, fragment: VideoFragmentPacket) -> Option<Vec<u8>> {
        let frame_number = fragment.header.frame_number;
        let fragment_number = fragment.header.fragment_number as usize;
        let total_fragments = fragment.header.total_fragments as usize;

        let entry = self.partials.entry(frame_number).or_insert_with(Vec::new);

        // Only push if there’s no fragment with this fragment_number already
        if !entry.iter().any(|f| f.header.fragment_number == fragment.header.fragment_number) {
            entry.push(fragment);
        } else {
            info!("Discarded {}", fragment_number);
            return None;
        }

        let fragment_count = entry.len();

        // If we’ve received all fragments for this frame
        if fragment_count == total_fragments {
            //info!("Full array of fragments received");
            // Sort fragments by fragment_number before reassembling
            entry.sort_by_key(|f| f.header.fragment_number);

            let mut full_data = Vec::new();
            for frag in entry.iter() {
                full_data.extend_from_slice(&frag.video_data);
            }

            // Remove the completed frame from the partials map
            self.partials.remove(&frame_number);
            return Some(full_data);
        }

        None
    }
}
