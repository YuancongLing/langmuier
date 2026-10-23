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
    invalid_adc[15..17].copy_from_slice(&0_u16.to_be_bytes());
    invalid_adc[19] = 0xFF;
    feed(&mut ingress, &mut controller, 70_004, &invalid_adc, now);
    feed(&mut ingress, &mut controller, 70_005, &first[..268], now);
    controller.stop_acquisition();
    let expected = [
        langmuir_core::decode_scientific_frame_with_sequence(&first, 65_535).unwrap(),
        langmuir_core::decode_scientific_frame_with_sequence(&backward, 0).unwrap(),
        langmuir_core::decode_scientific_frame_with_sequence(&first, 1).unwrap(),
    ];
    assert_eq!(assert_csv(&stem.with_extension("csv"), &expected), 375);
    assert_eq!(controller.storage_status.csv_rows_written, 375);
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
        "frame_index,sequence_u16,frame_seconds,frame_microseconds,sample_index,sample_time_us,ch1_adc,ch2_adc,integrity"
    );
    let mut rows = 0_u64;
    for (frame_index, frame) in frames.iter().enumerate() {
        for sample_index in 0..125 {
            let line = lines.next().expect("missing complete sample row").unwrap();
            let fields: Vec<_> = line.split(',').collect();
            assert_eq!(
                fields.len(),
                9,
                "CSV must contain only the declared columns"
            );
            let actual: Vec<u64> = fields[..8]
                .iter()
                .map(|value| value.parse().expect("CSV numeric fields are integers"))
                .collect();
            let sample_time = u64::from(frame.timestamp.utc_seconds) * 1_000_000
                + u64::from(frame.timestamp.microseconds)
                + u64::try_from(sample_index).unwrap() * 400;
            assert_eq!(
                actual,
                [
                    u64::try_from(frame_index).unwrap(),
                    u64::from(frame.sequence),
                    u64::from(frame.timestamp.utc_seconds),
                    u64::from(frame.timestamp.microseconds),
                    u64::try_from(sample_index).unwrap(),
                    sample_time,
                    u64::from(frame.channel_1[sample_index]),
                    u64::from(frame.channel_2[sample_index]),
                ],
                "frame={frame_index} sample={sample_index}"
            );
            assert_eq!(fields[8], "CRC_NOT_CHECKED");
            rows += 1;
        }
    }
    assert!(lines.next().is_none(), "unexpected extra CSV records");
    rows
}

/// Reconstruct ADC values directly from capture bytes, independently of the
/// production differential decoder and the chart representation.
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
            || !reference_adc_valid(wire)
        {
            continue;
        }
        let seconds = u32::from_be_bytes(wire[7..11].try_into().unwrap());
        let micros = u32::from_be_bytes(wire[11..15].try_into().unwrap());
        let sequence = u16::try_from(transport_sequence & 0xFFFF).unwrap();
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
        let mut channels = [[0_u32; 125]; 2];
        for (channel, samples) in channels.iter_mut().enumerate() {
            let base = 15 + channel * 2;
            let mut value = i32::from(u16::from_be_bytes([wire[base], wire[base + 1]]));
            samples[0] = u32::try_from(value).unwrap();
            for (index, sample) in samples.iter_mut().enumerate().skip(1) {
                let delta = i32::from(i8::from_ne_bytes([wire[19 + (index - 1) * 2 + channel]]));
                value += delta;
                *sample = u32::try_from(value).unwrap();
            }
        }
        frames.push(ScientificFrame {
            timestamp: langmuir_core::Timestamp {
                utc_seconds: seconds,
                microseconds: micros,
            },
            sequence,
            channel_1: Box::new(channels[0]),
            channel_2: Box::new(channels[1]),
        });
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
#[ignore = "requires original user capture; set LANGMUIR_USER_PCAP"]
fn e8_user_capture_scientific_csv_all_modes_readback() {
    let capture = std::env::var_os("LANGMUIR_USER_PCAP").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../102301.pcapng"),
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
        expected.len() * 125,
        expected.len() * 125,
        expected.first().map(|f| (f.sequence, f.timestamp)),
        expected
            .get(expected.len() * 37 / 100)
            .map(|f| (f.sequence, f.timestamp)),
        expected.last().map(|f| (f.sequence, f.timestamp))
    );
    assert_eq!(fs::read(capture).unwrap(), original);
}
