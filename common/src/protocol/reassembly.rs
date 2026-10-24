use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::{BufMut, Bytes, BytesMut};
use thiserror::Error;

use super::frame::{Frame, FrameFlags};
use super::wire::{FragmentHeader, WireError, HEADER_LEN};
use super::{MAX_FRAGMENTS, MAX_FRAME_BYTES};

/// Limits for the reassembler. These are deliberately conservative so that a
/// lossy or malicious peer cannot make the client grow without bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReassemblerConfig {
    pub max_frame_bytes: usize,
    pub max_fragments: u16,
    pub max_inflight_frames: usize,
    pub max_partial_bytes: usize,
    pub frame_timeout: Duration,
}

impl Default for ReassemblerConfig {
    fn default() -> Self {
        Self {
            max_frame_bytes: MAX_FRAME_BYTES,
            max_fragments: MAX_FRAGMENTS,
            max_inflight_frames: 4,
            max_partial_bytes: 2 * MAX_FRAME_BYTES,
            frame_timeout: Duration::from_millis(150),
        }
    }
}

/// Result of pushing one datagram into the reassembler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// The frame is not complete yet.
    Incomplete,
    /// A complete, verified frame is ready.
    Complete(Frame),
    /// The datagram was discarded. This is expected under loss, duplication, or
    /// protocol violations and must not terminate the connection.
    Rejected(RejectReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    Malformed(WireError),
    EmptyFrame,
    FrameTooLarge { len: usize, max: usize },
    ZeroFragments,
    TooManyFragments { count: u16, max: u16 },
    IndexOutOfRange { index: u16, count: u16 },
    FragmentCountExceedsFrameLen { count: u16, frame_len: u32 },
    EmptyPayload,
    PayloadExceedsFrame { payload: usize, frame_len: u32 },
    StaleFrame { frame_id: u64, latest: u64 },
    InconsistentMetadata { frame_id: u64 },
    Duplicate { frame_id: u64, index: u16 },
    LengthOverflow { frame_id: u64 },
    MemoryLimit { frame_id: u64 },
    IncompleteFrame { frame_id: u64 },
    FrameLengthMismatch { frame_id: u64, expected: u32, actual: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictionReason {
    Timeout,
    InflightLimit,
    MemoryLimit,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvictedFrame {
    pub frame_id: u64,
    pub reason: EvictionReason,
    pub received_bytes: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReassemblerStats {
    pub datagrams: u64,
    pub fragments_accepted: u64,
    pub duplicates: u64,
    pub rejected: u64,
    pub frames_completed: u64,
    pub frames_evicted: u64,
    pub bytes_accepted: u64,
    pub bytes_delivered: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigError {
    #[error("max_frame_bytes must be greater than zero")]
    ZeroMaxFrameBytes,

    #[error("max_frame_bytes ({0}) does not fit in u32")]
    FrameBytesTooLarge(usize),

    #[error("max_fragments must be greater than zero")]
    ZeroMaxFragments,

    #[error("max_inflight_frames must be greater than zero")]
    ZeroInflightFrames,

    #[error("max_partial_bytes ({partial}) must be at least max_frame_bytes ({frame})")]
    PartialBytesTooSmall { partial: usize, frame: usize },

    #[error("frame_timeout must be greater than zero")]
    ZeroTimeout,
}

/// Collects video fragments and returns only complete, length-verified frames.
///
/// Design rules:
/// - completion requires every index in `0..fragment_count` to be present;
/// - the sum of fragment payloads must equal `frame_len`;
/// - metadata is fixed by the first fragment and later mismatches drop the
///   frame;
/// - incomplete state is bounded by count, bytes, and time.
pub struct Reassembler {
    config: ReassemblerConfig,
    partials: HashMap<u64, PartialFrame>,
    total_partial_bytes: usize,
    latest_completed: Option<u64>,
    stats: ReassemblerStats,
}

impl Reassembler {
    pub fn new(config: ReassemblerConfig) -> Result<Self, ConfigError> {
        if config.max_frame_bytes == 0 {
            return Err(ConfigError::ZeroMaxFrameBytes);
        }
        if config.max_frame_bytes > u32::MAX as usize {
            return Err(ConfigError::FrameBytesTooLarge(config.max_frame_bytes));
        }
        if config.max_fragments == 0 {
            return Err(ConfigError::ZeroMaxFragments);
        }
        if config.max_inflight_frames == 0 {
            return Err(ConfigError::ZeroInflightFrames);
        }
        if config.max_partial_bytes < config.max_frame_bytes {
            return Err(ConfigError::PartialBytesTooSmall {
                partial: config.max_partial_bytes,
                frame: config.max_frame_bytes,
            });
        }
        if config.frame_timeout.is_zero() {
            return Err(ConfigError::ZeroTimeout);
        }

        Ok(Self {
            config,
            partials: HashMap::new(),
            total_partial_bytes: 0,
            latest_completed: None,
            stats: ReassemblerStats::default(),
        })
    }

    pub fn config(&self) -> &ReassemblerConfig {
        &self.config
    }

    pub fn stats(&self) -> &ReassemblerStats {
        &self.stats
    }

    pub fn inflight_frames(&self) -> usize {
        self.partials.len()
    }

    pub fn partial_bytes(&self) -> usize {
        self.total_partial_bytes
    }

    pub fn latest_completed(&self) -> Option<u64> {
        self.latest_completed
    }

    /// Feed one received datagram into the reassembler.
    ///
    /// `now` is passed explicitly so tests can exercise timeouts without
    /// sleeping. Callers should use a monotonic clock in production.
    pub fn push(&mut self, datagram: Bytes, now: Instant) -> PushOutcome {
        self.stats.datagrams += 1;

        let header = match FragmentHeader::decode(&datagram) {
            Ok(header) => header,
            Err(error) => return self.reject(RejectReason::Malformed(error)),
        };

        let payload_len = datagram.len() - HEADER_LEN;
        if let Err(reason) = self.validate_header(&header, payload_len) {
            return self.reject(reason);
        }

        if let Some(latest) = self.latest_completed {
            if header.frame_id <= latest {
                return self.reject(RejectReason::StaleFrame { frame_id: header.frame_id, latest });
            }
        }

        // Exclude the frame currently being extended: a late arrival should
        // revive it rather than expire it just before insertion.
        self.evict_expired_except(now, Some(header.frame_id));

        let existing_check = match self.partials.get(&header.frame_id) {
            None => None,
            Some(partial) => {
                if !partial.matches(&header) {
                    Some(ExistingCheck::Inconsistent)
                } else if partial.fragments[header.fragment_index as usize].is_some() {
                    Some(ExistingCheck::Duplicate)
                } else if partial.received_bytes.saturating_add(payload_len)
                    > partial.frame_len as usize
                {
                    Some(ExistingCheck::LengthOverflow)
                } else {
                    None
                }
            }
        };

        match existing_check {
            Some(ExistingCheck::Inconsistent) => {
                self.remove_partial(header.frame_id);
                self.stats.frames_evicted += 1;
                return self
                    .reject(RejectReason::InconsistentMetadata { frame_id: header.frame_id });
            }
            Some(ExistingCheck::Duplicate) => {
                self.stats.duplicates += 1;
                return self.reject(RejectReason::Duplicate {
                    frame_id: header.frame_id,
                    index: header.fragment_index,
                });
            }
            Some(ExistingCheck::LengthOverflow) => {
                self.remove_partial(header.frame_id);
                self.stats.frames_evicted += 1;
                return self.reject(RejectReason::LengthOverflow { frame_id: header.frame_id });
            }
            None => {}
        }

        let is_new_frame = !self.partials.contains_key(&header.frame_id);
        if !self.ensure_memory_capacity(header.frame_id, payload_len) {
            return self.reject(RejectReason::MemoryLimit { frame_id: header.frame_id });
        }
        if is_new_frame {
            self.ensure_inflight_capacity(header.frame_id);
            self.partials.insert(header.frame_id, PartialFrame::new(&header, now));
        }

        let complete = {
            let payload = datagram.slice(HEADER_LEN..);
            let partial =
                self.partials.get_mut(&header.frame_id).expect("partial frame was inserted");
            let index = header.fragment_index as usize;
            debug_assert!(partial.fragments[index].is_none());

            partial.fragments[index] = Some(payload);
            partial.received_count += 1;
            partial.received_bytes += payload_len;
            partial.last_seen = now;

            self.total_partial_bytes += payload_len;
            self.stats.fragments_accepted += 1;
            self.stats.bytes_accepted += payload_len as u64;

            partial.received_count == partial.fragment_count
        };

        if complete {
            self.complete_frame(header.frame_id)
        } else {
            PushOutcome::Incomplete
        }
    }

    /// Drop frames whose last fragment arrived longer than `frame_timeout` ago.
    ///
    /// Call this periodically from the receive loop; otherwise a frame that
    /// simply stops receiving fragments would only be evicted when another
    /// datagram arrives.
    pub fn evict_expired(&mut self, now: Instant) -> Vec<EvictedFrame> {
        self.evict_expired_except(now, None)
    }

    fn evict_expired_except(&mut self, now: Instant, except: Option<u64>) -> Vec<EvictedFrame> {
        let expired: Vec<u64> = self
            .partials
            .iter()
            .filter(|(frame_id, partial)| {
                Some(**frame_id) != except
                    && now.saturating_duration_since(partial.last_seen) >= self.config.frame_timeout
            })
            .map(|(frame_id, _)| *frame_id)
            .collect();

        let mut evicted = Vec::with_capacity(expired.len());
        for frame_id in expired {
            if let Some(partial) = self.remove_partial(frame_id) {
                self.stats.frames_evicted += 1;
                evicted.push(EvictedFrame {
                    frame_id,
                    reason: EvictionReason::Timeout,
                    received_bytes: partial.received_bytes,
                });
            }
        }
        evicted
    }

    fn validate_header(
        &self,
        header: &FragmentHeader,
        payload_len: usize,
    ) -> Result<(), RejectReason> {
        if header.frame_len == 0 {
            return Err(RejectReason::EmptyFrame);
        }
        if header.frame_len as usize > self.config.max_frame_bytes {
            return Err(RejectReason::FrameTooLarge {
                len: header.frame_len as usize,
                max: self.config.max_frame_bytes,
            });
        }
        if header.fragment_count == 0 {
            return Err(RejectReason::ZeroFragments);
        }
        if header.fragment_count > self.config.max_fragments {
            return Err(RejectReason::TooManyFragments {
                count: header.fragment_count,
                max: self.config.max_fragments,
            });
        }
        if header.fragment_index >= header.fragment_count {
            return Err(RejectReason::IndexOutOfRange {
                index: header.fragment_index,
                count: header.fragment_count,
            });
        }
        // Every accepted fragment has at least one byte, so a frame cannot
        // have more fragments than bytes.
        if header.fragment_count as u32 > header.frame_len {
            return Err(RejectReason::FragmentCountExceedsFrameLen {
                count: header.fragment_count,
                frame_len: header.frame_len,
            });
        }
        if payload_len == 0 {
            return Err(RejectReason::EmptyPayload);
        }
        if payload_len > header.frame_len as usize {
            return Err(RejectReason::PayloadExceedsFrame {
                payload: payload_len,
                frame_len: header.frame_len,
            });
        }
        Ok(())
    }

    fn complete_frame(&mut self, frame_id: u64) -> PushOutcome {
        let Some(partial) = self.partials.remove(&frame_id) else {
            self.stats.frames_evicted += 1;
            return PushOutcome::Rejected(RejectReason::IncompleteFrame { frame_id });
        };
        self.total_partial_bytes = self.total_partial_bytes.saturating_sub(partial.received_bytes);

        let expected = partial.frame_len as usize;
        let mut data = BytesMut::with_capacity(expected);
        for fragment in &partial.fragments {
            match fragment {
                Some(fragment) => data.put_slice(fragment),
                None => {
                    self.stats.frames_evicted += 1;
                    return PushOutcome::Rejected(RejectReason::IncompleteFrame { frame_id });
                }
            }
        }

        if data.len() != expected {
            self.stats.frames_evicted += 1;
            return PushOutcome::Rejected(RejectReason::FrameLengthMismatch {
                frame_id,
                expected: partial.frame_len,
                actual: data.len(),
            });
        }

        let frame = Frame::new(partial.id, partial.timestamp_us, partial.flags, data.freeze());

        self.latest_completed =
            Some(self.latest_completed.map_or(partial.id, |latest| latest.max(partial.id)));
        self.stats.frames_completed += 1;
        self.stats.bytes_delivered += expected as u64;

        self.evict_stale_partials(partial.id);

        PushOutcome::Complete(frame)
    }

    fn evict_stale_partials(&mut self, latest: u64) {
        let stale: Vec<u64> =
            self.partials.keys().copied().filter(|frame_id| *frame_id < latest).collect();

        for frame_id in stale {
            if self.remove_partial(frame_id).is_some() {
                self.stats.frames_evicted += 1;
            }
        }
    }

    fn ensure_inflight_capacity(&mut self, current: u64) {
        while self.partials.len() >= self.config.max_inflight_frames {
            if self.evict_oldest_except(Some(current), EvictionReason::InflightLimit).is_none() {
                break;
            }
        }
    }

    fn ensure_memory_capacity(&mut self, current: u64, needed: usize) -> bool {
        if needed > self.config.max_partial_bytes {
            return false;
        }

        while self.total_partial_bytes.saturating_add(needed) > self.config.max_partial_bytes {
            if self.evict_oldest_except(Some(current), EvictionReason::MemoryLimit).is_none() {
                break;
            }
        }

        self.total_partial_bytes.saturating_add(needed) <= self.config.max_partial_bytes
    }

    fn evict_oldest_except(
        &mut self,
        except: Option<u64>,
        reason: EvictionReason,
    ) -> Option<EvictedFrame> {
        let frame_id = self
            .partials
            .iter()
            .filter(|(frame_id, _)| Some(**frame_id) != except)
            .min_by_key(|(_, partial)| partial.first_seen)
            .map(|(frame_id, _)| *frame_id)?;

        let partial = self.remove_partial(frame_id)?;
        self.stats.frames_evicted += 1;
        Some(EvictedFrame { frame_id, reason, received_bytes: partial.received_bytes })
    }

    fn remove_partial(&mut self, frame_id: u64) -> Option<PartialFrame> {
        let partial = self.partials.remove(&frame_id)?;
        self.total_partial_bytes = self.total_partial_bytes.saturating_sub(partial.received_bytes);
        Some(partial)
    }

    fn reject(&mut self, reason: RejectReason) -> PushOutcome {
        self.stats.rejected += 1;
        PushOutcome::Rejected(reason)
    }
}

#[derive(Debug)]
struct PartialFrame {
    id: u64,
    frame_len: u32,
    fragment_count: u16,
    fragments: Vec<Option<Bytes>>,
    received_count: u16,
    received_bytes: usize,
    first_seen: Instant,
    last_seen: Instant,
    timestamp_us: u64,
    flags: FrameFlags,
}

impl PartialFrame {
    fn new(header: &FragmentHeader, now: Instant) -> Self {
        Self {
            id: header.frame_id,
            frame_len: header.frame_len,
            fragment_count: header.fragment_count,
            fragments: vec![None; header.fragment_count as usize],
            received_count: 0,
            received_bytes: 0,
            first_seen: now,
            last_seen: now,
            timestamp_us: header.timestamp_us,
            flags: header.flags,
        }
    }

    fn matches(&self, header: &FragmentHeader) -> bool {
        self.frame_len == header.frame_len
            && self.fragment_count == header.fragment_count
            && self.timestamp_us == header.timestamp_us
            && self.flags == header.flags
    }
}

#[derive(Debug)]
enum ExistingCheck {
    Inconsistent,
    Duplicate,
    LengthOverflow,
}
