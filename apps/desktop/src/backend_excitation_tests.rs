//! SYNTHETIC ingress/storage acceptance; no hardware AB capture is claimed.
use super::*;

fn ab(sequence: u8) -> Vec<u8> {
    let mut frame = vec![0; 269];
    frame[..7].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 1, 7, 0xAB]);
    let data = &mut frame[7..267];
    data[..4].copy_from_slice(&42_u32.to_be_bytes());
    data[8..12].copy_from_slice(&931_u32.to_be_bytes());
    data[12..14].copy_from_slice(&10_800_u16.to_be_bytes());
    data[14..16].copy_from_slice(&100_u16.to_be_bytes());
    data[16] = 0x55;
    data[17] = 1;
    data[22] = 73;
    data[23] = sequence;
    for i in 0..100 {
        data[60 + i * 2..62 + i * 2]
            .copy_from_slice(&(u16::try_from(i).unwrap() * 33).to_be_bytes());
    }
    frame[267..].copy_from_slice(&[0xDE, 0xAD]);
    frame
}

fn udp(sequence: u32, frame: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 24];
    bytes[..4].copy_from_slice(&[0x55, 0xAA, 1, 0xA5]);
    bytes[4..8].copy_from_slice(&sequence.to_be_bytes());
    bytes[8..12].copy_from_slice(&269_u32.to_be_bytes());
    bytes[12..16].copy_from_slice(&1_u32.to_be_bytes());
    bytes[20..22].copy_from_slice(&269_u16.to_be_bytes());
    bytes.extend_from_slice(frame);
    bytes
}

fn controller() -> Controller {
    let (tx, rx) = bounded(16);
    let (display, _) = bounded(2);
    Controller::new(tx, rx, display, Arc::default(), Arc::default())
}

#[test]
fn accepted_ab_uses_business_queue_and_preserves_latest_point_time() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    ingress.push(source, &udp(10, &ab(255)), |_| panic!("AB entered AA"));
    ingress.push(source, &udp(10, &ab(255)), |_| panic!("AB entered AA"));
    ingress.push(source, &udp(10, &ab(0)), |_| panic!("AB entered AA"));
    assert_eq!(ingress.receive_stats.excitation_frames, 3);
    assert_eq!(ingress.receive_stats.excitation_decoded, 1);
    assert_eq!(ingress.receive_stats.excitation_duplicates, 1);
    assert_eq!(ingress.receive_stats.excitation_conflicts, 1);
    assert_eq!(ingress.excitation_records.len(), 1);
    let record = ingress.excitation_records.pop_front().unwrap();
    assert_eq!(record.frame.sample_time_us(0), Some(40_930_800));
    assert_eq!(record.frame.sample_time_us(99), Some(42_000_000));
    assert_eq!(record.frame.actual_frequency_hz(), 0.931);
    assert_eq!(record.raw_record_id, 1);
    assert_eq!(record.outer_sequence, 10);
    assert_ne!(record.session, "");
}

#[test]
fn visual_queue_eviction_does_not_lose_ab_csv_records() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("SYNTHETIC-ab.csv");
    let mut controller = controller();
    controller.start_excitation_csv(&path.to_string_lossy());
    let source = "192.0.2.1:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    for sequence in 0..200 {
        ingress.push(source, &udp(sequence, &ab(0)), |_| {});
    }
    assert_eq!(ingress.packet_observations.len(), 128);
    assert_eq!(ingress.receive_stats.type_events_dropped, 72);
    assert_eq!(ingress.excitation_records.len(), 200);
    controller.consume_excitation_records(ingress.excitation_records.drain(..).collect());
    controller.stop_excitation_csv();
    let status = controller.excitation_storage_status();
    assert_eq!(status.enqueued, 200);
    assert_eq!(status.written, 200);
    assert_eq!(status.rows_written, 20_000);
    assert_eq!(status.dropped, 0);
    assert!(status.closed);
    assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 20_001);
    let before = fs::read(&path).unwrap();
    controller.start_excitation_csv(&path.to_string_lossy());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(controller.excitation_storage_status().error.is_some());
}

#[test]
fn ab_business_queue_overflow_and_decode_errors_are_explicit_and_bounded() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    for sequence in 0..1100 {
        ingress.push(source, &udp(sequence, &ab(0)), |_| {});
    }
    assert_eq!(ingress.excitation_records.len(), 1024);
    assert_eq!(ingress.receive_stats.excitation_decoded, 1100);
    assert_eq!(ingress.receive_stats.excitation_business_dropped, 76);
    let mut invalid = ab(1);
    invalid[7 + 24] = 1;
    ingress.push(source, &udp(1100, &invalid), |_| {});
    assert_eq!(ingress.receive_stats.excitation_decode_errors, 1);
    assert_eq!(ingress.excitation_errors.len(), 1);
    assert_eq!(ingress.excitation_errors[0].raw_record_id, 1101);
    assert_eq!(ingress.receive_stats.parse_failures, 0);
}

#[test]
fn valid_twenty_aa_ab_ac_variants_keep_science_and_outer_sequence_independent() {
    for auxiliary in [vec![0xAB, 0xAC], vec![0xAB], vec![0xAC], vec![0xAC, 0xAB]] {
        let source = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let mut types = vec![0xAA; 20];
        for (index, ty) in auxiliary.iter().copied().enumerate() {
            types.insert(index * 11, ty);
        }
        let mut aa = 0;
        let mut seq8 = 0;
        for (index, ty) in types.iter().copied().enumerate() {
            let mut bytes = ab(255);
            bytes[6] = ty;
            if ty == 0xAA {
                bytes[7..267].fill(0);
                bytes[7..11].copy_from_slice(&42_u32.to_be_bytes());
                bytes[265] = seq8;
                seq8 += 1;
            }
            ingress.push(
                source,
                &udp(65_530 + u32::try_from(index).unwrap(), &bytes),
                |_| aa += 1,
            );
        }
        assert_eq!(aa, 20);
        assert_eq!(ingress.receive_stats.accepted_frames, 20);
        assert_eq!(
            ingress.receive_stats.excitation_decoded,
            u64::from(auxiliary.contains(&0xAB))
        );
        assert_eq!(
            ingress
                .excitation_latest
                .as_ref()
                .map(|r| r.frame.wave_mv.len()),
            auxiliary.contains(&0xAB).then_some(100)
        );
        assert_eq!(
            ingress.receive_stats.gnss_attitude_frames,
            u64::from(auxiliary.contains(&0xAC))
        );
        assert_eq!(ingress.metrics.udp_sequence.gap_candidates, 0);
        assert_eq!(ingress.receive_stats.conflicts, 0);
    }
}

#[test]
fn ab_fragments_expire_and_flow_changes_cannot_mix_partial_waveforms() {
    let source = "192.0.2.1:1534".parse().unwrap();
    let other = "192.0.2.2:1534".parse().unwrap();
    let mut ingress = UdpLvdsIngress::new(source);
    let frame = ab(0);
    let fragment = |index: u16| {
        let (offset, size) = if index == 0 { (0, 100) } else { (100, 169) };
        let mut bytes = udp(99, &frame);
        bytes[12..14].copy_from_slice(&index.to_be_bytes());
        bytes[14..16].copy_from_slice(&2_u16.to_be_bytes());
        bytes[16..20].copy_from_slice(&u32::try_from(offset).unwrap().to_be_bytes());
        bytes[20..22].copy_from_slice(&u16::try_from(size).unwrap().to_be_bytes());
        bytes.truncate(24);
        bytes.extend_from_slice(&frame[offset..offset + size]);
        bytes
    };
    let now = Instant::now();
    ingress.push_at(source, &fragment(0), now, |_| {});
    ingress.push_at(other, &fragment(1), now, |_| {});
    assert_eq!(ingress.receive_stats.source_rejected_packets, 1);
    assert_eq!(ingress.receive_stats.excitation_decoded, 0);
    ingress.expire_at(now + Duration::from_secs(3));
    ingress.push_at(source, &fragment(1), now + Duration::from_secs(3), |_| {});
    assert_eq!(ingress.receive_stats.excitation_decoded, 0);
    let destination = RawDestination::SocketBinding("127.0.0.1:1535".parse().unwrap());
    ingress.push_observed_at(
        source,
        destination,
        &fragment(0),
        now + Duration::from_secs(3),
        Duration::ZERO,
        4,
        |_| {},
    );
    assert_eq!(ingress.receive_stats.excitation_decoded, 0);
    ingress.push_observed_at(
        source,
        destination,
        &fragment(1),
        now + Duration::from_secs(3),
        Duration::ZERO,
        5,
        |_| {},
    );
    assert_eq!(ingress.receive_stats.excitation_decoded, 1);
    assert_eq!(ingress.excitation_records.len(), 1);
    assert_eq!(ingress.excitation_records[0].raw_record_id, 5);
}

#[test]
fn non_mock_synthetic_pcap_replay_closes_ab_and_preserves_aa_files() {
    let temp = tempfile::tempdir().unwrap();
    let output = std::env::var_os("LANGMUIR_S02_EVIDENCE")
        .map_or_else(|| temp.path().to_path_buf(), PathBuf::from);
    fs::create_dir_all(&output).unwrap();
    let aa = output.join("SYNTHETIC-replay-AA");
    let ab = output.join("SYNTHETIC-replay-AB.csv");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/qa/lvds-multitype-s02/SYNTHETIC-20AA-1AB-1AC.pcapng");
    let mut controller = controller();
    assert!(controller.start_scientific_capture(ScientificSaveFormat::LpdAndCsv, &aa));
    controller.start_excitation_csv(&ab.to_string_lossy());
    let mut replay = PcapReplay::start(fixture.to_string_lossy().into_owned()).unwrap();
    replay.started = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
    controller.replay = Some(replay);
    controller.lvds_connected = true;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !controller.replay.as_ref().unwrap().progress.finished {
        assert!(Instant::now() < deadline, "PCAP reader failed to reach EOF");
        controller.drain_replay();
        thread::yield_now();
    }
    let replay = controller.replay.as_ref().unwrap();
    assert_eq!(replay.progress.error, None);
    assert_eq!(replay.progress.target_udp_packets, 22);
    assert_eq!(replay.ingress.receive_stats.accepted_frames, 20);
    assert_eq!(replay.ingress.receive_stats.excitation_decoded, 1);
    assert_eq!(replay.ingress.receive_stats.gnss_attitude_frames, 1);
    assert_eq!(replay.ingress.metrics.udp_sequence.gap_candidates, 0);
    let status = controller.excitation_storage_status();
    assert_eq!(
        (
            status.enqueued,
            status.written,
            status.rows_written,
            status.dropped
        ),
        (1, 1, 100, 0)
    );
    assert!(status.closed);
    assert!(!status.active);
    assert_eq!(status.error, None);
    assert_eq!(controller.storage_status.csv_rows_written, 2000);
    assert_eq!(
        fs::metadata(aa.with_extension("lpd")).unwrap().len(),
        40 + 20 * 409
    );
    assert_eq!(
        fs::read_to_string(aa.with_extension("csv"))
            .unwrap()
            .lines()
            .count(),
        2001
    );
    assert_eq!(fs::read_to_string(&ab).unwrap().lines().count(), 101);
    println!("S02 replay CSV evidence: {}", ab.display());
}

#[test]
fn live_ab_csv_provenance_uses_the_actual_raw_archive_session() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("SYNTHETIC-ab.lvdsraw");
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let receiver =
        UdpLvdsReceiver::bind("127.0.0.1:0".parse().unwrap(), sender.local_addr().unwrap())
            .unwrap();
    let destination = receiver.socket.local_addr().unwrap();
    let mut controller = controller();
    controller.lvds_udp = Some(receiver);
    controller.start_raw_recording(&archive.to_string_lossy());
    sender.send_to(&udp(7, &ab(0)), destination).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while controller
        .lvds_udp
        .as_ref()
        .unwrap()
        .ingress
        .excitation_latest
        .is_none()
    {
        assert!(Instant::now() < deadline);
        controller.drain_udp_lvds();
        thread::yield_now();
    }
    let session = controller
        .lvds_udp
        .as_ref()
        .unwrap()
        .ingress
        .excitation_latest
        .as_ref()
        .unwrap()
        .session
        .clone();
    controller.stop_raw_recording();
    let reader = langmuir_storage::RawReader::open(&archive).unwrap();
    assert_eq!(session, hex_bytes(&reader.header().session_id));
    assert_ne!(session, "unavailable");
}

#[test]
fn source_disconnect_closes_active_ab_csv_but_idle_preparation_survives() {
    let temp = tempfile::tempdir().unwrap();
    let mut controller = controller();
    controller.start_excitation_csv(&temp.path().join("prepared.csv").to_string_lossy());
    controller.disconnect(LinkKind::Lvds);
    assert!(controller.excitation_storage_status().active);
    controller.lvds_connected = true;
    controller.disconnect(LinkKind::Lvds);
    assert!(controller.excitation_storage_status().closed);
    assert!(!controller.excitation_storage_status().active);
}
