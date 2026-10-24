use std::time::{Duration, Instant};

use common::protocol::{
    payload, Bytes, FragmentHeader, Fragmenter, Frame, FrameFlags, PushOutcome, Reassembler,
    ReassemblerConfig, RejectReason, HEADER_LEN, MAX_FRAGMENTS,
};

fn sample_frame(len: usize, id: u64) -> Frame {
    let data: Vec<u8> = (0..len).map(|index| (index % 251) as u8).collect();
    Frame::new(id, 1_000 + id, FrameFlags::KEYFRAME, data)
}

fn with_payload_capacity(capacity: usize) -> Fragmenter {
    Fragmenter::new(HEADER_LEN + capacity).expect("valid datagram size")
}

fn test_config() -> ReassemblerConfig {
    ReassemblerConfig { frame_timeout: Duration::from_millis(100), ..ReassemblerConfig::default() }
}

fn new_reassembler() -> Reassembler {
    Reassembler::new(test_config()).expect("valid config")
}

fn header(
    frame_id: u64,
    frame_len: u32,
    fragment_index: u16,
    fragment_count: u16,
) -> FragmentHeader {
    FragmentHeader {
        flags: FrameFlags::KEYFRAME,
        frame_id,
        frame_len,
        fragment_index,
        fragment_count,
        timestamp_us: 42,
    }
}

fn datagram(header: FragmentHeader, data: &[u8]) -> Bytes {
    let mut bytes = Vec::with_capacity(HEADER_LEN + data.len());
    bytes.extend_from_slice(&header.encode());
    bytes.extend_from_slice(data);
    Bytes::from(bytes)
}

fn expect_complete(outcome: PushOutcome) -> Frame {
    match outcome {
        PushOutcome::Complete(frame) => frame,
        other => panic!("expected complete frame, got {other:?}"),
    }
}

#[test]
fn roundtrip_handles_sizes_and_reordering() {
    for len in [1, 9, 10, 11, 100, 101, 1_000, 10_000] {
        let original = sample_frame(len, 1);
        let fragments = with_payload_capacity(10).fragment(&original).unwrap();
        assert_eq!(fragments.len(), len.div_ceil(10));

        let mut reassembler = new_reassembler();
        let now = Instant::now();
        let mut completed = None;
        for fragment in fragments.iter().rev() {
            match reassembler.push(fragment.clone(), now) {
                PushOutcome::Complete(frame) => completed = Some(frame),
                PushOutcome::Incomplete => {}
                PushOutcome::Rejected(reason) => panic!("unexpected rejection: {reason:?}"),
            }
        }

        assert_eq!(completed, Some(original));
        assert_eq!(reassembler.stats().frames_completed, 1);
        assert_eq!(reassembler.stats().bytes_delivered, len as u64);
        assert_eq!(reassembler.inflight_frames(), 0);
        assert_eq!(reassembler.partial_bytes(), 0);
    }
}

#[test]
fn exact_capacity_is_one_fragment() {
    let original = sample_frame(10, 7);
    let fragments = with_payload_capacity(10).fragment(&original).unwrap();
    assert_eq!(fragments.len(), 1);

    let decoded = FragmentHeader::decode(&fragments[0]).unwrap();
    assert_eq!(decoded.fragment_count, 1);
    assert_eq!(decoded.fragment_index, 0);
    assert_eq!(decoded.frame_len, 10);
    assert_eq!(payload(&fragments[0]), &original.data[..]);
}

#[test]
fn duplicate_fragments_are_rejected_without_corrupting_frame() {
    let original = sample_frame(25, 2);
    let fragments = with_payload_capacity(10).fragment(&original).unwrap();
    let mut reassembler = new_reassembler();
    let now = Instant::now();

    assert_eq!(reassembler.push(fragments[0].clone(), now), PushOutcome::Incomplete);
    assert!(matches!(
        reassembler.push(fragments[0].clone(), now),
        PushOutcome::Rejected(RejectReason::Duplicate { .. })
    ));
    assert_eq!(reassembler.stats().duplicates, 1);

    let mut completed = None;
    for fragment in &fragments[1..] {
        if let PushOutcome::Complete(frame) = reassembler.push(fragment.clone(), now) {
            completed = Some(frame);
        }
    }

    assert_eq!(completed, Some(original));
}

#[test]
fn out_of_range_index_cannot_complete_a_frame() {
    let mut reassembler = new_reassembler();
    let now = Instant::now();

    // This is the old implementation's corruption case: index 7 is accepted
    // and counted, then index 0 makes the fragment count reach total_fragments.
    let bad = datagram(header(1, 10, 7, 2), &[1, 2, 3, 4, 5]);
    assert!(matches!(
        reassembler.push(bad, now),
        PushOutcome::Rejected(RejectReason::IndexOutOfRange { index: 7, count: 2 })
    ));

    let first = datagram(header(1, 10, 0, 2), &[1, 2, 3, 4, 5]);
    assert_eq!(reassembler.push(first, now), PushOutcome::Incomplete);
    assert_eq!(reassembler.inflight_frames(), 1);
    assert_eq!(reassembler.stats().frames_completed, 0);
}

#[test]
fn missing_fragment_is_evicted_by_timeout() {
    let fragments = with_payload_capacity(10).fragment(&sample_frame(25, 3)).unwrap();
    let mut reassembler = new_reassembler();
    let now = Instant::now();

    for fragment in &fragments[..fragments.len() - 1] {
        assert_eq!(reassembler.push(fragment.clone(), now), PushOutcome::Incomplete);
    }
    assert_eq!(reassembler.inflight_frames(), 1);

    let evicted = reassembler.evict_expired(now + Duration::from_millis(150));
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].frame_id, 3);
    assert_eq!(reassembler.inflight_frames(), 0);
    assert_eq!(reassembler.stats().frames_completed, 0);
    assert_eq!(reassembler.stats().frames_evicted, 1);
}

#[test]
fn inconsistent_metadata_drops_the_whole_frame() {
    let mut reassembler = new_reassembler();
    let now = Instant::now();

    let first = datagram(header(4, 10, 0, 2), &[0; 5]);
    assert_eq!(reassembler.push(first, now), PushOutcome::Incomplete);

    let inconsistent = datagram(header(4, 11, 1, 2), &[0; 5]);
    assert!(matches!(
        reassembler.push(inconsistent, now),
        PushOutcome::Rejected(RejectReason::InconsistentMetadata { frame_id: 4 })
    ));
    assert_eq!(reassembler.inflight_frames(), 0);
    assert_eq!(reassembler.stats().frames_completed, 0);
    assert_eq!(reassembler.stats().frames_evicted, 1);
}

#[test]
fn complete_count_with_wrong_total_length_is_rejected() {
    let mut reassembler = new_reassembler();
    let now = Instant::now();

    assert_eq!(
        reassembler.push(datagram(header(5, 10, 0, 2), &[0; 4]), now),
        PushOutcome::Incomplete
    );
    let outcome = reassembler.push(datagram(header(5, 10, 1, 2), &[0; 4]), now);
    assert!(matches!(
        outcome,
        PushOutcome::Rejected(RejectReason::FrameLengthMismatch {
            frame_id: 5,
            expected: 10,
            actual: 8
        })
    ));
    assert_eq!(reassembler.stats().frames_completed, 0);
    assert_eq!(reassembler.inflight_frames(), 0);
}

#[test]
fn stale_frames_are_rejected_after_a_newer_complete_frame() {
    let mut reassembler = new_reassembler();
    let now = Instant::now();

    let completed =
        expect_complete(reassembler.push(datagram(header(10, 4, 0, 1), &[1, 2, 3, 4]), now));
    assert_eq!(completed.id, 10);
    assert_eq!(reassembler.latest_completed(), Some(10));

    let stale = datagram(header(9, 4, 0, 1), &[1, 2, 3, 4]);
    assert!(matches!(
        reassembler.push(stale, now),
        PushOutcome::Rejected(RejectReason::StaleFrame { frame_id: 9, latest: 10 })
    ));
}

#[test]
fn inflight_limit_evicts_oldest_partial() {
    let config = ReassemblerConfig { max_inflight_frames: 2, ..test_config() };
    let mut reassembler = Reassembler::new(config).unwrap();
    let now = Instant::now();

    for frame_id in 1..=3 {
        let outcome = reassembler.push(datagram(header(frame_id, 4, 0, 2), &[0; 2]), now);
        assert_eq!(outcome, PushOutcome::Incomplete);
    }

    assert_eq!(reassembler.inflight_frames(), 2);
    assert_eq!(reassembler.stats().frames_evicted, 1);
}

#[test]
fn byte_limit_evicts_oldest_partial() {
    let config = ReassemblerConfig {
        max_frame_bytes: 8,
        max_fragments: 8,
        max_inflight_frames: 4,
        max_partial_bytes: 8,
        frame_timeout: Duration::from_millis(100),
    };
    let mut reassembler = Reassembler::new(config).unwrap();
    let now = Instant::now();

    for frame_id in 1..=3 {
        let outcome = reassembler.push(datagram(header(frame_id, 8, 0, 2), &[0; 4]), now);
        assert_eq!(outcome, PushOutcome::Incomplete);
    }

    assert_eq!(reassembler.partial_bytes(), 8);
    assert_eq!(reassembler.stats().frames_evicted, 1);
}

#[test]
fn byte_limit_is_checked_when_growing_an_existing_frame() {
    let config = ReassemblerConfig {
        max_frame_bytes: 8,
        max_fragments: 8,
        max_inflight_frames: 4,
        max_partial_bytes: 8,
        frame_timeout: Duration::from_millis(100),
    };
    let mut reassembler = Reassembler::new(config).unwrap();
    let now = Instant::now();

    assert_eq!(
        reassembler.push(datagram(header(1, 8, 0, 2), &[0; 4]), now),
        PushOutcome::Incomplete
    );
    assert_eq!(
        reassembler.push(datagram(header(2, 8, 0, 2), &[0; 4]), now),
        PushOutcome::Incomplete
    );
    assert_eq!(reassembler.partial_bytes(), 8);

    let completed = expect_complete(reassembler.push(datagram(header(1, 8, 1, 2), &[0; 4]), now));
    assert_eq!(completed.id, 1);
    assert_eq!(completed.len(), 8);
    assert_eq!(reassembler.stats().frames_evicted, 1);
    assert_eq!(reassembler.partial_bytes(), 0);
}

#[test]
fn malformed_and_invalid_fields_are_rejected() {
    let mut reassembler = new_reassembler();
    let now = Instant::now();

    assert!(matches!(
        reassembler.push(Bytes::from_static(b"short"), now),
        PushOutcome::Rejected(RejectReason::Malformed(_))
    ));

    let mut bad_magic = header(1, 4, 0, 1).encode().to_vec();
    bad_magic[0] = b'X';
    assert!(matches!(
        reassembler.push(Bytes::from(bad_magic), now),
        PushOutcome::Rejected(RejectReason::Malformed(_))
    ));

    assert!(matches!(
        reassembler.push(datagram(header(1, 0, 0, 1), &[1]), now),
        PushOutcome::Rejected(RejectReason::EmptyFrame)
    ));
    assert!(matches!(
        reassembler.push(datagram(header(1, 4, 0, 0), &[1]), now),
        PushOutcome::Rejected(RejectReason::ZeroFragments)
    ));
    assert!(matches!(
        reassembler.push(datagram(header(1, 4, 0, 1), &[]), now),
        PushOutcome::Rejected(RejectReason::EmptyPayload)
    ));
    assert!(matches!(
        reassembler.push(datagram(header(1, 4, 0, 1), &[1, 2, 3, 4, 5]), now),
        PushOutcome::Rejected(RejectReason::PayloadExceedsFrame { .. })
    ));
}

#[test]
fn fragmenter_rejects_invalid_inputs() {
    assert!(Fragmenter::new(HEADER_LEN).is_err());
    assert!(Fragmenter::new(HEADER_LEN + 1).is_ok());

    let fragmenter = with_payload_capacity(10);
    assert!(fragmenter.fragment(&sample_frame(0, 1)).is_err());

    let one_byte_fragments = Fragmenter::new(HEADER_LEN + 1).unwrap();
    let too_many = sample_frame(MAX_FRAGMENTS as usize + 1, 2);
    assert!(one_byte_fragments.fragment(&too_many).is_err());
}

#[test]
fn randomized_loss_reorder_and_duplication_never_corrupts_frames() {
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, upper: usize) -> usize {
            (self.next() as usize) % upper
        }
    }

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let now = Instant::now();

    for trial in 0..100 {
        let len = 1 + rng.below(3_000);
        let original = sample_frame(len, trial as u64);
        let mut fragments = with_payload_capacity(17).fragment(&original).unwrap();

        for index in (1..fragments.len()).rev() {
            let other = rng.below(index + 1);
            fragments.swap(index, other);
        }

        let mut reassembler = new_reassembler();
        let mut completed = None;
        for fragment in fragments {
            if rng.below(100) < 5 {
                continue;
            }
            if rng.below(100) < 3 {
                let _ = reassembler.push(fragment.clone(), now);
            }
            match reassembler.push(fragment, now) {
                PushOutcome::Complete(frame) => completed = Some(frame),
                PushOutcome::Incomplete => {}
                PushOutcome::Rejected(_) => {}
            }
        }

        match completed {
            Some(frame) => assert_eq!(frame, original),
            None => assert_eq!(reassembler.stats().frames_completed, 0),
        }
    }
}

#[test]
fn config_rejects_unsafe_limits() {
    assert!(Reassembler::new(ReassemblerConfig { max_frame_bytes: 0, ..test_config() }).is_err());

    assert!(Reassembler::new(ReassemblerConfig {
        max_frame_bytes: 100,
        max_partial_bytes: 50,
        ..test_config()
    })
    .is_err());

    assert!(
        Reassembler::new(ReassemblerConfig { max_inflight_frames: 0, ..test_config() }).is_err()
    );

    assert!(Reassembler::new(ReassemblerConfig { frame_timeout: Duration::ZERO, ..test_config() })
        .is_err());
}
