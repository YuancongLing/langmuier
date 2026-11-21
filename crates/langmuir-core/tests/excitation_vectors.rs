//! Expectations are frozen by an independent Python stdlib encoder.
use langmuir_core::{ExcitationError, decode_excitation_frame, parse_common_frame};

const TRIANGLE: &[u8; 269] = include_bytes!("fixtures/excitation/SYNTHETIC-ab-triangle.bin");
const SINE: &[u8; 269] = include_bytes!("fixtures/excitation/SYNTHETIC-ab-sine.bin");
const DC: &[u8; 269] = include_bytes!("fixtures/excitation/SYNTHETIC-ab-dc.bin");
const ZERO: &[u8; 269] = include_bytes!("fixtures/excitation/SYNTHETIC-ab-all_zero.bin");
const REAL_NO_SAMPLE_TIME: &[u8; 269] =
    include_bytes!("fixtures/excitation/REAL-202611212141-ab-no-sample-time.bin");
const EXPECTED_TRIANGLE: [u16; 100] = [
    0, 66, 132, 198, 264, 330, 396, 462, 528, 594, 660, 726, 792, 858, 924, 990, 1056, 1122, 1188,
    1254, 1320, 1386, 1452, 1518, 1584, 1650, 1716, 1782, 1848, 1914, 1980, 2046, 2112, 2178, 2244,
    2310, 2376, 2442, 2508, 2574, 2640, 2706, 2772, 2838, 2904, 2970, 3036, 3102, 3168, 3234, 3300,
    3234, 3168, 3102, 3036, 2970, 2904, 2838, 2772, 2706, 2640, 2574, 2508, 2442, 2376, 2310, 2244,
    2178, 2112, 2046, 1980, 1914, 1848, 1782, 1716, 1650, 1584, 1518, 1452, 1386, 1320, 1254, 1188,
    1122, 1056, 990, 924, 858, 792, 726, 660, 594, 528, 462, 396, 330, 264, 198, 132, 66,
];

#[test]
fn python_triangle_preserves_every_big_endian_mv_and_metadata() {
    // Catches wrong DATA offsets, byte order, truncated samples and AA conversion.
    let frame = decode_excitation_frame(&parse_common_frame(TRIANGLE).unwrap()).unwrap();
    assert_eq!(
        (frame.timestamp.utc_seconds, frame.timestamp.microseconds),
        (9, 250_000)
    );
    assert_eq!(
        (frame.f_act_mhz, frame.interval_us, frame.points),
        (931, 10800, 100)
    );
    assert_eq!(
        (frame.work_mode, frame.scan_freq, frame.scan_voltage_mv),
        (0x55, 1, 0)
    );
    assert_eq!((frame.ad9833_status, frame.dac8563_status), (0, 1));
    assert_eq!((frame.sci_frame_seq, frame.pa0_seq), (17, 255));
    assert_eq!(*frame.wave_mv, EXPECTED_TRIANGLE);
    assert!((frame.actual_frequency_hz() - 0.931).abs() < f64::EPSILON);
    assert_eq!(frame.raw_crc, [0x12, 0x34]);
    assert_eq!(frame.integrity(), "CRC_NOT_CHECKED");
}

#[test]
fn latest_point_timestamp_subtracts_99_variable_intervals_across_second_boundary() {
    // Catches treating AB time as wave[0], the AA 500us interval, or time wrapping.
    let frame = decode_excitation_frame(&parse_common_frame(TRIANGLE).unwrap()).unwrap();
    assert_eq!(frame.sample_time_us(0), Some(8_180_800));
    assert_eq!(frame.sample_time_us(1), Some(8_191_600));
    assert_eq!(frame.sample_time_us(50), Some(8_720_800));
    assert_eq!(frame.sample_time_us(99), Some(9_250_000));
    assert_eq!(frame.sample_time_us(100), None);
    assert_eq!(frame.sample_time_us(usize::MAX), None);
    let sine = decode_excitation_frame(&parse_common_frame(SINE).unwrap()).unwrap();
    assert_eq!(sine.sample_time_us(0), Some(8_980_800));
    assert_eq!(sine.sample_time_us(99), Some(10_050_000));
}

#[test]
fn independent_sequence_wrap_modes_status_and_dc_setpoint_are_preserved() {
    // Catches coupling AB seq to AA seq, requiring a frequency for DC, or
    // interpreting DC scan_freq as a meaningful scan frequency.
    let sine = decode_excitation_frame(&parse_common_frame(SINE).unwrap()).unwrap();
    assert_eq!(
        (sine.work_mode, sine.pa0_seq, sine.sci_frame_seq),
        (0xAA, 0, 200)
    );
    assert_eq!((sine.ad9833_status, sine.dac8563_status), (1, 0));
    assert_eq!(
        (
            sine.wave_mv[0],
            sine.wave_mv[25],
            sine.wave_mv[75],
            sine.wave_mv[99]
        ),
        (1650, 3300, 0, 1546)
    );
    let dc = decode_excitation_frame(&parse_common_frame(DC).unwrap()).unwrap();
    assert_eq!((dc.work_mode, dc.pa0_seq, dc.sci_frame_seq), (0xBB, 1, 3));
    assert_eq!(
        (dc.scan_voltage_mv, dc.scan_freq, dc.f_act_mhz),
        (5000, 255, 0)
    );
    assert_eq!(*dc.wave_mv, [2500; 100]);
    assert!(!dc.is_all_zero());
}

#[test]
fn zero_wave_and_frequency_and_arbitrary_crc_remain_accepted() {
    // Catches an invented data-valid flag or undocumented CRC gate.
    let frame = decode_excitation_frame(&parse_common_frame(ZERO).unwrap()).unwrap();
    assert!(frame.is_all_zero());
    assert!(frame.has_sample_timing());
    assert_eq!(frame.sample_time_us(0), Some(10_930_800));
    assert_eq!(frame.sample_time_us(99), Some(12_000_000));
    assert_eq!(frame.actual_frequency_hz(), 0.0);
    for crc in [[0, 0], [0xFF, 0xFF], [0x09, 0xD7], [0xEB, 0x90]] {
        let mut wire = *TRIANGLE;
        wire[267..].copy_from_slice(&crc);
        let decoded = decode_excitation_frame(&parse_common_frame(&wire).unwrap()).unwrap();
        assert_eq!(decoded.raw_crc, crc);
        assert_eq!(*decoded.wave_mv, EXPECTED_TRIANGLE);
    }
}

fn decode_error(data_offset: usize, bytes: &[u8]) -> ExcitationError {
    let mut wire = *TRIANGLE;
    wire[7 + data_offset..7 + data_offset + bytes.len()].copy_from_slice(bytes);
    decode_excitation_frame(&parse_common_frame(&wire).unwrap()).unwrap_err()
}

#[test]
fn invalid_points_reserved_and_mv_are_classified() {
    // Catches omitted validation or hiding a field error as CRC/communication loss.
    assert_eq!(
        decode_error(14, &[0, 99]),
        ExcitationError::InvalidPoints { actual: 99 }
    );
    assert_eq!(
        decode_error(24, &[1]),
        ExcitationError::NonZeroReserved {
            index: 0,
            actual: 1
        }
    );
    assert_eq!(
        decode_error(59, &[2]),
        ExcitationError::NonZeroReserved {
            index: 35,
            actual: 2
        }
    );
    assert_eq!(
        decode_error(60, &[0x0C, 0xE5]),
        ExcitationError::WaveOutOfRange {
            index: 0,
            actual: 3301
        }
    );
    assert_eq!(
        decode_error(258, &[0xFF, 0xFF]),
        ExcitationError::WaveOutOfRange {
            index: 99,
            actual: 65535
        }
    );
}

#[test]
fn mode_scan_setpoint_and_status_constraints_follow_workbook() {
    // Catches swapping mode rules or inventing normal/fault status meanings.
    assert_eq!(
        decode_error(16, &[0xCC]),
        ExcitationError::InvalidMode { actual: 0xCC }
    );
    assert_eq!(
        decode_error(17, &[101]),
        ExcitationError::InvalidScanFrequency { actual: 101 }
    );
    assert_eq!(
        decode_error(18, &[0, 1]),
        ExcitationError::InvalidScanVoltage {
            work_mode: 0x55,
            actual: 1
        }
    );
    assert_eq!(
        decode_error(16, &[0xBB, 255, 0x13, 0x89]),
        ExcitationError::InvalidScanVoltage {
            work_mode: 0xBB,
            actual: 5001
        }
    );
    assert_eq!(
        decode_error(20, &[2]),
        ExcitationError::InvalidStatus {
            field: "ad9833_status",
            actual: 2
        }
    );
    assert_eq!(
        decode_error(21, &[255]),
        ExcitationError::InvalidStatus {
            field: "dac8563_status",
            actual: 255
        }
    );
}

#[test]
fn invalid_microseconds_and_time_underflow_do_not_wrap_or_panic() {
    // Catches silently normalizing a malformed timestamp or u32 subtraction.
    assert_eq!(
        decode_error(4, &[0, 0x0F, 0x42, 0x40]),
        ExcitationError::InvalidMicroseconds { actual: 1_000_000 }
    );
    assert_eq!(
        decode_error(0, &[0, 0, 0, 0]),
        ExcitationError::TimestampUnderflow {
            latest_us: 250_000,
            span_us: 1_069_200
        }
    );
    let mut wire = *TRIANGLE;
    wire[7..11].copy_from_slice(&u32::MAX.to_be_bytes());
    wire[11..15].copy_from_slice(&999_999_u32.to_be_bytes());
    let frame = decode_excitation_frame(&parse_common_frame(&wire).unwrap()).unwrap();
    assert!(frame.has_sample_timing());
    assert_eq!(frame.sample_time_us(0), Some(4_294_967_294_930_799));
    assert_eq!(frame.sample_time_us(99), Some(4_294_967_295_999_999));
}

#[test]
fn zero_interval_is_accepted_without_fabricating_sample_times() {
    // Catches inventing a minimum interval or assigning time to untimed slots.
    let mut wire = *TRIANGLE;
    wire[19..21].copy_from_slice(&[0, 0]);
    let frame = decode_excitation_frame(&parse_common_frame(&wire).unwrap()).unwrap();
    assert!(!frame.has_sample_timing());
    assert_eq!(frame.sample_time_us(0), None);
    assert_eq!(frame.sample_time_us(99), None);
    assert_eq!(*frame.wave_mv, EXPECTED_TRIANGLE);
}

#[test]
fn actual_zero_interval_status_keeps_all_slots_without_inventing_100_times() {
    // Original capture SHA256 and exact packet provenance are recorded in
    // docs/qa/lvds-multitype-s02-real-02/interval-zero-regression-fixture.json.
    // Catches treating a legal configuration/status packet as a measured window.
    let frame = decode_excitation_frame(&parse_common_frame(REAL_NO_SAMPLE_TIME).unwrap()).unwrap();
    assert_eq!(
        (frame.timestamp.utc_seconds, frame.timestamp.microseconds),
        (367, 141_944)
    );
    assert_eq!(
        (frame.interval_us, frame.points, frame.f_act_mhz),
        (0, 100, 0)
    );
    assert_eq!(
        (frame.work_mode, frame.scan_freq, frame.scan_voltage_mv),
        (0x55, 0, 0)
    );
    assert_eq!((frame.pa0_seq, frame.sci_frame_seq), (183, 94));
    assert_eq!(*frame.wave_mv, [0; 100]);
    assert_eq!(frame.raw_crc, [0xB3, 0x36]);
    assert!(!frame.has_sample_timing());
    let times: Vec<_> = (0..100).map(|index| frame.sample_time_us(index)).collect();
    eprintln!(
        "actual raw AB 5 / outer 9407: first={:?}, last={:?}, all_100_equal={}",
        times[0],
        times[99],
        times.iter().all(|time| *time == times[0])
    );
    assert_eq!(times, vec![None; 100]);
}

#[test]
fn zero_interval_status_does_not_bypass_other_field_validation() {
    // Catches an early status-frame return that skips the normal AB checks.
    for (offset, bytes, expected) in [
        (
            14,
            vec![0, 99],
            ExcitationError::InvalidPoints { actual: 99 },
        ),
        (
            16,
            vec![0xCC],
            ExcitationError::InvalidMode { actual: 0xCC },
        ),
        (
            17,
            vec![101],
            ExcitationError::InvalidScanFrequency { actual: 101 },
        ),
        (
            20,
            vec![2],
            ExcitationError::InvalidStatus {
                field: "ad9833_status",
                actual: 2,
            },
        ),
        (
            24,
            vec![1],
            ExcitationError::NonZeroReserved {
                index: 0,
                actual: 1,
            },
        ),
        (
            60,
            vec![0x0C, 0xE5],
            ExcitationError::WaveOutOfRange {
                index: 0,
                actual: 3301,
            },
        ),
    ] {
        let mut wire = *REAL_NO_SAMPLE_TIME;
        wire[7 + offset..7 + offset + bytes.len()].copy_from_slice(&bytes);
        assert_eq!(
            decode_excitation_frame(&parse_common_frame(&wire).unwrap()),
            Err(expected)
        );
    }
}

#[test]
fn other_public_frame_types_are_not_decoded_as_excitation() {
    // Catches bypassing S01's existing type dispatch.
    let mut wire = *TRIANGLE;
    for kind in [0xAA, 0xAC, 0xDD] {
        wire[6] = kind;
        assert_eq!(
            decode_excitation_frame(&parse_common_frame(&wire).unwrap()),
            Err(ExcitationError::WrongPacketType { actual: kind })
        );
    }
}
