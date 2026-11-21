//! SYNTHETIC protocol fixtures. These are not captured FPGA AB/AC data.
use super::*;

fn synthetic_frame(packet_type: u8, seq8: u8) -> Vec<u8> {
    let mut bytes = vec![0; 269];
    bytes[..7].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, packet_type]);
    bytes[7..11].copy_from_slice(&42_u32.to_be_bytes());
    bytes[265] = seq8;
    bytes[267..269].copy_from_slice(&[0x12, 0x34]);
    bytes
}

fn synthetic_udp(sequence: u32, frame: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 24];
    bytes[..4].copy_from_slice(&[0x55, 0xAA, 1, 0xA5]);
    bytes[4..8].copy_from_slice(&sequence.to_be_bytes());
    bytes[8..12].copy_from_slice(&u32::try_from(frame.len()).unwrap().to_be_bytes());
    bytes[12..16].copy_from_slice(&1_u32.to_be_bytes());
    bytes[16..20].copy_from_slice(&0_u32.to_be_bytes());
    bytes[20..22].copy_from_slice(&u16::try_from(frame.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(frame);
    bytes
}

#[test]
fn synthetic_auxiliary_types_do_not_enter_science_or_count_as_parse_errors() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    for (sequence, packet_type) in [(7, 0xAB), (8, 0xAC)] {
        ingress.push(
            source,
            &synthetic_udp(sequence, &synthetic_frame(packet_type, 0)),
            |_| {
                panic!("auxiliary frame entered AA decoder");
            },
        );
    }
    assert_eq!(ingress.receive_stats.parse_failures, 0);
    assert_eq!(ingress.receive_stats.complete_frames, 2);
    assert_eq!(ingress.metrics.rejected_frames, 0);
}

#[test]
fn synthetic_interleaved_auxiliaries_do_not_create_outer_sequence_gaps() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    let mut science = Vec::new();
    for (sequence, packet_type, seq8) in [
        (100, 0xAA, 255),
        (101, 0xAB, 91),
        (102, 0xAC, 3),
        (103, 0xAA, 0),
    ] {
        ingress.push(
            source,
            &synthetic_udp(sequence, &synthetic_frame(packet_type, seq8)),
            |frame| science.push(frame.frame_seq_u8),
        );
    }
    assert_eq!(science, [255, 0]);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 4);
    assert_eq!(ingress.metrics.udp_sequence.gap_candidates, 0);
}

#[test]
fn synthetic_twenty_plus_auxiliary_variants_preserve_aa_and_real_science_files() {
    for auxiliaries in [vec![0xAB, 0xAC], vec![0xAB], vec![0xAC], vec![0xAC, 0xAB]] {
        let directory = tempfile::tempdir().unwrap();
        let stem = directory.path().join("SYNTHETIC-mixed-science");
        let (critical_tx, critical_rx) = bounded(128);
        let (display_tx, display_rx) = bounded(32);
        let mut controller = Controller::new(
            critical_tx,
            critical_rx,
            display_tx,
            Arc::default(),
            Arc::new(Mutex::new(BackendStateSnapshot::default())),
        );
        assert!(controller.start_scientific_capture(ScientificSaveFormat::LpdAndCsv, &stem));
        let source = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let mut types = vec![0xAA; 20];
        for (index, packet_type) in auxiliaries.iter().copied().enumerate() {
            types.insert(index * 11, packet_type);
        }
        let mut science_sequence = 0;
        for (ordinal, packet_type) in types.iter().copied().enumerate() {
            let bytes = synthetic_frame(packet_type, science_sequence);
            if packet_type == 0xAA {
                science_sequence += 1;
            }
            ingress.push(
                source,
                &synthetic_udp(65_530 + u32::try_from(ordinal).unwrap(), &bytes),
                |frame| controller.handle_scientific_frame(frame),
            );
        }
        controller.stop_acquisition();
        assert_eq!(ingress.receive_stats.science_frames, 20);
        assert_eq!(ingress.receive_stats.accepted_frames, 20);
        assert_eq!(
            ingress.receive_stats.excitation_frames,
            u64::from(auxiliaries.contains(&0xAB))
        );
        assert_eq!(
            ingress.receive_stats.gnss_attitude_frames,
            u64::from(auxiliaries.contains(&0xAC))
        );
        assert_eq!(ingress.receive_stats.unknown_type_frames, 0);
        assert_eq!(ingress.receive_stats.parse_failures, 0);
        assert_eq!(
            ingress.metrics.udp_sequence.observed_frames,
            u64::try_from(types.len()).unwrap()
        );
        assert_eq!(ingress.metrics.udp_sequence.gap_candidates, 0);
        assert_eq!(display_rx.len(), 20);
        assert_eq!(controller.storage_status.csv_frames_written, 20);
        assert_eq!(controller.storage_status.csv_rows_written, 2000);
        assert_eq!(controller.storage_status.lpd_frames_written, 20);
        assert_eq!(controller.storage_status.error, None);
        let csv = fs::read_to_string(stem.with_extension("csv")).unwrap();
        assert_eq!(csv.lines().count(), 2001);
        assert!(
            csv.lines()
                .skip(1)
                .all(|row| row.ends_with(",0,0,-32768,-32768,CRC_NOT_CHECKED"))
        );
        assert_eq!(
            fs::metadata(stem.with_extension("lpd")).unwrap().len(),
            40 + 20 * 409
        );
        let mut reader =
            langmuir_storage::BinaryV3Reader::open(stem.with_extension("lpd")).unwrap();
        for seq8 in 0..20 {
            let frame = reader.next_frame().unwrap().unwrap();
            assert_eq!(frame.frame_seq_u8, seq8);
            assert_eq!(*frame.channel_1, [0; 100]);
            assert_eq!(*frame.channel_2, [0; 100]);
        }
        assert_eq!(reader.next_frame().unwrap(), None);
    }
}

#[test]
fn synthetic_unknown_and_auxiliary_duplicates_conflicts_never_generate_science() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    for (sequence, packet_type, marker) in [
        (1, 0xAD, 0),
        (2, 0xAB, 0),
        (2, 0xAB, 0),
        (2, 0xAC, 1),
        (3, 0xAC, 0),
    ] {
        ingress.push(
            source,
            &synthetic_udp(sequence, &synthetic_frame(packet_type, marker)),
            |_| panic!("not science"),
        );
    }
    assert_eq!(ingress.receive_stats.unknown_type_frames, 1);
    assert_eq!(ingress.receive_stats.auxiliary_duplicates, 1);
    assert_eq!(ingress.receive_stats.auxiliary_conflicts, 1);
    assert_eq!(ingress.receive_stats.parse_failures, 0);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 3);
    assert_eq!(ingress.metrics.udp_sequence.gap_candidates, 0);
    assert_eq!(ingress.packet_observations.len(), 5);
    for observation in &ingress.packet_observations {
        assert_eq!(observation.frame.raw_crc(), [0x12, 0x34]);
        assert_eq!(observation.frame.data().len(), 260);
    }
}

#[test]
fn synthetic_auxiliary_fragments_count_only_complete_frames_and_expire() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    let bytes = synthetic_frame(0xAB, 3);
    let mut first = synthetic_udp(77, &bytes[..140]);
    first[8..12].copy_from_slice(&269_u32.to_be_bytes());
    first[14..16].copy_from_slice(&2_u16.to_be_bytes());
    let mut second = synthetic_udp(77, &bytes[140..]);
    second[8..12].copy_from_slice(&269_u32.to_be_bytes());
    second[12..14].copy_from_slice(&1_u16.to_be_bytes());
    second[14..16].copy_from_slice(&2_u16.to_be_bytes());
    second[16..20].copy_from_slice(&140_u32.to_be_bytes());
    let now = Instant::now();
    ingress.push_at(source, &second, now, |_| panic!("fragment"));
    assert_eq!(ingress.receive_stats.excitation_frames, 0);
    ingress.push_at(source, &first, now, |_| panic!("auxiliary"));
    assert_eq!(ingress.receive_stats.udp_packets, 2);
    assert_eq!(ingress.receive_stats.excitation_frames, 1);
    let observation = ingress.packet_observations.back().unwrap();
    assert_eq!(observation.frame.raw_bytes().as_slice(), bytes);
    assert_eq!(observation.raw_record_dependencies, [1, 2]);
    first[4..8].copy_from_slice(&78_u32.to_be_bytes());
    ingress.push_at(source, &first, now, |_| panic!("fragment"));
    assert_eq!(ingress.expire_at(now + Duration::from_secs(2)), 1);
    assert_eq!(ingress.receive_stats.excitation_frames, 1);
    assert_eq!(ingress.metrics.reassembly_timeouts, 1);
}

#[test]
fn synthetic_byte_synchronized_auxiliary_and_science_share_the_dispatcher() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    let mut scientific = 0;
    for (ordinal, packet_type) in [0xAB, 0xAC, 0xAA].into_iter().enumerate() {
        let mut payload = vec![0x12, 0x34, 0x56];
        payload.extend(synthetic_frame(packet_type, 0));
        ingress.push(
            source,
            &synthetic_udp(u32::try_from(ordinal).unwrap(), &payload),
            |_| scientific += 1,
        );
    }
    assert_eq!(scientific, 1);
    assert_eq!(ingress.receive_stats.excitation_frames, 1);
    assert_eq!(ingress.receive_stats.gnss_attitude_frames, 1);
    assert_eq!(ingress.receive_stats.parse_failures, 0);
    assert!(
        ingress
            .packet_observations
            .iter()
            .all(|observation| observation.byte_offset == 3)
    );
}

#[test]
fn synthetic_outer_continuity_is_independent_of_aa_business_validation() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    let mut delivered = Vec::new();
    for sequence in 0..3 {
        let mut frame = synthetic_frame(0xAA, u8::try_from(sequence).unwrap());
        if sequence == 1 {
            frame[11..15].copy_from_slice(&1_000_000_u32.to_be_bytes());
        }
        ingress.push(source, &synthetic_udp(sequence, &frame), |frame| {
            delivered.push(frame.frame_seq_u8);
        });
    }
    assert_eq!(delivered, [0, 2]);
    assert_eq!(ingress.receive_stats.parse_failures, 1);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 3);
    assert_eq!(ingress.metrics.udp_sequence.gap_candidates, 0);
    // A valid retransmission may still enter science after a bad timestamp;
    // outer observation deduplication must not poison science admission.
    ingress.push(
        source,
        &synthetic_udp(1, &synthetic_frame(0xAA, 1)),
        |frame| delivered.push(frame.frame_seq_u8),
    );
    assert_eq!(delivered, [0, 2, 1]);
    assert_eq!(ingress.metrics.udp_sequence.observed_frames, 3);
}

#[test]
fn synthetic_malformed_auxiliary_structure_never_counts_as_aa_decode_failure() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    ingress.recovery_enabled = true;
    for packet_type in [0xAB, 0xAC] {
        let mut frame = synthetic_frame(packet_type, 0);
        frame[2] = 0x04;
        ingress.push(
            source,
            &synthetic_udp(u32::from(packet_type), &frame),
            |_| panic!("bad auxiliary"),
        );
    }
    assert_eq!(ingress.receive_stats.science_frames, 0);
    assert_eq!(ingress.receive_stats.parse_failures, 0);
    assert_eq!(ingress.receive_stats.structure_errors, 2);
    assert_eq!(ingress.metrics.scientific_frames_delivered, 0);
    assert!(
        ingress
            .observations
            .iter()
            .all(|observation| observation.result.quality == FrameQuality::Rejected)
    );
}

#[test]
fn synthetic_auxiliary_event_overload_reports_loss_without_changing_frame_counts() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    for sequence in 0..129 {
        ingress.push(
            source,
            &synthetic_udp(sequence, &synthetic_frame(0xAB, 0)),
            |_| panic!("auxiliary"),
        );
    }
    assert_eq!(ingress.receive_stats.excitation_frames, 129);
    assert_eq!(ingress.packet_observations.len(), 128);
    assert_eq!(ingress.metrics.events_dropped, 1);
}
