//! Field encoders for the 2026-07-28 RS422 tables 2-5 through 2-9.
//!
//! All built-in values describe SYNTHETIC static positioning/attitude tests.
//! They are neither receiver measurements nor a propagated physical orbit.

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::additive_checksum;

/// UTC 2018-01-01T00:00:00 expressed in POSIX seconds.
pub const STAR_EPOCH_UNIX_SECONDS: u64 = 1_514_764_800;
/// GPS minus UTC since 2017-01-01; still 18 seconds at the 2026-11-16 audit.
pub const GNSS_GPS_MINUS_UTC: u8 = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastKind {
    Time,
    Attitude,
    Gnss,
}

impl BroadcastKind {
    pub const ALL: [Self; 3] = [Self::Time, Self::Attitude, Self::Gnss];
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Time => 0,
            Self::Attitude => 1,
            Self::Gnss => 2,
        }
    }
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Time => "整星时间",
            Self::Attitude => "姿态轨道",
            Self::Gnss => "GNSS定位",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BroadcastError(pub &'static str);
impl fmt::Display for BroadcastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for BroadcastError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StarTime {
    pub seconds: u32,
    pub microseconds: u32,
}

impl StarTime {
    /// Converts system UTC/POSIX time to the protocol epoch, flooring nanoseconds.
    ///
    /// No leap second has occurred between 2018-01-01 and the 2026-11-16 audit.
    /// Thus the built-in policy adds zero leap seconds since the epoch; it does
    /// not treat Unix seconds as star seconds or add GPS-UTC's 18 seconds.
    /// Future leap announcements require updating this policy; callers with an
    /// explicit leap history can use [`Self::from_unix_micros`]. OS clock slews
    /// and smears are inherited; this is a software simulator, not a UTC source.
    ///
    /// # Errors
    /// Returns an error before the epoch or beyond the protocol u32 range.
    pub fn from_system_time(time: SystemTime) -> Result<Self, BroadcastError> {
        let elapsed = time
            .duration_since(UNIX_EPOCH)
            .map_err(|_| BroadcastError("time precedes Unix epoch"))?;
        let micros = u64::try_from(elapsed.as_micros())
            .map_err(|_| BroadcastError("time exceeds microsecond range"))?;
        Self::from_unix_micros(micros, 0)
    }

    /// Converts POSIX microseconds, explicitly adding leap seconds since 2018.
    ///
    /// # Errors
    /// Rejects time before the star epoch and integer overflow.
    ///
    /// # Panics
    /// The internal conversion is bounded by a modulo of one million and cannot panic.
    pub fn from_unix_micros(
        unix_micros: u64,
        leap_seconds_since_2018: u32,
    ) -> Result<Self, BroadcastError> {
        let relative = unix_micros
            .checked_sub(STAR_EPOCH_UNIX_SECONDS * 1_000_000)
            .ok_or(BroadcastError("time precedes star epoch 2018 UTC"))?;
        let seconds = relative / 1_000_000 + u64::from(leap_seconds_since_2018);
        Ok(Self {
            seconds: u32::try_from(seconds)
                .map_err(|_| BroadcastError("star seconds overflow u32"))?,
            microseconds: u32::try_from(relative % 1_000_000)
                .expect("microseconds below one million"),
        })
    }

    fn validate(self) -> Result<(), BroadcastError> {
        if self.microseconds >= 1_000_000 {
            Err(BroadcastError("microseconds must be below one second"))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GnssFix {
    Invalid,
    Position,
    Orbit,
    Extrapolated,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GnssMode {
    Gps,
    Bds,
    GpsBds,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GnssPoint {
    pub label: &'static str,
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    pub height_m: f64,
    pub velocity_ecef_mps: [f64; 3],
    pub fix: GnssFix,
    pub mode: GnssMode,
    pub gps_satellites: u8,
    pub bds_satellites: u8,
}

pub const GNSS_PRESETS: [GnssPoint; 3] = [
    GnssPoint {
        label: "SYNTHETIC A · 赤道",
        latitude_deg: 0.,
        longitude_deg: 0.,
        height_m: 400_000.,
        velocity_ecef_mps: [0.; 3],
        fix: GnssFix::Position,
        mode: GnssMode::GpsBds,
        gps_satellites: 8,
        bds_satellites: 8,
    },
    GnssPoint {
        label: "SYNTHETIC B · 北半球",
        latitude_deg: 35.,
        longitude_deg: 116.,
        height_m: 450_000.,
        velocity_ecef_mps: [0.; 3],
        fix: GnssFix::Position,
        mode: GnssMode::GpsBds,
        gps_satellites: 8,
        bds_satellites: 8,
    },
    GnssPoint {
        label: "SYNTHETIC C · 南半球",
        latitude_deg: -35.,
        longitude_deg: -70.,
        height_m: 500_000.,
        velocity_ecef_mps: [0.; 3],
        fix: GnssFix::Position,
        mode: GnssMode::GpsBds,
        gps_satellites: 8,
        bds_satellites: 8,
    },
];

/// Physical units follow table 2-7; orbit elements are a[km], e, i, Ω, ω, M[deg].
#[derive(Debug, Clone, PartialEq)]
pub struct AttitudeData {
    pub angles_deg: [f64; 3],
    pub rates_deg_per_sec: [f64; 3],
    pub rotation_order: u8,
    pub orbit_elements: [f32; 6],
    pub magnetic_nt: [f64; 6],
}

impl AttitudeData {
    /// Explicit, deterministic test values; no field is an actual measurement.
    #[must_use]
    pub const fn synthetic() -> Self {
        Self {
            angles_deg: [1., -2., 3.],
            rates_deg_per_sec: [0.1, -0.2, 0.3],
            rotation_order: 2,
            orbit_elements: [7000., 0.001, 51.6, 20., 30., 40.],
            magnetic_nt: [6., -12., 18., -24., 30., -36.],
        }
    }
}

fn frame(data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(data.len() + 8);
    bytes.extend_from_slice(&[0xEB, 0x90, 0xF0, 0x55]);
    bytes.extend_from_slice(
        &u16::try_from(data.len() + 2)
            .expect("fixed broadcast size")
            .to_be_bytes(),
    );
    bytes.push(0xFF);
    bytes.extend_from_slice(data);
    bytes.push(additive_checksum(&bytes[6..]));
    bytes
}

/// Encodes table 2-5, flooring the subsecond part to 25µs units.
///
/// # Errors
/// Rejects invalid microseconds.
///
/// # Panics
/// The internal u16 conversion is reached only after checking the microsecond range.
pub fn encode_time_broadcast(time: StarTime) -> Result<Vec<u8>, BroadcastError> {
    time.validate()?;
    let mut data = [0; 6];
    data[..4].copy_from_slice(&time.seconds.to_be_bytes());
    data[4..].copy_from_slice(
        &u16::try_from(time.microseconds / 25)
            .expect("at most 39999")
            .to_be_bytes(),
    );
    Ok(frame(&data))
}

#[allow(clippy::cast_possible_truncation)] // Finite/in-range checks precede the conversion.
fn quantize(value: f64, unit: f64, min: i64, max: i64) -> Result<i64, BroadcastError> {
    let raw = (value / unit).round();
    #[allow(clippy::cast_precision_loss)]
    // Bounds here are at most exactly representable 32-bit integers.
    if !raw.is_finite() || raw < min as f64 || raw > max as f64 {
        return Err(BroadcastError("nonfinite or overflowing fixed-point field"));
    }
    Ok(raw as i64)
}

fn quantize_i32(value: f64, unit: f64) -> Result<i32, BroadcastError> {
    Ok(i32::try_from(quantize(
        value,
        unit,
        i64::from(i32::MIN),
        i64::from(i32::MAX),
    )?)
    .expect("validated i32"))
}

/// Encodes every W0–W58 field individually, including signed 24-bit angles.
///
/// # Errors
/// Rejects nonfinite/overflowing values, invalid time, and reserved rotation orders.
///
/// # Panics
/// Internal integer conversions follow explicit range checks and cannot panic.
pub fn encode_attitude_broadcast(
    time: StarTime,
    data: &AttitudeData,
) -> Result<Vec<u8>, BroadcastError> {
    time.validate()?;
    if data.rotation_order > 5 || data.orbit_elements.iter().any(|v| !v.is_finite()) {
        return Err(BroadcastError(
            "invalid rotation order or nonfinite orbit element",
        ));
    }
    let mut w = [0; 59];
    w[..4].copy_from_slice(&time.seconds.to_be_bytes());
    w[4..7].copy_from_slice(&time.microseconds.to_be_bytes()[1..]);
    for (axis, value) in data.angles_deg.iter().enumerate() {
        let raw = i32::try_from(quantize(*value, 0.000_05, -8_388_608, 8_388_607)?)
            .expect("validated i24");
        w[7 + axis * 3..10 + axis * 3].copy_from_slice(&raw.to_be_bytes()[1..]);
    }
    for (axis, value) in data.rates_deg_per_sec.iter().enumerate() {
        let raw = i16::try_from(quantize(
            *value,
            0.000_1,
            i64::from(i16::MIN),
            i64::from(i16::MAX),
        )?)
        .expect("validated i16");
        w[16 + axis * 2..18 + axis * 2].copy_from_slice(&raw.to_be_bytes());
    }
    w[22] = data.rotation_order << 5;
    for (index, value) in data.orbit_elements.iter().enumerate() {
        w[23 + index * 4..27 + index * 4].copy_from_slice(&value.to_be_bytes());
    }
    for (index, value) in data.magnetic_nt.iter().enumerate() {
        let raw = i16::try_from(quantize(
            *value,
            6.,
            i64::from(i16::MIN),
            i64::from(i16::MAX),
        )?)
        .expect("validated i16");
        w[47 + index * 2..49 + index * 2].copy_from_slice(&raw.to_be_bytes());
    }
    Ok(frame(&w))
}

/// Converts one WGS-84 geodetic position to Earth-fixed XYZ meters.
///
/// # Errors
/// Rejects invalid latitude/longitude or nonfinite height.
pub fn wgs84_ecef(point: GnssPoint) -> Result<[f64; 3], BroadcastError> {
    if !point.latitude_deg.is_finite()
        || !(-90.0..=90.0).contains(&point.latitude_deg)
        || !point.longitude_deg.is_finite()
        || !(-180.0..=180.0).contains(&point.longitude_deg)
        || !point.height_m.is_finite()
    {
        return Err(BroadcastError("invalid WGS-84 geodetic coordinates"));
    }
    let a = 6_378_137.0;
    let f = 1.0 / 298.257_223_563;
    let e2 = f * (2.0 - f);
    let lat = point.latitude_deg.to_radians();
    let lon = point.longitude_deg.to_radians();
    let n = a / (1.0 - e2 * lat.sin().powi(2)).sqrt();
    Ok([
        (n + point.height_m) * lat.cos() * lon.cos(),
        (n + point.height_m) * lat.cos() * lon.sin(),
        (n * (1.0 - e2) + point.height_m) * lat.sin(),
    ])
}

/// Next independent 14-bit GNSS packet sequence counter.
#[must_use]
pub const fn next_gnss_sequence(sequence: u16) -> u16 {
    sequence.wrapping_add(1) & 0x3FFF
}

/// Encodes the complete table 2-9 payload, including both checksums.
///
/// Ancillary defaults are synthetic: command/reset/upload/error counters and
/// uptime/clock drift/extrapolation are zero; SNR 45, GDOP 2, versions 0. Board
/// temperature +23°C is the table's default. W91 local seconds follow the same
/// test clock as W24 UTC. The primary/secondary ECEF solutions coincide only
/// for Position; the table requires secondary position/velocity zero otherwise.
/// No actual receiver, firmware, tracking or orbit health is being reported.
///
/// # Errors
/// Rejects invalid time, sequence, satellite nibbles and overflowing coordinates/velocity.
pub fn encode_gnss_broadcast(
    time: StarTime,
    point: GnssPoint,
    sequence: u16,
) -> Result<Vec<u8>, BroadcastError> {
    time.validate()?;
    if sequence > 0x3FFF || point.gps_satellites > 15 || point.bds_satellites > 15 {
        return Err(BroadcastError(
            "GNSS sequence or satellite nibble out of range",
        ));
    }
    let xyz = wgs84_ecef(point)?;
    let position = xyz.map(|value| quantize_i32(value, 0.01));
    let velocity = point
        .velocity_ecef_mps
        .map(|value| quantize_i32(value, 0.01));
    let llh = [
        quantize_i32(point.latitude_deg.to_radians(), 0.000_000_1)?,
        quantize_i32(point.longitude_deg.to_radians(), 0.000_000_1)?,
        quantize_i32(point.height_m, 0.01)?,
    ];
    let mut w = [0; 114];
    // W1–8: version 000, telemetry 0, secondary header 1, APID 1F5.
    w[..2].copy_from_slice(&0x09F5u16.to_be_bytes());
    w[2..4].copy_from_slice(&(0xC000 | sequence).to_be_bytes());
    w[4..8].copy_from_slice(&[0, 0x6B, 3, 0xFF]);
    let fix = match point.fix {
        GnssFix::Invalid => 0,
        GnssFix::Position => 1,
        GnssFix::Orbit => 2,
        GnssFix::Extrapolated => 3,
    };
    let mode = match point.mode {
        GnssMode::Gps => 1,
        GnssMode::Bds => 2,
        GnssMode::GpsBds => 4,
    };
    let pps = u8::from(matches!(point.fix, GnssFix::Position | GnssFix::Orbit));
    w[8] = (fix << 6) | (mode << 3) | (u8::from(point.mode == GnssMode::Bds) << 2) | (pps << 1);
    // W10: both basebands follow valid positioning; BB1/autonomous/default
    // parameters, jump monitor off, orbit forbidden for these static tests.
    w[9] = match point.fix {
        GnssFix::Invalid => 1,
        GnssFix::Position => 0xC1,
        GnssFix::Orbit | GnssFix::Extrapolated => 0xC0,
    };
    w[10] = 45;
    w[11] = 45;
    w[12] = (point.gps_satellites << 4) | point.bds_satellites;
    w[13] = w[12];
    // W15–17 command counters/last command, W18–19 signed clock drift,
    // W20–23 uptime remain explicitly initialized synthetic zero.
    w[23..27].copy_from_slice(&time.seconds.to_be_bytes());
    let stars = point
        .gps_satellites
        .saturating_add(point.bds_satellites)
        .min(15);
    w[27] = (stars << 4) | stars;
    // W29/30 reset counts and reasons: initial state 0.
    for axis in 0..3 {
        let pos = position[axis]?;
        let vel = velocity[axis]?;
        if point.fix != GnssFix::Invalid {
            w[30 + axis * 4..34 + axis * 4].copy_from_slice(&pos.to_be_bytes());
            w[42 + axis * 4..46 + axis * 4].copy_from_slice(&vel.to_be_bytes());
        }
        if point.fix == GnssFix::Position {
            w[54 + axis * 4..58 + axis * 4].copy_from_slice(&pos.to_be_bytes());
            w[66 + axis * 4..70 + axis * 4].copy_from_slice(&vel.to_be_bytes());
        }
        if point.fix != GnssFix::Invalid {
            w[78 + axis * 4..82 + axis * 4].copy_from_slice(&llh[axis].to_be_bytes());
        }
    }
    w[90..94].copy_from_slice(&time.seconds.to_be_bytes());
    // W95–96 extrapolation time 0, W97 GPS-UTC, W98 default +23°C,
    // W99/100 unversioned synthetic boot/application software.
    w[96] = GNSS_GPS_MINUS_UTC;
    w[97] = 23;
    // W101–104: communication/Tmark normal; synthetic 45dB band (2);
    // orbit-invalid health bit for Position/Invalid (no propagated orbit).
    let health = (2u32 << 23)
        | if matches!(point.fix, GnssFix::Position | GnssFix::Invalid) {
            1 << 7
        } else {
            0
        };
    w[100..104].copy_from_slice(&health.to_be_bytes());
    // W105 CAN resets, W106–109 upload state/length/version, W110 SRAM
    // counts, W111 status and W112 signed convergence: synthetic zero.
    // W113: PPS enabled, default BDS params, CAN-A, GDOP=2.
    w[112] = 2;
    w[113] = additive_checksum(&w[8..113]);
    Ok(frame(&w))
}

/// Produces the selected complete frame, with explicit synthetic attitude defaults.
///
/// # Errors
/// Returns the selected field encoder's validation error.
pub fn encode_broadcast(
    kind: BroadcastKind,
    time: StarTime,
    point: GnssPoint,
    sequence: u16,
) -> Result<Vec<u8>, BroadcastError> {
    match kind {
        BroadcastKind::Time => encode_time_broadcast(time),
        BroadcastKind::Attitude => encode_attitude_broadcast(time, &AttitudeData::synthetic()),
        BroadcastKind::Gnss => encode_gnss_broadcast(time, point, sequence),
    }
}
