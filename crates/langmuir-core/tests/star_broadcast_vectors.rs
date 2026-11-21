use std::time::{Duration, UNIX_EPOCH};

use langmuir_core::{
    AttitudeData, BroadcastKind, GNSS_PRESETS, GnssFix, GnssMode, StarTime,
    encode_attitude_broadcast, encode_broadcast, encode_gnss_broadcast, encode_time_broadcast,
    next_gnss_sequence, wgs84_ecef,
};

const FIXED: StarTime = StarTime {
    seconds: 280_022_400,
    microseconds: 999_999,
};

fn unhex(input: &str) -> Vec<u8> {
    input
        .trim()
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn valid_outer(frame: &[u8], length: usize) {
    assert_eq!(frame.len(), length);
    assert_eq!(&frame[..4], &[0xEB, 0x90, 0xF0, 0x55]);
    assert_eq!(
        usize::from(u16::from_be_bytes([frame[4], frame[5]])),
        length - 6
    );
    assert_eq!(frame[6], 0xFF);
    assert_eq!(
        frame[length - 1],
        frame[6..length - 1]
            .iter()
            .fold(0u8, |sum, b| sum.wrapping_add(*b))
    );
}

#[test]
fn fixed_utc_time_matches_independent_python_golden() {
    let time =
        StarTime::from_system_time(UNIX_EPOCH + Duration::new(1_794_787_200, 999_999_000)).unwrap();
    assert_eq!(time, FIXED);
    let frame = encode_time_broadcast(time).unwrap();
    valid_outer(&frame, 14);
    assert_eq!(
        frame,
        unhex(include_str!("../../../docs/qa/star-simulator/time.hex"))
    );
}

#[test]
fn time_quantization_floors_and_preserves_second_boundary() {
    for (micros, expected) in [
        (0, 0),
        (24, 0),
        (25, 1),
        (999_974, 39_998),
        (999_975, 39_999),
        (999_999, 39_999),
    ] {
        let frame = encode_time_broadcast(StarTime {
            seconds: 0x0102_0304,
            microseconds: micros,
        })
        .unwrap();
        assert_eq!(&frame[7..11], &[1, 2, 3, 4]);
        assert_eq!(u16::from_be_bytes([frame[11], frame[12]]), expected);
    }
    let before =
        StarTime::from_system_time(UNIX_EPOCH + Duration::new(1_514_764_800, 999_999_999)).unwrap();
    let after = StarTime::from_system_time(UNIX_EPOCH + Duration::new(1_514_764_801, 0)).unwrap();
    assert_eq!((before.seconds, before.microseconds), (0, 999_999));
    assert_eq!((after.seconds, after.microseconds), (1, 0));
    assert!(StarTime::from_system_time(UNIX_EPOCH + Duration::from_secs(1_514_764_799)).is_err());
    assert!(
        StarTime::from_system_time(
            UNIX_EPOCH + Duration::from_secs(1_514_764_800 + u64::from(u32::MAX) + 1)
        )
        .is_err()
    );
    assert!(
        encode_time_broadcast(StarTime {
            seconds: 0,
            microseconds: 1_000_000
        })
        .is_err()
    );
}

#[test]
fn attitude_matches_independent_golden_and_all_field_offsets() {
    let frame = encode_attitude_broadcast(FIXED, &AttitudeData::synthetic()).unwrap();
    valid_outer(&frame, 67);
    assert_eq!(
        frame,
        unhex(include_str!("../../../docs/qa/star-simulator/attitude.hex"))
    );
    let w = &frame[7..66];
    assert_eq!(&w[4..7], &[0x0F, 0x42, 0x3F]);
    assert_eq!(
        &w[7..16],
        &[0x00, 0x4E, 0x20, 0xFF, 0x63, 0xC0, 0x00, 0xEA, 0x60]
    );
    assert_eq!(&w[16..22], &[0x03, 0xE8, 0xF8, 0x30, 0x0B, 0xB8]);
    assert_eq!(w[22], 0x40);
    assert_eq!(&w[23..27], &[0x45, 0xDA, 0xC0, 0]);
    assert_eq!(
        &w[47..59],
        &[0, 1, 0xFF, 0xFE, 0, 3, 0xFF, 0xFC, 0, 5, 0xFF, 0xFA]
    );
}

#[test]
fn attitude_rejects_quantization_overflow_nonfinite_and_reserved_rotation() {
    let mut data = AttitudeData::synthetic();
    data.angles_deg[0] = 420.;
    assert!(encode_attitude_broadcast(FIXED, &data).is_err());
    data = AttitudeData::synthetic();
    data.rates_deg_per_sec[0] = -4.;
    assert!(encode_attitude_broadcast(FIXED, &data).is_err());
    data = AttitudeData::synthetic();
    data.orbit_elements[0] = f32::NAN;
    assert!(encode_attitude_broadcast(FIXED, &data).is_err());
    data = AttitudeData::synthetic();
    data.magnetic_nt[5] = 200_000.;
    assert!(encode_attitude_broadcast(FIXED, &data).is_err());
    data = AttitudeData::synthetic();
    data.rotation_order = 6;
    assert!(encode_attitude_broadcast(FIXED, &data).is_err());
}

#[test]
fn three_complete_gnss_points_match_independent_python_golden() {
    let fixtures = [
        include_str!("../../../docs/qa/star-simulator/gnss-0.hex"),
        include_str!("../../../docs/qa/star-simulator/gnss-1.hex"),
        include_str!("../../../docs/qa/star-simulator/gnss-2.hex"),
    ];
    let xyz = [
        [677_813_700, 0, 0],
        [-245_445_989, 503_238_854, 389_597_631],
        [192_899_459, -529_986_907, -392_465_513],
    ];
    for (index, point) in GNSS_PRESETS.iter().copied().enumerate() {
        let frame = encode_gnss_broadcast(FIXED, point, 16_383).unwrap();
        valid_outer(&frame, 122);
        assert_eq!(frame, unhex(fixtures[index]));
        let w = &frame[7..121];
        assert_eq!(&w[..8], &[0x09, 0xF5, 0xFF, 0xFF, 0, 0x6B, 3, 0xFF]);
        assert_eq!(u16::from_be_bytes([w[0], w[1]]) & 0x7FF, 0x1F5);
        assert_eq!(w[8], 0x62);
        for (axis, expected) in xyz[index].iter().enumerate() {
            assert_eq!(
                i32::from_be_bytes(w[30 + axis * 4..34 + axis * 4].try_into().unwrap()),
                *expected
            );
        }
        assert_eq!(&w[30..54], &w[54..78]);
        let inner = w[8..113].iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
        assert_eq!(w[113], inner);
        assert!(point.label.contains("SYNTHETIC"));
    }
}

#[test]
fn gnss_sequence_wraps_at_fourteen_bits() {
    assert_eq!(next_gnss_sequence(16_382), 16_383);
    assert_eq!(next_gnss_sequence(16_383), 0);
    let f = encode_gnss_broadcast(FIXED, GNSS_PRESETS[0], 0).unwrap();
    assert_eq!(&f[9..11], &[0xC0, 0]);
    assert!(encode_gnss_broadcast(FIXED, GNSS_PRESETS[0], 16_384).is_err());
}

#[test]
fn gnss_status_modes_validity_and_signed_velocity_are_encoded() {
    let mut p = GNSS_PRESETS[1];
    p.mode = GnssMode::Bds;
    p.fix = GnssFix::Extrapolated;
    p.velocity_ecef_mps = [-1.25, 2.5, -3.75];
    p.gps_satellites = 0;
    p.bds_satellites = 7;
    let f = encode_gnss_broadcast(FIXED, p, 1).unwrap();
    let w = &f[7..121];
    assert_eq!(w[8], 0xD4);
    assert_eq!(w[12], 0x07);
    assert_eq!(w[13], 0x07);
    assert_eq!(
        &w[42..54],
        &[
            (-125i32).to_be_bytes(),
            250i32.to_be_bytes(),
            (-375i32).to_be_bytes()
        ]
        .concat()
    );
    assert!(w[54..78].iter().all(|b| *b == 0));
    p.fix = GnssFix::Invalid;
    let f = encode_gnss_broadcast(FIXED, p, 2).unwrap();
    assert!(f[37..97].iter().all(|b| *b == 0));
}

#[test]
fn coordinate_conversion_is_consistent_and_rejects_invalid_or_unrepresentable_points() {
    let ecef = wgs84_ecef(GNSS_PRESETS[0]).unwrap();
    assert!((ecef[0] - 6_778_137.).abs() < 1e-6);
    for (field, value) in [(0, 91.), (1, -181.), (2, f64::NAN), (2, 30_000_000.)] {
        let mut p = GNSS_PRESETS[0];
        match field {
            0 => p.latitude_deg = value,
            1 => p.longitude_deg = value,
            _ => p.height_m = value,
        }
        assert!(encode_gnss_broadcast(FIXED, p, 0).is_err());
    }
    let mut p = GNSS_PRESETS[0];
    p.gps_satellites = 16;
    assert!(encode_gnss_broadcast(FIXED, p, 0).is_err());
}

#[test]
fn dispatch_uses_same_complete_formats() {
    for kind in BroadcastKind::ALL {
        let f = encode_broadcast(kind, FIXED, GNSS_PRESETS[0], 16_383).unwrap();
        valid_outer(&f, [14, 67, 122][kind.index()]);
    }
}

#[test]
fn orbit_status_enables_orbit_and_explicit_leap_history_adjusts_epoch_seconds() {
    let mut point = GNSS_PRESETS[0];
    point.fix = GnssFix::Orbit;
    point.mode = GnssMode::Gps;
    point.bds_satellites = 0;
    let frame = encode_gnss_broadcast(FIXED, point, 0).unwrap();
    assert_eq!(frame[15], 0x8A);
    assert_eq!(
        frame[16] & 1,
        0,
        "an orbit-valid synthetic status cannot claim orbit is forbidden"
    );
    assert_eq!(
        StarTime::from_unix_micros(1_514_764_801_999_999, 1).unwrap(),
        StarTime {
            seconds: 2,
            microseconds: 999_999
        }
    );
}
