//! Independent on-disk checks; the ignored acceptance requires the original capture.

use super::*;
use crate::bridge::ScientificSaveFormat;

#[test]
fn csv_keeps_accepted_order_and_raw_samples_with_display_backpressure() {
    let directory = tempdir().unwrap();
    let stem = directory.path().join("accepted-science");
    let (mut controller, _critical, display) = controller_with_display_capacity(1);
    assert!(controller.start_scientific_capture(ScientificSaveFormat::Csv, &stem));
    let mut ingress = UdpLvdsIngress::new(SOURCE.parse().unwrap());
    let now = Instant::now();
    let first = next_golden();
    let backward = golden();
    for (sequence, wire) in [
        (65_535, &first),
        (65_536, &backward),
        (65_536, &backward),
        (65_537, &first),
    ] {
        feed(&mut ingress, &mut controller, sequence, wire, now);
    }
    for (sequence, offset, replacement) in [
        (70_000, 0, 0), // header
        (70_001, 2, 0), // address
        (70_002, 4, 0), // declared length
    ] {
        let mut invalid = first.clone();
        invalid[offset] = replacement;
        feed(&mut ingress, &mut controller, sequence, &invalid, now);
    }
    let mut invalid_time = first.clone();
    invalid_time[11..15].copy_from_slice(&1_000_000_u32.to_be_bytes());
    feed(&mut ingress, &mut controller, 70_003, &invalid_time, now);
    let mut invalid_adc = first.clone();
    invalid_adc[266] = 0xFF;
    feed(&mut ingress, &mut controller, 70_004, &invalid_adc, now);
    feed(&mut ingress, &mut controller, 70_005, &first[..268], now);
    controller.stop_acquisition();
    let expected = [
        langmuir_core::decode_scientific_frame(&first).unwrap(),
        langmuir_core::decode_scientific_frame(&backward).unwrap(),
        langmuir_core::decode_scientific_frame(&first).unwrap(),
    ];
    assert_eq!(assert_csv(&stem.with_extension("csv"), &expected), 300);
    assert_eq!(controller.storage_status.csv_rows_written, 300);
    assert_eq!(controller.storage_status.enqueued, 3);
    assert!(controller.storage_status.closed);
    assert_eq!(controller.storage_status.error, None);
    assert_eq!(ingress.receive_stats.accepted_frames, 3);
    assert_eq!(ingress.receive_stats.duplicates, 1);
    assert_eq!(
        display.len(),
        1,
        "display congestion must not discard science CSV frames"
    );
    assert!(
        controller
            .lvds_display_dropped
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 2
    );
}

fn assert_csv(path: &Path, frames: &[ScientificFrame]) -> u64 {
    use std::io::BufRead as _;
    let mut lines = std::io::BufReader::new(fs::File::open(path).unwrap()).lines();
    assert_eq!(
        lines.next().unwrap().unwrap(),
        "frame_index,frame_seconds,frame_microseconds,frame_seq_u8,sample_index,sample_time_us,ch1_code10,ch2_code10,ch1_adc16_reconstructed,ch2_adc16_reconstructed,integrity"
    );
    let mut rows = 0_u64;
    for (frame_index, frame) in frames.iter().enumerate() {
        for sample_index in 0..100 {
            let line = lines.next().expect("missing complete sample row").unwrap();
            let fields: Vec<_> = line.split(',').collect();
            assert_eq!(fields.len(), 11);
            let actual: Vec<i64> = fields[..10].iter().map(|v| v.parse().unwrap()).collect();
            let c1 = i64::from(frame.channel_1[sample_index]);
            let c2 = i64::from(frame.channel_2[sample_index]);
            assert_eq!(
                actual,
                [
                    i64::try_from(frame_index).unwrap(),
                    i64::from(frame.timestamp.utc_seconds),
                    i64::from(frame.timestamp.microseconds),
                    i64::from(frame.frame_seq_u8),
                    i64::try_from(sample_index).unwrap(),
                    i64::from(frame.timestamp.utc_seconds) * 1_000_000
                        + i64::from(frame.timestamp.microseconds)
                        + i64::try_from(sample_index).unwrap() * 500,
                    c1,
                    c2,
                    c1 * 64 - 32768,
                    c2 * 64 - 32768
                ]
            );
            assert_eq!(fields[10], "CRC_NOT_CHECKED");
            rows += 1;
        }
    }
    assert!(lines.next().is_none(), "unexpected extra CSV records");
    rows
}

/// Direct envelope and 40-bit packed-code reference; no production decoder.
fn independently_decode_adc(capture: &Path) -> Vec<ScientificFrame> {
    let mut reader = langmuir_transport::PcapNgReader::new(std::io::BufReader::new(
        fs::File::open(capture).unwrap(),
    ));
    let mut frames = Vec::new();
    let mut recent = std::collections::VecDeque::<(u32, Duration, Vec<u8>)>::new();
    while let Some(packet) = reader.next_packet().unwrap() {
        let Some(udp) = packet
            .udp
            .filter(|udp| udp.source.to_string() == super::super::DEPLOYED_LVDS_UDP_SOURCE)
        else {
            continue;
        };
        // This acceptance fixture consists of single-datagram frames. Inspect
        // the envelope directly so the oracle does not reuse its parser.
        let envelope = &udp.payload;
        assert!(envelope.len() >= 24);
        assert_eq!(&envelope[..4], &[0x55, 0xAA, 1, 0xA5]);
        assert_eq!(u32::from_be_bytes(envelope[8..12].try_into().unwrap()), 269);
        assert_eq!(u16::from_be_bytes(envelope[12..14].try_into().unwrap()), 0);
        assert_eq!(u16::from_be_bytes(envelope[14..16].try_into().unwrap()), 1);
        assert_eq!(u32::from_be_bytes(envelope[16..20].try_into().unwrap()), 0);
        assert_eq!(
            u16::from_be_bytes(envelope[20..22].try_into().unwrap()),
            269
        );
        let transport_sequence = u32::from_be_bytes(envelope[4..8].try_into().unwrap());
        let wire = &envelope[24..];
        if wire.len() != 269
            || wire[..7] != [0xEB, 0x90, 0x55, 0xAA, 1, 7, 0xAA]
            || u32::from_be_bytes(wire[11..15].try_into().unwrap()) >= 1_000_000
            || wire[266] != 0
        {
            continue;
        }
        while recent.front().is_some_and(|(_, inserted, _)| {
            packet.timestamp.saturating_sub(*inserted) >= Duration::from_secs(5)
        }) {
            recent.pop_front();
        }
        if recent
            .iter()
            .any(|(prior, _, _)| *prior == transport_sequence)
        {
            continue;
        }
        if recent.len() == 256 {
            recent.pop_front();
        }
        recent.push_back((transport_sequence, packet.timestamp, wire.to_vec()));
        frames.push(reference_frame(wire));
    }
    frames
}

fn rollback_indexes(frames: &[ScientificFrame]) -> Vec<usize> {
    frames
        .windows(2)
        .enumerate()
        .filter_map(|(index, frames)| {
            let time = |frame: &ScientificFrame| {
                u64::from(frame.timestamp.utc_seconds) * 1_000_000
                    + u64::from(frame.timestamp.microseconds)
            };
            (time(&frames[1]) < time(&frames[0])).then_some(index + 1)
        })
        .collect()
}

#[test]
#[ignore = "requires new-protocol capture; set LANGMUIR_USER_PCAP (synthetic fixtures must be labeled)"]
fn e8_user_capture_scientific_csv_all_modes_readback() {
    let capture = std::env::var_os("LANGMUIR_USER_PCAP").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../new-protocol.pcapng"),
        std::path::PathBuf::from,
    );
    let original =
        fs::read(&capture).expect("original user PCAP required; do not substitute mock data");
    let census = independently_count_capture(&capture);
    let expected = independently_decode_adc(&capture);
    assert_eq!(
        expected, census.frames,
        "independent envelope, admission, time and ADC oracle"
    );
    assert_ne!(expected, []);
    let rollback_indexes = rollback_indexes(&expected);
    for format in [
        ScientificSaveFormat::Csv,
        ScientificSaveFormat::Lpd,
        ScientificSaveFormat::LpdAndCsv,
    ] {
        let (mut controller, _critical, display) = controller_with_display_capacity(128);
        controller.start_pcap_replay(capture.to_str().unwrap(), Some(format));
        assert!(
            controller.acquisition_running,
            "writer created before replay delivery"
        );
        controller.replay.as_mut().unwrap().started = Instant::now()
            .checked_sub(Duration::from_hours(24))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut display_frames = 0_u64;
        while !controller.replay.as_ref().unwrap().progress.finished {
            controller.drain_replay();
            display_frames += u64::try_from(display.try_iter().count()).unwrap();
            assert!(Instant::now() < deadline, "PCAP reader timeout");
            std::thread::yield_now();
        }
        let status = &controller.storage_status;
        let replay = controller.replay.as_ref().unwrap();
        assert_eq!(replay.progress.error, None);
        assert_eq!(
            (
                replay.progress.total_packets,
                replay.progress.target_udp_packets
            ),
            (census.total, census.target)
        );
        assert_eq!(
            replay.ingress.receive_stats.accepted_frames,
            u64::try_from(expected.len()).unwrap()
        );
        assert!(!controller.acquisition_running);
        assert!(status.closed);
        assert_eq!(status.error, None);
        assert_eq!(status.enqueued, u64::try_from(expected.len()).unwrap());
        assert_eq!(status.sink_written, status.enqueued);
        assert_eq!(display_frames, status.enqueued);
        if let Some(path) = &status.csv_path {
            let rows = assert_csv(Path::new(path), &expected);
            assert_eq!(rows, status.csv_rows_written);
            assert_eq!(status.csv_frames_written, status.enqueued);
            println!(
                "CSV_ACCEPTANCE format={format:?} total={} target={} accepted={} rows={rows} lines={} bytes={} closed={} path={path}",
                census.total,
                census.target,
                expected.len(),
                rows + 1,
                fs::metadata(path).unwrap().len(),
                status.closed
            );
        }
        if let Some(path) = &status.lpd_path {
            assert_scientific_file(&fs::read(path).unwrap(), &expected);
            assert_eq!(status.lpd_frames_written, status.enqueued);
        }
    }
    if let Some(path) = std::env::var_os("LANGMUIR_GUI_SCIENCE_CSV") {
        let rows = assert_csv(Path::new(&path), &expected);
        println!(
            "GUI_CSV_READBACK rows={rows} all_fields=PASS path={}",
            Path::new(&path).display()
        );
    }
    println!(
        "CSV_INDEPENDENT_READBACK frames={} rows={} ADC_pairs={} rollback_indexes={rollback_indexes:?} first={:?} random_middle={:?} last={:?}",
        expected.len(),
        expected.len() * 100,
        expected.len() * 100,
        expected.first().map(|f| (f.frame_seq_u8, f.timestamp)),
        expected
            .get(expected.len() * 37 / 100)
            .map(|f| (f.frame_seq_u8, f.timestamp)),
        expected.last().map(|f| (f.frame_seq_u8, f.timestamp))
    );
    assert_eq!(fs::read(capture).unwrap(), original);
}
