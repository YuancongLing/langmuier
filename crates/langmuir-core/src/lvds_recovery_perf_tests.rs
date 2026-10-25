//! Explicit, local CPU measurements using SYNTHETIC protocol fixtures.
//!
//! These timings exercise the production Rust entry points, but do not measure
//! device throughput, UDP scheduling, storage, GUI latency, or process memory.

use std::hint::black_box;
use std::time::Instant;

use super::{
    FrameQuality, LvdsBitAlignmentRecoverer, MAX_RECOVERY_PAYLOAD_BYTES, RecoveryAnchor,
    RecoveryContext, RecoveryResult, ReferenceCrcStatus,
};
use crate::lvds::test_support::valid_frame;
use crate::{LVDS_FRAME_LEN, decode_scientific_frame};

const BATCHES: usize = 7;
const WARMUPS: usize = 10;
const FIXED_HEADER: [u8; 7] = [0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA];

struct Measurement {
    name: &'static str,
    iterations: u32,
    ns_per_call: [f64; BATCHES],
}

impl Measurement {
    fn new(name: &'static str, iterations: u32) -> Self {
        Self {
            name,
            iterations,
            ns_per_call: [0.0; BATCHES],
        }
    }

    fn measure_batch<T>(&mut self, batch: usize, mut operation: impl FnMut() -> T) {
        let started = Instant::now();
        for _ in 0..self.iterations {
            // Keep input handling, allocations, and result destruction in the
            // measured production call. Assertions and printing stay outside.
            black_box(operation());
        }
        self.ns_per_call[batch] =
            started.elapsed().as_secs_f64() * 1_000_000_000.0 / f64::from(self.iterations);
    }

    fn median(&self) -> f64 {
        let mut sorted = self.ns_per_call;
        sorted.sort_by(f64::total_cmp);
        sorted[BATCHES / 2]
    }

    fn print(&self) {
        // Each line is independently machine-readable JSON after its prefix.
        println!(
            "SYNTHETIC_RUST_MEASUREMENT {{\"path\":\"{}\",\"iterations_per_batch\":{},\"batches\":{},\"median_ns_per_call\":{:.3},\"batch_ns_per_call\":{:?}}}",
            self.name,
            self.iterations,
            BATCHES,
            self.median(),
            self.ns_per_call,
        );
    }
}

fn continuous_context() -> RecoveryContext {
    RecoveryContext {
        udp_sequence: 0,
        previous: Some(RecoveryAnchor {
            udp_sequence: u32::MAX,
            frame_seq_u8: 254,
            sample_time_us: 1_700_000_000_073_456,
        }),
    }
}

// Independently pack individual bits, without invoking production alignment.
fn prefix_bits(bytes: &[u8], count: usize) -> Vec<u8> {
    let mut packed = vec![0_u8; (bytes.len() * 8 + count).div_ceil(8)];
    for (byte_index, byte) in bytes.iter().enumerate() {
        for bit in 0..8 {
            if byte & (1 << (7 - bit)) != 0 {
                let output_bit = count + byte_index * 8 + bit;
                packed[output_bit / 8] |= 1 << (7 - output_bit % 8);
            }
        }
    }
    packed
}

fn assert_bounded_result(result: &RecoveryResult, payload_len: usize) {
    assert!(result.candidate_count <= 2);
    assert!(result.bytes.len() <= LVDS_FRAME_LEN);
    assert!(result.available_bits <= LVDS_FRAME_LEN * 8);
    if let Some(offset) = result.bit_offset {
        assert!(offset + result.available_bits <= payload_len * 8);
        assert!(offset < MAX_RECOVERY_PAYLOAD_BYTES * 8);
    }
}

fn assert_native_fixture(bytes: &[u8]) {
    let direct = decode_scientific_frame(bytes).expect("valid SYNTHETIC native fixture");
    assert_eq!(direct.channel_1.len(), 100);
    assert_eq!(direct.channel_2.len(), 100);
    assert_eq!(&direct.channel_1[..4], &[1023, 0, 512, 1]);
    assert_eq!(&direct.channel_2[..4], &[0, 1023, 0, 1023]);
    assert_eq!(direct.frame_seq_u8, 255);
    assert_eq!(direct.sample_time_us(0), Some(1_700_000_000_123_456));
    for enabled in [false, true] {
        let result = LvdsBitAlignmentRecoverer::inspect(bytes, enabled, continuous_context());
        assert_eq!(result.quality, FrameQuality::Native);
        assert_eq!(
            result.crc_reference.status,
            ReferenceCrcStatus::NotEvaluated
        );
        assert_eq!(result.frame.as_ref(), Some(&direct));
        assert_eq!(result.bytes, bytes);
        assert_bounded_result(&result, bytes.len());
    }
}

fn assert_recovery_fixture(bytes: &[u8], expected: FrameQuality, available_bits: usize) {
    let result = LvdsBitAlignmentRecoverer::inspect(bytes, true, continuous_context());
    assert_eq!(result.quality, expected, "{}", result.reason);
    assert_eq!(result.available_bits, available_bits);
    assert_bounded_result(&result, bytes.len());
    assert_eq!(result.frame.as_ref().unwrap().channel_1[0], 1023);
    assert_eq!(result.frame.as_ref().unwrap().channel_2[99], 1023);
}

fn assert_adversarial_fixture(bytes: &[u8]) {
    let result = LvdsBitAlignmentRecoverer::inspect(bytes, true, continuous_context());
    assert_eq!(result.quality, FrameQuality::Rejected, "{}", result.reason);
    assert!(result.frame.is_none());
    assert_eq!(result.bytes, [0_u8; 0]);
    assert_bounded_result(&result, bytes.len());
}

fn measurements() -> [Measurement; 9] {
    let (fast_calls, search_calls, flood_calls) = if cfg!(debug_assertions) {
        (2_000, 100, 10)
    } else {
        (50_000, 2_000, 250)
    };
    [
        Measurement::new("direct_native_269B", fast_calls),
        Measurement::new("inspect_native_recovery_off_269B", fast_calls),
        Measurement::new("inspect_native_recovery_on_269B", fast_calls),
        Measurement::new("inspect_confirmed_1bit_270B", search_calls),
        Measurement::new("inspect_confirmed_4bit_270B", search_calls),
        Measurement::new("inspect_incomplete_4bit_269B", search_calls),
        Measurement::new("inspect_zero_4096B", search_calls),
        Measurement::new("inspect_header_flood_4096B", flood_calls),
        Measurement::new("inspect_oversized_65535B", fast_calls),
    ]
}

fn warm_up(native: &[u8], recovery_inputs: [&[u8]; 6]) {
    for _ in 0..WARMUPS {
        black_box(decode_scientific_frame(black_box(native))).unwrap();
        for enabled in [false, true] {
            black_box(LvdsBitAlignmentRecoverer::inspect(
                black_box(native),
                enabled,
                continuous_context(),
            ));
        }
        for bytes in recovery_inputs {
            black_box(LvdsBitAlignmentRecoverer::inspect(
                black_box(bytes),
                true,
                continuous_context(),
            ));
        }
    }
}

fn run_batches(native: &[u8], inputs: [&[u8]; 6], measurements: &mut [Measurement; 9]) {
    for batch in 0..BATCHES {
        // Rotate native comparisons to reduce systematic ordering bias.
        for index in 0..3 {
            let path = (index + batch) % 3;
            match path {
                0 => measurements[path]
                    .measure_batch(batch, || decode_scientific_frame(black_box(native))),
                1 | 2 => measurements[path].measure_batch(batch, || {
                    LvdsBitAlignmentRecoverer::inspect(
                        black_box(native),
                        path == 2,
                        continuous_context(),
                    )
                }),
                _ => unreachable!(),
            }
        }
        for (index, bytes) in inputs.into_iter().enumerate() {
            measurements[index + 3].measure_batch(batch, || {
                LvdsBitAlignmentRecoverer::inspect(black_box(bytes), true, continuous_context())
            });
        }
    }
}

/// Catches loss of the native fast path's data semantics, accidental promotion
/// of dense false headers to science, and accepting oversized payloads. Timing
/// has no hardware-dependent pass threshold; this is an explicit QA command.
#[test]
#[ignore = "SYNTHETIC local CPU benchmark; run explicitly, preferably --release"]
fn synthetic_production_recovery_cpu_measurements() {
    let native = valid_frame();
    let shifted_1 = prefix_bits(native.as_ref(), 1);
    let shifted_4 = prefix_bits(native.as_ref(), 4);
    let incomplete_4 = &shifted_4[..LVDS_FRAME_LEN];
    let zeros = vec![0_u8; MAX_RECOVERY_PAYLOAD_BYTES];
    let repeating_headers: Vec<_> = FIXED_HEADER
        .into_iter()
        .cycle()
        .take(MAX_RECOVERY_PAYLOAD_BYTES)
        .collect();
    let mut header_flood = prefix_bits(&repeating_headers, 4);
    header_flood.truncate(MAX_RECOVERY_PAYLOAD_BYTES);
    // A valid frame at the start would decode if the input bound were skipped.
    let mut oversized = vec![0_u8; 65_535];
    oversized[..LVDS_FRAME_LEN].copy_from_slice(native.as_ref());

    assert_eq!(native.len(), 269);
    assert_eq!(zeros.len(), 4096);
    assert_eq!(header_flood.len(), 4096);
    assert_eq!(oversized.len(), 65_535);
    assert_native_fixture(native.as_ref());
    assert_recovery_fixture(&shifted_1, FrameQuality::Recovered1Bit, 2152);
    assert_recovery_fixture(&shifted_4, FrameQuality::Recovered4Bit, 2152);
    assert_recovery_fixture(incomplete_4, FrameQuality::RecoveryCandidate, 2148);
    for bytes in [&zeros, &header_flood, &oversized] {
        assert_adversarial_fixture(bytes);
    }
    let rejected = LvdsBitAlignmentRecoverer::inspect(&oversized, true, continuous_context());
    assert_eq!(rejected.candidate_count, 0);
    assert_eq!(rejected.available_bits, 0);

    let mut measurements = measurements();
    let recovery_inputs: [&[u8]; 6] = [
        &shifted_1,
        &shifted_4,
        incomplete_4,
        &zeros,
        &header_flood,
        &oversized,
    ];
    warm_up(native.as_ref(), recovery_inputs);

    println!(
        "SYNTHETIC production Rust CPU benchmark; profile={}, arch={}, os={}, warmups={WARMUPS}, batches={BATCHES}; elapsed includes result destruction; no device, UDP, storage, GUI, RSS or sustained-rate acceptance",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::env::consts::ARCH,
        std::env::consts::OS,
    );
    run_batches(native.as_ref(), recovery_inputs, &mut measurements);
    // Recheck classification after timing; timing thresholds are intentionally
    // not used to turn a faster machine into a different correctness result.
    assert_native_fixture(native.as_ref());
    assert_recovery_fixture(&shifted_1, FrameQuality::Recovered1Bit, 2152);
    assert_recovery_fixture(&shifted_4, FrameQuality::Recovered4Bit, 2152);
    assert_recovery_fixture(incomplete_4, FrameQuality::RecoveryCandidate, 2148);
    for bytes in [&zeros, &header_flood, &oversized] {
        assert_adversarial_fixture(bytes);
    }
    for measurement in &measurements {
        measurement.print();
    }
    println!(
        "SYNTHETIC_RUST_NATIVE_RATIOS {{\"inspect_off_over_direct\":{:.6},\"inspect_on_over_direct\":{:.6}}}",
        measurements[1].median() / measurements[0].median(),
        measurements[2].median() / measurements[0].median(),
    );
    println!(
        "SYNTHETIC boundary assertions passed: max input searched=4096B, max output=269B/2152 bits, max retained candidate count=2; 65535B input rejected with zero candidates/output. These are public result/source bounds, not instrumented search-step or allocator measurements."
    );
}
