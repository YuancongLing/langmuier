//! Independently constructed SYNTHETIC protocol fixtures, not FPGA captures.

use std::time::{Duration, Instant};

use langmuir_core::{
    FrameError, LvdsPacketType, MAX_RECOVERY_PAYLOAD_BYTES, UdpLvdsDisposition, UdpLvdsFrameGate,
    decode_scientific_frame, find_common_frame, parse_common_frame,
};

fn synthetic_frame(packet_type: u8) -> [u8; 269] {
    let mut bytes = [0xCC; 269];
    bytes[..7].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, packet_type]);
    bytes[7] = 0x12;
    bytes[266] = 0xA5;
    bytes[267..].copy_from_slice(&[0x12, 0x34]);
    bytes
}

#[test]
fn synthetic_common_protocol_preserves_all_three_types_without_science_or_crc_checks() {
    for (code, expected) in [
        (0xAA, LvdsPacketType::Science),
        (0xAB, LvdsPacketType::Excitation),
        (0xAC, LvdsPacketType::GnssAttitude),
    ] {
        let bytes = synthetic_frame(code);
        let common = parse_common_frame(&bytes).unwrap();
        assert_eq!(common.packet_type(), expected);
        assert_eq!(common.packet_type().code(), code);
        assert_eq!(
            (common.src(), common.dst(), common.frame_len()),
            (0x55, 0xAA, 269)
        );
        assert_eq!(common.raw_bytes(), &bytes);
        assert_eq!(common.data().len(), 260);
        assert_eq!(common.data()[0], 0x12);
        assert_eq!(common.data()[259], 0xA5);
        assert_eq!(common.raw_crc(), [0x12, 0x34]);
        assert!(decode_scientific_frame(common.raw_bytes()).is_err());
    }
}

#[test]
fn synthetic_unknown_common_type_retains_original_data_and_trailer() {
    let bytes = synthetic_frame(0xD3);
    let common = parse_common_frame(&bytes).unwrap();
    assert_eq!(common.packet_type(), LvdsPacketType::Unknown(0xD3));
    assert_eq!(common.packet_type().code(), 0xD3);
    assert_eq!(common.raw_bytes(), &bytes);
    assert!(decode_scientific_frame(common.raw_bytes()).is_err());
    for trailer in [[0, 0], [0xFF, 0xFF], [0x09, 0xD7]] {
        let mut candidate = bytes;
        candidate[267..].copy_from_slice(&trailer);
        assert_eq!(parse_common_frame(&candidate).unwrap().raw_crc(), trailer);
    }
}

#[test]
fn synthetic_common_protocol_rejects_wrong_lengths_marker_addresses_and_declared_length() {
    let bytes = synthetic_frame(0xAB);
    for length in [0, 1, 6, 7, 260, 268] {
        assert!(
            matches!(parse_common_frame(&bytes[..length]), Err(FrameError::UnexpectedLength { actual, .. }) if actual == length)
        );
    }
    let mut oversized = bytes.to_vec();
    oversized.push(0);
    assert!(matches!(
        parse_common_frame(&oversized),
        Err(FrameError::UnexpectedLength { actual: 270, .. })
    ));
    for index in [0, 1, 2, 3, 4, 5] {
        let mut invalid = bytes;
        invalid[index] ^= 1;
        assert!(parse_common_frame(&invalid).is_err(), "offset {index}");
    }
}

#[test]
fn synthetic_common_sync_finds_each_known_type_after_noise_and_false_markers() {
    for packet_type in [0xAA, 0xAB, 0xAC] {
        let bytes = synthetic_frame(packet_type);
        let mut payload = vec![0xEB, 0x90, 0x04, 0xFF, 1, 7, 0xAA, 0x11];
        payload.extend(bytes);
        payload.extend([0xEB, 0x90]);
        let (offset, common) = find_common_frame(&payload).unwrap();
        assert_eq!(offset, 8);
        assert_eq!(common.packet_type().code(), packet_type);
        assert_eq!(common.raw_bytes(), &bytes);
    }
}

#[test]
fn synthetic_common_sync_accepts_unknown_only_at_the_exact_native_boundary() {
    let unknown = synthetic_frame(0xD3);
    assert_eq!(find_common_frame(&unknown).unwrap().0, 0);
    let mut prefixed = vec![0x11];
    prefixed.extend(unknown);
    assert!(find_common_frame(&prefixed).is_none());
}

#[test]
fn synthetic_common_sync_never_guesses_between_two_complete_known_frames() {
    for second_type in [0xAA, 0xAB, 0xAC] {
        let mut payload = synthetic_frame(0xAA).to_vec();
        payload.extend(synthetic_frame(second_type));
        assert!(find_common_frame(&payload).is_none());
    }
}

#[test]
fn synthetic_common_sync_honors_the_payload_bound_and_requires_a_complete_frame() {
    let bytes = synthetic_frame(0xAC);
    let mut at_limit = vec![0x11; MAX_RECOVERY_PAYLOAD_BYTES - 269];
    at_limit.extend(bytes);
    assert_eq!(
        find_common_frame(&at_limit).unwrap().0,
        MAX_RECOVERY_PAYLOAD_BYTES - 269
    );
    at_limit.push(0);
    assert!(find_common_frame(&at_limit).is_none());
    assert!(find_common_frame(&bytes[..268]).is_none());
    for index in [2, 3, 4, 5] {
        let mut wrong = bytes;
        wrong[index] ^= 1;
        let mut payload = vec![0x11];
        payload.extend(wrong);
        assert!(find_common_frame(&payload).is_none());
    }
}

#[test]
fn synthetic_common_gate_compares_full_u32_and_every_original_wire_byte() {
    let now = Instant::now();
    let mut gate = UdpLvdsFrameGate::default();
    let aa = synthetic_frame(0xAA);
    let ab = synthetic_frame(0xAB);
    let ac = synthetic_frame(0xAC);
    assert_eq!(gate.observe_at(1, &aa, now), UdpLvdsDisposition::Accepted);
    assert_eq!(
        gate.observe_at(65_537, &ab, now),
        UdpLvdsDisposition::Accepted
    );
    assert_eq!(
        gate.observe_at(u32::MAX, &ac, now),
        UdpLvdsDisposition::Accepted
    );
    assert_eq!(gate.observe_at(0, &ab, now), UdpLvdsDisposition::Accepted);
    assert_eq!(gate.observe_at(1, &aa, now), UdpLvdsDisposition::Duplicate);
    let mut trailer_changed = aa;
    trailer_changed[268] ^= 1;
    assert_eq!(
        gate.observe_at(1, &trailer_changed, now),
        UdpLvdsDisposition::SequenceConflict
    );
    assert_eq!(
        gate.observe_at(1, &ab, now),
        UdpLvdsDisposition::SequenceConflict
    );
    assert_eq!(gate.observe_at(1, &aa, now), UdpLvdsDisposition::Duplicate);
}

#[test]
fn synthetic_common_gate_duplicates_and_conflicts_do_not_extend_the_five_second_ttl() {
    let now = Instant::now();
    let mut gate = UdpLvdsFrameGate::default();
    let bytes = synthetic_frame(0xD3);
    assert_eq!(
        gate.observe_at(8, &bytes, now),
        UdpLvdsDisposition::Accepted
    );
    assert_eq!(
        gate.observe_at(8, &bytes, now + Duration::from_millis(4_999)),
        UdpLvdsDisposition::Duplicate
    );
    assert_eq!(
        gate.observe_at(
            8,
            &synthetic_frame(0xAC),
            now + Duration::from_millis(4_999)
        ),
        UdpLvdsDisposition::SequenceConflict
    );
    assert_eq!(
        gate.observe_at(8, &bytes, now + Duration::from_secs(5)),
        UdpLvdsDisposition::Accepted
    );
    gate.expire_at(now);
    assert_eq!(
        gate.observe_at(8, &bytes, now),
        UdpLvdsDisposition::Duplicate
    );
    gate.expire_at(now + Duration::from_secs(10));
    assert_eq!(
        gate.observe_at(8, &bytes, now + Duration::from_secs(10)),
        UdpLvdsDisposition::Accepted
    );
}

#[test]
fn synthetic_common_gate_evicts_oldest_when_256_frame_capacity_is_full() {
    let now = Instant::now();
    let bytes = synthetic_frame(0xAB);
    let mut gate = UdpLvdsFrameGate::default();
    for sequence in 0..=256 {
        assert_eq!(
            gate.observe_at(sequence, &bytes, now),
            UdpLvdsDisposition::Accepted
        );
    }
    assert_eq!(
        gate.observe_at(256, &bytes, now),
        UdpLvdsDisposition::Duplicate
    );
    assert_eq!(
        gate.observe_at(0, &bytes, now),
        UdpLvdsDisposition::Accepted
    );
}
