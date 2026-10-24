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
    BatchSink, BatchWriter, BinaryConfig, BinaryV3Sink as BinarySink, StorageServiceConfig,
    V3_FRAME_LEN as BINARY_FRAME_LEN, V3_HEADER_LEN as BINARY_HEADER_LEN,
};
use tempfile::tempdir;

use super::{Controller, UdpLvdsIngress};
use crate::bridge::{BackendEvent, BackendStateSnapshot, LvdsReceiveStats};

#[path = "backend_scientific_csv_tests.rs"]
mod scientific_csv_tests;

const SOURCE: &str = "192.0.2.1:1534";

pub(super) fn controller() -> (Controller, Receiver<BackendEvent>, Receiver<BackendEvent>) {
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
    super::protocol_10bit_tests::golden_frame(7)
}

fn next_golden() -> Vec<u8> {
    let mut wire = golden();
    wire[7..11].copy_from_slice(&1_694_498_817_u32.to_be_bytes());
    wire[11..15].copy_from_slice(&49_800_u32.to_be_bytes());
    wire[265] = 8;
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

pub(super) fn feed(
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
fn e1_e5_e6_real_file_round_trip_keeps_only_decodable_deduplicated_science() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("strict-science.lpd");
    let (mut controller, _critical, display) = controller();
    start_file(&mut controller, &path);
    let mut ingress = UdpLvdsIngress::new(SOURCE.parse().unwrap());
    let first = golden();
    let second = next_golden();
    let mut bad_crc = first.clone();
    bad_crc[19] ^= 1; // Bounded ADC change, original received CRC retained.
    let mut bad_header = first.clone();
    bad_header[0] ^= 1;
    let mut bad_time = first.clone();
    bad_time[11..15].copy_from_slice(&1_000_000_u32.to_be_bytes());
    let mut bad_reserved = first.clone();
    bad_reserved[266] = 1;
    let now = Instant::now();
    for (sequence, wire) in [
        (100, &first),
        (101, &bad_crc),
        (102, &bad_header),
        (103, &bad_time),
        (100, &first),
        (100, &second),
        (104, &bad_reserved),
        (105, &second),
    ] {
        feed(&mut ingress, &mut controller, sequence, wire, now);
    }
    let stats = ingress.receive_stats;
    assert_eq!(stats.complete_frames, 8);
    assert_eq!((stats.parsed_frames, stats.parse_failures), (5, 3));
    assert_eq!(
        (
            stats.header_errors,
            stats.timestamp_errors,
            stats.field_errors
        ),
        (1, 1, 1)
    );
    assert_eq!(
        (stats.accepted_frames, stats.duplicates, stats.conflicts),
        (3, 1, 1)
    );
    assert_eq!(ingress.metrics.checksum_failures, 0);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 3);
    assert_eq!(ingress.metrics.scientific_frames_delivered, 3);

    let events: Vec<_> = display.try_iter().collect();
    assert_eq!(events.len(), 3);
    assert!(matches!(&events[0], BackendEvent::ScientificSamples { .. }));
    let BackendEvent::ScientificSamples { channel_1, .. } = &events[1] else {
        panic!("decodable data must use the ordinary scientific event");
    };
    assert_eq!(
        (channel_1[0], channel_1[1], channel_1[99]),
        (32704.0, -32768.0, -32704.0)
    );
    assert!(matches!(&events[2], BackendEvent::ScientificSamples { .. }));

    controller.stop_acquisition();
    assert!(!controller.acquisition_running);
    assert!(controller.scientific_writer.is_none());
    let expected = [&first, &bad_crc, &second]
        .map(|wire| langmuir_core::decode_scientific_frame(wire).unwrap());
    assert_scientific_file(&fs::read(&path).unwrap(), &expected);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

fn assert_scientific_file(bytes: &[u8], expected: &[ScientificFrame]) {
    // Independent v3 metadata and record offsets; v2 remains separately tested.
    assert_eq!(
        &bytes[..40],
        &[
            b'L', b'M', b'P', b'R', b'O', b'B', b'E', 0, 3, 0, 40, 0, 4, 3, 2, 1, 100, 0, 0, 0,
            244, 1, 0, 0, 153, 1, 0, 0, 1, 0, 0, 0, 3, 0, 0, 0, 2, 0, 8, 0,
        ]
    );
    assert_eq!(
        bytes.len(),
        usize::from(BINARY_HEADER_LEN) + expected.len() * BINARY_FRAME_LEN as usize
    );
    for (record, frame) in bytes[40..].as_chunks::<409>().0.iter().zip(expected) {
        assert_eq!(read_u32(record, 0), frame.timestamp.utc_seconds);
        assert_eq!(read_u32(record, 4), frame.timestamp.microseconds);
        assert_eq!(record[8], frame.frame_seq_u8);
        for index in 0..100 {
            assert_eq!(
                u16::from_le_bytes(record[9 + index * 2..11 + index * 2].try_into().unwrap()),
                frame.channel_1[index]
            );
            assert_eq!(
                u16::from_le_bytes(record[209 + index * 2..211 + index * 2].try_into().unwrap()),
                frame.channel_2[index]
            );
        }
    }
}

#[test]
fn e2_tail_and_decodable_science_changes_plot_and_save_without_crc_recalculation() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("no-bad-science.lpd");
    let (mut controller, _critical, display) = controller();
    start_file(&mut controller, &path);
    let mut ingress = UdpLvdsIngress::new(SOURCE.parse().unwrap());
    let original = golden();
    let mut tail_changed = original.clone();
    tail_changed[267..269].copy_from_slice(&[0, 0]);
    let mut science_changed = original.clone();
    science_changed[19] ^= 1;
    let wires = [original, tail_changed, science_changed];
    let mut expected = Vec::new();
    for (index, wire) in wires.iter().enumerate() {
        let sequence = u16::try_from(index + 1).unwrap();
        expected.push(langmuir_core::decode_scientific_frame(wire).unwrap());
        feed(
            &mut ingress,
            &mut controller,
            u32::from(sequence),
            wire,
            Instant::now(),
        );
    }
    assert_eq!(display.len(), 3);
    assert!(
        display
            .try_iter()
            .all(|event| matches!(event, BackendEvent::ScientificSamples { .. }))
    );
    assert_eq!(ingress.receive_stats.accepted_frames, 3);
    assert_eq!(ingress.receive_stats.parse_failures, 0);
    assert_eq!(ingress.metrics.checksum_failures, 0);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 3);
    controller.stop_acquisition();
    assert_eq!(controller.storage_status.sink_written, 3);
    assert_scientific_file(&fs::read(path).unwrap(), &expected);
}

#[test]
fn t3_legal_frame_plots_and_saves_through_the_single_science_route() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("legal.lpd");
    let (mut controller, _critical, display) = controller();
    start_file(&mut controller, &path);
    let mut ingress = UdpLvdsIngress::new(SOURCE.parse().unwrap());
    feed(&mut ingress, &mut controller, 1, &golden(), Instant::now());
    let BackendEvent::ScientificSamples {
        channel_1,
        channel_2,
        ..
    } = display.try_recv().unwrap()
    else {
        panic!("a legal scientific frame must plot");
    };
    assert_eq!((channel_1.len(), channel_2.len()), (100, 100));
    assert_eq!(
        (
            ingress.receive_stats.parsed_frames,
            ingress.receive_stats.parse_failures
        ),
        (1, 0)
    );
    controller.stop_acquisition();
    assert_eq!(controller.storage_status.enqueued, 1);
    assert_eq!(controller.storage_status.sink_written, 1);
    assert_eq!(fs::read(path).unwrap().len(), 40 + 409);
}

#[test]
fn e3_science_acceptance_requires_structure_time_and_reserved_byte() {
    let mut ingress = UdpLvdsIngress::new(SOURCE.parse().unwrap());
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
        assert!(ingress.last_parse_error.is_some());
    }
    let mut bad_time = golden();
    bad_time[11..15].copy_from_slice(&u32::MAX.to_be_bytes());
    feed(&mut ingress, &mut controller, 10, &bad_time, now);
    assert!(display.is_empty());
    for reserved in [1_u8, 255] {
        let mut invalid = golden();
        invalid[266] = reserved;
        feed(
            &mut ingress,
            &mut controller,
            20 + u32::from(reserved),
            &invalid,
            now,
        );
        assert!(display.is_empty(), "reserved byte must be zero");
    }
    assert_eq!(ingress.receive_stats.header_errors, 1);
    assert_eq!(ingress.receive_stats.length_errors, 1);
    assert_eq!(ingress.receive_stats.field_errors, 5);
    assert_eq!(ingress.receive_stats.timestamp_errors, 1);
    assert_eq!(ingress.receive_stats.adc_errors, 0);
    assert_eq!(ingress.receive_stats.parsed_frames, 0);
    assert_eq!(ingress.receive_stats.parse_failures, 8);
    assert_eq!(ingress.metrics.scientific_frames_delivered, 0);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 0);
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
        wire[265] = sequence.to_be_bytes()[3];
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
        assert_eq!(sample_interval_seconds, 0.0005);
        assert_eq!((channel_1.len(), channel_2.len()), (100, 100));
        for index in 0..100 {
            let step = f64::from(u32::try_from(index).unwrap());
            assert_eq!(
                channel_1[index],
                [32704.0, -32768.0, 0.0, -32704.0][index % 4]
            );
            assert_eq!(
                channel_2[index],
                [-32768.0, 32704.0, -32768.0, 32704.0][index % 4]
            );
            let sample_time = channel_1_start_seconds + step * sample_interval_seconds;
            let total_micros =
                999_800 + u64::from(sequence) * 50_000 + u64::try_from(index).unwrap() * 500;
            let sample_utc = 1_694_498_816 + u32::try_from(total_micros / 1_000_000).unwrap();
            let sample_micros = u32::try_from(total_micros % 1_000_000).unwrap();
            let expected_time = f64::from(sample_utc) + f64::from(sample_micros) / 1_000_000.0;
            assert!((sample_time - expected_time).abs() < 1e-6);
        }
        assert!(display.is_empty());
        assert!(controller.scientific_writer.is_none());
    }
    assert_eq!(ingress.receive_stats.accepted_frames, 400);
    assert_eq!(ingress.receive_stats.parsed_frames, 400);
    assert_eq!(ingress.receive_stats.parse_failures, 0);
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
fn e8_no_crc_gate_keeps_ttl_recovery_dedup_and_full_sequence_monitoring_unchanged() {
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
    assert_eq!(ingress.metrics.sequence_conflicts, 2);
    assert_eq!(ingress.metrics.checksum_failures, 0);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 4);
    assert_eq!(ingress.metrics.udp_sequence.contiguous_frames, 1);
    assert_eq!(ingress.metrics.udp_sequence.wrap_events, 1);
    assert_eq!(ingress.metrics.udp_sequence.forward_jumps, 1);
    assert_eq!(ingress.metrics.udp_sequence.late_fills, 1);
    assert_eq!(ingress.metrics.udp_sequence.pending_gaps, 0);
    assert_eq!(ingress.metrics.udp_sequence.sequence_anomalies, 0);
    assert_eq!(ingress.receive_stats.accepted_frames, 4);
    controller.stop_acquisition();
    let frames = stored.lock().unwrap();
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame.frame_seq_u8)
            .collect::<Vec<_>>(),
        [7, 7, 7, 8]
    );
    assert_eq!(
        display
            .try_iter()
            .filter(|event| matches!(event, BackendEvent::ScientificSamples { .. }))
            .count(),
        4
    );
    // This memory sink establishes queue/sink delivery only. Disk read-back
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
    assert_eq!(ingress.receive_stats.udp_packets, 0);
    ingress.push_at(source, &[0x55, 0xAA], now, |_| {
        panic!("short UDP cannot deliver")
    });
    assert_eq!(
        ingress.receive_state(now),
        "正在收到 UDP 数据 / 等待完整 LVDS"
    );
    let mut bad = golden();
    bad[0] ^= 1;
    ingress.push_at(source, &datagram(1, &bad), now, |_| {
        panic!("invalid structure cannot deliver")
    });
    assert_eq!(ingress.receive_state(now), "收到 LVDS，解析失败");
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
        delivered += 1;
    });
    assert_eq!(delivered, 2);
    assert_eq!(ingress.receive_state(deadline), "正在收到有效科学帧");
}

#[test]
fn e10_saving_blocks_source_changes_and_preserves_the_current_science_writer() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("mode-boundary.lpd");
    let (mut controller, critical, _display) = controller();
    start_file(&mut controller, &path);
    let source_before = controller.lvds_source.clone();
    controller.start_pcap_replay("does-not-exist.pcapng", None);
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
    assert_eq!(fs::read(path).unwrap().len(), 40);
    let (fresh, _critical, _display) = self::controller();
    assert!(!fresh.acquisition_running);
    assert!(fresh.scientific_writer.is_none());
}

struct CaptureResult {
    total: u64,
    target: u64,
    stats: LvdsReceiveStats,
    bytes: Vec<u8>,
    events: u64,
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
    let storage_error = controller.storage_status.error.as_deref().unwrap();
    assert!(
        storage_error.contains("injected science write failure"),
        "unexpected storage error: {storage_error}"
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

fn capture_to_scientific_file(
    capture: &Path,
    path: &Path,
    expected: &[ScientificFrame],
) -> CaptureResult {
    // One production replay tick consumes at most 64 packets. A bounded 128
    // event display channel lets this test inspect every event without
    // confusing rendering backpressure with protocol rejection.
    let (mut controller, _critical, display) = controller_with_display_capacity(128);
    controller.start_pcap_replay(capture.to_str().unwrap(), None);
    let replay = controller.replay.as_mut().expect("production PCAP replay");
    // Advance only the replay clock, not device timestamps or packet order.
    // Pacing itself has separate 50 ms boundary tests.
    replay.started = Instant::now()
        .checked_sub(Duration::from_hours(24))
        .unwrap();
    start_file(&mut controller, path);
    let mut normal_events = 0_u64;
    let mut points = [0_u64; 2];
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        controller.drain_replay();
        assert!(display.len() <= 64, "bounded per-tick delivery batch");
        for event in display.try_iter() {
            let BackendEvent::ScientificSamples {
                channel_1_start_seconds,
                channel_2_start_seconds,
                channel_1,
                channel_2,
                sample_interval_seconds,
            } = event
            else {
                panic!("unexpected display event during capture processing");
            };
            let frame = &expected[usize::try_from(normal_events).unwrap()];
            normal_events += 1;
            assert_eq!(channel_1_start_seconds, frame.channel_1_base_seconds());
            assert_eq!(channel_2_start_seconds, frame.channel_2_base_seconds());
            assert_eq!(sample_interval_seconds, 0.0005);
            assert_eq!((channel_1.len(), channel_2.len()), (100, 100));
            assert!(
                channel_1
                    .iter()
                    .chain(channel_2.iter())
                    .all(|v| v.is_finite())
            );
            for index in 0..100 {
                assert_eq!(
                    channel_1[index],
                    f64::from(i32::from(frame.channel_1[index]) * 64 - 32768)
                );
                assert_eq!(
                    channel_2[index],
                    f64::from(i32::from(frame.channel_2[index]) * 64 - 32768)
                );
            }
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
    let stats = replay.ingress.receive_stats;
    assert!(!controller.acquisition_running, "EOF closes actual writer");
    assert!(controller.storage_status.closed);
    assert_eq!(controller.storage_status.error, None);
    let bytes = fs::read(path).unwrap();
    assert_eq!((bytes.len() - 40) % 409, 0);
    let file_frames = u64::try_from((bytes.len() - 40) / 409).unwrap();
    assert_eq!(normal_events, stats.accepted_frames);
    assert_eq!(file_frames, stats.accepted_frames);
    assert_eq!(controller.storage_status.enqueued, stats.accepted_frames);
    assert_eq!(controller.storage_status.sink_written, file_frames);
    println!(
        "USER_CAPTURE crc_check=disabled total={} target={} stats={stats:?} plot_events={} points={points:?} enqueued={} sink_written={} closed={} file_frames={} file_bytes={}",
        total,
        target,
        normal_events,
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
        events: normal_events,
        points,
    }
}

#[derive(Default)]
struct CaptureCensus {
    total: u64,
    target: u64,
    complete: u64,
    structure_valid: u64,
    parsed: u64,
    frames: Vec<ScientificFrame>,
    duplicates: u64,
    conflicts: u64,
}

fn independently_count_capture(capture: &Path) -> CaptureCensus {
    let source: SocketAddr = super::DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap();
    let mut reader = langmuir_transport::PcapNgReader::new(std::io::BufReader::new(
        fs::File::open(capture).unwrap(),
    ));
    let mut census = CaptureCensus::default();
    let mut recent = std::collections::VecDeque::<(u32, Duration, Vec<u8>)>::new();
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
        // Independent fixed-byte, integer-time and 40-bit packed-code oracle.
        // The trailing two bytes never participate in validity.
        let structure_ok = wire[..7] == [0xEB, 0x90, 0x55, 0xAA, 1, 7, 0xAA] && wire[266] == 0;
        census.structure_valid += u64::from(structure_ok);
        let time_ok = u32::from_be_bytes(wire[11..15].try_into().unwrap()) < 1_000_000;
        let valid = structure_ok && time_ok;
        let sequence = parsed.header.frame_sequence;
        let decoded = langmuir_core::decode_scientific_frame(wire);
        assert_eq!(
            decoded.is_ok(),
            valid,
            "independent structure/time/ADC oracle"
        );
        let Ok(frame) = decoded else { continue };
        assert_eq!(frame, reference_frame(wire));
        census.parsed += 1;
        // Independently apply the documented five-second / 256-entry FIFO
        // acceptance policy to the capture's original monotonic timestamps.
        while recent.front().is_some_and(|(_, inserted, _)| {
            packet.timestamp.saturating_sub(*inserted) >= Duration::from_secs(5)
        }) {
            recent.pop_front();
        }
        if let Some((_, _, prior)) = recent.iter().find(|(stored, _, _)| *stored == sequence) {
            if prior == wire {
                census.duplicates += 1;
            } else {
                census.conflicts += 1;
            }
            continue;
        }
        if recent.len() == 256 {
            recent.pop_front();
        }
        recent.push_back((sequence, packet.timestamp, wire.to_vec()));
        census.frames.push(frame);
    }
    census
}

fn reference_frame(wire: &[u8]) -> ScientificFrame {
    let decode = |block: &[u8]| {
        let mut values = Box::new([0_u16; 100]);
        for (group_index, bytes) in block.as_chunks::<5>().0.iter().enumerate() {
            let packed = bytes
                .iter()
                .fold(0_u64, |acc, byte| acc * 256 + u64::from(*byte));
            for point in 0..4 {
                values[group_index * 4 + point] =
                    u16::try_from((packed >> ((3 - point) * 10)) & 1023).unwrap();
            }
        }
        values
    };
    ScientificFrame {
        timestamp: langmuir_core::Timestamp {
            utc_seconds: u32::from_be_bytes(wire[7..11].try_into().unwrap()),
            microseconds: u32::from_be_bytes(wire[11..15].try_into().unwrap()),
        },
        frame_seq_u8: wire[265],
        channel_1: decode(&wire[15..140]),
        channel_2: decode(&wire[140..265]),
    }
}

#[test]
#[ignore = "requires user capture"]
fn e7_user_capture_no_crc_science_roundtrip() {
    let capture_path = std::env::var_os("LANGMUIR_USER_PCAP").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../2026101902--朗缪尔.pcapng"),
        std::path::PathBuf::from,
    );
    let capture = capture_path.as_path();
    let original = fs::read(capture)
        .expect("place the user capture in the project root or set LANGMUIR_USER_PCAP");
    let expected = independently_count_capture(capture);
    println!(
        "USER_CAPTURE independent total={} target={} complete={} structure={} parsed={} accepted={} duplicates={} conflicts={}",
        expected.total,
        expected.target,
        expected.complete,
        expected.structure_valid,
        expected.parsed,
        expected.frames.len(),
        expected.duplicates,
        expected.conflicts
    );
    let directory = tempdir().unwrap();
    let actual = capture_to_scientific_file(
        capture,
        &directory.path().join("science.lpd"),
        &expected.frames,
    );
    assert!(actual.target > 0);
    assert!(actual.total >= actual.target);
    assert_eq!(
        (actual.total, actual.target),
        (expected.total, expected.target)
    );
    assert_eq!(actual.stats.complete_frames, expected.complete);
    assert_eq!(
        actual.stats.structure_valid_frames,
        expected.structure_valid
    );
    assert_eq!(actual.stats.parsed_frames, expected.parsed);
    assert_eq!(
        actual.stats.parse_failures,
        expected.complete - expected.parsed
    );
    assert_eq!(actual.stats.duplicates, expected.duplicates);
    assert_eq!(actual.stats.conflicts, expected.conflicts);
    assert_eq!(actual.events, u64::try_from(expected.frames.len()).unwrap());
    assert_eq!(actual.points, [actual.events * 100; 2]);
    assert_scientific_file(&actual.bytes, &expected.frames);
    if let Some(path) = std::env::var_os("LANGMUIR_GUI_SCIENCE_FILE") {
        // Optional read-only audit of a file closed by the actual GUI. Saving
        // may start after replay, so require an exact complete capture suffix.
        let gui_bytes = fs::read(Path::new(&path)).expect("read closed GUI scientific file");
        let header_len = usize::from(BINARY_HEADER_LEN);
        let record_len = usize::try_from(BINARY_FRAME_LEN).unwrap();
        assert!(
            gui_bytes.len() >= header_len + record_len,
            "GUI file must contain science records"
        );
        assert_eq!((gui_bytes.len() - header_len) % record_len, 0);
        let first_record = &gui_bytes[header_len..header_len + record_len];
        let utc = read_u32(first_record, 0);
        let micros = read_u32(first_record, 4);
        let sequence = first_record[8];
        let first_index = expected
            .frames
            .iter()
            .position(|frame| {
                frame.timestamp.utc_seconds == utc
                    && frame.timestamp.microseconds == micros
                    && frame.frame_seq_u8 == sequence
            })
            .expect("GUI first record must match the original capture");
        assert_scientific_file(&gui_bytes, &expected.frames[first_index..]);
        println!(
            "GUI_SCIENCE_FILE path={} file_bytes={} frames={} first_index={} readback=PASS",
            Path::new(&path).display(),
            gui_bytes.len(),
            (gui_bytes.len() - header_len) / record_len,
            first_index
        );
    }
    assert_eq!(fs::read(capture).unwrap(), original);
}
