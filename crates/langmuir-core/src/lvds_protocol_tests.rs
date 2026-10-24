use super::{
    LVDS_FRAME_LEN, decode_scientific_frame, diagnose_scientific_frame,
    test_support::{golden_frame, refresh_crc16, valid_frame},
    validate_scientific_frame_crc16,
};
use crate::{FrameError, Protocol};

#[test]
fn final_addresses_decode_literal_55aa_without_fixture_constants() {
    let mut wire = [0_u8; 269];
    wire[..15].copy_from_slice(&[
        0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA, 0x65, 0x53, 0xF1, 0, 0, 0, 0, 0,
    ]);
    wire[265] = 19;
    let frame = decode_scientific_frame(&wire).unwrap();
    assert_eq!(frame.timestamp.utc_seconds, 1_700_000_000);
    assert_eq!(frame.frame_seq_u8, 19);
    assert_eq!(frame.channel_1.as_ref(), &[0; 100]);
    assert_eq!(frame.channel_2.as_ref(), &[0; 100]);
    assert_eq!(frame.sample_time_us(99), Some(1_700_000_000_049_500));
}

#[test]
fn final_addresses_reject_04ff_and_each_wrong_address_independently() {
    for (addresses, field, expected, actual) in [
        ([0x04, 0xFF], "sender address", 0x55, 0x04),
        ([0x04, 0xAA], "sender address", 0x55, 0x04),
        ([0x55, 0xFF], "receiver address", 0xAA, 0xFF),
    ] {
        let mut wire = [0_u8; 269];
        wire[..7].copy_from_slice(&[0xEB, 0x90, addresses[0], addresses[1], 1, 7, 0xAA]);
        assert_eq!(
            decode_scientific_frame(&wire),
            Err(FrameError::UnexpectedField {
                protocol: Protocol::Lvds,
                field,
                expected,
                actual,
            })
        );
    }
}

#[test]
fn final_addresses_gate_rejection_does_not_poison_later_55aa_same_sequence() {
    let mut wrong = [0_u8; 269];
    wrong[..7].copy_from_slice(&[0xEB, 0x90, 0x04, 0xFF, 1, 7, 0xAA]);
    let mut correct = [0_u8; 269];
    correct[..7].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 1, 7, 0xAA]);
    let now = std::time::Instant::now();
    let mut gate = crate::UdpLvdsScienceGate::default();
    let rejected = crate::ReassembledLvdsFrame {
        frame_sequence: 0x1234_5678,
        bytes: wrong.to_vec(),
    };
    assert!(matches!(
        gate.accept_at(&rejected, now),
        Err(FrameError::UnexpectedField {
            field: "sender address",
            expected: 0x55,
            actual: 0x04,
            ..
        })
    ));
    assert_eq!(gate.metrics().validated_frames, 0);
    assert_eq!(gate.metrics().accepted_frames, 0);
    assert_eq!(gate.metrics().cached_frames, 0);
    let accepted = crate::ReassembledLvdsFrame {
        frame_sequence: 0x1234_5678,
        bytes: correct.to_vec(),
    };
    let delivered = gate.accept_at(&accepted, now).unwrap();
    assert_eq!(delivered.disposition(), crate::UdpLvdsDisposition::Accepted);
    assert!(delivered.into_frame().is_some());
    assert_eq!(gate.metrics().validated_frames, 1);
    assert_eq!(gate.metrics().accepted_frames, 1);
    assert_eq!(gate.metrics().cached_frames, 1);
}

#[test]
fn fixed_structure_rejects_wrong_addresses_reserved_type_and_lengths() {
    let good = golden_frame();
    for length in 0..LVDS_FRAME_LEN {
        assert!(matches!(decode_scientific_frame(&good[..length]),
            Err(FrameError::UnexpectedLength { actual, .. }) if actual == length));
    }
    let mut oversized = good.to_vec();
    oversized.push(0);
    assert!(matches!(
        decode_scientific_frame(&oversized),
        Err(FrameError::UnexpectedLength { actual: 270, .. })
    ));
    for offset in [0, 1] {
        let mut bad = good;
        bad[offset] ^= 1;
        assert!(matches!(
            decode_scientific_frame(&bad),
            Err(FrameError::InvalidHeader { .. })
        ));
    }
    for offset in [4, 5] {
        let mut bad = good;
        bad[offset] ^= 1;
        assert!(matches!(
            decode_scientific_frame(&bad),
            Err(FrameError::InvalidDeclaredLength { .. })
        ));
    }
    for (offset, field) in [
        (2, "sender address"),
        (3, "receiver address"),
        (6, "data type"),
        (266, "DATA251 reserved byte"),
    ] {
        let mut bad = good;
        bad[offset] ^= 1;
        assert!(matches!(decode_scientific_frame(&bad),
            Err(FrameError::UnexpectedField { field: actual, .. }) if actual == field));
    }
    let mut wrong_addresses = good;
    wrong_addresses[2..4].copy_from_slice(&[0x04, 0xFF]);
    assert!(matches!(
        decode_scientific_frame(&wrong_addresses),
        Err(FrameError::UnexpectedField {
            field: "sender address",
            expected: 0x55,
            actual: 0x04,
            ..
        })
    ));
}

#[test]
fn uniform_channels_cover_zero_and_maximum_without_cross_talk() {
    let mut wire = golden_frame();
    wire[15..140].fill(0);
    wire[140..265].fill(0xFF);
    let decoded = decode_scientific_frame(&wire).unwrap();
    assert_eq!(decoded.channel_1.as_ref(), &[0_u16; 100]);
    assert_eq!(decoded.channel_2.as_ref(), &[1023_u16; 100]);
}

#[test]
fn different_first_middle_and_final_groups_keep_their_exact_positions() {
    let mut wire = golden_frame();
    wire[20..25].fill(0);
    wire[135..140].fill(0xFF);
    wire[145..150].fill(0xFF);
    wire[260..265].copy_from_slice(&[0xFF, 0xC0, 8, 0, 1]);
    let decoded = decode_scientific_frame(&wire).unwrap();
    assert_eq!(&decoded.channel_1[..8], &[1023, 0, 512, 1, 0, 0, 0, 0]);
    assert_eq!(&decoded.channel_1[96..], &[1023, 1023, 1023, 1023]);
    assert_eq!(
        &decoded.channel_2[..8],
        &[0, 1023, 0, 1023, 1023, 1023, 1023, 1023]
    );
    assert_eq!(&decoded.channel_2[96..], &[1023, 0, 512, 1]);
}

#[test]
fn integer_sample_time_starts_at_timestamp_and_crosses_seconds_without_extra_center_offset() {
    let decoded = decode_scientific_frame(&golden_frame()).unwrap();
    assert_eq!(decoded.timestamp.utc_seconds, 1_694_498_816);
    assert_eq!(decoded.timestamp.microseconds, 999_800);
    assert_eq!(decoded.sample_time_us(0), Some(1_694_498_816_999_800));
    assert_eq!(decoded.sample_time_us(1), Some(1_694_498_817_000_300));
    assert_eq!(decoded.sample_time_us(99), Some(1_694_498_817_049_300));
    assert_eq!(decoded.sample_time_us(100), None);
    assert_eq!(decoded.sample_time_us(usize::MAX), None);
    assert_eq!(
        decoded.channel_1_base_seconds(),
        decoded.channel_2_base_seconds()
    );
    assert!((decoded.channel_1_sample_time(99).unwrap() - 1_694_498_817.049_3).abs() < 1e-6);
    assert_eq!(decoded.channel_1_sample_time(100), None);
}

#[test]
fn maximum_device_seconds_use_wide_integer_time_and_invalid_microseconds_are_rejected() {
    let mut wire = golden_frame();
    wire[7..11].copy_from_slice(&u32::MAX.to_be_bytes());
    wire[11..15].copy_from_slice(&999_999_u32.to_be_bytes());
    let decoded = decode_scientific_frame(&wire).unwrap();
    assert_eq!(decoded.sample_time_us(99), Some(4_294_967_296_049_499));
    for micros in [1_000_000_u32, u32::MAX] {
        wire[11..15].copy_from_slice(&micros.to_be_bytes());
        assert!(matches!(
            decode_scientific_frame(&wire),
            Err(FrameError::FieldOutOfRange {
                field: "microseconds",
                ..
            })
        ));
    }
}

#[test]
fn inner_sequence_wrap_preserves_samples_and_transmitted_device_time() {
    let first_wire = golden_frame();
    let mut second_wire = first_wire;
    second_wire[265] = 0;
    second_wire[7..11].copy_from_slice(&1_694_498_817_u32.to_be_bytes());
    second_wire[11..15].copy_from_slice(&49_800_u32.to_be_bytes());
    let first = decode_scientific_frame(&first_wire).unwrap();
    let second = decode_scientific_frame(&second_wire).unwrap();
    assert_eq!((first.frame_seq_u8, second.frame_seq_u8), (255, 0));
    assert_eq!(second.channel_1, first.channel_1);
    assert_eq!(second.channel_2, first.channel_2);
    assert_eq!(
        second.sample_time_us(0).unwrap() - first.sample_time_us(0).unwrap(),
        50_000
    );
}

#[test]
fn device_time_rollback_is_preserved_without_inserting_or_deleting_samples() {
    let first = decode_scientific_frame(&golden_frame()).unwrap();
    let mut wire = golden_frame();
    wire[7..11].copy_from_slice(&1_u32.to_be_bytes());
    let rollback = decode_scientific_frame(&wire).unwrap();
    assert_eq!(rollback.sample_time_us(0), Some(1_999_800));
    assert!(rollback.sample_time_us(0) < first.sample_time_us(0));
    assert_eq!(rollback.channel_1.len(), 100);
    assert_eq!(rollback.channel_1, first.channel_1);
}

#[test]
fn unchecked_crc_trailers_do_not_change_accepted_science_or_received_bytes() {
    let original = golden_frame();
    let expected = decode_scientific_frame(&original).unwrap();
    for trailer in [[0, 0], [0xFF, 0xFF], [0x09, 0xD7], [0x12, 0x35]] {
        let mut wire = original;
        wire[267..269].copy_from_slice(&trailer);
        let received = wire;
        assert_eq!(decode_scientific_frame(&wire), Ok(expected.clone()));
        assert_eq!(wire, received);
    }
    let mut changed_code = original;
    changed_code[100] ^= 1;
    assert_ne!(
        decode_scientific_frame(&changed_code).unwrap().channel_1,
        expected.channel_1
    );
}

#[test]
fn hundred_new_frames_produce_ten_thousand_samples_per_channel() {
    let mut points = [0; 2];
    for inner in 0..100_u8 {
        let mut wire = golden_frame();
        wire[265] = inner;
        let decoded = decode_scientific_frame(&wire).unwrap();
        points[0] += decoded.channel_1.len();
        points[1] += decoded.channel_2.len();
    }
    assert_eq!(points, [10_000, 10_000]);
}

#[test]
fn offline_crc_reference_stays_separate_from_production_acceptance() {
    let mut wire = golden_frame();
    refresh_crc16(&mut wire);
    assert_eq!(validate_scientific_frame_crc16(&wire), Ok(()));
    let valid = diagnose_scientific_frame(&wire);
    assert_eq!(valid.error, None);
    assert_eq!(valid.preview, None);
    wire[267] ^= 1;
    assert!(matches!(
        validate_scientific_frame_crc16(&wire),
        Err(FrameError::ChecksumMismatch {
            protocol: Protocol::Lvds,
            ..
        })
    ));
    let result = diagnose_scientific_frame(&wire);
    assert!(matches!(
        result.error,
        Some(FrameError::ChecksumMismatch { .. })
    ));
    assert_eq!(result.secondary_error, None);
    assert_eq!(
        result.preview.unwrap().frame(),
        &decode_scientific_frame(&wire).unwrap()
    );
}

#[test]
fn offline_diagnosis_never_previews_invalid_time_or_reserved_structure() {
    let mut bad_time = *valid_frame();
    bad_time[11..15].copy_from_slice(&1_000_000_u32.to_be_bytes());
    let result = diagnose_scientific_frame(&bad_time);
    assert!(matches!(
        result.error,
        Some(FrameError::ChecksumMismatch { .. })
    ));
    assert!(matches!(
        result.secondary_error,
        Some(FrameError::FieldOutOfRange {
            field: "microseconds",
            ..
        })
    ));
    assert_eq!(result.preview, None);
    refresh_crc16(&mut bad_time);
    assert!(matches!(
        diagnose_scientific_frame(&bad_time).error,
        Some(FrameError::FieldOutOfRange {
            field: "microseconds",
            ..
        })
    ));
    let mut bad_reserved = golden_frame();
    bad_reserved[266] = 1;
    let result = diagnose_scientific_frame(&bad_reserved);
    assert!(matches!(
        result.error,
        Some(FrameError::UnexpectedField {
            field: "DATA251 reserved byte",
            ..
        })
    ));
    assert_eq!(result.preview, None);
    for len in 0..LVDS_FRAME_LEN {
        let result = diagnose_scientific_frame(&bad_reserved[..len]);
        assert_eq!(result.received_crc, None);
        assert_eq!(result.calculated_crc, None);
        assert_eq!(result.preview, None);
    }
}

#[test]
fn arbitrary_candidates_and_all_packed_bit_patterns_remain_bounded_without_panics() {
    for length in 0..=LVDS_FRAME_LEN + 8 {
        let _ = decode_scientific_frame(&vec![0xEB; length]);
        let _ = diagnose_scientific_frame(&vec![0xEB; length]);
    }
    let mut state = 0x1357_9BDF_u32;
    for _ in 0..128 {
        let mut wire = golden_frame();
        for byte in &mut wire[15..265] {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *byte = state.to_be_bytes()[0];
        }
        let decoded = decode_scientific_frame(&wire).unwrap();
        assert!(
            decoded
                .channel_1
                .iter()
                .chain(decoded.channel_2.iter())
                .all(|code| *code <= 1023)
        );
        let _ = diagnose_scientific_frame(&wire);
    }
}
