//! SYNTHETIC protocol fixtures; these do not establish FPGA AB/AC acceptance.
use super::super::{UdpLvdsReceiver, capture_history_tests, raw_storage};
use super::*;
use langmuir_storage::{RawArchive, RawConfig, RawDestination, RawHeader, RawOrigin, RawReader};
use std::net::UdpSocket;

fn auxiliary(packet_type: u8, marker: u8) -> Vec<u8> {
    let mut bytes = vec![marker; 269];
    bytes[..7].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, packet_type]);
    bytes[267..].copy_from_slice(&[0xDE, 0xAD]);
    bytes
}

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

fn mixed_inputs() -> Vec<Vec<u8>> {
    let mut inputs = vec![envelope(1000, &auxiliary(0xAB, 0xB3))];
    for ordinal in 0..20_u8 {
        if ordinal == 9 {
            inputs.push(envelope(1010, &auxiliary(0xAC, 0xC4)));
        }
        inputs.push(envelope(
            1000 + u32::try_from(inputs.len()).unwrap(),
            &capture_history_tests::wire(ordinal, u32::from(ordinal)),
        ));
    }
    inputs
}

fn write_raw(path: &Path, inputs: &[Vec<u8>]) {
    let source = DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap();
    let destination = "192.168.1.10:1534".parse().unwrap();
    let mut config = RawConfig::new(path);
    config.service.queue_capacity = inputs.len().max(128) + 1;
    let mut archive = RawArchive::start(
        config,
        RawHeader {
            origin: RawOrigin::PcapReplay,
            created_at: Duration::ZERO,
            expected_source: source,
            binding: destination,
            session_id: [0x51; 16],
        },
    )
    .unwrap();
    for (index, input) in inputs.iter().enumerate() {
        let receipt = archive
            .try_record(
                Duration::from_secs(1_700_000_000)
                    + Duration::from_millis(u64::try_from(index).unwrap() * 5),
                source,
                RawDestination::Captured(destination),
                true,
                Some(u64::try_from(index).unwrap()),
                input,
            )
            .unwrap();
        assert!(receipt.queued);
    }
    let status = archive.stop().unwrap();
    assert!(status.complete);
    assert_eq!(status.written, u64::try_from(inputs.len()).unwrap());
}

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

fn write_pcap(path: &Path, inputs: &[Vec<u8>]) {
    let mut section = 0x1A2B_3C4D_u32.to_le_bytes().to_vec();
    section.extend_from_slice(&1_u16.to_le_bytes());
    section.extend_from_slice(&0_u16.to_le_bytes());
    section.extend_from_slice(&u64::MAX.to_le_bytes());
    let mut bytes = block(0x0A0D_0D0A, section);
    let mut interface = 1_u16.to_le_bytes().to_vec();
    interface.extend_from_slice(&0_u16.to_le_bytes());
    interface.extend_from_slice(&65535_u32.to_le_bytes());
    bytes.extend(block(1, interface));
    for (index, payload) in inputs.iter().enumerate() {
        let length = u16::try_from(payload.len() + 28).unwrap();
        let mut packet = vec![0; 42];
        packet[12..14].copy_from_slice(&0x0800_u16.to_be_bytes());
        packet[14] = 0x45;
        packet[16..18].copy_from_slice(&length.to_be_bytes());
        packet[22] = 64;
        packet[23] = 17;
        packet[26..30].copy_from_slice(&[192, 168, 1, 128]);
        packet[30..34].copy_from_slice(&[192, 168, 1, 10]);
        packet[34..36].copy_from_slice(&1534_u16.to_be_bytes());
        packet[36..38].copy_from_slice(&1534_u16.to_be_bytes());
        packet[38..40].copy_from_slice(&(length - 20).to_be_bytes());
        packet.extend_from_slice(payload);
        let ticks = 1_700_000_000_000_000_u64 + u64::try_from(index).unwrap() * 5000;
        let mut body = 0_u32.to_le_bytes().to_vec();
        body.extend_from_slice(&u32::try_from(ticks >> 32).unwrap().to_le_bytes());
        body.extend_from_slice(&u32::try_from(ticks & 0xFFFF_FFFF).unwrap().to_le_bytes());
        body.extend_from_slice(&u32::try_from(packet.len()).unwrap().to_le_bytes());
        body.extend_from_slice(&u32::try_from(packet.len()).unwrap().to_le_bytes());
        body.extend(packet);
        bytes.extend(block(6, body));
    }
    std::fs::write(path, bytes).unwrap();
}

fn snapshot(path: &Path) -> (HistoryStatus, HistoryWindow, PathBuf) {
    let session = HistorySession::start(path.to_owned(), 51, false).unwrap();
    let mut window = None;
    let mut audit_path = None;
    loop {
        match session.events.recv_timeout(Duration::from_secs(5)).unwrap() {
            BackendEvent::HistoryWindow(value) => window = Some(*value),
            BackendEvent::RecoveryStatus(value) => audit_path = value.audit_path.map(PathBuf::from),
            BackendEvent::HistoryStatus(value) if !value.loading => {
                assert_eq!(value.error, None);
                return (*value, window.unwrap(), audit_path.unwrap());
            }
            _ => {}
        }
    }
}

fn packet_audit(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|row| row["kind"] == "LVDS_PACKET")
        .collect()
}

fn expected_hex(bytes: &[u8]) -> String {
    let mut text = String::new();
    for byte in bytes {
        std::fmt::Write::write_fmt(&mut text, format_args!("{byte:02X}")).unwrap();
    }
    text
}

#[test]
fn synthetic_mixed_raw_and_pcap_history_keep_auxiliaries_out_of_science_errors() {
    let directory = tempfile::tempdir().unwrap();
    let inputs = mixed_inputs();
    assert_eq!(inputs.len(), 22);
    for extension in ["lvdsraw", "pcapng"] {
        let path = directory
            .path()
            .join(format!("SYNTHETIC-multitype.{extension}"));
        if extension == "lvdsraw" {
            write_raw(&path, &inputs);
        } else {
            write_pcap(&path, &inputs);
        }
        let original = std::fs::read(&path).unwrap();
        let (status, window, audit_path) = snapshot(&path);
        assert_eq!(status.read_records, 22);
        assert_eq!(
            status.rejected, 0,
            "valid AB/AC must not be science rejections"
        );
        assert_eq!(status.native, 20);
        let counts = status.receive_stats;
        assert_eq!((counts.udp_packets, counts.complete_frames), (22, 22));
        assert_eq!(
            (
                counts.science_frames,
                counts.excitation_frames,
                counts.gnss_attitude_frames
            ),
            (20, 1, 1)
        );
        assert_eq!(
            (
                counts.unknown_type_frames,
                counts.structure_errors,
                counts.parse_failures
            ),
            (0, 0, 0)
        );
        assert_eq!((counts.parsed_frames, counts.accepted_frames), (20, 20));
        assert_eq!(status.auxiliary_frames.len(), 2);
        let audit = packet_audit(&audit_path);
        assert_eq!(audit.len(), 2);
        for (observation, (index, sequence)) in status
            .auxiliary_frames
            .iter()
            .zip([(0_usize, 1000_u32), (10, 1010)])
        {
            assert_eq!(observation.outer_sequence, sequence);
            assert_eq!(observation.record_id, u64::try_from(index + 1).unwrap());
            assert_eq!(
                observation.raw_record_dependencies,
                [u64::try_from(index + 1).unwrap()]
            );
            assert_eq!(
                observation.received_unix_nanos,
                1_700_000_000_000_000_000 + u64::try_from(index).unwrap() * 5_000_000
            );
            assert_eq!(observation.source, DEPLOYED_LVDS_UDP_SOURCE);
            assert_eq!(observation.byte_offset, 0);
            assert_eq!(
                observation.disposition,
                langmuir_core::UdpLvdsDisposition::Accepted
            );
            assert_eq!(
                observation.frame.raw_bytes().as_slice(),
                &inputs[index][24..]
            );
            assert_eq!(observation.frame.data().as_slice(), &inputs[index][31..291]);
            assert_eq!(observation.frame.raw_crc(), [0xDE, 0xAD]);
        }
        for (row, index) in audit.iter().zip([0_usize, 10]) {
            assert_eq!(row["raw_frame_hex"], expected_hex(&inputs[index][24..]));
            assert_eq!(row["data_hex"], expected_hex(&inputs[index][31..291]));
            assert_eq!(row["raw_crc_hex"], "DEAD");
            assert_eq!(row["crc_status"], "CRC_NOT_CHECKED");
        }
        assert_eq!(
            (window.channel_1.len(), window.channel_2.len()),
            (2000, 2000)
        );
        assert_eq!(window.frames.len(), 20);
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
}

#[test]
fn synthetic_auxiliary_only_history_does_not_fabricate_science() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("SYNTHETIC-auxiliary-only.lvdsraw");
    write_raw(
        &path,
        &[
            envelope(10, &auxiliary(0xAB, 0xB3)),
            envelope(11, &auxiliary(0xAC, 0xC4)),
        ],
    );
    let (status, window, _) = snapshot(&path);
    assert_eq!(status.rejected, 0, "auxiliary-only input is valid");
    assert_eq!(status.native, 0);
    assert_eq!(
        (
            status.receive_stats.science_frames,
            status.receive_stats.excitation_frames,
            status.receive_stats.gnss_attitude_frames
        ),
        (0, 1, 1)
    );
    assert_eq!(status.auxiliary_frames.len(), 2);
    assert_eq!(status.first_device_time, None);
    assert!(window.channel_1.is_empty() && window.channel_2.is_empty() && window.frames.is_empty());
}

#[test]
fn synthetic_auxiliary_history_retains_duplicates_conflicts_and_unknown_types() {
    use langmuir_core::UdpLvdsDisposition::{Accepted, Duplicate, SequenceConflict};

    let directory = tempfile::tempdir().unwrap();
    let path = directory
        .path()
        .join("SYNTHETIC-auxiliary-dispositions.lvdsraw");
    let first = envelope(200, &auxiliary(0xAB, 0xB3));
    let inputs = [
        first.clone(),
        first,
        envelope(200, &auxiliary(0xAB, 0xB4)),
        envelope(201, &auxiliary(0xAC, 0xC4)),
        envelope(202, &auxiliary(0xD1, 0xD2)),
    ];
    write_raw(&path, &inputs);
    let (status, window, _) = snapshot(&path);
    let counts = status.receive_stats;
    assert_eq!(
        (
            counts.excitation_frames,
            counts.gnss_attitude_frames,
            counts.unknown_type_frames
        ),
        (3, 1, 1)
    );
    assert_eq!(
        (counts.auxiliary_duplicates, counts.auxiliary_conflicts),
        (1, 1)
    );
    assert_eq!(
        (
            counts.science_frames,
            counts.parsed_frames,
            counts.parse_failures
        ),
        (0, 0, 0)
    );
    assert_eq!(status.auxiliary_frames.len(), 5);
    assert!(window.channel_1.is_empty() && window.channel_2.is_empty());
    assert_eq!(
        status
            .auxiliary_frames
            .iter()
            .map(|observation| observation.disposition)
            .collect::<Vec<_>>(),
        [Accepted, Duplicate, SequenceConflict, Accepted, Accepted]
    );
}

#[test]
fn synthetic_auxiliary_history_bounds_recent_frames_without_losing_type_counts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("SYNTHETIC-many-auxiliary.lvdsraw");
    let inputs: Vec<_> = (0..140_u32)
        .map(|sequence| envelope(sequence, &auxiliary(0xAB, 0xB3)))
        .collect();
    write_raw(&path, &inputs);
    let (status, window, audit_path) = snapshot(&path);
    assert_eq!(status.receive_stats.excitation_frames, 140);
    assert_eq!(status.auxiliary_frames.len(), 128);
    assert_eq!(status.auxiliary_frames.first().unwrap().outer_sequence, 12);
    assert_eq!(status.auxiliary_frames.last().unwrap().outer_sequence, 139);
    assert_eq!(packet_audit(&audit_path).len(), 140);
    assert!(window.channel_1.is_empty() && window.channel_2.is_empty());
}

#[test]
fn synthetic_localhost_raw_roundtrip_preserves_all_types_and_rejected_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory
        .path()
        .join("SYNTHETIC-all-types-and-errors.lvdsraw");
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
    let source = sender.local_addr().unwrap();
    let mut receiver = UdpLvdsReceiver::bind("127.0.0.1:0".parse().unwrap(), source).unwrap();
    let binding = receiver.socket.local_addr().unwrap();
    let mut raw = raw_storage::RawRecording::start(
        &path,
        RawHeader {
            origin: RawOrigin::LiveUdp,
            created_at: Duration::ZERO,
            expected_source: source,
            binding,
            session_id: [0x57; 16],
        },
    )
    .unwrap();
    let science = envelope(100, &capture_history_tests::wire(1, 0));
    let mut bad_header = auxiliary(0xAB, 0xB3);
    bad_header[0] = 0;
    let inputs = [
        science.clone(),
        envelope(101, &auxiliary(0xAB, 0xB3)),
        envelope(102, &auxiliary(0xAC, 0xC4)),
        envelope(103, &bad_header),
        envelope(104, &auxiliary(0xAA, 0xE5)[..268]),
        vec![0xE6; 24],
        science,
        envelope(105, &auxiliary(0xD1, 0xD2)),
        envelope(
            106,
            &capture_history_tests::shift_four(&auxiliary(0xAC, 0xC4)),
        ),
        vec![0xF8; 19],
    ];
    let mut native = 0;
    for (index, payload) in inputs.iter().enumerate() {
        let socket = if index == 9 { &stranger } else { &sender };
        socket.send_to(payload, binding).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while raw.status().received < u64::try_from(index + 1).unwrap() && Instant::now() < deadline
        {
            receiver
                .drain_recorded(32, Some(&mut raw), |_| native += 1)
                .unwrap();
            std::thread::yield_now();
        }
        assert_eq!(raw.status().received, u64::try_from(index + 1).unwrap());
    }
    let status = raw.stop();
    assert!(status.complete && status.closed);
    assert_eq!(
        (
            status.received,
            status.enqueued,
            status.written,
            status.dropped
        ),
        (10, 10, 10, 0)
    );
    assert_eq!(native, 1);
    let mut reader = RawReader::open(path).unwrap();
    for (index, expected) in inputs.iter().enumerate() {
        let record = reader.next_record().unwrap().unwrap();
        assert_eq!(record.id, u64::try_from(index + 1).unwrap());
        assert_eq!(record.payload, *expected, "payload record {}", index + 1);
        assert_eq!(
            record.source,
            if index == 9 {
                stranger.local_addr().unwrap()
            } else {
                source
            }
        );
        assert_eq!(record.destination, RawDestination::SocketBinding(binding));
        assert_eq!(record.source_matches, index != 9);
        assert_eq!(record.origin_packet_index, None);
        assert!(!record.timestamp.is_zero());
    }
    assert!(reader.next_record().unwrap().is_none());
    let footer = reader.summary().unwrap();
    assert!(footer.complete && footer.closed);
    assert_eq!(
        (footer.observed, footer.written, footer.dropped),
        (10, 10, 0)
    );
}
