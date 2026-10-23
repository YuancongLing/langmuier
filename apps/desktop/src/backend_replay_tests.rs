//! Replay control tests inject capture time; they never open a socket or sleep.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, bounded};
use langmuir_storage::{BatchWriter, BinaryConfig, BinarySink, StorageServiceConfig};
use langmuir_transport::{CapturePacket, CapturedUdp};

use super::{Controller, DEPLOYED_LVDS_UDP_SOURCE, PcapReplay, UdpLvdsIngress};
use crate::bridge::{BackendEvent, BackendStateSnapshot, ReplayProgress};

#[test]
fn csv_capture_closes_at_replay_eof_and_preserves_all_raw_samples() {
    let (replay, tx, _cancelled) = replay(Instant::now());
    let (mut controller, _display) = controller();
    controller.replay = Some(replay);
    controller.lvds_connected = true;
    let directory = tempfile::tempdir().unwrap();
    let stem = directory.path().join("science");
    assert!(controller.start_scientific_capture(crate::bridge::ScientificSaveFormat::Csv, &stem,));
    for sequence in [65_535, 65_536] {
        tx.send(Ok(Some(captured(
            Duration::ZERO,
            datagram(sequence, &wire(), 1),
        ))))
        .unwrap();
    }
    tx.send(Ok(None)).unwrap();
    controller.drain_replay();
    let status = &controller.storage_status;
    assert!(status.closed);
    assert_eq!(status.error, None);
    assert_eq!((status.enqueued, status.sink_written), (2, 2));
    assert_eq!(
        (status.csv_frames_written, status.csv_rows_written),
        (2, 250)
    );
    assert_eq!(status.lpd_frames_written, 0);
    assert!(status.lpd_path.is_none());
    assert!(!stem.with_extension("lpd").exists());
    let csv = std::fs::read_to_string(stem.with_extension("csv")).unwrap();
    let lines: Vec<_> = csv.lines().collect();
    assert_eq!(lines.len(), 251);
    assert_eq!(
        lines[1],
        "0,65535,1694498816,999800,0,1694498816999800,4660,43981,CRC_NOT_CHECKED"
    );
    assert_eq!(
        lines[125],
        "0,65535,1694498816,999800,124,1694498817049400,4784,43857,CRC_NOT_CHECKED"
    );
    assert_eq!(
        lines[126],
        "1,0,1694498816,999800,0,1694498816999800,4660,43981,CRC_NOT_CHECKED"
    );
}

#[test]
fn dual_capture_manual_stop_closes_matching_outputs_and_keeps_plotting() {
    let (replay, tx, _cancelled) = replay(Instant::now());
    let (mut controller, display) = controller();
    controller.replay = Some(replay);
    controller.lvds_connected = true;
    let directory = tempfile::tempdir().unwrap();
    let stem = directory.path().join("dual");
    assert!(
        controller.start_scientific_capture(crate::bridge::ScientificSaveFormat::LpdAndCsv, &stem,)
    );
    tx.send(Ok(Some(captured(Duration::ZERO, datagram(7, &wire(), 1)))))
        .unwrap();
    controller.drain_replay();
    controller.stop_acquisition_at_ui_boundary();
    tx.send(Ok(Some(captured(Duration::ZERO, datagram(8, &wire(), 1)))))
        .unwrap();
    controller.drain_replay();
    assert_eq!(display.try_iter().count(), 2);
    let status = &controller.storage_status;
    assert!(status.closed);
    assert_eq!(status.error, None);
    assert_eq!(
        (status.csv_frames_written, status.lpd_frames_written),
        (1, 1)
    );
    assert_eq!(status.csv_rows_written, 125);
    assert_eq!(
        std::fs::read(stem.with_extension("lpd")).unwrap().len(),
        1042
    );
    assert_eq!(
        std::fs::read_to_string(stem.with_extension("csv"))
            .unwrap()
            .lines()
            .count(),
        126
    );
    assert!(!controller.acquisition_running);
    assert!(!controller.replay.as_ref().unwrap().progress.finished);
}

#[test]
fn capture_creation_failure_reports_error_without_starting_or_overwriting() {
    let (mut controller, _display) = controller();
    let directory = tempfile::tempdir().unwrap();
    let stem = directory.path().join("occupied");
    std::fs::write(stem.with_extension("csv"), "retain existing capture").unwrap();
    assert!(!controller.start_scientific_capture(crate::bridge::ScientificSaveFormat::Csv, &stem,));
    assert!(!controller.acquisition_running);
    assert!(controller.scientific_writer.is_none());
    assert!(controller.storage_status.error.is_some());
    assert_eq!(controller.storage_status.enqueued, 0);
    assert_eq!(controller.storage_status.csv_rows_written, 0);
    assert_eq!(
        std::fs::read_to_string(stem.with_extension("csv")).unwrap(),
        "retain existing capture"
    );
}

#[test]
fn replay_read_error_closes_csv_and_marks_incomplete_capture() {
    let (replay, tx, _cancelled) = replay(Instant::now());
    let (mut controller, _display) = controller();
    controller.replay = Some(replay);
    let directory = tempfile::tempdir().unwrap();
    let stem = directory.path().join("interrupted");
    assert!(controller.start_scientific_capture(crate::bridge::ScientificSaveFormat::Csv, &stem,));
    tx.send(Ok(Some(captured(Duration::ZERO, datagram(8, &wire(), 1)))))
        .unwrap();
    tx.send(Err("broken PCAP".to_owned())).unwrap();
    controller.drain_replay();
    assert!(controller.storage_status.closed);
    assert!(
        controller
            .storage_status
            .error
            .as_ref()
            .unwrap()
            .contains("broken PCAP")
    );
    assert_eq!(controller.storage_status.csv_rows_written, 125);
    assert!(stem.with_extension("csv.incomplete").exists());
}

#[test]
fn detached_capture_progress_still_updates_closed_counts_and_retains_error() {
    let (mut controller, _display) = controller();
    controller.storage_status.format = crate::bridge::ScientificSaveFormat::LpdAndCsv;
    controller.storage_status.error = Some("shutdown timed out; capture incomplete".to_owned());
    controller.capture_progress = Some(Arc::new(Mutex::new(
        crate::bridge::ScientificStorageStatus {
            csv_frames_written: 2,
            csv_rows_written: 250,
            lpd_frames_written: 1,
            closed: true,
            ..crate::bridge::ScientificStorageStatus::default()
        },
    )));
    controller.drain_storage();
    assert!(controller.storage_status.closed);
    assert_eq!(controller.storage_status.csv_rows_written, 250);
    assert_eq!(controller.storage_status.lpd_frames_written, 1);
    assert_eq!(controller.storage_status.sink_written, 1);
    assert!(
        controller
            .storage_status
            .error
            .as_ref()
            .unwrap()
            .contains("timed out")
    );
}

type PacketResult = Result<Option<CapturePacket>, String>;

fn replay(started: Instant) -> (PcapReplay, Sender<PacketResult>, Receiver<()>) {
    let (tx, packets) = bounded(64);
    let (cancel, cancelled) = bounded(1);
    (
        PcapReplay {
            packets,
            cancel,
            join: None,
            ingress: UdpLvdsIngress::new(DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap()),
            progress: ReplayProgress {
                path: "synthetic-timestamps.pcapng".to_owned(),
                total_packets: 0,
                target_udp_packets: 0,
                finished: false,
                error: None,
            },
            pending: None,
            first_timestamp: None,
            last_timestamp: None,
            started,
        },
        tx,
        cancelled,
    )
}

fn wire() -> Vec<u8> {
    let mut frame = vec![0; 269];
    frame[..19].copy_from_slice(&[
        0xEB, 0x90, 0x55, 0xAA, 1, 7, 0xAA, 0x65, 0, 0, 0, 0, 0x0F, 0x41, 0x78, 0x12, 0x34, 0xAB,
        0xCD,
    ]);
    for pair in frame[19..267].as_chunks_mut::<2>().0 {
        pair.copy_from_slice(&[1, 0xFF]);
    }
    frame[267..269].copy_from_slice(&[0x44, 0xC8]);
    frame
}

fn datagram(sequence: u32, body: &[u8], fragments: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0x55, 0xAA, 1, 0xA5]);
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(&269_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(&fragments.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(body);
    bytes
}

fn captured(timestamp: Duration, payload: Vec<u8>) -> CapturePacket {
    CapturePacket {
        timestamp,
        udp: Some(CapturedUdp {
            source: DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap(),
            destination: "192.168.1.100:1534".parse().unwrap(),
            payload,
        }),
    }
}

fn controller() -> (Controller, Receiver<BackendEvent>) {
    let (critical_tx, critical_rx) = bounded(64);
    let (display_tx, display_rx) = bounded(8);
    (
        Controller::new(
            critical_tx,
            critical_rx,
            display_tx,
            Arc::default(),
            Arc::new(Mutex::new(BackendStateSnapshot::default())),
        ),
        display_rx,
    )
}

#[test]
fn replay_four_hundred_frames_ignore_tail_and_refresh_both_channels_without_saving() {
    let started = Instant::now();
    let (replay, tx, _cancelled) = replay(started);
    let (mut controller, display) = controller();
    controller.replay = Some(replay);
    let mut points = [0_u64; 2];
    for sequence in 0..400_u32 {
        let elapsed_micros = 999_800 + sequence * 50_000;
        let utc = 1_694_498_816 + elapsed_micros / 1_000_000;
        let micros = elapsed_micros % 1_000_000;
        let mut raw = wire();
        raw[7..11].copy_from_slice(&utc.to_be_bytes());
        raw[11..15].copy_from_slice(&micros.to_be_bytes());
        // Keep arbitrary tail bytes while scientific timestamps change.
        raw[267..269].copy_from_slice(&[0, 0]);
        let delay = Duration::from_millis(u64::from(sequence) * 50);
        tx.send(Ok(Some(captured(delay, datagram(sequence, &raw, 1)))))
            .unwrap();
        controller.replay.as_mut().unwrap().started = Instant::now().checked_sub(delay).unwrap();
        controller.drain_replay();
        let BackendEvent::ScientificSamples {
            channel_1_start_seconds,
            channel_2_start_seconds,
            sample_interval_seconds,
            channel_1,
            channel_2,
        } = display
            .try_recv()
            .expect("default replay must deliver a plotting event")
        else {
            panic!("decodable replay data uses the ordinary scientific route");
        };
        let expected_time = f64::from(utc) + f64::from(micros) / 1_000_000.0;
        assert_eq!(channel_1_start_seconds, expected_time);
        assert_eq!(channel_2_start_seconds, expected_time);
        assert_eq!(sample_interval_seconds, 0.0004);
        assert_eq!((channel_1.len(), channel_2.len()), (125, 125));
        assert_eq!((channel_1[0], channel_1[124]), (4660.0, 4784.0));
        assert_eq!((channel_2[0], channel_2[124]), (43_981.0, 43_857.0));
        points[0] += u64::try_from(channel_1.len()).unwrap();
        points[1] += u64::try_from(channel_2.len()).unwrap();
        assert!(display.is_empty());
        assert!(tx.len() <= 1);
        assert!(controller.scientific_writer.is_none());
    }
    tx.send(Ok(None)).unwrap();
    controller.drain_replay();
    let replay = controller.replay.as_ref().unwrap();
    assert!(replay.progress.finished);
    assert_eq!(replay.progress.error, None);
    assert_eq!(replay.ingress.receive_stats.complete_frames, 400);
    assert_eq!(replay.ingress.receive_stats.parsed_frames, 400);
    assert_eq!(replay.ingress.receive_stats.parse_failures, 0);
    assert_eq!(replay.ingress.receive_stats.accepted_frames, 400);
    assert_eq!(replay.ingress.metrics.udp_sequence.observed_frames, 400);
    assert_eq!(points, [50_000; 2]);
    assert_eq!(controller.storage_status.enqueued, 0);
}

#[test]
fn mixed_replay_preserves_science_order_and_saves_all_decodable_unique_frames() {
    let started = Instant::now();
    let (replay, tx, _cancelled) = replay(started);
    let (mut controller, display) = controller();
    controller.replay = Some(replay);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mixed-science.lpd");
    controller.scientific_writer = Some(
        BatchWriter::start(
            BinarySink::create(BinaryConfig::new(&path)).unwrap(),
            StorageServiceConfig::default(),
        )
        .unwrap(),
    );
    controller.capture_path = Some(path.clone());
    controller.acquisition_running = true;
    let good = wire();
    let mut bad = good.clone();
    bad[268] ^= 1;
    let mut invalid = bad.clone();
    invalid[0] ^= 1;
    for (sequence, raw) in [(1, &bad), (2, &good), (3, &bad), (4, &invalid)] {
        tx.send(Ok(Some(captured(
            Duration::ZERO,
            datagram(sequence, raw, 1),
        ))))
        .unwrap();
    }
    controller.drain_replay();
    let events: Vec<_> = display.try_iter().collect();
    assert_eq!(events.len(), 3);
    assert!(
        events
            .iter()
            .all(|event| matches!(event, BackendEvent::ScientificSamples { .. }))
    );
    assert!(controller.acquisition_running);
    // A complete duplicate stays suppressed even though the CRC gate is gone.
    tx.send(Ok(Some(captured(Duration::ZERO, datagram(3, &bad, 1)))))
        .unwrap();
    tx.send(Ok(Some(captured(Duration::ZERO, datagram(5, &bad, 1)))))
        .unwrap();
    tx.send(Ok(None)).unwrap();
    controller.drain_replay();
    assert!(matches!(
        display.try_recv().unwrap(),
        BackendEvent::ScientificSamples { .. }
    ));
    assert!(display.is_empty());
    assert!(controller.storage_status.closed);
    assert_eq!(controller.storage_status.enqueued, 4);
    assert_eq!(controller.storage_status.sink_written, 4);
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(bytes.len(), 32 + 4 * 1010);
    let sequences: Vec<_> = bytes[32..]
        .as_chunks::<1010>()
        .0
        .iter()
        .map(|record| u16::from_le_bytes(record[8..10].try_into().unwrap()))
        .collect();
    assert_eq!(sequences, [1, 2, 3, 5]);
    let stats = controller.replay.as_ref().unwrap().ingress.receive_stats;
    assert_eq!(stats.complete_frames, 6);
    assert_eq!((stats.parsed_frames, stats.parse_failures), (5, 1));
    assert_eq!(stats.accepted_frames, 4);
    assert_eq!(stats.duplicates, 1);
    assert_eq!(stats.header_errors, 1);
}

#[test]
fn replay_preserves_packet_time_pacing_and_explicit_eof() {
    let started = Instant::now();
    let (mut replay, tx, _cancelled) = replay(started);
    let epoch = Duration::from_secs(1_700_000_000);
    for (sequence, delay) in [(100, 0), (101, 50)] {
        tx.send(Ok(Some(captured(
            epoch + Duration::from_millis(delay),
            datagram(sequence, &wire(), 1),
        ))))
        .unwrap();
    }
    tx.send(Ok(None)).unwrap();
    let mut frames = Vec::new();
    replay.drain_deliveries(started, &mut frames);
    assert_eq!(frames.len(), 1);
    assert_eq!(replay.progress.total_packets, 1);
    assert!(!replay.progress.finished);
    replay.drain_deliveries(started + Duration::from_millis(49), &mut frames);
    assert_eq!(frames.len(), 1);
    replay.drain_deliveries(started + Duration::from_millis(50), &mut frames);
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame.sequence)
            .collect::<Vec<_>>(),
        [100, 101]
    );
    assert_eq!(replay.progress.total_packets, 2);
    assert_eq!(replay.progress.target_udp_packets, 2);
    assert!(replay.progress.finished);
    assert!(replay.progress.error.is_none());
}

#[test]
fn replay_silence_expires_residuals_and_sequence_gaps_before_next_packet() {
    let started = Instant::now();
    let (mut replay, tx, _cancelled) = replay(started);
    let raw = wire();
    for (delay, payload) in [
        (0, datagram(100, &raw, 1)),
        (100, datagram(102, &raw, 1)),
        (200, datagram(200, &raw[..100], 2)),
        (3_000, datagram(103, &raw, 1)),
    ] {
        tx.send(Ok(Some(captured(Duration::from_millis(delay), payload))))
            .unwrap();
    }
    tx.send(Ok(None)).unwrap();
    let mut frames = Vec::new();
    replay.drain_deliveries(started + Duration::from_millis(200), &mut frames);
    assert_eq!(frames.len(), 2);
    assert_eq!(replay.ingress.metrics.udp_sequence.pending_gaps, 1);
    assert_eq!(replay.ingress.metrics.reassembly_pending_frames, 1);
    replay.drain_deliveries(started + Duration::from_millis(2_100), &mut frames);
    assert_eq!(frames.len(), 2);
    assert_eq!(replay.ingress.metrics.udp_sequence.pending_gaps, 0);
    assert_eq!(replay.ingress.metrics.udp_sequence.expired_gaps, 1);
    assert_eq!(replay.ingress.metrics.reassembly_pending_frames, 1);
    replay.drain_deliveries(started + Duration::from_millis(2_200), &mut frames);
    assert_eq!(replay.ingress.metrics.reassembly_pending_frames, 0);
    assert_eq!(replay.ingress.metrics.reassembly_timeouts, 1);
    assert!(!replay.progress.finished);
    replay.drain_deliveries(started + Duration::from_secs(3), &mut frames);
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame.sequence)
            .collect::<Vec<_>>(),
        [100, 102, 103]
    );
    assert!(replay.progress.finished);
}

#[test]
fn replay_counts_all_packets_and_filters_original_source_without_rewriting() {
    let started = Instant::now();
    let (mut replay, tx, _cancelled) = replay(started);
    tx.send(Ok(Some(CapturePacket {
        timestamp: Duration::ZERO,
        udp: None,
    })))
    .unwrap();
    let mut other = captured(Duration::ZERO, datagram(9, &wire(), 1));
    other.udp.as_mut().unwrap().source = "192.0.2.9:1534".parse().unwrap();
    tx.send(Ok(Some(other))).unwrap();
    tx.send(Ok(Some(captured(Duration::ZERO, datagram(10, &wire(), 1)))))
        .unwrap();
    tx.send(Ok(None)).unwrap();
    let mut frames = Vec::new();
    replay.drain_deliveries(started, &mut frames);
    assert_eq!(replay.progress.total_packets, 3);
    assert_eq!(replay.progress.target_udp_packets, 1);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].sequence, 10);
    assert_eq!(replay.ingress.receive_stats.udp_packets, 1);
}

#[test]
fn replay_reports_reader_errors_and_unexpected_disconnect_separately_from_eof() {
    let started = Instant::now();
    let (mut failed, tx, _cancelled) = replay(started);
    tx.send(Err("malformed capture fixture".to_owned()))
        .unwrap();
    failed.drain_deliveries(started, &mut Vec::new());
    assert!(failed.progress.finished);
    assert_eq!(
        failed.progress.error.as_deref(),
        Some("malformed capture fixture")
    );

    let (mut disconnected, tx, _cancelled) = replay(started);
    drop(tx);
    disconnected.drain_deliveries(started, &mut Vec::new());
    assert!(disconnected.progress.finished);
    assert!(
        disconnected
            .progress
            .error
            .as_deref()
            .unwrap()
            .contains("异常终止")
    );
}

#[test]
fn replay_rejects_backward_timestamps_without_delivering_the_old_packet() {
    let started = Instant::now();
    let (mut replay, tx, _cancelled) = replay(started);
    for (sequence, seconds) in [(100, 10), (101, 9)] {
        tx.send(Ok(Some(captured(
            Duration::from_secs(seconds),
            datagram(sequence, &wire(), 1),
        ))))
        .unwrap();
    }
    let mut frames = Vec::new();
    replay.drain_deliveries(started, &mut frames);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].sequence, 100);
    assert!(replay.progress.finished);
    assert!(
        replay
            .progress
            .error
            .as_deref()
            .unwrap()
            .contains("时间倒退")
    );
}

#[test]
fn replay_drop_requests_cancellation_without_waiting_for_a_packet() {
    let (replay, _tx, cancelled) = replay(Instant::now());
    drop(replay);
    assert_eq!(cancelled.try_recv(), Ok(()));
}

#[test]
fn background_reader_preserves_capture_bytes_and_reports_truncation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("empty-section.pcapng");
    let section = [
        0x0A, 0x0D, 0x0D, 0x0A, 28, 0, 0, 0, 0x4D, 0x3C, 0x2B, 0x1A, 1, 0, 0, 0, 255, 255, 255,
        255, 255, 255, 255, 255, 28, 0, 0, 0,
    ];
    std::fs::write(&path, section).unwrap();
    let replay = PcapReplay::start(path.display().to_string()).unwrap();
    assert_eq!(
        replay.packets.recv_timeout(Duration::from_secs(2)).unwrap(),
        Ok(None)
    );
    assert_eq!(std::fs::read(&path).unwrap(), section);
    drop(replay);

    let broken = directory.path().join("truncated.pcapng");
    std::fs::write(&broken, &section[..11]).unwrap();
    let replay = PcapReplay::start(broken.display().to_string()).unwrap();
    assert!(
        replay
            .packets
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_err()
    );
    assert_eq!(std::fs::read(&broken).unwrap(), section[..11]);
}
