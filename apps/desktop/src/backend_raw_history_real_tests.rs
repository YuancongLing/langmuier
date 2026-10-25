//! Explicit real-file acceptance. No fallback, generated input, sockets or commands.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use langmuir_core::{
    FrameQuality, LvdsBitAlignmentRecoverer, RecoveryAnchor, RecoveryContext, RecoveryResult,
    ReferenceCrcStatus, ScientificFrame, parse_udp_lvds_datagram,
};
use langmuir_storage::{
    RawArchive, RawConfig, RawDestination, RawHeader, RawOrigin, RawReader, RawRecord,
};
use langmuir_transport::PcapNgReader;
use serde_json::Value;

fn required_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| {
        panic!("explicit real-file acceptance requires {name}; missing files are not a pass")
    }))
}

fn required_endpoint(name: &str) -> SocketAddr {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("explicit real-file acceptance requires {name}"))
        .parse()
        .unwrap_or_else(|error| panic!("invalid endpoint {name}: {error}"))
}

fn independent_rows() -> Vec<Value> {
    let path = required_path("LANGMUIR_RAW_TEST_REFERENCE_JSONL");
    BufReader::new(File::open(path).unwrap())
        .lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(&line.unwrap())
                .unwrap_or_else(|error| panic!("independent JSONL row {index}: {error}"))
        })
        .collect()
}

fn json_u64(value: &Value, field: &str) -> u64 {
    value[field]
        .as_u64()
        .unwrap_or_else(|| panic!("independent field {field} must be an unsigned integer"))
}

fn compare_preview(frame: &ScientificFrame, selected: &Value, record: usize) -> usize {
    assert_eq!(
        u64::from(frame.timestamp.utc_seconds),
        json_u64(selected, "seconds")
    );
    assert_eq!(
        u64::from(frame.timestamp.microseconds),
        json_u64(selected, "microseconds")
    );
    assert_eq!(u64::from(frame.frame_seq_u8), json_u64(selected, "seq8"));
    assert_eq!(
        frame.sample_time_us(0),
        Some(json_u64(selected, "device_time_us"))
    );
    let mut count = 0;
    for (name, actual) in [
        ("channel_1_code10", &frame.channel_1),
        ("channel_2_code10", &frame.channel_2),
    ] {
        let expected = selected[name]
            .as_array()
            .expect("independent channel array");
        assert_eq!(expected.len(), 100, "record {record} {name}");
        assert_eq!(actual.len(), 100);
        for (sample, (&code, expected)) in actual.iter().zip(expected.iter()).enumerate() {
            assert_eq!(
                Some(u64::from(code)),
                expected.as_u64(),
                "record {record} {name} sample {sample}"
            );
            count += 1;
        }
    }
    count
}

fn compare_recovery(actual: &RecoveryResult, expected: &Value, record: usize) -> usize {
    assert_eq!(
        Some(actual.quality.label()),
        expected["quality"].as_str(),
        "record {record} quality: {}",
        actual.reason
    );
    let selected = &expected["selected"];
    if selected.is_null() {
        assert!(
            actual.frame.is_none(),
            "record {record} has no independently valid prefix"
        );
        assert!(actual.bit_offset.is_none());
        assert_eq!(actual.available_bits, 0);
        return 0;
    }
    assert_eq!(
        actual.bit_offset,
        Some(usize::try_from(json_u64(selected, "bit_start")).unwrap())
    );
    assert_eq!(
        actual.available_bits,
        usize::try_from(json_u64(selected, "available_bits"))
            .unwrap()
            .min(2152)
    );
    assert_eq!(u64::from(actual.shift), json_u64(selected, "shift"));
    assert_eq!(
        actual.validation.full_bits,
        selected["complete_269_bytes"].as_bool().unwrap()
    );
    if actual.quality == FrameQuality::Native {
        // The independent audit may evaluate native CRC offline. Production must not.
        assert_eq!(
            actual.crc_reference.status,
            ReferenceCrcStatus::NotEvaluated
        );
        assert_eq!(actual.crc_reference.received, None);
    } else {
        let calculated = selected["reference_crc"]["calculated"].as_str().unwrap();
        assert_eq!(
            actual.crc_reference.calculated,
            Some(u16::from_str_radix(calculated, 16).unwrap())
        );
        if !actual.validation.full_bits {
            assert_eq!(
                actual.crc_reference.status,
                ReferenceCrcStatus::NotEvaluated
            );
            assert_eq!(
                actual.crc_reference.received, None,
                "missing CRC tail must not be fabricated"
            );
            assert!(actual.bytes.len() < 269);
        }
    }
    compare_preview(
        actual
            .frame
            .as_ref()
            .expect("independently valid science prefix"),
        selected,
        record,
    )
}

#[test]
#[ignore = "requires LANGMUIR_RAW_TEST_PCAP and independently generated LANGMUIR_RAW_TEST_REFERENCE_JSONL"]
fn real_capture_recovery_matches_independent_bit_boundary_audit() {
    let expected = independent_rows();
    assert!(!expected.is_empty(), "explicit actual capture is empty");
    let mut reader = PcapNgReader::new(BufReader::new(
        File::open(required_path("LANGMUIR_RAW_TEST_PCAP")).unwrap(),
    ));
    let (mut index, mut codes) = (0, 0);
    let mut previous = None;
    let mut previous_stream = None;
    let mut quality_counts = BTreeMap::<&str, usize>::new();
    while let Some(packet) = reader.next_packet().unwrap() {
        let Some(udp) = packet.udp else { continue };
        let row = expected
            .get(index)
            .expect("production reader has an extra UDP record");
        assert_eq!(
            row["source"].as_str(),
            Some(udp.source.to_string().as_str())
        );
        assert_eq!(
            row["destination"].as_str(),
            Some(udp.destination.to_string().as_str())
        );
        let stream = (udp.source, udp.destination, row["session"].as_str());
        if previous_stream != Some(stream) {
            previous = None;
        }
        let datagram = parse_udp_lvds_datagram(&udp.payload).unwrap();
        assert_eq!(
            u64::from(datagram.header.frame_sequence),
            json_u64(row, "outer_sequence_u32")
        );
        assert_eq!(
            (datagram.header.segment_index, datagram.header.segment_count),
            (0, 1)
        );
        assert_eq!(datagram.header.byte_offset, 0);
        let result = LvdsBitAlignmentRecoverer::inspect(
            datagram.payload,
            true,
            RecoveryContext {
                udp_sequence: datagram.header.frame_sequence,
                previous,
            },
        );
        codes += compare_recovery(&result, row, index);
        *quality_counts.entry(result.quality.label()).or_default() += 1;
        if matches!(
            result.quality,
            FrameQuality::Native | FrameQuality::Recovered1Bit | FrameQuality::Recovered4Bit
        ) {
            let frame = result.frame.as_ref().unwrap();
            previous = Some(RecoveryAnchor {
                udp_sequence: datagram.header.frame_sequence,
                frame_seq_u8: frame.frame_seq_u8,
                sample_time_us: frame.sample_time_us(0).unwrap(),
            });
        } else if result.quality == FrameQuality::Rejected {
            previous = None;
        }
        previous_stream = Some(stream);
        index += 1;
    }
    assert_eq!(
        index,
        expected.len(),
        "production PCAP reader must reach exact UDP EOF"
    );
    let expected_codes = expected
        .iter()
        .filter(|row| !row["selected"].is_null())
        .count()
        * 200;
    assert_eq!(codes, expected_codes);
    println!(
        "{}",
        serde_json::json!({"status":"PASS","udp_records":index,"preview_code10_values_checked":codes,"quality_counts":quality_counts,"cross_record_bits_used":false})
    );
}

fn capture_records(source: SocketAddr) -> Vec<RawRecord> {
    let mut reader = PcapNgReader::new(BufReader::new(
        File::open(required_path("LANGMUIR_RAW_TEST_PCAP")).unwrap(),
    ));
    let (mut records, mut packet_index) = (Vec::new(), 0_u64);
    while let Some(packet) = reader.next_packet().unwrap() {
        if let Some(udp) = packet.udp {
            records.push(RawRecord {
                id: u64::try_from(records.len()).unwrap() + 1,
                timestamp: packet.timestamp,
                source: udp.source,
                destination: RawDestination::Captured(udp.destination),
                source_matches: udp.source == source,
                origin_packet_index: Some(packet_index),
                payload: udp.payload,
            });
        }
        packet_index += 1;
    }
    records
}

#[test]
#[ignore = "requires explicit LANGMUIR_RAW_TEST_PCAP, EXPECTED_SOURCE, BINDING and new ARCHIVE output path"]
fn real_capture_raw_archive_preserves_every_received_udp_record() {
    let path = required_path("LANGMUIR_RAW_TEST_ARCHIVE");
    assert!(!path.exists(), "exclusive real-file output must be new");
    let source = required_endpoint("LANGMUIR_RAW_TEST_EXPECTED_SOURCE");
    let expected = capture_records(source);
    assert_ne!(expected.len(), 0);
    let created_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let header = RawHeader {
        origin: RawOrigin::PcapReplay,
        created_at,
        expected_source: source,
        binding: required_endpoint("LANGMUIR_RAW_TEST_BINDING"),
        session_id: created_at.as_nanos().to_le_bytes(),
    };
    let mut config = RawConfig::new(&path);
    config.service.queue_capacity = expected.len() + 1;
    config.service.control_timeout = Duration::from_secs(10);
    let mut archive = RawArchive::start(config, header.clone()).unwrap();
    for record in &expected {
        let receipt = archive
            .try_record(
                record.timestamp,
                record.source,
                record.destination,
                record.source_matches,
                record.origin_packet_index,
                &record.payload,
            )
            .unwrap();
        assert_eq!(receipt.id, record.id);
        assert!(
            receipt.queued,
            "raw archive rejected an actual observed UDP payload"
        );
    }
    let snapshot = archive.stop().unwrap();
    let count = u64::try_from(expected.len()).unwrap();
    assert!(snapshot.closed && snapshot.complete, "{snapshot:?}");
    assert_eq!(
        (snapshot.observed, snapshot.enqueued, snapshot.written),
        (count, count, count)
    );
    assert_eq!(snapshot.dropped, 0);
    assert_eq!(snapshot.error, None);
    let payload_bytes: usize = expected.iter().map(|record| record.payload.len()).sum();
    assert_eq!(
        snapshot.payload_bytes,
        u64::try_from(payload_bytes).unwrap()
    );
    let mut reader = RawReader::open(&path).unwrap();
    assert_eq!(reader.header(), &header);
    for record in &expected {
        assert_eq!(
            reader.next_record().unwrap().as_ref(),
            Some(record),
            "raw record {} metadata and exact payload",
            record.id
        );
    }
    assert_eq!(
        reader.next_record().unwrap(),
        None,
        "exact clean footer/EOF required"
    );
    assert_eq!(reader.summary(), Some(&snapshot));
    let bytes = fs::metadata(&path).unwrap().len();
    assert_eq!(
        bytes,
        128 + count * 100 + u64::try_from(payload_bytes).unwrap() + 92
    );
    println!(
        "{}",
        serde_json::json!({"status":"PASS","archive":path,"udp_records":count,"udp_payload_bytes":payload_bytes,"file_bytes":bytes,"normal_footer_eof":true,"all_metadata_and_payload_bytes_equal":true})
    );
}
