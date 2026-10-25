use super::*;

#[test]
fn raw_sink_exclusively_creates_a_versioned_header() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("capture.lvdsraw");
    let config = RawConfig::new(&path);
    let header = header();
    let _sink = RawSink::create(&config, header.clone(), RawCaptureAccounting::default()).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..8], b"LVDSRAW\0");
    assert_eq!(u16::from_le_bytes(bytes[8..10].try_into().unwrap()), 1);
    assert_eq!(bytes.len(), 128);
    assert!(RawSink::create(&config, header, RawCaptureAccounting::default()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

fn header() -> RawHeader {
    RawHeader {
        origin: RawOrigin::PcapReplay,
        created_at: Duration::new(1_700_000_000, 123),
        expected_source: "192.168.1.128:1534".parse().unwrap(),
        binding: "192.168.1.100:1534".parse().unwrap(),
        session_id: [7; 16],
    }
}

fn record(id: u64, payload: &[u8]) -> RawRecord {
    RawRecord {
        id,
        timestamp: Duration::new(1_700_000_000, 123),
        source: header().expected_source,
        destination: RawDestination::Captured(header().binding),
        source_matches: true,
        origin_packet_index: Some(7),
        payload: payload.to_vec(),
    }
}

fn account(accounting: &RawCaptureAccounting, count: u64) {
    for id in 1..=count {
        assert_eq!(accounting.observe().unwrap(), id);
        accounting.record_enqueued();
    }
}

#[test]
fn raw_archive_preserves_all_payloads_endpoints_order_and_full_duration_range() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("roundtrip.lvdsraw");
    let mut archive = RawArchive::start(RawConfig::new(&path), header()).unwrap();
    let payloads = [
        vec![0xEB; 268],
        vec![0xEB; 268],
        Vec::new(),
        vec![0, 0xFF, 1],
    ];
    for (index, payload) in payloads.iter().enumerate() {
        let source = if index == 3 {
            "192.0.2.9:42".parse().unwrap()
        } else {
            header().expected_source
        };
        let receipt = archive
            .try_record(
                Duration::new(u64::MAX - 1, 999_999_999),
                source,
                RawDestination::Captured(header().binding),
                index != 3,
                Some(u64::try_from(index).unwrap() + 10),
                payload,
            )
            .unwrap();
        assert_eq!(receipt.id, u64::try_from(index).unwrap() + 1);
        assert!(receipt.queued);
    }
    let status = archive.stop().unwrap();
    assert_eq!(
        (
            status.observed,
            status.enqueued,
            status.written,
            status.dropped
        ),
        (4, 4, 4, 0)
    );
    assert!(status.closed && status.complete);
    let mut reader = RawReader::open(&path).unwrap();
    assert_eq!(reader.header(), &header());
    for (index, payload) in payloads.iter().enumerate() {
        let raw = reader.next_record().unwrap().unwrap();
        assert_eq!(raw.id, u64::try_from(index).unwrap() + 1);
        assert_eq!(&raw.payload, payload);
        assert_eq!(raw.timestamp, Duration::new(u64::MAX - 1, 999_999_999));
        assert_eq!(raw.source_matches, index != 3);
        assert_eq!(
            raw.origin_packet_index,
            Some(u64::try_from(index).unwrap() + 10)
        );
    }
    assert!(reader.next_record().unwrap().is_none());
    assert!(reader.summary().unwrap().complete);
}

// Literal byte offsets intentionally avoid all production encoding helpers.
fn independent_fixture() -> Vec<u8> {
    let mut bytes = vec![0; 128 + 104 + 92];
    bytes[..8].copy_from_slice(b"LVDSRAW\0");
    bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
    bytes[10..12].copy_from_slice(&128_u16.to_le_bytes());
    bytes[12..16].copy_from_slice(&0x0102_0304_u32.to_le_bytes());
    bytes[16..20].copy_from_slice(&2_u32.to_le_bytes());
    bytes[20..24].copy_from_slice(&65_535_u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&1_700_000_000_u64.to_le_bytes());
    bytes[32..36].copy_from_slice(&123_u32.to_le_bytes());
    bytes[40..56].fill(7);
    let mut endpoint = [0; 24];
    endpoint[0] = 4;
    endpoint[2..4].copy_from_slice(&1534_u16.to_le_bytes());
    endpoint[4..8].copy_from_slice(&[192, 168, 1, 128]);
    bytes[56..80].copy_from_slice(&endpoint);
    bytes[160..184].copy_from_slice(&endpoint);
    endpoint[7] = 100;
    bytes[80..104].copy_from_slice(&endpoint);
    bytes[184..208].copy_from_slice(&endpoint);
    let data = &mut bytes[128..232];
    data[..2].copy_from_slice(&1_u16.to_le_bytes());
    data[2..4].copy_from_slice(&96_u16.to_le_bytes());
    data[4..8].copy_from_slice(&104_u32.to_le_bytes());
    data[8..16].copy_from_slice(&1_u64.to_le_bytes());
    data[16..24].copy_from_slice(&1_700_000_000_u64.to_le_bytes());
    data[24..28].copy_from_slice(&123_u32.to_le_bytes());
    data[28..32].copy_from_slice(&7_u32.to_le_bytes());
    data[80..84].copy_from_slice(&4_u32.to_le_bytes());
    data[88..96].copy_from_slice(&7_u64.to_le_bytes());
    data[96..100].copy_from_slice(&[0, 0xEB, 0x90, 0xFF]);
    data[100..104].copy_from_slice(&104_u32.to_le_bytes());
    let footer = &mut bytes[232..];
    footer[..2].copy_from_slice(&2_u16.to_le_bytes());
    footer[2..4].copy_from_slice(&88_u16.to_le_bytes());
    footer[4..8].copy_from_slice(&92_u32.to_le_bytes());
    for offset in [8, 16, 24, 48, 64] {
        footer[offset..offset + 8].copy_from_slice(&1_u64.to_le_bytes());
    }
    footer[40..48].copy_from_slice(&4_u64.to_le_bytes());
    footer[56..60].copy_from_slice(&1_u32.to_le_bytes());
    footer[60..64].copy_from_slice(&1_u32.to_le_bytes());
    footer[88..92].copy_from_slice(&92_u32.to_le_bytes());
    bytes
}

#[test]
fn raw_reader_and_writer_match_independently_constructed_version_one_bytes() {
    let fixture = independent_fixture();
    let mut reader = RawReader::new(std::io::Cursor::new(&fixture)).unwrap();
    assert_eq!(reader.header(), &header());
    assert_eq!(
        reader.next_record().unwrap(),
        Some(record(1, &[0, 0xEB, 0x90, 0xFF]))
    );
    assert!(reader.next_record().unwrap().is_none());
    let directory = tempfile::tempdir().unwrap();
    let config = RawConfig::new(directory.path().join("exact.lvdsraw"));
    let accounting = RawCaptureAccounting::default();
    account(&accounting, 1);
    let mut sink = RawSink::create(&config, header(), accounting.clone()).unwrap();
    sink.write_batch(&[record(1, &[0, 0xEB, 0x90, 0xFF])])
        .unwrap();
    accounting.request_finish();
    sink.close().unwrap();
    assert_eq!(std::fs::read(&config.path).unwrap(), fixture);
}

#[test]
fn raw_reader_rejects_every_truncation_and_aligned_eof_without_footer() {
    let fixture = independent_fixture();
    for end in 0..fixture.len() {
        let outcome =
            RawReader::new(std::io::Cursor::new(&fixture[..end])).and_then(|mut reader| {
                while reader.next_record()?.is_some() {}
                Ok(())
            });
        assert!(outcome.is_err(), "truncation at {end} was accepted");
    }
    let mut reader = RawReader::new(std::io::Cursor::new(&fixture[..232])).unwrap();
    assert!(reader.next_record().unwrap().is_some());
    assert!(matches!(reader.next_record(), Err(RawError::Incomplete)));
}

#[test]
fn raw_reader_rejects_invalid_lengths_versions_flags_ids_and_accounting() {
    let fixture = independent_fixture();
    for (offset, value) in [
        (0, 0),
        (8, 2),
        (10, 127),
        (16, 3),
        (20, 0),
        (36, 1),
        (104, 1),
        (128, 3),
        (130, 95),
        (132, 0xFF),
        (136, 0),
        (156, 0),
        (157, 1),
        (160, 5),
        (185, 1),
        (208, 0xFF),
        (212, 1),
        (228, 1),
        (240, 2),
        (248, 2),
        (256, 2),
        (264, 1),
        (272, 5),
        (280, 2),
        (288, 3),
        (292, 3),
        (296, 2),
        (304, 1),
        (320, 1),
    ] {
        let mut broken = fixture.clone();
        broken[offset] = value;
        let result = RawReader::new(std::io::Cursor::new(broken)).and_then(|mut reader| {
            while reader.next_record()?.is_some() {}
            Ok(())
        });
        assert!(result.is_err(), "mutation at {offset} was accepted");
    }
    let mut trailing = fixture;
    trailing.push(0);
    let mut reader = RawReader::new(std::io::Cursor::new(trailing)).unwrap();
    assert!(reader.next_record().unwrap().is_some());
    assert!(reader.next_record().is_err());
}

#[test]
fn raw_archive_preserves_ipv6_scope_and_destination_semantics() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ipv6.lvdsraw");
    let source = SocketAddr::V6(SocketAddrV6::new("fe80::1".parse().unwrap(), 1534, 0, 9));
    let mut metadata = header();
    metadata.origin = RawOrigin::LiveUdp;
    metadata.expected_source = source;
    metadata.binding = "[::]:1534".parse().unwrap();
    let mut archive = RawArchive::start(RawConfig::new(&path), metadata.clone()).unwrap();
    archive
        .try_record(
            Duration::ZERO,
            source,
            RawDestination::SocketBinding(metadata.binding),
            true,
            None,
            &[0x80],
        )
        .unwrap();
    archive
        .try_record(
            Duration::ZERO,
            source,
            RawDestination::Unknown,
            true,
            None,
            &[],
        )
        .unwrap();
    archive.stop().unwrap();
    let mut reader = RawReader::open(path).unwrap();
    let first = reader.next_record().unwrap().unwrap();
    assert_eq!(first.source, source);
    assert_eq!(
        first.destination,
        RawDestination::SocketBinding(metadata.binding)
    );
    assert_eq!(
        reader.next_record().unwrap().unwrap().destination,
        RawDestination::Unknown
    );
    assert!(reader.next_record().unwrap().is_none());
}

struct GatedSink {
    sink: RawSink,
    entered: crossbeam_channel::Sender<()>,
    release: crossbeam_channel::Receiver<()>,
    first: bool,
}

impl BatchSink<RawRecord> for GatedSink {
    type Error = RawError;
    fn write_batch(&mut self, records: &[RawRecord]) -> Result<(), Self::Error> {
        if self.first {
            self.first = false;
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
        }
        self.sink.write_batch(records)
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.sink.flush()
    }
    fn close(&mut self) -> Result<(), Self::Error> {
        self.sink.close()
    }
}

#[test]
fn raw_queue_overflow_preserves_reliable_counts_and_incomplete_footer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("overflow.lvdsraw");
    let mut config = RawConfig::new(&path);
    config.service.queue_capacity = 1;
    config.service.batch_size = 1;
    let accounting = RawCaptureAccounting::default();
    let sink = RawSink::create(&config, header(), accounting.clone()).unwrap();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (release_tx, release_rx) = crossbeam_channel::bounded(1);
    let mut archive = RawArchive {
        path: path.clone(),
        writer: Some(
            BatchWriter::start(
                GatedSink {
                    sink,
                    entered: entered_tx,
                    release: release_rx,
                    first: true,
                },
                config.service,
            )
            .unwrap(),
        ),
        accounting,
    };
    let submit = |archive: &mut RawArchive| {
        archive
            .try_record(
                Duration::ZERO,
                header().expected_source,
                RawDestination::Captured(header().binding),
                true,
                None,
                &[0xA5],
            )
            .unwrap()
    };
    assert!(submit(&mut archive).queued);
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(submit(&mut archive).queued);
    assert!(!submit(&mut archive).queued);
    assert_eq!(archive.status().dropped, 1);
    release_tx.send(()).unwrap();
    let result = archive.stop().unwrap();
    assert_eq!(
        (
            result.observed,
            result.enqueued,
            result.written,
            result.dropped
        ),
        (3, 2, 2, 1)
    );
    assert!(result.closed && !result.complete && result.error.is_some());
    let mut reader = RawReader::open(path).unwrap();
    assert_eq!(reader.next_record().unwrap().unwrap().id, 1);
    assert_eq!(reader.next_record().unwrap().unwrap().id, 2);
    assert!(reader.next_record().unwrap().is_none());
    assert!(!reader.summary().unwrap().complete);
    assert_eq!(reader.summary().unwrap().last_observed_id, 3);
}

#[test]
fn raw_archive_drop_is_abandoned_while_explicit_empty_stop_is_complete() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("abandoned.lvdsraw");
    let archive = RawArchive::start(RawConfig::new(&path), header()).unwrap();
    let accounting = archive.accounting();
    drop(archive);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !accounting.snapshot().closed && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(!accounting.snapshot().complete);
    let mut reader = RawReader::open(path).unwrap();
    assert!(reader.next_record().unwrap().is_none());
    assert!(!reader.summary().unwrap().complete);
    let empty = directory.path().join("empty.lvdsraw");
    let status = RawArchive::start(RawConfig::new(empty), header())
        .unwrap()
        .stop()
        .unwrap();
    assert!(status.complete && status.closed);
    assert_eq!(status.observed, 0);
}

struct FaultWriter {
    bytes: Arc<std::sync::Mutex<Vec<u8>>>,
    fail_after: usize,
    fail_flush: usize,
    flushes: usize,
}

impl Write for FaultWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut output = self.bytes.lock().unwrap();
        let count = bytes
            .len()
            .min(self.fail_after.saturating_sub(output.len()));
        if count == 0 {
            return Err(io::Error::other("injected raw write failure"));
        }
        output.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        if self.flushes == self.fail_flush {
            Err(io::Error::other("injected raw flush failure"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn raw_write_flush_and_close_errors_never_confirm_complete_or_retry_failed_bytes() {
    for (fail_after, fail_flush, failure_on_close) in [
        (128 + 100, usize::MAX, false),
        (usize::MAX, 2, false),
        (usize::MAX, 3, true),
    ] {
        let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let accounting = RawCaptureAccounting::default();
        account(&accounting, 1);
        let output = FaultWriter {
            bytes: Arc::clone(&bytes),
            fail_after,
            fail_flush,
            flushes: 0,
        };
        let mut sink = RawSink::from_writer(output, 64, header(), accounting.clone()).unwrap();
        let batch = sink.write_batch(&[record(1, &[0xAB; 64])]);
        if failure_on_close {
            batch.unwrap();
            accounting.request_finish();
            assert!(sink.close().is_err());
        } else {
            assert!(batch.is_err());
            assert_eq!(accounting.snapshot().written, 0);
        }
        assert!(!accounting.snapshot().closed && !accounting.snapshot().complete);
        assert!(accounting.snapshot().error.is_some());
        assert!(sink.write_batch(&[record(2, &[1])]).is_err());
        let before_drop = bytes.lock().unwrap().clone();
        drop(sink);
        assert_eq!(
            *bytes.lock().unwrap(),
            before_drop,
            "Drop retried failed bytes"
        );
    }
}

#[test]
fn raw_writer_rejects_invalid_records_before_any_batch_bytes_are_written() {
    let directory = tempfile::tempdir().unwrap();
    for bad_record in [
        record(0, &[1]),
        record(1, &vec![0; 65_536]),
        RawRecord {
            source_matches: false,
            ..record(1, &[1])
        },
    ] {
        let path = directory.path().join(format!(
            "bad-{}-{}.lvdsraw",
            bad_record.id,
            bad_record.payload.len()
        ));
        let accounting = RawCaptureAccounting::default();
        let mut sink =
            RawSink::create(&RawConfig::new(&path), header(), accounting.clone()).unwrap();
        assert!(sink.write_batch(&[bad_record]).is_err());
        assert_eq!(std::fs::metadata(path).unwrap().len(), 128);
        assert_eq!(accounting.snapshot().written, 0);
    }
}

#[test]
fn raw_late_close_cannot_erase_a_failure_reported_while_flush_was_blocked() {
    struct CloseGate {
        entered: crossbeam_channel::Sender<()>,
        release: crossbeam_channel::Receiver<()>,
        flushes: usize,
    }
    impl Write for CloseGate {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flushes == 3 {
                self.entered.send(()).unwrap();
                self.release.recv().unwrap();
            }
            Ok(())
        }
    }
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (release_tx, release_rx) = crossbeam_channel::bounded(1);
    let accounting = RawCaptureAccounting::default();
    account(&accounting, 1);
    let mut sink = RawSink::from_writer(
        CloseGate {
            entered: entered_tx,
            release: release_rx,
            flushes: 0,
        },
        64,
        header(),
        accounting.clone(),
    )
    .unwrap();
    sink.write_batch(&[record(1, &[0xA5])]).unwrap();
    accounting.request_finish();
    let close = std::thread::spawn(move || sink.close());
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    accounting.mark_failed("raw stop deadline expired");
    release_tx.send(()).unwrap();
    close.join().unwrap().unwrap();
    let status = accounting.snapshot();
    assert!(status.closed);
    assert!(!status.complete, "late close erased the failed stop state");
    assert_eq!(status.error.as_deref(), Some("raw stop deadline expired"));
}
