use super::*;
use crate::lvds::test_support::{golden_frame, valid_frame};

// Pack bit by bit rather than inverting the production shift formula.
fn received_bits(frame: &[u8], prefix_bits: usize) -> Vec<u8> {
    let mut bits = vec![false; prefix_bits];
    for byte in frame {
        for bit in (0..8).rev() {
            bits.push(byte & (1 << bit) != 0);
        }
    }
    let mut received = vec![0; bits.len().div_ceil(8)];
    for (index, bit) in bits.into_iter().enumerate() {
        if bit {
            received[index / 8] |= 1 << (7 - index % 8);
        }
    }
    received
}

fn context() -> RecoveryContext {
    RecoveryContext {
        udp_sequence: 0,
        previous: Some(RecoveryAnchor {
            udp_sequence: u32::MAX,
            frame_seq_u8: 254,
            sample_time_us: 1_700_000_000_073_456,
        }),
    }
}

#[test]
fn native_crc_mismatch_remains_native_without_running_recovery_reference() {
    let frame = golden_frame();
    let result = LvdsBitAlignmentRecoverer::inspect(&frame, true, context());
    assert_eq!(result.quality, FrameQuality::Native);
    assert_eq!(result.bytes, frame);
    assert_eq!(result.frame.unwrap().channel_1[0], 1023);
    assert_eq!(
        result.crc_reference.status,
        ReferenceCrcStatus::NotEvaluated
    );
    assert_eq!(result.candidate_count, 1);
}

#[test]
fn byte_sync_accepts_one_complete_frame_but_rejects_ambiguous_payloads() {
    let mut payload = vec![0x11, 0xEB, 0x90, 0x22];
    payload.extend(golden_frame());
    payload.extend([0xEB, 0x90]);
    let result = LvdsBitAlignmentRecoverer::inspect(&payload, false, context());
    assert_eq!(result.quality, FrameQuality::Native);
    assert_eq!(result.bit_offset, Some(32));
    assert_eq!(result.bytes, golden_frame());
    payload.extend(golden_frame());
    let ambiguous = LvdsBitAlignmentRecoverer::inspect(&payload, true, context());
    assert_eq!(ambiguous.quality, FrameQuality::Rejected);
    assert_eq!(ambiguous.candidate_count, 2);
    assert!(ambiguous.frame.is_none());
}

#[test]
fn full_real_bits_restore_both_one_bit_phases_and_four_bit_alignment() {
    let frame = valid_frame();
    for (shift, quality) in [
        (1, FrameQuality::Recovered1Bit),
        (7, FrameQuality::Recovered1Bit),
        (4, FrameQuality::Recovered4Bit),
    ] {
        let received = received_bits(frame.as_ref(), shift);
        let original = received.clone();
        let result = LvdsBitAlignmentRecoverer::inspect(&received, true, context());
        assert_eq!(result.quality, quality, "shift {shift}: {}", result.reason);
        assert_eq!(result.bytes, frame.as_ref());
        assert_eq!(result.shift, u8::try_from(shift).unwrap());
        assert_eq!(result.crc_reference.status, ReferenceCrcStatus::Match);
        assert!(result.validation.full_bits && result.validation.unique);
        assert_eq!(
            result.frame.unwrap().sample_time_us(99),
            Some(1_700_000_000_172_956)
        );
        assert_eq!(received, original);
    }
}

#[test]
fn incomplete_tail_is_diagnostic_only_and_is_never_padded() {
    let frame = valid_frame();
    let mut received = received_bits(frame.as_ref(), 4);
    received.pop();
    assert_eq!(received.len(), 269);
    let result = LvdsBitAlignmentRecoverer::inspect(&received, true, context());
    assert_eq!(result.quality, FrameQuality::RecoveryCandidate);
    assert_eq!(result.bytes.len(), 268);
    assert_eq!(result.available_bits, 2148);
    assert!(!result.validation.full_bits);
    assert_eq!(
        result.crc_reference.status,
        ReferenceCrcStatus::NotEvaluated
    );
    let preview = result.frame.unwrap();
    assert_eq!(preview.channel_1.len(), 100);
    assert_eq!(preview.channel_2[99], 1023);
    assert_eq!(
        LvdsBitAlignmentRecoverer::inspect(&frame[..268], true, context()).quality,
        FrameQuality::Rejected
    );
}

#[test]
fn confirmation_needs_crc_and_independent_sequence_time_evidence() {
    let frame = valid_frame();
    let received = received_bits(frame.as_ref(), 1);
    let contexts = [
        RecoveryContext::default(),
        RecoveryContext {
            udp_sequence: 2,
            ..context()
        },
        RecoveryContext {
            previous: Some(RecoveryAnchor {
                frame_seq_u8: 253,
                ..context().previous.unwrap()
            }),
            ..context()
        },
        RecoveryContext {
            previous: Some(RecoveryAnchor {
                sample_time_us: 1_700_000_000_073_455,
                ..context().previous.unwrap()
            }),
            ..context()
        },
    ];
    for ctx in contexts {
        assert_eq!(
            LvdsBitAlignmentRecoverer::inspect(&received, true, ctx).quality,
            FrameQuality::RecoveryCandidate
        );
    }
    let mut bad_crc = frame.to_vec();
    bad_crc[268] ^= 0x80;
    let result = LvdsBitAlignmentRecoverer::inspect(&received_bits(&bad_crc, 4), true, context());
    assert_eq!(result.quality, FrameQuality::RecoveryCandidate);
    assert_eq!(result.crc_reference.status, ReferenceCrcStatus::Mismatch);
}

#[test]
fn disabled_recovery_noise_wrong_fields_and_resource_limit_never_make_science() {
    let shifted = received_bits(valid_frame().as_ref(), 1);
    assert_eq!(
        LvdsBitAlignmentRecoverer::inspect(&shifted, false, context()).quality,
        FrameQuality::Rejected
    );
    let mut wrong = valid_frame().to_vec();
    wrong[266] = 1;
    assert_eq!(
        LvdsBitAlignmentRecoverer::inspect(&received_bits(&wrong, 4), true, context()).quality,
        FrameQuality::Rejected
    );
    for noise in [vec![], vec![0xEB, 0x90], vec![0xAA; 4096], vec![0; 4097]] {
        assert_eq!(
            LvdsBitAlignmentRecoverer::inspect(&noise, true, context()).quality,
            FrameQuality::Rejected
        );
    }
    let mut ambiguous = received_bits(valid_frame().as_ref(), 1);
    ambiguous.extend(received_bits(valid_frame().as_ref(), 4));
    assert_eq!(
        LvdsBitAlignmentRecoverer::inspect(&ambiguous, true, context()).quality,
        FrameQuality::Rejected
    );
}

#[test]
fn raw_and_normalized_hashes_use_standard_sha256() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
