//! Synthetic unit fixtures and opt-in, read-only original TXT/GUI acceptance.
use super::*;
use crate::bridge::ScientificSaveFormat;

pub(super) fn golden_frame(inner_sequence: u8) -> Vec<u8> {
    let mut wire = vec![0; 269];
    wire[..7].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA]);
    wire[7..11].copy_from_slice(&1_694_498_816_u32.to_be_bytes());
    wire[11..15].copy_from_slice(&999_800_u32.to_be_bytes());
    for group in wire[15..140].as_chunks_mut::<5>().0 {
        group.copy_from_slice(&[0xFF, 0xC0, 0x08, 0x00, 0x01]);
    }
    for group in wire[140..265].as_chunks_mut::<5>().0 {
        group.copy_from_slice(&[0x00, 0x3F, 0xF0, 0x03, 0xFF]);
    }
    wire[265] = inner_sequence;
    wire[267..].copy_from_slice(&[0xDE, 0xAD]);
    wire
}

#[test]
fn new_protocol_enters_plot_and_both_files_without_crc_or_time_shift() {
    let directory = tempfile::tempdir().unwrap();
    let stem = directory.path().join("new-science");
    let (mut controller, _critical, display) = super::e2e_tests::controller();
    assert!(controller.start_scientific_capture(ScientificSaveFormat::LpdAndCsv, &stem));
    let mut ingress = UdpLvdsIngress::new("192.0.2.1:1534".parse().unwrap());
    let wire = golden_frame(255);
    super::e2e_tests::feed(
        &mut ingress,
        &mut controller,
        0x1234_5678,
        &wire,
        Instant::now(),
    );
    controller.stop_acquisition();
    assert_eq!(ingress.receive_stats.accepted_frames, 1);
    let BackendEvent::ScientificSamples {
        channel_1,
        channel_2,
        sample_interval_seconds,
        channel_1_start_seconds,
        ..
    } = display.try_recv().unwrap()
    else {
        panic!("science samples missing")
    };
    assert_eq!(sample_interval_seconds, 0.0005);
    assert_eq!(channel_1.len(), 100);
    assert_eq!(&channel_1[..4], &[32704.0, -32768.0, 0.0, -32704.0]);
    assert_eq!(&channel_2[..4], &[-32768.0, 32704.0, -32768.0, 32704.0]);
    assert!((channel_1_start_seconds - 1_694_498_816.999_8).abs() < 0.000_001);
    let csv = std::fs::read_to_string(stem.with_extension("csv")).unwrap();
    let lines: Vec<_> = csv.lines().collect();
    assert_eq!(lines.len(), 101);
    assert_eq!(
        lines[1],
        "0,1694498816,999800,255,0,1694498816999800,1023,0,32704,-32768,CRC_NOT_CHECKED"
    );
    assert_eq!(
        lines[100],
        "0,1694498816,999800,255,99,1694498817049300,1,1023,-32704,32704,CRC_NOT_CHECKED"
    );
    let bytes = std::fs::read(stem.with_extension("lpd")).unwrap();
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 3);
    assert_eq!(bytes.len(), 40 + 409);
}

#[test]
fn inner_sequence_wrap_and_device_rollback_preserve_csv_order_independently_of_udp() {
    let directory = tempfile::tempdir().unwrap();
    let stem = directory.path().join("wrap-rollback");
    let (mut controller, _critical, _display) = super::e2e_tests::controller();
    assert!(controller.start_scientific_capture(ScientificSaveFormat::LpdAndCsv, &stem));
    let mut ingress = UdpLvdsIngress::new("192.0.2.1:1534".parse().unwrap());
    let first = golden_frame(255);
    let mut second = golden_frame(0);
    second[7..11].copy_from_slice(&1_u32.to_be_bytes());
    for (outer, wire) in [(u32::MAX, &first), (0, &second)] {
        super::e2e_tests::feed(&mut ingress, &mut controller, outer, wire, Instant::now());
    }
    controller.stop_acquisition();
    assert_eq!(ingress.metrics.udp_sequence.wrap_events, 1);
    assert_eq!(ingress.receive_stats.accepted_frames, 2);
    let mut reader = langmuir_storage::BinaryV3Reader::open(stem.with_extension("lpd")).unwrap();
    let first = reader.next_frame().unwrap().unwrap();
    let second = reader.next_frame().unwrap().unwrap();
    assert_eq!((first.frame_seq_u8, second.frame_seq_u8), (255, 0));
    assert_eq!(second.timestamp.utc_seconds, 1);
    assert!(reader.next_frame().unwrap().is_none());
    let csv = std::fs::read_to_string(stem.with_extension("csv")).unwrap();
    let lines: Vec<_> = csv.lines().collect();
    assert_eq!(lines.len(), 201);
    assert_eq!(
        lines[101],
        "1,1,999800,0,0,1999800,1023,0,32704,-32768,CRC_NOT_CHECKED"
    );
}

#[test]
fn synthetic_txt_loader_preserves_hex_bytes_and_rejects_incomplete_lines() {
    let wire = golden_frame(7);
    let mut hex = String::new();
    for byte in &wire {
        write!(hex, "{byte:02X}").unwrap();
    }
    assert_eq!(
        parse_original_txt(&format!("  {hex} \r\n")).unwrap(),
        [wire]
    );
    assert!(parse_original_txt(&hex[..536]).is_err());
    assert!(parse_original_txt("").is_err());
    assert!(parse_original_txt(&format!("{hex}\n\n{hex}")).is_err());
    let invalid = format!("GG{}", &hex[2..]);
    assert!(parse_original_txt(&invalid).is_err());
}

#[test]
fn synthetic_reference_check_detects_each_csv_column_and_missing_samples() {
    let frame = ScientificFrame {
        timestamp: langmuir_core::Timestamp {
            utc_seconds: 1,
            microseconds: 0,
        },
        frame_seq_u8: 7,
        channel_1: Box::new([0; 100]),
        channel_2: Box::new([1023; 100]),
    };
    let mut csv = format!("{REFERENCE_CSV_HEADER}\n");
    for index in 0..100 {
        let time = 1_000_000 + index * 500;
        writeln!(
            csv,
            "0,1,0,7,{index},{time},0,1023,-32768,32704,CRC_NOT_CHECKED"
        )
        .unwrap();
    }
    assert_eq!(
        verify_reference_csv(&csv, std::slice::from_ref(&frame)).unwrap(),
        100
    );
    for column in 0..11 {
        let mut rows: Vec<_> = csv.lines().map(str::to_owned).collect();
        let mut values: Vec<_> = rows[1].split(',').map(str::to_owned).collect();
        values[column] = "999".to_owned();
        rows[1] = values.join(",");
        assert!(verify_reference_csv(&rows.join("\n"), std::slice::from_ref(&frame)).is_err());
    }
    let incomplete = csv.lines().take(100).collect::<Vec<_>>().join("\n");
    assert!(verify_reference_csv(&incomplete, &[frame]).is_err());
}

const REFERENCE_CSV_HEADER: &str = "frame_index,frame_seconds,frame_microseconds,frame_seq_u8,sample_index,sample_time_us,ch1_code10,ch2_code10,ch1_adc16_reconstructed,ch2_adc16_reconstructed,integrity";

fn parse_original_txt(text: &str) -> Result<Vec<Vec<u8>>, String> {
    let mut frames = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let hex = line.trim();
        if hex.len() != 538 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!(
                "TXT line {} must contain exactly 269 HEX bytes",
                index + 1
            ));
        }
        let bytes = hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16)
                    .expect("ASCII HEX checked above")
            })
            .collect();
        frames.push(bytes);
    }
    if frames.is_empty() {
        return Err("TXT contains no frame lines".to_owned());
    }
    Ok(frames)
}

fn verify_reference_csv(text: &str, frames: &[ScientificFrame]) -> Result<usize, String> {
    let mut lines = text.lines();
    if lines.next() != Some(REFERENCE_CSV_HEADER) {
        return Err("reference CSV must have the exact 11-column science header".to_owned());
    }
    let names: Vec<_> = REFERENCE_CSV_HEADER.split(',').collect();
    let mut count = 0;
    for (index, line) in lines.enumerate() {
        let frame = frames
            .get(index / 100)
            .ok_or_else(|| format!("extra CSV row {index}"))?;
        let fields: Vec<_> = line.split(',').collect();
        if fields.len() != 11 {
            return Err(format!("CSV data row {index} must have 11 columns"));
        }
        let actual = decoded_reference_values(frame, index / 100, index % 100);
        for (column, (&field, &value)) in fields[..10].iter().zip(&actual).enumerate() {
            let reference = field
                .parse::<i64>()
                .map_err(|error| format!("CSV row {index} {}: {error}", names[column]))?;
            if reference != value {
                return Err(format!(
                    "CSV row {index} {}: reference {reference}, Rust {value}",
                    names[column]
                ));
            }
        }
        if fields[10] != "CRC_NOT_CHECKED" {
            return Err(format!("CSV row {index} integrity must be CRC_NOT_CHECKED"));
        }
        count += 1;
    }
    if count != frames.len() * 100 {
        return Err(format!(
            "CSV has {count} sample rows for {} frames",
            frames.len()
        ));
    }
    Ok(count)
}

fn decoded_reference_values(
    frame: &ScientificFrame,
    frame_index: usize,
    index: usize,
) -> [i64; 10] {
    [
        i64::try_from(frame_index).unwrap(),
        i64::from(frame.timestamp.utc_seconds),
        i64::from(frame.timestamp.microseconds),
        i64::from(frame.frame_seq_u8),
        i64::try_from(index).unwrap(),
        i64::try_from(frame.sample_time_us(index).unwrap()).unwrap(),
        i64::from(frame.channel_1[index]),
        i64::from(frame.channel_2[index]),
        i64::from(reconstruct_adc16(frame.channel_1[index])),
        i64::from(reconstruct_adc16(frame.channel_2[index])),
    ]
}

struct OriginalAcceptance {
    raw: Vec<Vec<u8>>,
    decoded: Vec<ScientificFrame>,
    reference_csv: String,
}

fn required_acceptance_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| {
        panic!("explicit original-data acceptance requires {name}; missing input is not a skip")
    }))
}

fn read_original_acceptance() -> OriginalAcceptance {
    let txt = fs::read_to_string(required_acceptance_path("LANGMUIR_USER_LVDS_TXT"))
        .expect("read original lvds.txt without rewriting or substituting fixtures");
    let raw = parse_original_txt(&txt).unwrap();
    assert_eq!(raw.len(), 20, "supplied original TXT baseline");
    let decoded: Vec<_> = raw
        .iter()
        .enumerate()
        .map(|(index, wire)| {
            assert_eq!(&wire[..7], &[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA]);
            assert_eq!(wire[266], 0, "TXT frame {index} reserved byte");
            let frame = langmuir_core::decode_scientific_frame(wire)
                .unwrap_or_else(|error| panic!("TXT frame {index}: {error}"));
            assert_eq!(frame.timestamp.utc_seconds, 1_700_000_000);
            assert_eq!(
                frame.timestamp.microseconds,
                u32::try_from(index).unwrap() * 50_000
            );
            assert_eq!(usize::from(frame.frame_seq_u8), index);
            for sample in 0..100 {
                assert!(frame.channel_1[sample] <= 1023 && frame.channel_2[sample] <= 1023);
                assert_eq!(
                    frame.sample_time_us(sample).unwrap(),
                    1_700_000_000_000_000 + u64::try_from(index * 100 + sample).unwrap() * 500
                );
            }
            frame
        })
        .collect();
    assert_eq!(&decoded[0].channel_1[..4], &[886, 805, 723, 642]);
    assert_eq!(&decoded[0].channel_2[..4], &[846, 784, 721, 659]);
    assert_eq!(reconstruct_adc16(decoded[0].channel_1[0]), 23_936);
    assert_eq!(reconstruct_adc16(decoded[0].channel_2[0]), 21_376);
    let reference_csv = fs::read_to_string(required_acceptance_path("LANGMUIR_USER_REFERENCE_CSV"))
        .expect("read independent Python reference CSV generated from original TXT");
    assert_eq!(
        verify_reference_csv(&reference_csv, &decoded).unwrap(),
        2000
    );
    OriginalAcceptance {
        raw,
        decoded,
        reference_csv,
    }
}

#[test]
#[ignore = "requires original lvds.txt and independent reference; missing required env fails"]
fn final55_original_txt_decoder_and_address_gate_contract() {
    let original = read_original_acceptance();
    let directory = tempfile::tempdir().unwrap();
    for wrong_address in [false, true] {
        let stem = directory.path().join(if wrong_address {
            "negative04ff"
        } else {
            "original55aa"
        });
        let (mut controller, _critical, display) = super::e2e_tests::controller();
        assert!(controller.start_scientific_capture(ScientificSaveFormat::LpdAndCsv, &stem));
        let mut ingress = UdpLvdsIngress::new("192.0.2.1:1534".parse().unwrap());
        let now = Instant::now();
        let mut plotted = 0;
        let mut points = [0; 2];
        for (index, raw) in original.raw.iter().enumerate() {
            let mut wire = raw.clone();
            if wrong_address {
                wire[2..4].copy_from_slice(&[0x04, 0xFF]);
                assert!(matches!(
                    langmuir_core::decode_scientific_frame(&wire),
                    Err(FrameError::UnexpectedField {
                        field: "sender address",
                        expected: 0x55,
                        actual: 0x04,
                        ..
                    })
                ));
            }
            super::e2e_tests::feed(
                &mut ingress,
                &mut controller,
                u32::try_from(index).unwrap(),
                &wire,
                now + Duration::from_millis(u64::try_from(index).unwrap() * 50),
            );
            for event in display.try_iter() {
                assert_original_plot(event, &original.decoded[index], &mut points);
                plotted += 1;
            }
        }
        controller.stop_acquisition();
        assert_eq!(ingress.receive_stats.udp_packets, 20);
        assert_eq!(ingress.receive_stats.complete_frames, 20);
        if wrong_address {
            assert_eq!((ingress.receive_stats.accepted_frames, plotted), (0, 0));
            assert_eq!(points, [0; 2]);
            assert_eq!(ingress.receive_stats.parsed_frames, 0);
            assert_eq!(ingress.receive_stats.field_errors, 20);
            assert_eq!(ingress.receive_stats.parse_failures, 20);
            assert_eq!(ingress.metrics.scientific_frames_delivered, 0);
            assert_eq!(controller.storage_status.enqueued, 0);
            assert_eq!(controller.storage_status.sink_written, 0);
            assert_eq!(controller.storage_status.csv_rows_written, 0);
            assert_eq!(controller.storage_status.csv_frames_written, 0);
            assert_eq!(controller.storage_status.lpd_frames_written, 0);
            assert_eq!(
                fs::read_to_string(stem.with_extension("csv"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
            assert_eq!(fs::metadata(stem.with_extension("lpd")).unwrap().len(), 40);
        } else {
            assert_eq!((ingress.receive_stats.accepted_frames, plotted), (20, 20));
            assert_eq!(ingress.receive_stats.structure_valid_frames, 20);
            assert_eq!(ingress.receive_stats.parsed_frames, 20);
            assert_eq!(points, [2000; 2]);
            assert_eq!(ingress.receive_stats.parse_failures, 0);
            assert_eq!(controller.storage_status.sink_written, 20);
            assert_native_science_files(
                &stem.with_extension("csv"),
                &stem.with_extension("lpd"),
                &original,
            );
        }
        assert!(controller.storage_status.closed);
        assert_eq!(controller.storage_status.error, None);
    }
}

fn assert_original_plot(event: BackendEvent, expected: &ScientificFrame, points: &mut [usize; 2]) {
    let BackendEvent::ScientificSamples {
        channel_1,
        channel_2,
        channel_1_start_seconds,
        channel_2_start_seconds,
        sample_interval_seconds,
    } = event
    else {
        panic!("original frame must use the scientific plot route");
    };
    assert_eq!(sample_interval_seconds, 0.0005);
    assert_eq!(channel_1_start_seconds, channel_2_start_seconds);
    assert_eq!(
        channel_1_start_seconds,
        f64::from(expected.timestamp.utc_seconds)
            + f64::from(expected.timestamp.microseconds) / 1_000_000.0
    );
    for (index, (channel, decoded)) in [
        (&channel_1, &expected.channel_1),
        (&channel_2, &expected.channel_2),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(channel.len(), 100);
        for (&value, &code) in channel.iter().zip(decoded.iter()) {
            assert_eq!(value, f64::from(reconstruct_adc16(code)));
        }
        points[index] += channel.len();
    }
}

#[test]
#[ignore = "requires original TXT/reference, derived PCAP and complete native GUI CSV/LPD; missing env fails"]
fn final55_derived_pcap_and_native_files_match_original_txt() {
    let original = read_original_acceptance();
    assert_derived_pcap(&original.raw);
    assert_native_science_files(
        &required_acceptance_path("LANGMUIR_GUI_SCIENCE_CSV"),
        &required_acceptance_path("LANGMUIR_GUI_SCIENCE_FILE"),
        &original,
    );
}

fn assert_derived_pcap(raw: &[Vec<u8>]) {
    let source: SocketAddr = std::env::var("LANGMUIR_USER_PCAP_SOURCE")
        .expect("LANGMUIR_USER_PCAP_SOURCE required")
        .parse()
        .unwrap();
    let destination: SocketAddr = std::env::var("LANGMUIR_USER_PCAP_DESTINATION")
        .expect("LANGMUIR_USER_PCAP_DESTINATION required")
        .parse()
        .unwrap();
    let start: u32 = std::env::var("LANGMUIR_USER_OUTER_SEQUENCE_START")
        .expect("LANGMUIR_USER_OUTER_SEQUENCE_START required")
        .parse()
        .unwrap();
    let mut reader = PcapNgReader::new(std::io::BufReader::new(
        fs::File::open(required_acceptance_path("LANGMUIR_USER_PCAP")).unwrap(),
    ));
    let mut first_time = None;
    let mut count = 0;
    while let Some(packet) = reader.next_packet().unwrap() {
        let expected = raw.get(count).expect("derived PCAP has extra packet");
        let first = *first_time.get_or_insert(packet.timestamp);
        assert_eq!(
            packet
                .timestamp
                .checked_sub(first)
                .expect("derived packet timestamps must not go backward"),
            Duration::from_millis(u64::try_from(count).unwrap() * 50)
        );
        let udp = packet.udp.expect("every derived packet must contain UDP");
        assert_eq!((udp.source, udp.destination), (source, destination));
        let datagram = parse_udp_lvds_datagram(&udp.payload).unwrap();
        assert_eq!(
            datagram.header.frame_sequence,
            start.wrapping_add(u32::try_from(count).unwrap())
        );
        assert_eq!(
            (datagram.header.frame_length, datagram.header.segment_length),
            (269, 269)
        );
        assert_eq!(
            (datagram.header.segment_index, datagram.header.segment_count),
            (0, 1)
        );
        assert_eq!((datagram.header.byte_offset, datagram.header.flags), (0, 0));
        assert_eq!(
            datagram.payload, expected,
            "derived PCAP inner frame {count} must preserve original TXT bytes"
        );
        count += 1;
    }
    assert_eq!(count, raw.len());
}

fn assert_native_science_files(csv: &Path, lpd: &Path, original: &OriginalAcceptance) {
    let csv_text = fs::read_to_string(csv).unwrap();
    assert_eq!(
        verify_reference_csv(&csv_text, &original.decoded).unwrap(),
        2000
    );
    assert_eq!(
        csv_text.lines().collect::<Vec<_>>(),
        original.reference_csv.lines().collect::<Vec<_>>()
    );
    let bytes = fs::read(lpd).unwrap();
    assert_eq!(bytes.len(), 8220);
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 3);
    let mut reader = langmuir_storage::BinaryV3Reader::open(lpd).unwrap();
    let mut records = 0;
    while let Some(frame) = reader.next_frame().unwrap() {
        assert_eq!(
            &frame,
            original.decoded.get(records).expect("extra LPD record")
        );
        records += 1;
    }
    assert_eq!(records, original.decoded.len());
}
