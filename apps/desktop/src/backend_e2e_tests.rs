use std::{
    fs,
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, bounded};
use langmuir_core::ScientificFrame;
use langmuir_storage::{
    BINARY_FRAME_LEN, BINARY_HEADER_LEN, BatchSink, BatchWriter, BinaryConfig, BinarySink,
    StorageServiceConfig,
};
use tempfile::tempdir;

use super::{Controller, UdpLvdsIngress};
use crate::bridge::{BackendEvent, BackendStateSnapshot, LinkKind, LvdsDiagnosticStats};

const SOURCE: &str = "192.0.2.1:1534";

fn controller() -> (Controller, Receiver<BackendEvent>, Receiver<BackendEvent>) {
    controller_with_display_capacity(8)
}

fn controller_with_display_capacity(
    capacity: usize,
) -> (Controller, Receiver<BackendEvent>, Receiver<BackendEvent>) {
    let (critical_tx, critical_rx) = bounded(64);
    let (display_tx, display_rx) = bounded(capacity);
    let controller = Controller::new(
        critical_tx,
        critical_rx.clone(),
        display_tx,
        Arc::default(),
        Arc::new(Mutex::new(BackendStateSnapshot::default())),
    );
    (controller, critical_rx, display_rx)
}

fn golden() -> Vec<u8> {
    let mut wire = vec![0_u8; 269];
    wire[..19].copy_from_slice(&[
        0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA, 0x65, 0, 0, 0, 0, 0x0F, 0x41, 0x78, 0x12, 0x34,
        0xAB, 0xCD,
    ]);
    for pair in wire[19..267].as_chunks_mut::<2>().0 {
        pair.copy_from_slice(&[1, 0xFF]);
    }
    // Fixed result from the manufacturer's independent C reference vector.
    wire[267..269].copy_from_slice(&[0x44, 0xC8]);
    wire
}

fn next_golden() -> Vec<u8> {
    let mut wire = golden();
    wire[7..11].copy_from_slice(&1_694_498_817_u32.to_be_bytes());
    wire[11..15].copy_from_slice(&49_800_u32.to_be_bytes());
    wire[267..269].copy_from_slice(&[0x5E, 0xF9]);
    wire
}

fn datagram(sequence: u32, wire: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(24 + wire.len());
    bytes.extend_from_slice(&[0x55, 0xAA, 1, 0xA5]);
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(&u32::try_from(wire.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 1]);
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&u16::try_from(wire.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(wire);
    bytes
}

fn feed(
    ingress: &mut UdpLvdsIngress,
    controller: &mut Controller,
    sequence: u32,
    wire: &[u8],
    now: Instant,
) {
    ingress.push_at(
        SOURCE.parse().unwrap(),
        &datagram(sequence, wire),
        now,
        |frame| {
            controller.handle_scientific_frame(frame);
        },
    );
    if let Some(preview) = ingress.preview.take() {
        // Same isolated display route as the production receiver. Diagnostic
        // events never call the normal handler or enter the scientific writer.
        controller.emit_display(LinkKind::Lvds, preview);
    }
}

fn start_file(controller: &mut Controller, path: &Path) {
    let sink = BinarySink::create(BinaryConfig::new(path)).unwrap();
    controller.scientific_writer = Some(
        BatchWriter::start(
            sink,
            StorageServiceConfig {
                batch_size: 2,
                ..StorageServiceConfig::default()
            },
        )
        .unwrap(),
    );
    controller.capture_path = Some(path.to_path_buf());
    controller.acquisition_running = true;
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[test]
fn e1_e5_e6_real_file_round_trip_keeps_only_strict_accepted_science() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("strict-science.lpd");
    let (mut controller, _critical, display) = controller();
    start_file(&mut controller, &path);
    let mut ingress = UdpLvdsIngress::new(SOURCE.parse().unwrap());
    assert!(ingress.diagnostic_preview);
    let first = golden();
    let second = next_golden();
    let mut bad_crc = first.clone();
    bad_crc[19] ^= 1; // Bounded ADC change, original received CRC retained.
    let mut bad_header = first.clone();
    bad_header[0] ^= 1;
    let mut bad_time = first.clone();
    bad_time[11..15].copy_from_slice(&1_000_000_u32.to_be_bytes());
    let mut bad_adc = first.clone();
    bad_adc[15..17].copy_from_slice(&0_u16.to_be_bytes());
    bad_adc[19] = 0x80;
    let now = Instant::now();
    for (sequence, wire) in [
        (100, &first),
        (101, &bad_crc),
        (102, &bad_header),
        (103, &bad_time),
        (100, &first),
        (100, &second),
        (104, &bad_adc),
        (105, &second),
    ] {
        feed(&mut ingress, &mut controller, sequence, wire, now);
    }
    let stats = ingress.diagnostic_stats;
    assert_eq!(stats.complete_frames, 8);
    assert_eq!((stats.crc_passed, stats.crc_failed), (5, 3));
    assert_eq!(
        (
            stats.header_errors,
            stats.timestamp_errors,
            stats.adc_errors
        ),
        (1, 1, 1)
    );
    assert_eq!(
        (stats.accepted_frames, stats.duplicates, stats.conflicts),
        (2, 1, 1)
    );
    assert_eq!(stats.diagnostic_candidates, 1);
    assert_eq!(ingress.metrics.checksum_failures, 3);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 2);
    assert_eq!(ingress.metrics.scientific_frames_delivered, 2);

    let events: Vec<_> = display.try_iter().collect();
    assert_eq!(events.len(), 3);
    assert!(matches!(&events[0], BackendEvent::ScientificSamples { .. }));
    let BackendEvent::DiagnosticSamples {
        sequence,
        channel_1,
        ..
    } = &events[1]
    else {
        panic!("CRC failure must use the separate unverified event");
    };
    assert_eq!(*sequence, Some(101));
    assert_eq!(
        (channel_1[0], channel_1[1], channel_1[124]),
        (4660.0, 4660.0, 4783.0)
    );
    assert!(matches!(&events[2], BackendEvent::ScientificSamples { .. }));

    controller.stop_acquisition();
    assert!(!controller.acquisition_running);
    assert!(controller.scientific_writer.is_none());
    assert_golden_file(&path);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

fn assert_golden_file(path: &Path) {
    let bytes = fs::read(path).unwrap();
    // Independent fixed v2 header, rather than a header rebuilt by the sink.
    assert_eq!(
        &bytes[..32],
        &[
            b'L', b'M', b'P', b'R', b'O', b'B', b'E', 0, 2, 0, 32, 0, 4, 3, 2, 1, 125, 0, 0, 0,
            144, 1, 0, 0, 242, 3, 0, 0, 1, 0, 0, 0,
        ]
    );
    assert_eq!(
        bytes.len(),
        usize::from(BINARY_HEADER_LEN) + 2 * BINARY_FRAME_LEN as usize
    );
    for (record, (sequence, utc, micros)) in bytes[32..].as_chunks::<1010>().0.iter().zip([
        (100_u16, 1_694_498_816_u32, 999_800_u32),
        (105, 1_694_498_817, 49_800),
    ]) {
        assert_eq!(read_u32(record, 0), utc);
        assert_eq!(read_u32(record, 4), micros);
        assert_eq!(
            u16::from_le_bytes(record[8..10].try_into().unwrap()),
            sequence
        );
        for index in 0..125 {
            let step = u32::try_from(index).unwrap();
            assert_eq!(read_u32(record, 10 + index * 4), 4660 + step);
            assert_eq!(read_u32(record, 510 + index * 4), 43_981 - step);
        }
    }
}

#[test]
fn e2_strict_crc_failure_never_creates_normal_or_diagnostic_waveform_or_file_record() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("no-bad-science.lpd");
    let (mut controller, _critical, display) = controller();
    start_file(&mut controller, &path);
    let mut ingress = UdpLvdsIngress::new(SOURCE.parse().unwrap());
    ingress.diagnostic_preview = false;
    let mut wire = golden();
    wire[19] ^= 1;
    feed(&mut ingress, &mut controller, 1, &wire, Instant::now());
    assert!(!ingress.diagnostic_preview);
    assert!(display.is_empty());
    assert_eq!(ingress.diagnostic_stats.crc_failed, 1);
    assert_eq!(ingress.diagnostic_stats.accepted_frames, 0);
    // Candidate quality is measured independently of the display filter.
    assert_eq!(ingress.diagnostic_stats.diagnostic_candidates, 1);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 0);
    assert_eq!(ingress.latest.unwrap().raw, wire);
    controller.stop_acquisition();
    assert_eq!(fs::read(path).unwrap().len(), 32);
}

#[test]
fn t3_legal_frames_plot_and_save_with_both_crc_filter_settings() {
    let directory = tempdir().unwrap();
    for allow_unverified in [true, false] {
        let path = directory
            .path()
            .join(format!("legal-{allow_unverified}.lpd"));
        let (mut controller, _critical, display) = controller();
        start_file(&mut controller, &path);
        let mut ingress = UdpLvdsIngress::new(SOURCE.parse().unwrap());
        ingress.diagnostic_preview = allow_unverified;
        feed(&mut ingress, &mut controller, 1, &golden(), Instant::now());
        let BackendEvent::ScientificSamples {
            channel_1,
            channel_2,
            ..
        } = display.try_recv().unwrap()
        else {
            panic!("valid CRC must plot regardless of the filter");
        };
        assert_eq!((channel_1.len(), channel_2.len()), (125, 125));
        assert_eq!(
            (
                ingress.diagnostic_stats.crc_passed,
                ingress.diagnostic_stats.crc_failed
            ),
            (1, 0)
        );
        controller.stop_acquisition();
        assert_eq!(controller.storage_status.enqueued, 1);
        assert_eq!(controller.storage_status.sink_written, 1);
        assert_eq!(fs::read(path).unwrap().len(), 32 + 1010);
    }
}

#[test]
fn e3_diagnostic_preview_requires_structure_time_and_all_adc_to_be_valid() {
    let mut ingress = UdpLvdsIngress::new(SOURCE.parse().unwrap());
    ingress.diagnostic_preview = true;
    let (mut controller, _critical, display) = controller();
    let now = Instant::now();
    for (index, offset) in [0_usize, 2, 3, 4, 6].into_iter().enumerate() {
        let mut wire = golden();
        wire[offset] ^= 1;
        wire[268] ^= 1;
        feed(
            &mut ingress,
            &mut controller,
            u32::try_from(index).unwrap(),
            &wire,
            now,
        );
        assert!(display.is_empty(), "malformed fixed field at {offset}");
        assert_eq!(ingress.latest.as_ref().unwrap().raw, wire);
    }
    let mut bad_time = golden();
    bad_time[11..15].copy_from_slice(&u32::MAX.to_be_bytes());
    feed(&mut ingress, &mut controller, 10, &bad_time, now);
    assert!(display.is_empty());
    for channel in [0_usize, 1] {
        let mut bad_adc = golden();
        bad_adc[19..267].fill(0);
        bad_adc[15 + channel * 2..17 + channel * 2].copy_from_slice(&u16::MAX.to_be_bytes());
        bad_adc[19 + 123 * 2 + channel] = 1;
        feed(
            &mut ingress,
            &mut controller,
            20 + u32::try_from(channel).unwrap(),
            &bad_adc,
            now,
        );
        assert!(
            display.is_empty(),
            "last sample overflow on channel {channel}"
        );
    }
    assert_eq!(ingress.diagnostic_stats.header_errors, 1);
    assert_eq!(ingress.diagnostic_stats.length_errors, 1);
    assert_eq!(ingress.diagnostic_stats.field_errors, 3);
    assert_eq!(ingress.diagnostic_stats.timestamp_errors, 1);
    assert_eq!(ingress.diagnostic_stats.adc_errors, 2);
    assert_eq!(ingress.diagnostic_stats.diagnostic_candidates, 0);
    assert_eq!(ingress.metrics.scientific_frames_delivered, 0);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 0);
}

fn refresh_crc_reference(wire: &mut [u8]) {
    let value = crc_reference(&wire[7..267]);
    wire[267..269].copy_from_slice(&value.to_be_bytes());
}

fn crc_reference(bytes: &[u8]) -> u16 {
    // Deliberately independent bitwise reference; no production CRC call.
    let mut value = 0xFFFF_u16;
    for byte in bytes {
        value ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            value = if value & 0x8000 == 0 {
                value << 1
            } else {
                (value << 1) ^ 0x1021
            };
        }
    }
    value
}

#[test]
fn e4_four_hundred_live_frames_keep_all_points_and_device_time_without_saving() {
    let (mut controller, _critical, display) = controller();
    let mut ingress = UdpLvdsIngress::new(SOURCE.parse().unwrap());
    let now = Instant::now();
    for sequence in 0..400_u32 {
        let micros = 999_800 + sequence * 50_000;
        let utc = 1_694_498_816 + micros / 1_000_000;
        let offset = micros % 1_000_000;
        let mut wire = golden();
        wire[7..11].copy_from_slice(&utc.to_be_bytes());
        wire[11..15].copy_from_slice(&offset.to_be_bytes());
        let first_1 = 1000 + u16::try_from(sequence).unwrap();
        let first_2 = 20_000 + u16::try_from(sequence).unwrap();
        wire[15..17].copy_from_slice(&first_1.to_be_bytes());
        wire[17..19].copy_from_slice(&first_2.to_be_bytes());
        refresh_crc_reference(&mut wire);
        feed(
            &mut ingress,
            &mut controller,
            sequence,
            &wire,
            now + Duration::from_millis(u64::from(sequence) * 50),
        );
        let BackendEvent::ScientificSamples {
            channel_1_start_seconds,
            channel_2_start_seconds,
            sample_interval_seconds,
            channel_1,
            channel_2,
        } = display.try_recv().unwrap()
        else {
            panic!("normal live frame must produce a scientific event");
        };
        let expected_start = f64::from(utc) + f64::from(offset) / 1_000_000.0;
        assert_eq!(channel_1_start_seconds, expected_start);
        assert_eq!(channel_2_start_seconds, expected_start);
        assert_eq!(sample_interval_seconds, 0.0004);
        assert_eq!((channel_1.len(), channel_2.len()), (125, 125));
        for index in 0..125 {
            let step = f64::from(u32::try_from(index).unwrap());
            assert_eq!(channel_1[index], f64::from(first_1) + step);
            assert_eq!(channel_2[index], f64::from(first_2) - step);
            let sample_time = channel_1_start_seconds + step * sample_interval_seconds;
            let total_micros =
                999_800 + u64::from(sequence) * 50_000 + u64::try_from(index).unwrap() * 400;
            let sample_utc = 1_694_498_816 + u32::try_from(total_micros / 1_000_000).unwrap();
            let sample_micros = u32::try_from(total_micros % 1_000_000).unwrap();
            let expected_time = f64::from(sample_utc) + f64::from(sample_micros) / 1_000_000.0;
            assert!((sample_time - expected_time).abs() < 1e-6);
        }
        assert!(display.is_empty());
        assert!(controller.scientific_writer.is_none());
    }
    assert_eq!(ingress.diagnostic_stats.accepted_frames, 400);
    assert_eq!(ingress.diagnostic_stats.crc_passed, 400);
    assert_eq!(ingress.diagnostic_stats.crc_failed, 0);
    assert_eq!(ingress.diagnostic_stats.diagnostic_candidates, 0);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 400);
    assert_eq!(ingress.metrics.udp_sequence.contiguous_frames, 399);
    assert_eq!(ingress.metrics.udp_sequence.sequence_anomalies, 0);
    assert_eq!(ingress.metrics.udp_sequence.gap_candidates, 0);
    assert!(!controller.acquisition_running);
}

struct RecordingSink(Arc<Mutex<Vec<ScientificFrame>>>);

impl BatchSink<ScientificFrame> for RecordingSink {
    type Error = std::io::Error;

    fn write_batch(&mut self, frames: &[ScientificFrame]) -> Result<(), Self::Error> {
        self.0.lock().unwrap().extend_from_slice(frames);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[test]
fn e8_diagnostics_leave_ttl_recovery_dedup_and_full_sequence_monitoring_unchanged() {
    let (mut controller, _critical, display) = controller();
    let stored = Arc::new(Mutex::new(Vec::new()));
    controller.scientific_writer = Some(
        BatchWriter::start(
            RecordingSink(Arc::clone(&stored)),
            StorageServiceConfig::default(),
        )
        .unwrap(),
    );
    controller.acquisition_running = true;
    let source: SocketAddr = SOURCE.parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    ingress.diagnostic_preview = true;
    let now = Instant::now();
    let first = golden();
    for sequence in 1000..1064 {
        let mut partial = datagram(sequence, &first[..140]);
        partial[8..12].copy_from_slice(&269_u32.to_be_bytes());
        partial[14..16].copy_from_slice(&2_u16.to_be_bytes());
        ingress.push_at(source, &partial, now, |_| {
            panic!("partial frame cannot deliver")
        });
    }
    feed(&mut ingress, &mut controller, 1100, &first, now);
    assert_eq!(ingress.metrics.reassembly_capacity_rejections, 1);
    assert_eq!(ingress.metrics.reassembly_pending_frames, 64);
    let recovered = now + Duration::from_secs(2);
    let second = next_golden();
    let mut bad_crc = second.clone();
    bad_crc[268] ^= 1;
    for (sequence, wire) in [
        (u32::MAX, &first),
        (0, &first),
        (0, &first),
        (0, &second),
        (2, &first),
        (1, &bad_crc),
        (1, &second),
    ] {
        feed(&mut ingress, &mut controller, sequence, wire, recovered);
    }
    assert_eq!(ingress.metrics.reassembly_timeouts, 64);
    assert_eq!(ingress.metrics.reassembly_pending_frames, 0);
    assert_eq!(ingress.metrics.duplicate_frames_suppressed, 1);
    assert_eq!(ingress.metrics.sequence_conflicts, 1);
    assert_eq!(ingress.metrics.checksum_failures, 1);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 4);
    assert_eq!(ingress.metrics.udp_sequence.contiguous_frames, 1);
    assert_eq!(ingress.metrics.udp_sequence.wrap_events, 1);
    assert_eq!(ingress.metrics.udp_sequence.forward_jumps, 1);
    assert_eq!(ingress.metrics.udp_sequence.late_fills, 1);
    assert_eq!(ingress.metrics.udp_sequence.pending_gaps, 0);
    assert_eq!(ingress.metrics.udp_sequence.sequence_anomalies, 0);
    assert_eq!(ingress.diagnostic_stats.diagnostic_candidates, 1);
    controller.stop_acquisition();
    let frames = stored.lock().unwrap();
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame.sequence)
            .collect::<Vec<_>>(),
        [u16::MAX, 0, 2, 1]
    );
    assert_eq!(
        display
            .try_iter()
            .filter(|event| matches!(event, BackendEvent::ScientificSamples { .. }))
            .count(),
        4
    );
    // This memory sink establishes queue/sink isolation only. Disk read-back
    // and graceful close are independently covered by the real-file test.
}

#[test]
fn e10_udp_receive_state_uses_data_and_exact_timeout_instead_of_bind_success() {
    let source: SocketAddr = SOURCE.parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    let now = Instant::now();
    assert_eq!(ingress.receive_state(now), "已绑定，等待数据");
    ingress.push_at(
        "192.0.2.2:1534".parse().unwrap(),
        &datagram(1, &golden()),
        now,
        |_| {
            panic!("unconfigured source must not deliver");
        },
    );
    assert_eq!(ingress.receive_state(now), "已绑定，等待数据");
    assert_eq!(ingress.diagnostic_stats.udp_packets, 0);
    ingress.push_at(source, &[0x55, 0xAA], now, |_| {
        panic!("short UDP cannot deliver")
    });
    assert_eq!(
        ingress.receive_state(now),
        "正在收到 UDP 数据 / 等待完整 LVDS"
    );
    let mut bad = golden();
    bad[268] ^= 1;
    ingress.push_at(source, &datagram(1, &bad), now, |_| {
        panic!("CRC failure cannot deliver")
    });
    assert_eq!(ingress.receive_state(now), "收到 LVDS，校验失败");
    let mut delivered = 0;
    ingress.push_at(source, &datagram(1, &golden()), now, |_| delivered += 1);
    assert_eq!(delivered, 1);
    assert_eq!(ingress.receive_state(now), "正在收到有效科学帧");
    let deadline = now + Duration::from_secs(2);
    assert_eq!(
        ingress.receive_state(deadline.checked_sub(Duration::from_nanos(1)).unwrap()),
        "正在收到有效科学帧"
    );
    assert_eq!(ingress.receive_state(deadline), "数据流超时 / 中断");
    ingress.push_at(source, &[0x55, 0xAA], deadline, |_| {
        panic!("short UDP cannot deliver")
    });
    assert_eq!(
        ingress.receive_state(deadline),
        "正在收到 UDP 数据 / 等待完整 LVDS"
    );
    ingress.push_at(source, &datagram(2, &golden()), deadline, |_| {
        delivered += 1
    });
    assert_eq!(delivered, 2);
    assert_eq!(ingress.receive_state(deadline), "正在收到有效科学帧");
}

#[test]
fn e10_saving_allows_plot_filter_changes_but_blocks_source_changes_and_restarts_permissive() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("mode-boundary.lpd");
    let (mut controller, critical, _display) = controller();
    assert!(controller.lvds_status().diagnostic_preview);
    controller.set_diagnostic_preview(false);
    assert!(!controller.lvds_status().diagnostic_preview);
    start_file(&mut controller, &path);
    let source_before = controller.lvds_source.clone();
    controller.set_diagnostic_preview(true);
    controller.start_pcap_replay("does-not-exist.pcapng");
    assert!(controller.lvds_status().diagnostic_preview);
    assert!(controller.replay.is_none());
    assert_eq!(controller.lvds_source, source_before);
    assert_eq!(controller.capture_path.as_deref(), Some(path.as_path()));
    assert!(controller.acquisition_running);
    assert!(controller.scientific_writer.is_some());
    let faults: Vec<_> = critical
        .try_iter()
        .filter_map(|event| match event {
            BackendEvent::Fault { source, message } => Some((source, message)),
            _ => None,
        })
        .collect();
    assert!(faults.iter().all(|(source, _)| source != "CRC16"));
    assert!(
        faults
            .iter()
            .any(|(source, message)| source == "PCAP" && message.contains("停止正式保存"))
    );
    controller.stop_acquisition();
    assert!(controller.storage_status.closed);
    assert_eq!(controller.storage_status.sink_written, 0);
    assert_eq!(fs::read(path).unwrap().len(), 32);
    controller.set_diagnostic_preview(true);
    assert!(controller.lvds_status().diagnostic_preview);
    let (fresh, _critical, _display) = self::controller();
    assert!(fresh.lvds_status().diagnostic_preview);
}

struct CaptureResult {
    total: u64,
    target: u64,
    stats: LvdsDiagnosticStats,
    bytes: Vec<u8>,
    verified_events: u64,
    unverified_events: u64,
    points: [u64; 2],
}

struct FailingScienceSink;

impl BatchSink<ScientificFrame> for FailingScienceSink {
    type Error = std::io::Error;

    fn write_batch(&mut self, _: &[ScientificFrame]) -> Result<(), Self::Error> {
        Err(std::io::Error::other("injected science write failure"))
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[test]
fn storage_failure_is_reported_without_rolling_back_science_acceptance() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("failed.lpd");
    let (mut controller, critical, display) = controller();
    controller.scientific_writer =
        Some(BatchWriter::start(FailingScienceSink, StorageServiceConfig::default()).unwrap());
    controller.capture_path = Some(path);
    controller.acquisition_running = true;
    let mut ingress = UdpLvdsIngress::new(SOURCE.parse().unwrap());
    feed(
        &mut ingress,
        &mut controller,
        65537,
        &golden(),
        Instant::now(),
    );
    let before = ingress.metrics;
    assert!(matches!(
        display.try_recv().unwrap(),
        BackendEvent::ScientificSamples { .. }
    ));
    assert_eq!(controller.storage_status.enqueued, 1);
    assert!(
        controller
            .scientific_writer
            .as_ref()
            .unwrap()
            .flush()
            .is_err()
    );
    controller.stop_acquisition();
    assert!(!controller.acquisition_running);
    assert!(!controller.storage_status.closed);
    assert!(
        controller
            .storage_status
            .error
            .as_deref()
            .unwrap()
            .contains("injected science write failure")
    );
    assert_eq!(controller.storage_status.sink_written, 0);
    assert_eq!(ingress.metrics, before);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 1);
    assert_eq!(ingress.metrics.dedup_cached_frames, 1);
    assert!(directory.path().join("failed.lpd.incomplete").is_file());
    assert!(
        critical
            .try_iter()
            .any(|event| matches!(event, BackendEvent::Fault { .. }))
    );
}

fn capture_to_scientific_file(capture: &Path, path: &Path, diagnostic: bool) -> CaptureResult {
    // One production replay tick consumes at most 64 packets. A bounded 128
    // event display channel lets this test inspect every event without
    // confusing rendering backpressure with protocol rejection.
    let (mut controller, _critical, display) = controller_with_display_capacity(128);
    assert!(controller.lvds_status().diagnostic_preview);
    if !diagnostic {
        controller.set_diagnostic_preview(false);
    }
    controller.start_pcap_replay(capture.to_str().unwrap());
    let replay = controller.replay.as_mut().expect("production PCAP replay");
    assert_eq!(replay.ingress.diagnostic_preview, diagnostic);
    // Advance only the replay clock, not device timestamps or packet order.
    // Pacing itself has separate 50 ms boundary tests.
    replay.started = Instant::now()
        .checked_sub(Duration::from_hours(24))
        .unwrap();
    start_file(&mut controller, path);
    let mut normal_events = 0_u64;
    let mut diagnostic_events = 0_u64;
    let mut points = [0_u64; 2];
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        controller.drain_replay();
        assert!(display.len() <= 64, "bounded per-tick delivery batch");
        for event in display.try_iter() {
            let (channel_1, channel_2, interval) = match event {
                BackendEvent::ScientificSamples {
                    channel_1,
                    channel_2,
                    sample_interval_seconds,
                    ..
                } => {
                    normal_events += 1;
                    (channel_1, channel_2, sample_interval_seconds)
                }
                BackendEvent::DiagnosticSamples {
                    channel_1,
                    channel_2,
                    sample_interval_seconds,
                    ..
                } => {
                    diagnostic_events += 1;
                    (channel_1, channel_2, sample_interval_seconds)
                }
                _ => panic!("unexpected display event during capture processing"),
            };
            assert_eq!(interval, 0.0004);
            assert_eq!((channel_1.len(), channel_2.len()), (125, 125));
            assert!(
                channel_1
                    .iter()
                    .chain(channel_2.iter())
                    .all(|v| v.is_finite())
            );
            points[0] += u64::try_from(channel_1.len()).unwrap();
            points[1] += u64::try_from(channel_2.len()).unwrap();
        }
        if controller.replay.as_ref().unwrap().progress.finished {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background reader did not finish"
        );
        std::thread::yield_now();
    }
    let replay = controller.replay.as_ref().unwrap();
    assert_eq!(replay.progress.error, None);
    let total = replay.progress.total_packets;
    let target = replay.progress.target_udp_packets;
    let stats = replay.ingress.diagnostic_stats;
    assert!(!controller.acquisition_running, "EOF closes actual writer");
    assert!(controller.storage_status.closed);
    assert_eq!(controller.storage_status.error, None);
    let bytes = fs::read(path).unwrap();
    assert_eq!((bytes.len() - 32) % 1010, 0);
    let file_frames = u64::try_from((bytes.len() - 32) / 1010).unwrap();
    assert_eq!(normal_events, stats.accepted_frames);
    assert_eq!(
        diagnostic_events,
        if diagnostic {
            stats.diagnostic_candidates
        } else {
            0
        }
    );
    assert_eq!(file_frames, stats.accepted_frames);
    assert_eq!(controller.storage_status.enqueued, stats.accepted_frames);
    assert_eq!(controller.storage_status.sink_written, file_frames);
    println!(
        "USER_CAPTURE filter={} total={} target={} stats={stats:?} verified_plot_events={} unverified_plot_events={} points={points:?} enqueued={} sink_written={} closed={} file_frames={} file_bytes={}",
        if diagnostic {
            "default_allow_unverified"
        } else {
            "strict_crc_only"
        },
        total,
        target,
        normal_events,
        diagnostic_events,
        controller.storage_status.enqueued,
        controller.storage_status.sink_written,
        controller.storage_status.closed,
        file_frames,
        bytes.len()
    );
    CaptureResult {
        total,
        target,
        stats,
        bytes,
        verified_events: normal_events,
        unverified_events: diagnostic_events,
        points,
    }
}

#[derive(Default, Debug)]
struct CaptureCensus {
    total: u64,
    target: u64,
    complete: u64,
    structure_valid: u64,
    crc_passed: u64,
    crc_failed: u64,
    decodable_bad_crc: u64,
}

fn independently_count_capture(capture: &Path) -> CaptureCensus {
    let source: SocketAddr = super::DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap();
    let mut reader = langmuir_transport::PcapNgReader::new(std::io::BufReader::new(
        fs::File::open(capture).unwrap(),
    ));
    let mut census = CaptureCensus::default();
    while let Some(packet) = reader.next_packet().unwrap() {
        census.total += 1;
        let Some(udp) = packet.udp.filter(|udp| udp.source == source) else {
            continue;
        };
        census.target += 1;
        let parsed = langmuir_core::parse_udp_lvds_datagram(&udp.payload).unwrap();
        // This particular external fixture contains complete single-datagram
        // frames. The application still uses its production reassembler.
        assert_eq!(
            (parsed.header.segment_count, parsed.header.byte_offset),
            (1, 0)
        );
        let wire = parsed.payload;
        assert_eq!(wire.len(), 269);
        census.complete += 1;
        let received = u16::from_be_bytes(wire[267..269].try_into().unwrap());
        let crc_ok = crc_reference(&wire[7..267]) == received;
        census.crc_passed += u64::from(crc_ok);
        census.crc_failed += u64::from(!crc_ok);
        // Independent fixed-byte, integer-time and differential-ADC oracle.
        // It neither calls diagnose_scientific_frame nor repairs wire bytes.
        let structure_ok = wire[..7] == [0xEB, 0x90, 0x55, 0xAA, 1, 7, 0xAA];
        census.structure_valid += u64::from(structure_ok);
        let time_ok = u32::from_be_bytes(wire[11..15].try_into().unwrap()) < 1_000_000;
        if !crc_ok && structure_ok && time_ok && reference_adc_valid(wire) {
            census.decodable_bad_crc += 1;
        }
    }
    census
}

fn reference_adc_valid(wire: &[u8]) -> bool {
    for channel in 0..2 {
        let first = 15 + channel * 2;
        let mut sample = i32::from(u16::from_be_bytes([wire[first], wire[first + 1]]));
        for index in 0..124 {
            sample += i32::from(i8::from_be_bytes([wire[19 + index * 2 + channel]]));
            if !(0..=65_535).contains(&sample) {
                return false;
            }
        }
    }
    true
}

#[test]
#[ignore = "requires user capture"]
fn e7_user_capture_strict_and_diagnostic_use_same_ingress_and_never_save_bad_crc() {
    let capture = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../2026101902--朗缪尔.pcapng"
    ));
    let original = fs::read(capture).expect("place the user capture in the project root");
    let expected = independently_count_capture(capture);
    println!("USER_CAPTURE independent census={expected:?}");
    let directory = tempdir().unwrap();
    let strict = capture_to_scientific_file(capture, &directory.path().join("strict.lpd"), false);
    let diagnostic =
        capture_to_scientific_file(capture, &directory.path().join("diagnostic.lpd"), true);
    assert!(strict.target > 0);
    assert!(strict.total >= strict.target);
    assert_eq!(
        (strict.total, strict.target),
        (diagnostic.total, diagnostic.target)
    );
    assert_eq!(strict.stats, diagnostic.stats);
    assert_eq!(
        (diagnostic.total, diagnostic.target),
        (expected.total, expected.target)
    );
    assert_eq!(diagnostic.stats.complete_frames, expected.complete);
    assert_eq!(
        diagnostic.stats.structure_valid_frames,
        expected.structure_valid
    );
    assert_eq!(diagnostic.stats.crc_passed, expected.crc_passed);
    assert_eq!(diagnostic.stats.crc_failed, expected.crc_failed);
    assert_eq!(
        diagnostic.stats.diagnostic_candidates,
        expected.decodable_bad_crc
    );
    assert_eq!(diagnostic.unverified_events, expected.decodable_bad_crc);
    assert_eq!(strict.unverified_events, 0);
    assert_eq!(strict.verified_events, diagnostic.verified_events);
    let default_points = (diagnostic.verified_events + expected.decodable_bad_crc) * 125;
    assert_eq!(diagnostic.points, [default_points; 2]);
    assert_eq!(strict.points, [strict.verified_events * 125; 2]);
    assert_eq!(strict.bytes, diagnostic.bytes);
    assert_eq!(fs::read(capture).unwrap(), original);
}
