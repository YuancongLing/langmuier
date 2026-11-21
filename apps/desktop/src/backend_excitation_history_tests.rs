//! Synthetic AB history contracts. These fixtures do not claim hardware acceptance.
use super::*;
use langmuir_storage::{RawArchive, RawConfig, RawDestination, RawHeader, RawOrigin};

fn excitation_wire(pa0: u8, seconds: u32, micros: u32) -> Vec<u8> {
    let mut bytes = vec![0; 269];
    bytes[..7].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 1, 7, 0xAB]);
    let data = &mut bytes[7..267];
    data[..4].copy_from_slice(&seconds.to_be_bytes());
    data[4..8].copy_from_slice(&micros.to_be_bytes());
    data[8..12].copy_from_slice(&931_u32.to_be_bytes());
    data[12..14].copy_from_slice(&10800_u16.to_be_bytes());
    data[14..16].copy_from_slice(&100_u16.to_be_bytes());
    data[16] = 0x55;
    data[17] = 1;
    data[20] = 0;
    data[21] = 1;
    data[22] = pa0.wrapping_add(17);
    data[23] = pa0;
    for index in 0..100 {
        data[60 + index * 2..62 + index * 2]
            .copy_from_slice(&(u16::try_from(index).unwrap() * 33).to_be_bytes());
    }
    data[258..260].copy_from_slice(&3300_u16.to_be_bytes());
    bytes[267..].copy_from_slice(&[0xAD, 0xDE]);
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
            session_id: [0x52; 16],
        },
    )
    .unwrap();
    for (index, payload) in inputs.iter().enumerate() {
        assert!(
            archive
                .try_record(
                    Duration::from_secs(1_700_000_000)
                        + Duration::from_millis(u64::try_from(index).unwrap() * 5),
                    source,
                    RawDestination::Captured(destination),
                    true,
                    Some(u64::try_from(index).unwrap()),
                    payload,
                )
                .unwrap()
                .queued
        );
    }
    assert!(archive.stop().unwrap().complete);
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

fn inputs_with_errors() -> Vec<Vec<u8>> {
    let first = envelope(100, &excitation_wire(255, 2, 0));
    let mut conflict = excitation_wire(255, 2, 0);
    conflict[68] = 1;
    let mut ac = vec![0xC4; 269];
    ac[..7].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 1, 7, 0xAC]);
    let mut invalid = excitation_wire(9, 2, 0);
    invalid[21..23].copy_from_slice(&99_u16.to_be_bytes());
    vec![
        first.clone(),
        first,
        envelope(100, &conflict),
        envelope(101, &ac),
        envelope(102, &invalid),
        envelope(103, &excitation_wire(0, 3, 0)),
        envelope(104, &excitation_wire(1, 2, 500_000)),
    ]
}

fn wait_status(session: &HistorySession, accept: impl Fn(&HistoryStatus) -> bool) -> HistoryStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let BackendEvent::HistoryStatus(status) =
            session.events.recv_timeout(Duration::from_secs(2)).unwrap()
            && accept(&status)
        {
            return *status;
        }
    }
    panic!("history operation did not finish");
}

#[test]
fn excitation_history_raw_and_pcap_count_only_admitted_valid_frames_and_keep_error_raw_id() {
    let directory = tempfile::tempdir().unwrap();
    for extension in ["lvdsraw", "pcapng"] {
        let path = directory.path().join(format!("SYNTHETIC-ab.{extension}"));
        let inputs = inputs_with_errors();
        if extension == "lvdsraw" {
            write_raw(&path, &inputs);
        } else {
            write_pcap(&path, &inputs);
        }
        let original = std::fs::read(&path).unwrap();
        let session = HistorySession::start(path.clone(), 52, false).unwrap();
        let status = wait_status(&session, |status| !status.loading);
        assert_eq!(status.error, None);
        assert_eq!((status.native, status.rejected), (0, 0));
        assert_eq!(status.read_records, 7);
        assert_eq!(status.excitation.frames, 3);
        assert_eq!(status.excitation.selected_index, Some(2));
        let selected = status.excitation.selected.unwrap();
        assert_eq!(selected.raw_record_id, 7);
        assert_eq!(selected.outer_sequence, 104);
        assert_eq!(selected.frame.pa0_seq, 1);
        assert_eq!(selected.frame.sample_time_us(0), Some(1_430_800));
        assert_eq!(selected.frame.sample_time_us(99), Some(2_500_000));
        if extension == "lvdsraw" {
            assert_eq!(selected.session, "52525252525252525252525252525252");
        }
        assert_eq!(status.excitation.errors.len(), 1);
        assert_eq!(status.excitation.errors[0].raw_record_id, 5);
        assert_eq!(status.excitation.errors[0].outer_sequence, 102);
        assert!(status.excitation.errors[0].reason.contains("100"));
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
}

#[test]
fn excitation_history_selection_preserves_each_overlapping_frame_in_arrival_order() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("SYNTHETIC-navigation.lvdsraw");
    write_raw(&path, &inputs_with_errors());
    let session = HistorySession::start(path, 53, false).unwrap();
    wait_status(&session, |status| !status.loading);
    for (index, pa0, expected_start, expected_end) in [
        (0, 255, 930_800, 2_000_000),
        (1, 0, 1_930_800, 3_000_000),
        (2, 1, 1_430_800, 2_500_000),
        (1, 0, 1_930_800, 3_000_000),
    ] {
        session
            .command(HistoryCommand::ExcitationFrame(index))
            .unwrap();
        let status = wait_status(&session, |status| {
            !status.excitation.loading && status.excitation.selected_index == Some(index)
        });
        assert_eq!(status.generation, 53);
        let selected = status.excitation.selected.unwrap();
        assert_eq!(selected.frame.pa0_seq, pa0);
        assert_eq!(selected.frame.wave_mv.len(), 100);
        assert_eq!(selected.frame.sample_time_us(0), Some(expected_start));
        assert_eq!(selected.frame.sample_time_us(99), Some(expected_end));
        assert_eq!(status.first_device_time, None);
    }
}

#[test]
fn excitation_history_export_keeps_all_samples_overlaps_and_arrival_order() {
    let directory = tempfile::tempdir().unwrap();
    for extension in ["lvdsraw", "pcapng"] {
        let path = directory
            .path()
            .join(format!("SYNTHETIC-export.{extension}"));
        let output = directory.path().join(format!("ab-{extension}.csv"));
        let inputs = inputs_with_errors();
        if extension == "lvdsraw" {
            write_raw(&path, &inputs);
        } else {
            write_pcap(&path, &inputs);
        }
        let session = HistorySession::start(path, 54, false).unwrap();
        let initial = wait_status(&session, |status| !status.loading);
        let provenance = initial.excitation.selected.unwrap();
        session
            .command(HistoryCommand::ExportExcitation {
                path: output.to_string_lossy().into_owned(),
            })
            .unwrap();
        let status = wait_status(&session, |status| {
            status.excitation.export_path.is_some() && !status.excitation.exporting
        });
        assert_eq!(status.excitation.export_error, None);
        let text = std::fs::read_to_string(&output).unwrap();
        let rows: Vec<_> = text
            .lines()
            .map(|line| line.split(',').collect::<Vec<_>>())
            .collect();
        assert_eq!(rows.len(), 301);
        assert_eq!(
            &rows[1][..11],
            &[
                "0", "1", "100", "255", "16", "2", "0", "0", "930800", "0", "0.000"
            ]
        );
        assert_eq!(&rows[100][7..11], &["99", "2000000", "3300", "3.300"]);
        assert_eq!(&rows[101][..4], &["1", "6", "103", "0"]);
        assert_eq!(&rows[201][..4], &["2", "7", "104", "1"]);
        assert_eq!(rows[201][8], "1430800");
        assert!(rows[1].contains(&"CRC_NOT_CHECKED"));
        assert_eq!(rows[1][19].trim_matches('"'), provenance.source);
        assert_eq!(rows[1][20].trim_matches('"'), provenance.session);
        assert_eq!(rows[1][21], "1700000000000000000");
        let before = std::fs::read(&output).unwrap();
        session
            .command(HistoryCommand::ExportExcitation {
                path: output.to_string_lossy().into_owned(),
            })
            .unwrap();
        let status = wait_status(&session, |status| status.excitation.export_error.is_some());
        assert!(!status.excitation.exporting);
        assert_eq!(std::fs::read(&output).unwrap(), before);
    }
}

#[test]
fn excitation_history_drains_business_queue_per_record_and_bounds_error_page() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("SYNTHETIC-bounded-ab.lvdsraw");
    let mut inputs: Vec<_> = (0..1100_u32)
        .map(|sequence| {
            envelope(
                sequence,
                &excitation_wire(u8::try_from(sequence % 256).unwrap(), 10, 0),
            )
        })
        .collect();
    let mut invalid = excitation_wire(7, 10, 0);
    invalid[31] = 1;
    inputs.extend((1100..1240_u32).map(|sequence| envelope(sequence, &invalid)));
    write_raw(&path, &inputs);
    let session = HistorySession::start(path, 55, false).unwrap();
    let status = wait_status(&session, |status| !status.loading);
    assert_eq!(status.excitation.frames, 1100);
    assert_eq!(status.excitation.selected_index, Some(1099));
    assert_eq!(status.excitation.errors.len(), 128);
    assert_eq!(
        status.excitation.errors.first().unwrap().raw_record_id,
        1113
    );
    assert_eq!(status.excitation.errors.last().unwrap().raw_record_id, 1240);
    assert_eq!(status.anomalies, []);
}

#[test]
fn excitation_history_mixed_twenty_science_frames_keep_their_science_window() {
    let directory = tempfile::tempdir().unwrap();
    for extension in ["lvdsraw", "pcapng"] {
        let path = directory
            .path()
            .join(format!("SYNTHETIC-valid-mixed.{extension}"));
        let mut inputs = vec![envelope(1000, &excitation_wire(255, 2, 0))];
        let mut ac = vec![0xC4; 269];
        ac[..7].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 1, 7, 0xAC]);
        for ordinal in 0..20_u8 {
            if ordinal == 9 {
                inputs.push(envelope(1010, &ac));
            }
            inputs.push(envelope(
                1000 + u32::try_from(inputs.len()).unwrap(),
                &super::super::capture_history_tests::wire(ordinal, u32::from(ordinal)),
            ));
        }
        if extension == "lvdsraw" {
            write_raw(&path, &inputs);
        } else {
            write_pcap(&path, &inputs);
        }
        let session = HistorySession::start(path, 56, false).unwrap();
        let mut window = None;
        let status = loop {
            match session.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                BackendEvent::HistoryWindow(value) => window = Some(*value),
                BackendEvent::HistoryStatus(status) if !status.loading => break status,
                _ => {}
            }
        };
        assert_eq!((status.native, status.rejected), (20, 0));
        assert_eq!(status.receive_stats.udp_packets, 22);
        assert_eq!(status.excitation.frames, 1);
        assert_eq!(status.receive_stats.gnss_attitude_frames, 1);
        assert_eq!(status.excitation.selected.unwrap().frame.wave_mv.len(), 100);
        let window = window.unwrap();
        assert_eq!(
            (window.channel_1.len(), window.channel_2.len()),
            (2000, 2000)
        );
        assert_eq!(window.frames.len(), 20);
    }
}

#[test]
fn excitation_history_frame_selection_can_be_superseded_without_aa_command_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("SYNTHETIC-cancel-selection.lvdsraw");
    let inputs: Vec<_> = (0..1500_u32)
        .map(|sequence| {
            envelope(
                sequence,
                &excitation_wire(u8::try_from(sequence % 256).unwrap(), 10, 0),
            )
        })
        .collect();
    write_raw(&path, &inputs);
    let session = HistorySession::start(path, 57, false).unwrap();
    wait_status(&session, |status| !status.loading);
    session
        .command(HistoryCommand::ExcitationFrame(1498))
        .unwrap();
    session.command(HistoryCommand::ExcitationFrame(1)).unwrap();
    session.command(HistoryCommand::Play(false)).unwrap();
    let status = wait_status(&session, |status| {
        !status.excitation.loading && status.excitation.selected_index == Some(1)
    });
    assert_eq!(status.excitation.selected.unwrap().frame.pa0_seq, 1);
    assert!(!status.playing);
}

#[test]
fn excitation_history_selection_and_export_preserve_fragment_and_duplicate_context() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("SYNTHETIC-fragmented-ab.lvdsraw");
    let wire = excitation_wire(255, 2, 0);
    let fragment = |sequence: u32, index: u16, offset: usize, length: usize| {
        let mut packet = envelope(sequence, &wire[offset..offset + length]);
        packet[8..12].copy_from_slice(&269_u32.to_be_bytes());
        packet[12..14].copy_from_slice(&index.to_be_bytes());
        packet[14..16].copy_from_slice(&2_u16.to_be_bytes());
        packet[16..20].copy_from_slice(&u32::try_from(offset).unwrap().to_be_bytes());
        packet
    };
    write_raw(
        &path,
        &[
            fragment(200, 0, 0, 100),
            fragment(200, 1, 100, 169),
            fragment(200, 0, 0, 100),
            fragment(200, 1, 100, 169),
            fragment(201, 0, 0, 100),
        ],
    );
    let session = HistorySession::start(path, 58, false).unwrap();
    let status = wait_status(&session, |status| !status.loading);
    assert_eq!(status.excitation.frames, 1);
    assert_eq!(status.excitation.selected.unwrap().raw_record_id, 2);
    session.command(HistoryCommand::ExcitationFrame(0)).unwrap();
    let status = wait_status(&session, |status| {
        !status.excitation.loading && status.excitation.selected_index == Some(0)
    });
    assert_eq!(status.excitation.selected.unwrap().raw_record_id, 2);
    let output = directory.path().join("fragments.csv");
    session
        .command(HistoryCommand::ExportExcitation {
            path: output.to_string_lossy().into_owned(),
        })
        .unwrap();
    let status = wait_status(&session, |status| {
        status.excitation.export_path.is_some() && !status.excitation.exporting
    });
    assert_eq!(status.excitation.export_error, None);
    let csv = std::fs::read_to_string(output).unwrap();
    assert_eq!(csv.lines().count(), 101);
    assert_eq!(csv.lines().nth(1).unwrap().split(',').nth(1), Some("2"));
}

#[test]
fn excitation_history_rejected_queue_request_does_not_cancel_an_admitted_selection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("SYNTHETIC-queue-full.lvdsraw");
    write_raw(&path, &[envelope(100, &excitation_wire(255, 2, 0))]);
    let (_events_tx, events) = bounded(1);
    let (commands, requests) = bounded(1);
    let (cancel, _cancelled) = bounded(1);
    // Exercise the real admission method with a deliberately saturated queue.
    // No worker drains it until the admitted request is selected below.
    let session = HistorySession {
        path,
        events,
        commands: Mutex::new(commands),
        selection_epoch: Arc::new(AtomicU64::new(0)),
        excitation_epoch: Arc::new(AtomicU64::new(0)),
        cancel,
        join: None,
    };
    session.command(HistoryCommand::ExcitationFrame(0)).unwrap();
    assert!(session.command(HistoryCommand::ExcitationFrame(0)).is_err());
    let (revision, HistoryCommand::ExcitationFrame(index)) = requests.recv().unwrap() else {
        panic!("the admitted frame selection was not queued");
    };
    let selection = excitation::FrameSelection::start(
        &session.path,
        false,
        index,
        revision,
        session.excitation_epoch.clone(),
    )
    .unwrap();
    let selected = selection
        .result
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    assert!(
        selected.is_some(),
        "a rejected request cancelled the admitted selection"
    );
    assert_eq!(selected.unwrap().frame.pa0_seq, 255);
}
