//! Synthetic protocol integration and real localhost UDP; no hardware links.
use super::*;
use crate::bridge::{BackendEvent, ScientificSaveFormat};

fn envelope(sequence: u32, wire: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x55, 0xAA, 1, 0xA5];
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(&u32::try_from(wire.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&u16::try_from(wire.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(wire);
    bytes
}
pub(super) fn wire(sequence: u8, ordinal: u32) -> Vec<u8> {
    let mut bytes = protocol_10bit_tests::golden_frame(sequence);
    bytes[7..11].copy_from_slice(&(1_700_000_000 + ordinal / 20).to_be_bytes());
    bytes[11..15].copy_from_slice(&((ordinal % 20) * 50_000).to_be_bytes());
    // Manufacturer reference CRC input excludes the seven fixed header bytes.
    let crc = langmuir_core::crc16_ccitt_false(&bytes[7..267]);
    bytes[267..269].copy_from_slice(&crc.to_be_bytes());
    bytes
}
pub(super) fn shift_four(bytes: &[u8]) -> Vec<u8> {
    let mut shifted = vec![0_u8; bytes.len() + 1];
    for (index, &byte) in bytes.iter().enumerate() {
        for bit in 0..8 {
            if byte & (0x80 >> bit) != 0 {
                let target = index * 8 + bit + 4;
                shifted[target / 8] |= 0x80 >> (target % 8);
            }
        }
    }
    shifted
}

fn partial(sequence: u32, payload: &[u8], first: bool) -> Vec<u8> {
    let offset = if first { 0 } else { 140 };
    let end = if first { 140 } else { payload.len() };
    let mut bytes = envelope(sequence, &payload[offset..end]);
    bytes[8..12].copy_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
    bytes[12..14].copy_from_slice(&u16::from(!first).to_be_bytes());
    bytes[14..16].copy_from_slice(&2_u16.to_be_bytes());
    bytes[16..20].copy_from_slice(&u32::try_from(offset).unwrap().to_be_bytes());
    bytes
}

#[test]
fn recovery_joint_gates_preserve_duplicates_and_expire_the_anchor_at_exact_ttl() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let stranger = "192.0.2.2:1534".parse().unwrap();
    let now = Instant::now();
    let ttl = langmuir_core::DEFAULT_UDP_LVDS_REASSEMBLY_TTL;
    for (age, expected) in [
        (
            ttl.checked_sub(Duration::from_nanos(1)).unwrap(),
            FrameQuality::Recovered4Bit,
        ),
        (ttl, FrameQuality::RecoveryCandidate),
    ] {
        let mut ingress = UdpLvdsIngress::new(source);
        ingress.recovery_enabled = true;
        let mut native = 0;
        ingress.push_at(source, &envelope(100, &wire(10, 0)), now, |_| native += 1);
        ingress.push_at(
            source,
            &envelope(100, &wire(10, 0)),
            (now + ttl).checked_sub(Duration::from_millis(1)).unwrap(),
            |_| native += 1,
        );
        ingress.push_at(
            stranger,
            &envelope(999, &wire(99, 99)),
            (now + ttl).checked_sub(Duration::from_millis(1)).unwrap(),
            |_| panic!("wrong source"),
        );
        ingress.push_at(
            source,
            &envelope(101, &shift_four(&wire(11, 1))),
            now + age,
            |_| native += 1,
        );
        assert_eq!(
            ingress.observations.back().unwrap().result.quality,
            expected,
            "duplicate must not renew anchor TTL"
        );
        assert_eq!(native, 1);
    }
    for anchored_source in [source, stranger] {
        let mut old = UdpLvdsIngress::new(source);
        old.recovery_enabled = true;
        old.push_at(anchored_source, &envelope(100, &wire(10, 0)), now, |_| {});
        old.push_at(
            source,
            &envelope(101, &shift_four(&wire(11, 1))),
            now,
            |_| panic!("recovery is not native"),
        );
        assert_eq!(
            old.observations.back().unwrap().result.quality,
            if anchored_source == source {
                FrameQuality::Recovered4Bit
            } else {
                FrameQuality::RecoveryCandidate
            },
            "only the configured source can establish an anchor"
        );
        let mut fresh = UdpLvdsIngress::new(source);
        fresh.recovery_enabled = true;
        fresh.push_at(
            source,
            &envelope(101, &shift_four(&wire(11, 1))),
            now,
            |_| panic!("recovery is not native"),
        );
        assert_eq!(
            fresh.observations.back().unwrap().result.quality,
            FrameQuality::RecoveryCandidate,
            "no cross-source or cross-session anchor"
        );
    }
}

#[test]
fn recovery_joint_gates_break_on_conflicts_gaps_expiry_and_late_native_then_recover() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let now = Instant::now();
    for interruption in 0..5 {
        let mut ingress = UdpLvdsIngress::new(source);
        ingress.recovery_enabled = true;
        ingress.push_at(source, &envelope(100, &wire(10, 0)), now, |_| {});
        let shifted = shift_four(&wire(11, 1));
        let mut later = now + Duration::from_millis(50);
        let expected_sequence = if interruption == 4 { 102 } else { 101 };
        let candidate = if interruption == 4 {
            shift_four(&wire(12, 2))
        } else {
            shifted.clone()
        };
        match interruption {
            0 => ingress.push_at(source, &envelope(100, &wire(99, 9)), later, |_| {
                panic!("sequence conflict")
            }),
            1 => ingress.push_at(
                source,
                &envelope(102, &shift_four(&wire(12, 2))),
                later,
                |_| panic!("candidate"),
            ),
            2 => {
                let fragment = partial(101, &shifted, true);
                ingress.push_at(source, &fragment, later, |_| panic!("partial"));
                let mut conflict = fragment;
                conflict[24] ^= 1;
                ingress.push_at(source, &conflict, later, |_| panic!("conflicting fragment"));
                assert_eq!(ingress.metrics.reassembly_pending_frames, 0);
            }
            3 => {
                ingress.push_at(source, &partial(101, &shifted, true), later, |_| {
                    panic!("partial")
                });
                later += langmuir_core::DEFAULT_UDP_LVDS_REASSEMBLY_TTL;
                assert_eq!(ingress.expire_at(later), 1);
                assert_eq!(ingress.metrics.reassembly_buffered_bytes, 0);
            }
            4 => {
                ingress.push_at(source, &envelope(103, &wire(13, 3)), later, |_| {});
                ingress.push_at(source, &envelope(101, &wire(11, 1)), later, |_| {});
            }
            _ => unreachable!(),
        }
        ingress.push_at(
            source,
            &envelope(expected_sequence, &candidate),
            later,
            |_| panic!("candidate is not native"),
        );
        assert_eq!(
            ingress.observations.back().unwrap().result.quality,
            FrameQuality::RecoveryCandidate,
            "interruption case {interruption} reused an unsafe anchor"
        );
        assert!(!ingress.observations.back().unwrap().delivered);
        ingress.push_at(source, &envelope(110, &wire(20, 10)), later, |_| {});
        ingress.push_at(
            source,
            &envelope(111, &shift_four(&wire(21, 11))),
            later,
            |_| panic!("recovery is not native"),
        );
        assert_eq!(
            ingress.observations.back().unwrap().result.quality,
            FrameQuality::Recovered4Bit,
            "fresh native anchor must restore case {interruption}"
        );
        assert!(ingress.observations.back().unwrap().delivered);
    }
}

#[test]
fn recovery_joint_gates_keep_repeated_fragments_and_sequence_wrap_continuous() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let now = Instant::now();
    for (outer, inner) in [(100_u32, 10_u8), (u32::MAX, 255)] {
        let mut ingress = UdpLvdsIngress::new(source);
        ingress.recovery_enabled = true;
        ingress.push_at(source, &envelope(outer, &wire(inner, 0)), now, |_| {});
        let shifted = shift_four(&wire(inner.wrapping_add(1), 1));
        let next = outer.wrapping_add(1);
        for fragment in [
            partial(next, &shifted, true),
            partial(next, &shifted, true),
            partial(next, &shifted, false),
        ] {
            ingress.push_at(source, &fragment, now + Duration::from_millis(50), |_| {
                panic!("recovery is not native")
            });
        }
        assert_eq!(
            ingress.observations.back().unwrap().result.quality,
            FrameQuality::Recovered4Bit
        );
        assert!(ingress.observations.back().unwrap().delivered);
        ingress.push_at(
            source,
            &envelope(next, &shifted),
            now + Duration::from_millis(60),
            |_| panic!("recovery is not native"),
        );
        assert!(
            !ingress.observations.back().unwrap().delivered,
            "repeated recovery is never redelivered"
        );
        ingress.push_at(
            source,
            &envelope(
                next.wrapping_add(1),
                &shift_four(&wire(inner.wrapping_add(2), 2)),
            ),
            now + Duration::from_millis(100),
            |_| panic!("recovery is not native"),
        );
        assert_eq!(
            ingress.observations.back().unwrap().result.quality,
            FrameQuality::Recovered4Bit,
            "an exact repeated recovered frame cannot break the next reliable link"
        );
        assert!(ingress.observations.back().unwrap().delivered);
        assert_eq!(ingress.metrics.reassembly_pending_frames, 0);
    }
}

#[test]
fn destination_change_cannot_join_fragments_from_different_flows() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let now = Instant::now();
    let first = RawDestination::Captured("192.0.2.100:1534".parse().unwrap());
    let second = RawDestination::Captured("192.0.2.101:1534".parse().unwrap());
    let mut ingress = UdpLvdsIngress::new(source);
    ingress.recovery_enabled = true;
    ingress.push_observed_at(source, first, &envelope(100, &wire(10, 0)), now, 1, |_| {});
    let shifted = shift_four(&wire(11, 1));
    ingress.push_observed_at(source, first, &partial(101, &shifted, true), now, 2, |_| {
        panic!("partial")
    });
    ingress.push_observed_at(
        source,
        second,
        &partial(101, &shifted, false),
        now,
        3,
        |_| panic!("cross-flow partial"),
    );
    assert_eq!(
        ingress.observations.len(),
        1,
        "bytes from different destinations cannot reassemble"
    );
    assert_eq!(ingress.metrics.reassembly_pending_frames, 1);
    ingress.push_observed_at(
        source,
        second,
        &partial(101, &shifted, true),
        now,
        4,
        |_| panic!("recovery is not native"),
    );
    assert_eq!(
        ingress.observations.back().unwrap().result.quality,
        FrameQuality::RecoveryCandidate,
        "the new flow has no native anchor"
    );
    assert_eq!(
        ingress.metrics.reassembled_frames, 2,
        "flow reset preserves cumulative reassembly accounting"
    );
    assert!(
        ingress
            .audit_rows
            .iter()
            .any(|row| row["destination"] == format!("{second:?}"))
    );
}

#[test]
fn confirmed_recovery_cannot_consume_a_later_native_frame_storage_eligibility() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let now = Instant::now();
    let mut ingress = UdpLvdsIngress::new(source);
    ingress.recovery_enabled = true;
    let mut native = Vec::new();
    for (outer, payload) in [
        (100, wire(10, 0)),
        (101, shift_four(&wire(11, 1))),
        (101, wire(11, 1)),
        (101, wire(11, 1)),
    ] {
        ingress.push_at(source, &envelope(outer, &payload), now, |frame| {
            native.push(frame.frame_seq_u8);
        });
    }
    assert_eq!(
        ingress.observations[1].result.quality,
        FrameQuality::Recovered4Bit
    );
    assert_eq!(
        native,
        [10, 11],
        "confirmed diagnostic recovery must not suppress the genuine native record"
    );
    assert!(ingress.observations[2].delivered);
    assert!(!ingress.observations[3].delivered);
    let directory = tempfile::tempdir().unwrap();
    let stem = directory
        .path()
        .join("SYNTHETIC-native-after-confirmed-recovery");
    let (mut controller, _critical, _display) = e2e_tests::controller();
    assert!(controller.start_scientific_capture(ScientificSaveFormat::LpdAndCsv, &stem));
    controller.consume_observations(
        ingress.observations.drain(..).collect(),
        ingress.audit_rows.drain(..).collect(),
    );
    controller.stop_acquisition();
    let mut lpd = langmuir_storage::BinaryV3Reader::open(stem.with_extension("lpd")).unwrap();
    assert_eq!(lpd.next_frame().unwrap().unwrap().frame_seq_u8, 10);
    assert_eq!(lpd.next_frame().unwrap().unwrap().frame_seq_u8, 11);
    assert!(lpd.next_frame().unwrap().is_none());
    let csv = std::fs::read_to_string(stem.with_extension("csv")).unwrap();
    assert_eq!(csv.lines().count(), 201);
    assert!(
        csv.lines()
            .skip(1)
            .all(|line| line.ends_with("CRC_NOT_CHECKED"))
    );
}

#[test]
fn pcap_destination_change_cannot_confirm_recovery_or_duplicate_native_delivery() {
    use langmuir_transport::CapturedUdp;
    let source: SocketAddr = "192.0.2.1:1534".parse().unwrap();
    let first_target: SocketAddr = "192.0.2.100:1534".parse().unwrap();
    let second_target: SocketAddr = "192.0.2.101:1534".parse().unwrap();
    let (tx, packets) = bounded(8);
    let (cancel, _cancelled) = bounded(1);
    let started = Instant::now();
    let mut ingress = UdpLvdsIngress::new(source);
    ingress.recovery_enabled = true;
    let mut replay = PcapReplay {
        packets,
        cancel,
        join: None,
        ingress,
        progress: ReplayProgress {
            path: "SYNTHETIC-destinations.pcapng".to_owned(),
            total_packets: 0,
            target_udp_packets: 0,
            finished: false,
            error: None,
        },
        pending: None,
        first_timestamp: None,
        last_timestamp: None,
        started,
    };
    for (index, destination, payload) in [
        (0_u64, first_target, envelope(100, &wire(10, 0))),
        (1, second_target, envelope(101, &shift_four(&wire(11, 1)))),
        (2, first_target, envelope(100, &wire(10, 0))),
    ] {
        tx.send(Ok(Some(CapturePacket {
            timestamp: Duration::from_millis(index * 50),
            udp: Some(CapturedUdp {
                source,
                destination,
                payload,
            }),
        })))
        .unwrap();
    }
    let mut deliveries = Vec::new();
    replay.drain_recorded(started + Duration::from_millis(100), &mut deliveries, None);
    assert_eq!(
        replay.ingress.observations[1].result.quality,
        FrameQuality::RecoveryCandidate,
        "a complete bit frame at another captured destination cannot borrow the previous flow's anchor"
    );
    assert!(!replay.ingress.observations[1].delivered);
    assert_eq!(
        deliveries.len(),
        1,
        "switching destinations must not erase native dedup history"
    );
}

#[test]
fn mixed_native_recovered_native_display_keeps_order_and_native_storage_isolation() {
    let directory = tempfile::tempdir().unwrap();
    let stem = directory.path().join("SYNTHETIC-mixed-quality");
    let (mut controller, _critical, display) = e2e_tests::controller();
    assert!(controller.start_scientific_capture(ScientificSaveFormat::LpdAndCsv, &stem));
    let source = "192.0.2.1:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    ingress.recovery_enabled = true;
    let middle = shift_four(&wire(11, 1));
    let mut native = Vec::new();
    for (outer, payload) in [(100, wire(10, 0)), (101, middle), (102, wire(12, 2))] {
        ingress.push(source, &envelope(outer, &payload), |frame| {
            native.push(frame);
        });
    }
    assert_eq!(native.len(), 2);
    assert_eq!(
        ingress.observations[1].result.quality,
        FrameQuality::Recovered4Bit
    );
    let observations = ingress.observations.drain(..).collect();
    let rows = ingress.audit_rows.drain(..).collect();
    controller.consume_observations(observations, rows);
    controller.stop_acquisition();
    let events: Vec<_> = display.try_iter().collect();
    assert_eq!(events.len(), 3);
    let times: Vec<_> = events
        .iter()
        .map(|event| match event {
            BackendEvent::ScientificSamples {
                channel_1_start_seconds,
                ..
            } => *channel_1_start_seconds,
            BackendEvent::QualifiedScientificSamples(samples) => {
                assert_eq!(samples.quality, FrameQuality::Recovered4Bit);
                samples.start_seconds
            }
            _ => panic!("unexpected display event"),
        })
        .collect();
    assert_eq!(times, [1_700_000_000.0, 1_700_000_000.05, 1_700_000_000.1]);
    assert!(matches!(events[0], BackendEvent::ScientificSamples { .. }));
    assert!(matches!(
        events[1],
        BackendEvent::QualifiedScientificSamples(_)
    ));
    assert!(matches!(events[2], BackendEvent::ScientificSamples { .. }));
    assert_eq!(
        std::fs::metadata(stem.with_extension("lpd")).unwrap().len(),
        40 + 2 * 409
    );
    assert_eq!(
        std::fs::read_to_string(stem.with_extension("csv"))
            .unwrap()
            .lines()
            .count(),
        201
    );
}

#[test]
fn localhost_raw_hook_records_every_udp_before_source_parser_and_duplicate_filters() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("SYNTHETIC-localhost.lvdsraw");
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let source = sender.local_addr().unwrap();
    let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut receiver = UdpLvdsReceiver::bind("127.0.0.1:0".parse().unwrap(), source).unwrap();
    let binding = receiver.socket.local_addr().unwrap();
    let mut raw = raw_storage::RawRecording::start(
        &path,
        langmuir_storage::RawHeader {
            origin: langmuir_storage::RawOrigin::LiveUdp,
            created_at: Duration::ZERO,
            expected_source: source,
            binding,
            session_id: [6; 16],
        },
    )
    .unwrap();
    let valid = envelope(42, &wire(1, 0));
    let inputs = [valid.clone(), vec![0; 24], vec![0; 268], valid];
    for input in &inputs {
        sender.send_to(input, binding).unwrap();
    }
    stranger.send_to(&[0xAA; 19], binding).unwrap();
    let mut native = 0;
    let deadline = Instant::now() + Duration::from_secs(2);
    while raw.status().received < 5 && Instant::now() < deadline {
        receiver
            .drain_recorded(32, Some(&mut raw), |_| native += 1)
            .unwrap();
        std::thread::yield_now();
    }
    let status = raw.stop();
    assert_eq!((status.received, status.written, status.dropped), (5, 5, 0));
    assert!(status.complete);
    assert_eq!(native, 1);
    assert_eq!(receiver.ingress.metrics.duplicate_frames_suppressed, 1);
    let mut reader = langmuir_storage::RawReader::open(path).unwrap();
    let mut records = Vec::new();
    while let Some(record) = reader.next_record().unwrap() {
        records.push(record);
    }
    assert_eq!(
        records
            .iter()
            .filter(|record| record.source_matches)
            .count(),
        4
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.payload.len() == 268)
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.payload == inputs[0])
            .count(),
        2
    );
    assert!(records.iter().all(
        |record| record.destination == langmuir_storage::RawDestination::SocketBinding(binding)
    ));
}

fn history_envelope_inputs() -> Vec<(SocketAddr, Vec<u8>)> {
    let source = DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap();
    let mut truncated = envelope(101, &wire(1, 0));
    truncated.pop();
    let oversized = envelope(102, &vec![0; 4097]);
    let fragment = partial(103, &wire(3, 0), true);
    let mut conflict = fragment.clone();
    conflict[24] ^= 1;
    let native = envelope(200, &wire(20, 1));
    vec![
        (source, vec![0; 24]),
        (source, vec![0x55, 0xAA, 1, 0xA5]),
        (source, truncated),
        (source, oversized),
        (source, fragment),
        (source, conflict),
        (source, native.clone()),
        (source, native),
        ("192.0.2.99:1534".parse().unwrap(), vec![0; 24]),
    ]
}

fn history_envelope_ready(
    path: &Path,
) -> (
    crate::bridge::HistoryStatus,
    crate::bridge::HistoryWindow,
    PathBuf,
) {
    let session = history::HistorySession::start(path.to_owned(), 77, true).unwrap();
    let mut window = None;
    let mut audit = None;
    loop {
        match session.events.recv_timeout(Duration::from_secs(5)).unwrap() {
            BackendEvent::HistoryWindow(value) => window = Some(*value),
            BackendEvent::RecoveryStatus(value) => audit = value.audit_path.map(PathBuf::from),
            BackendEvent::HistoryStatus(value) if !value.loading => {
                return (*value, window.unwrap(), audit.unwrap());
            }
            _ => {}
        }
    }
}

fn assert_history_envelope_anomalies(path: &Path, inputs: &[(SocketAddr, Vec<u8>)]) {
    let original = fs::read(path).unwrap();
    let (status, window, audit_path) = history_envelope_ready(path);
    assert_eq!(status.error, None);
    assert_eq!(status.read_records, 9);
    assert_eq!(
        status.rejected, 5,
        "bad UDP records must enter the anomaly list"
    );
    assert_eq!(
        (status.native, status.recovered, status.candidates),
        (2, 0, 0)
    );
    assert_eq!(
        window.channel_1.len(),
        100,
        "native duplicate must stay suppressed"
    );
    assert_eq!(window.channel_2.len(), 100);
    assert_eq!(status.anomalies.len(), 5);
    let audit: Vec<serde_json::Value> = fs::read_to_string(audit_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|row: &serde_json::Value| row["kind"] == "UDP_REJECTED")
        .collect();
    assert_eq!(audit.len(), 5);
    for (index, (record, sequence, reason)) in [
        (1_u64, None, "magic"),
        (2, None, "at least 24"),
        (3, Some(101_u32), "declares"),
        (4, Some(102), "4096"),
        (6, Some(103), "conflicting"),
    ]
    .into_iter()
    .enumerate()
    {
        let anomaly = &status.anomalies[index];
        assert_eq!(anomaly.record_id, record);
        assert_eq!(anomaly.outer_sequence, sequence);
        assert_eq!(anomaly.quality, FrameQuality::Rejected);
        assert_eq!(anomaly.device_time_seconds, None);
        assert_eq!(anomaly.frame_seq_u8, None);
        assert!(anomaly.reason.contains(reason), "{}", anomaly.reason);
        assert_eq!(anomaly.source, path.to_string_lossy());
        assert_eq!(audit[index]["raw_record_id"], record);
        assert_eq!(audit[index]["outer_sequence"], serde_json::json!(sequence));
        assert_eq!(audit[index]["source"], DEPLOYED_LVDS_UDP_SOURCE);
        assert_eq!(
            audit[index]["payload_hex"],
            hex_bytes(&inputs[usize::try_from(record - 1).unwrap()].1)
        );
        assert!(audit[index]["reason"].as_str().unwrap().contains(reason));
    }
    assert_eq!(fs::read(path).unwrap(), original, "history is read-only");
}

#[test]
fn history_envelope_raw_rejections_keep_provenance_and_native_dedup() {
    use langmuir_storage::{RawArchive, RawConfig, RawHeader, RawOrigin};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("SYNTHETIC-envelope.lvdsraw");
    let source = DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap();
    let destination = "192.168.1.10:1534".parse().unwrap();
    let mut archive = RawArchive::start(
        RawConfig::new(&path),
        RawHeader {
            origin: RawOrigin::PcapReplay,
            created_at: Duration::ZERO,
            expected_source: source,
            binding: destination,
            session_id: [0xE1; 16],
        },
    )
    .unwrap();
    let inputs = history_envelope_inputs();
    for (index, (endpoint, payload)) in inputs.iter().enumerate() {
        archive
            .try_record(
                Duration::from_millis(u64::try_from(index).unwrap() * 5),
                *endpoint,
                RawDestination::Captured(destination),
                *endpoint == source,
                Some(u64::try_from(index).unwrap()),
                payload,
            )
            .unwrap();
    }
    assert!(archive.stop().unwrap().complete);
    assert_history_envelope_anomalies(&path, &inputs);
}

#[test]
fn history_envelope_rejection_does_not_erase_pending_fragment_provenance() {
    let source = DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap();
    let destination = RawDestination::Captured("192.168.1.10:1534".parse().unwrap());
    let mut ingress = UdpLvdsIngress::new(source);
    let payload = wire(30, 0);
    let first = partial(300, &payload, true);
    let mut malformed = envelope(300, &payload);
    malformed.pop();
    let last = partial(300, &payload, false);
    let mut delivered = 0;
    for (index, datagram) in [&first, &malformed, &last].into_iter().enumerate() {
        ingress.push_observed_at(
            source,
            destination,
            datagram,
            Instant::now(),
            u64::try_from(index + 1).unwrap(),
            |_| delivered += 1,
        );
    }
    assert_eq!(delivered, 1);
    let alignment = ingress
        .audit_rows
        .iter()
        .find(|row| row["kind"] == "SCIENCE_ALIGNMENT")
        .unwrap();
    assert_eq!(
        alignment["raw_record_dependencies"],
        serde_json::json!([1, 3])
    );
}

#[test]
fn history_envelope_pcap_rejections_keep_provenance_and_native_dedup() {
    fn block(kind: u32, mut body: Vec<u8>) -> Vec<u8> {
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        let length = u32::try_from(body.len() + 12).unwrap();
        let mut bytes = kind.to_le_bytes().to_vec();
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend(body);
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("SYNTHETIC-envelope.pcapng");
    let mut section = 0x1A2B_3C4D_u32.to_le_bytes().to_vec();
    section.extend_from_slice(&1_u16.to_le_bytes());
    section.extend_from_slice(&0_u16.to_le_bytes());
    section.extend_from_slice(&u64::MAX.to_le_bytes());
    let mut bytes = block(0x0A0D_0D0A, section);
    let mut interface = 1_u16.to_le_bytes().to_vec();
    interface.extend_from_slice(&0_u16.to_le_bytes());
    interface.extend_from_slice(&65535_u32.to_le_bytes());
    bytes.extend(block(1, interface));
    let inputs = history_envelope_inputs();
    for (index, (source, payload)) in inputs.iter().enumerate() {
        let SocketAddr::V4(source) = source else {
            unreachable!()
        };
        let length = u16::try_from(payload.len() + 28).unwrap();
        let mut packet = vec![0; 42];
        packet[12..14].copy_from_slice(&0x0800_u16.to_be_bytes());
        packet[14] = 0x45;
        packet[16..18].copy_from_slice(&length.to_be_bytes());
        packet[22] = 64;
        packet[23] = 17;
        packet[26..30].copy_from_slice(&source.ip().octets());
        packet[30..34].copy_from_slice(&[192, 168, 1, 10]);
        packet[34..36].copy_from_slice(&source.port().to_be_bytes());
        packet[36..38].copy_from_slice(&1534_u16.to_be_bytes());
        packet[38..40].copy_from_slice(&(length - 20).to_be_bytes());
        packet.extend_from_slice(payload);
        let mut body = vec![0; 8];
        body.extend_from_slice(&(u32::try_from(index).unwrap() * 5000).to_le_bytes());
        body.extend_from_slice(&u32::try_from(packet.len()).unwrap().to_le_bytes());
        body.extend_from_slice(&u32::try_from(packet.len()).unwrap().to_le_bytes());
        body.extend(packet);
        bytes.extend(block(6, body));
    }
    fs::write(&path, bytes).unwrap();
    assert_history_envelope_anomalies(&path, &inputs);
}
