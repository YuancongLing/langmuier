//! Explicit real-PCAP acceptance through production replay and storage.
//! No GUI automation, synthetic fallback, socket, or control transmission.
use super::*;
use crate::bridge::{HistoryStatus, HistoryWindow};
use langmuir_core::{ExcitationRecord, ScientificFrame};
use langmuir_storage::{BinaryV3Reader, RawDestination, RawHeader, RawOrigin, RawReader};
use serde_json::{Value, json};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

fn required_path(name: &str) -> PathBuf {
    std::env::var_os(name).map_or_else(
        || panic!("real acceptance requires explicit {name}; no generated fallback"),
        PathBuf::from,
    )
}

fn number(value: &Value, key: &str) -> u64 {
    value[key]
        .as_u64()
        .unwrap_or_else(|| panic!("independent field {key}: {value}"))
}

fn rows<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("independent array {key}"))
        .as_slice()
}

fn compare_science(actual: &ScientificFrame, expected: &Value) {
    assert_eq!(
        u64::from(actual.timestamp.utc_seconds),
        number(expected, "seconds")
    );
    assert_eq!(
        u64::from(actual.timestamp.microseconds),
        number(expected, "microseconds")
    );
    assert_eq!(u64::from(actual.frame_seq_u8), number(expected, "seq8"));
    for (name, channel) in [
        ("ch1_code10", &actual.channel_1),
        ("ch2_code10", &actual.channel_2),
    ] {
        let expected_samples = rows(expected, name);
        assert_eq!(channel.len(), expected_samples.len());
        for (index, (&code, expected_code)) in channel.iter().zip(expected_samples).enumerate() {
            assert_eq!(
                Some(u64::from(code)),
                expected_code.as_u64(),
                "AA {} {name}[{index}]",
                expected["frame_index"]
            );
            assert_eq!(
                actual.sample_time_us(index),
                expected["sample_time_us"][index].as_u64()
            );
        }
    }
}

fn compare_excitation(actual: &ExcitationRecord, expected: &Value, session: &str) {
    assert_eq!(actual.session, session);
    assert_eq!(actual.source, expected["source"].as_str().unwrap());
    assert_eq!(
        actual.received_unix_nanos,
        number(expected, "received_unix_nanos")
    );
    let frame = &actual.frame;
    for (name, actual) in [
        ("raw_record_id", actual.raw_record_id),
        ("outer_sequence", u64::from(actual.outer_sequence)),
        ("seconds", u64::from(frame.timestamp.utc_seconds)),
        ("microseconds", u64::from(frame.timestamp.microseconds)),
        ("f_act_mhz", u64::from(frame.f_act_mhz)),
        ("interval_us", u64::from(frame.interval_us)),
        ("points", u64::from(frame.points)),
        ("work_mode", u64::from(frame.work_mode)),
        ("scan_freq", u64::from(frame.scan_freq)),
        ("scan_voltage_mv", u64::from(frame.scan_voltage_mv)),
        ("ad9833_status", u64::from(frame.ad9833_status)),
        ("dac8563_status", u64::from(frame.dac8563_status)),
        ("sci_frame_seq", u64::from(frame.sci_frame_seq)),
        ("pa0_seq", u64::from(frame.pa0_seq)),
    ] {
        assert_eq!(
            actual,
            number(expected, name),
            "AB {} {name}",
            expected["frame_index"]
        );
    }
    assert!(
        hex_bytes(&frame.raw_crc).eq_ignore_ascii_case(expected["raw_crc_hex"].as_str().unwrap())
    );
    for (index, &sample) in frame.wave_mv.iter().enumerate() {
        assert_eq!(Some(u64::from(sample)), expected["wave_mv"][index].as_u64());
        assert_eq!(
            frame.sample_time_us(index),
            expected["sample_time_us"][index].as_u64()
        );
    }
    assert_eq!(frame.integrity(), "CRC_NOT_CHECKED");
    assert_eq!(
        frame.interval_us > 0,
        expected["waveform_status"] == "TIMED_SAMPLES"
    );
}

fn raw_readback(path: &Path, manifest: &Value, header: Option<&RawHeader>) -> Value {
    let mut reader = RawReader::open(path).unwrap();
    if let Some(header) = header {
        assert_eq!(reader.header(), header);
    }
    let mut payload_bytes = 0_u64;
    for expected in rows(manifest, "datagrams") {
        let actual = reader.next_record().unwrap().expect("missing original UDP");
        assert_eq!(actual.id, number(expected, "raw_record_id"));
        assert_eq!(
            actual.origin_packet_index,
            Some(number(expected, "packet_index"))
        );
        assert_eq!(
            actual.timestamp.as_nanos(),
            u128::from(number(expected, "timestamp_ns"))
        );
        assert_eq!(
            actual.source.to_string(),
            expected["source"].as_str().unwrap()
        );
        assert_eq!(
            actual.destination,
            RawDestination::Captured(expected["destination"].as_str().unwrap().parse().unwrap())
        );
        assert_eq!(
            actual.source_matches,
            expected["source_matches"].as_bool().unwrap()
        );
        assert!(
            hex_bytes(&actual.payload)
                .eq_ignore_ascii_case(expected["payload_hex"].as_str().unwrap()),
            "raw record {} bytes differ",
            actual.id
        );
        payload_bytes += u64::try_from(actual.payload.len()).unwrap();
    }
    assert!(
        reader.next_record().unwrap().is_none(),
        "extra UDP or invalid END/EOF"
    );
    assert!(
        reader.next_record().unwrap().is_none(),
        "EOF must remain exact"
    );
    let footer = reader.summary().unwrap();
    let count = u64::try_from(rows(manifest, "datagrams").len()).unwrap();
    assert_eq!(
        (
            footer.observed,
            footer.enqueued,
            footer.written,
            footer.dropped
        ),
        (count, count, count, 0)
    );
    assert_eq!(footer.payload_bytes, payload_bytes);
    assert!(footer.complete && footer.closed);
    assert_eq!(footer.error, None);
    json!({"records":count,"payload_bytes":payload_bytes,"all_payload_and_metadata_equal":true,
        "received":footer.observed,"enqueued":footer.enqueued,"written":footer.written,"dropped":footer.dropped,
        "closed":footer.closed,"complete":footer.complete,"error":footer.error,
        "complete_END_exact_EOF":true,"session_id_hex":hex_bytes(&reader.header().session_id)})
}

fn wait_history(
    session: &history::HistorySession,
    accept: impl Fn(&HistoryStatus) -> bool,
) -> (HistoryStatus, Option<HistoryWindow>) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut window = None;
    while Instant::now() < deadline {
        let event = match session.events.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => event,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                panic!("history worker disconnected")
            }
        };
        match event {
            BackendEvent::HistoryWindow(value) => window = Some(*value),
            BackendEvent::HistoryStatus(value) if accept(&value) => return (*value, window),
            _ => {}
        }
    }
    panic!("real history operation timed out");
}

fn independent_seconds(micros: u64) -> f64 {
    f64::from(u32::try_from(micros / 1_000_000).unwrap())
        + f64::from(u32::try_from(micros % 1_000_000).unwrap()) / 1_000_000.0
}

fn wait_window(session: &history::HistorySession, range: [f64; 2]) -> HistoryWindow {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let event = match session.events.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => event,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                panic!("history worker disconnected")
            }
        };
        match event {
            BackendEvent::HistoryWindow(window)
                if (window.range[0] - range[0]).abs() < 1.0e-9
                    && (window.range[1] - range[1]).abs() < 1.0e-9 =>
            {
                return *window;
            }
            BackendEvent::HistoryStatus(status) => assert_eq!(status.error, None),
            _ => {}
        }
    }
    panic!("history sample window timed out");
}

#[expect(
    clippy::too_many_lines,
    reason = "full real capture history and export contract"
)]
fn history_readback(
    path: &Path,
    manifest: &Value,
    generation: u64,
    session_id: Option<&str>,
    export_path: Option<&Path>,
) -> (Value, Option<Value>) {
    let session = history::HistorySession::start(path.to_owned(), generation, false).unwrap();
    let (status, _) = wait_history(&session, |status| !status.loading);
    assert_eq!(status.error, None);
    assert_eq!(
        status.native,
        u64::try_from(rows(manifest, "aa").len()).unwrap()
    );
    assert_eq!(
        (status.rejected, status.recovered, status.candidates),
        (0, 0, 0)
    );
    let is_lpd = path.extension().is_some_and(|extension| extension == "lpd");
    // 1000 science frames span about 50s. Every requested view stays below the
    // production 60s limit, while these disjoint sample groups cover the file.
    let mut checked_samples = 0_usize;
    let mut windows_checked = 0_usize;
    for group in rows(manifest, "aa").chunks(1000) {
        let range = [
            independent_seconds(group[0]["sample_time_us"][0].as_u64().unwrap()) - 0.000_1,
            independent_seconds(
                group.last().unwrap()["sample_time_us"][99]
                    .as_u64()
                    .unwrap(),
            ) + 0.000_1,
        ];
        assert!(range[1] - range[0] < 60.0);
        session
            .command(history::HistoryCommand::Window(range[0], range[1]))
            .unwrap();
        let window = wait_window(&session, range);
        assert_eq!(window.channel_1.len(), group.len() * 100);
        assert_eq!(window.channel_2.len(), group.len() * 100);
        for (frame_offset, expected) in group.iter().enumerate() {
            for sample_index in 0..100 {
                let index = frame_offset * 100 + sample_index;
                for (points, name) in [
                    (&window.channel_1, "ch1_code10"),
                    (&window.channel_2, "ch2_code10"),
                ] {
                    let point = &points[index];
                    let code =
                        i32::try_from(expected[name][sample_index].as_u64().unwrap()).unwrap();
                    assert!((point.value - f64::from(code * 64 - 32_768)).abs() < f64::EPSILON);
                    let time = independent_seconds(
                        expected["sample_time_us"][sample_index].as_u64().unwrap(),
                    );
                    assert!((point.time_seconds - time).abs() < 1.0e-9);
                    assert_eq!(
                        point.record_id,
                        if is_lpd {
                            number(expected, "frame_index") + 1
                        } else {
                            number(expected, "raw_record_id")
                        }
                    );
                }
                checked_samples += 1;
            }
        }
        windows_checked += 1;
    }
    assert_eq!(checked_samples, rows(manifest, "aa").len() * 100);
    let mut export_metadata = None;
    if is_lpd {
        assert_eq!(status.excitation.frames, 0);
    } else {
        let expected_ab = rows(manifest, "ab");
        assert_eq!(
            status.excitation.frames,
            u64::try_from(expected_ab.len()).unwrap()
        );
        assert_eq!(status.excitation.errors, []);
        let source_session = session_id.map_or_else(
            || format!("history:{}", path.to_string_lossy()),
            str::to_owned,
        );
        // Every real AB is selected; reversing at both ends verifies navigation
        // as well as transitions across all actual configuration stages.
        let selection_order = [0, 1, 0]
            .into_iter()
            .chain(1..expected_ab.len())
            .chain([expected_ab.len() - 2, expected_ab.len() - 1]);
        for index in selection_order {
            let ordinal = u64::try_from(index).unwrap();
            session
                .command(history::HistoryCommand::ExcitationFrame(ordinal))
                .unwrap();
            let (selected, _) = wait_history(&session, |status| {
                !status.excitation.loading && status.excitation.selected_index == Some(ordinal)
            });
            assert_eq!(selected.error, None);
            compare_excitation(
                selected.excitation.selected.as_ref().unwrap(),
                &expected_ab[index],
                &source_session,
            );
            if index % 25 == 0 {
                println!(
                    "S02_REAL02_HISTORY path={} selected_AB_index={index}",
                    path.display()
                );
                std::io::stdout().flush().unwrap();
            }
        }
        let output = export_path.expect("AB history export requires exclusive output");
        session
            .command(history::HistoryCommand::ExportExcitation {
                path: output.to_string_lossy().into_owned(),
            })
            .unwrap();
        let (exported, _) = wait_history(&session, |status| {
            status.excitation.export_path.as_deref() == Some(output.to_string_lossy().as_ref())
                && !status.excitation.exporting
        });
        assert_eq!(exported.excitation.export_error, None);
        assert_eq!(
            fs::read_to_string(output).unwrap().lines().count(),
            expected_ab.len() * 100 + 1
        );
        export_metadata = Some(
            json!({"input_kind":if session_id.is_some() {"lvdsraw"} else {"pcapng"},
            "input_path":path,"path":output,"protocol_session":source_session}),
        );
    }
    (
        json!({"path":path,"AA_frames":status.native,"AA_samples_each_channel":checked_samples,
        "AA_windows_checked":windows_checked,"AB_frames":status.excitation.frames,
        "AB_all_frames_and_previous_next_checked":!is_lpd,"native_GUI":false,
        "read_records":status.read_records,"index_entries":status.index_entries}),
        export_metadata,
    )
}

fn counters(stats: &crate::bridge::LvdsReceiveStats) -> Value {
    json!({"udp_packets":stats.udp_packets,"source_rejected_packets":stats.source_rejected_packets,
        "outer_valid":stats.outer_valid,"complete_frames":stats.complete_frames,
        "science_frames":stats.science_frames,"accepted_frames":stats.accepted_frames,
        "excitation_frames":stats.excitation_frames,"excitation_decoded":stats.excitation_decoded,
        "gnss_attitude_frames":stats.gnss_attitude_frames,
        "parse_failures":stats.parse_failures,"structure_errors":stats.structure_errors,
        "excitation_decode_errors":stats.excitation_decode_errors,
        "excitation_business_dropped":stats.excitation_business_dropped,
        "duplicates":stats.duplicates,"conflicts":stats.conflicts,
        "auxiliary_duplicates":stats.auxiliary_duplicates,"auxiliary_conflicts":stats.auxiliary_conflicts,
        "all_counters_debug":format!("{stats:?}")})
}

#[test]
#[ignore = "explicit real PCAP, independent manifest, new output directory, and ~175s normal replay required"]
#[allow(clippy::too_many_lines)]
#[expect(
    clippy::assertions_on_constants,
    reason = "this explicit ignored real-file test must refuse a mock-enabled binary at runtime"
)]
fn s02_real02_nonmock_paced_production_full_storage_and_history() {
    assert!(
        !cfg!(feature = "mock"),
        "run with --no-default-features, never a mock backend"
    );
    let input = required_path("LANGMUIR_S02_REAL02_INPUT")
        .canonicalize()
        .unwrap();
    let manifest_path = required_path("LANGMUIR_S02_REAL02_MANIFEST");
    let output = required_path("LANGMUIR_S02_REAL02_OUTPUT");
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["schema_version"], 2);
    let original = fs::read(&input).unwrap();
    let input_hash = langmuir_core::sha256_hex(&original);
    assert_eq!(
        Some(input_hash.as_str()),
        manifest["input"]["sha256"].as_str()
    );
    fs::create_dir(&output)
        .expect("output directory must be new; existing outputs are never overwritten");
    let aa_stem = output.join("science");
    let ab_path = output.join("excitation.csv");
    let raw_path = output.join("capture.lvdsraw");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let raw_header = RawHeader {
        origin: RawOrigin::PcapReplay,
        created_at: now,
        expected_source: DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap(),
        binding: "0.0.0.0:0".parse().unwrap(),
        session_id: now.as_nanos().to_le_bytes(),
    };
    let (mut controller, _critical, display) = super::e2e_tests::controller();
    assert!(controller.start_scientific_capture(ScientificSaveFormat::LpdAndCsv, &aa_stem));
    controller.start_excitation_csv(&ab_path.to_string_lossy());
    controller.raw_recording =
        Some(raw_storage::RawRecording::start(&raw_path, raw_header.clone()).unwrap());
    assert!(
        aa_stem.with_extension("lpd").exists()
            && aa_stem.with_extension("csv").exists()
            && ab_path.exists()
            && raw_path.exists()
    );
    assert!(
        controller.excitation_storage_status().active && controller.scientific_writer.is_some()
    );
    let replay = PcapReplay::start(input.to_string_lossy().into_owned()).unwrap();
    let protocol_session = replay.ingress.audit_context["protocol_session"]
        .as_str()
        .unwrap()
        .to_owned();
    let metadata = json!({"excitation_protocol_session":protocol_session,
        "raw_header":{"origin":"PCAP_REPLAY","created_at_ns":u64::try_from(now.as_nanos()).unwrap(),
            "session_id_hex":hex_bytes(&raw_header.session_id),"expected_source":raw_header.expected_source.to_string(),
            "binding":raw_header.binding.to_string()},"pcap_path":input});
    let first_capture_ns = number(&rows(&manifest, "datagrams")[0], "timestamp_ns");
    let last_capture_ns = number(rows(&manifest, "datagrams").last().unwrap(), "timestamp_ns");
    let capture_duration =
        Duration::from_nanos(last_capture_ns.checked_sub(first_capture_ns).unwrap());
    let started = replay.started;
    controller.replay = Some(replay);
    controller.lvds_connected = true;
    controller.lvds_source = format!("PCAP: {}", input.display());
    println!(
        "S02_REAL02_PACED_BEGIN pid={} unix_ns={} output={} writers_ready_before_first_frame=true",
        std::process::id(),
        now.as_nanos(),
        output.display()
    );
    std::io::stdout().flush().unwrap();
    let mut observed_ab = Vec::new();
    let mut next_progress = Duration::from_secs(5);
    while !controller.replay.as_ref().unwrap().progress.finished {
        assert!(
            started.elapsed() < capture_duration + Duration::from_secs(30),
            "normal paced replay failed to reach EOF"
        );
        controller.drain_replay();
        controller.drain_storage();
        controller.poll_excitation_storage();
        let replay = controller.replay.as_ref().unwrap();
        if let Some(record) = &replay.ingress.excitation_latest
            && observed_ab
                .last()
                .is_none_or(|previous: &ExcitationRecord| {
                    previous.raw_record_id != record.raw_record_id
                })
        {
            let expected = &rows(&manifest, "ab")[observed_ab.len()];
            compare_excitation(record, expected, &protocol_session);
            observed_ab.push(record.clone());
        }
        if started.elapsed() >= next_progress {
            println!(
                "S02_REAL02_PROGRESS elapsed_s={:.3} source_udp={} AA={} AB={} raw_written={} display_unconsumed={}",
                started.elapsed().as_secs_f64(),
                replay.ingress.receive_stats.udp_packets,
                replay.ingress.receive_stats.accepted_frames,
                replay.ingress.receive_stats.excitation_decoded,
                controller.raw_recording.as_ref().unwrap().status().written,
                display.len()
            );
            std::io::stdout().flush().unwrap();
            next_progress += Duration::from_secs(5);
        }
        thread::sleep(Duration::from_millis(2));
    }
    let wall_seconds = started.elapsed().as_secs_f64();
    let replay = controller.replay.as_ref().unwrap();
    assert_eq!(replay.progress.error, None);
    let stats = replay.ingress.receive_stats;
    let expected_count = |key| u64::try_from(rows(&manifest, key).len()).unwrap();
    let matched_udp = u64::try_from(
        rows(&manifest, "datagrams")
            .iter()
            .filter(|record| record["source_matches"] == true)
            .count(),
    )
    .unwrap();
    // S01 udp_packets counts the admitted source; raw observation is broader.
    assert_eq!(stats.udp_packets, matched_udp);
    assert_eq!(stats.accepted_frames, expected_count("aa"));
    assert_eq!(stats.science_frames, expected_count("aa"));
    assert_eq!(stats.parsed_frames, expected_count("aa"));
    assert_eq!(stats.excitation_frames, expected_count("ab"));
    assert_eq!(stats.excitation_decoded, expected_count("ab"));
    assert_eq!(stats.gnss_attitude_frames, expected_count("ac"));
    assert_eq!(stats.complete_frames, stats.outer_valid);
    assert_eq!(stats.structure_valid_frames, stats.outer_valid);
    assert_eq!(
        stats.source_rejected_packets,
        expected_count("datagrams") - matched_udp
    );
    assert_eq!(
        stats.outer_valid,
        stats.science_frames + stats.excitation_frames + stats.gnss_attitude_frames
    );
    for (name, value) in [
        ("structure_errors", stats.structure_errors),
        ("unknown_type_frames", stats.unknown_type_frames),
        ("parse_failures", stats.parse_failures),
        ("header_errors", stats.header_errors),
        ("length_errors", stats.length_errors),
        ("field_errors", stats.field_errors),
        ("timestamp_errors", stats.timestamp_errors),
        ("adc_errors", stats.adc_errors),
        ("duplicates", stats.duplicates),
        ("conflicts", stats.conflicts),
        ("AB_error", stats.excitation_decode_errors),
        ("AB_business_dropped", stats.excitation_business_dropped),
        ("AB_duplicate", stats.excitation_duplicates),
        ("AB_conflict", stats.excitation_conflicts),
        ("auxiliary_duplicate", stats.auxiliary_duplicates),
        ("auxiliary_conflict", stats.auxiliary_conflicts),
    ] {
        assert_eq!(value, 0, "unexpected {name}");
    }
    assert_eq!(
        u64::try_from(observed_ab.len()).unwrap(),
        expected_count("ab")
    );
    let outer_metrics = replay.ingress.metrics.udp_sequence;
    assert_eq!(
        (
            outer_metrics.gap_candidates,
            outer_metrics.sequence_anomalies
        ),
        (0, 0)
    );
    let capture_span = replay
        .last_timestamp
        .unwrap()
        .checked_sub(replay.first_timestamp.unwrap())
        .unwrap();
    assert_eq!(
        capture_span, capture_duration,
        "PCAP nanosecond timing must match the independent audit"
    );
    assert!(
        wall_seconds >= capture_span.as_secs_f64(),
        "normal pacing was accelerated"
    );
    assert!(
        wall_seconds - capture_span.as_secs_f64() < 5.0,
        "replay fell behind normal capture pace"
    );
    let excitation_status = controller.excitation_storage_status();
    assert_eq!(
        (
            excitation_status.enqueued,
            excitation_status.written,
            excitation_status.rows_written,
            excitation_status.dropped
        ),
        (
            expected_count("ab"),
            expected_count("ab"),
            expected_count("ab") * 100,
            0
        )
    );
    assert!(excitation_status.closed && !excitation_status.active);
    assert_eq!(excitation_status.error, None);
    let science_status = controller.storage_status.clone();
    assert_eq!(
        (
            science_status.enqueued,
            science_status.sink_written,
            science_status.csv_frames_written,
            science_status.lpd_frames_written
        ),
        (
            expected_count("aa"),
            expected_count("aa"),
            expected_count("aa"),
            expected_count("aa")
        )
    );
    assert_eq!(science_status.csv_rows_written, expected_count("aa") * 100);
    assert!(science_status.closed && !controller.acquisition_running);
    assert_eq!(science_status.error, None);
    let raw_status = controller.raw_recording.take().unwrap().stop();
    assert_eq!(
        (
            raw_status.received,
            raw_status.enqueued,
            raw_status.written,
            raw_status.dropped
        ),
        (
            expected_count("datagrams"),
            expected_count("datagrams"),
            expected_count("datagrams"),
            0
        )
    );
    assert!(raw_status.complete && raw_status.closed && !raw_status.active);
    assert_eq!(raw_status.error, None);
    let raw_check = raw_readback(&raw_path, &manifest, Some(&raw_header));
    let mut science = BinaryV3Reader::open(aa_stem.with_extension("lpd")).unwrap();
    for expected in rows(&manifest, "aa") {
        compare_science(&science.next_frame().unwrap().unwrap(), expected);
    }
    assert!(science.next_frame().unwrap().is_none());
    assert_eq!(
        fs::metadata(aa_stem.with_extension("lpd")).unwrap().len(),
        40 + 409 * expected_count("aa")
    );
    assert_eq!(
        fs::read_to_string(aa_stem.with_extension("csv"))
            .unwrap()
            .lines()
            .count(),
        rows(&manifest, "aa").len() * 100 + 1
    );
    assert_eq!(
        fs::read_to_string(&ab_path).unwrap().lines().count(),
        rows(&manifest, "ab").len() * 100 + 1
    );
    let display_dropped = controller.lvds_display_dropped.load(Ordering::Relaxed);
    assert!(
        display_dropped > 0,
        "acceptance must exercise an unconsumed visual queue"
    );
    println!(
        "S02_REAL02_PACED_END pid={} elapsed_s={wall_seconds:.6} all_writers_closed=true",
        std::process::id()
    );
    std::io::stdout().flush().unwrap();
    // Also exercise the Controller request path used by the PCAP UI: it exports
    // the whole file on its own worker, independently of the playback position.
    let ui_entry_raw = output.join("ui-entry-full.lvdsraw");
    controller.start_raw_recording(&ui_entry_raw.to_string_lossy());
    let export_deadline = Instant::now() + Duration::from_secs(20);
    while controller.raw_export.as_ref().unwrap().status().active {
        assert!(Instant::now() < export_deadline);
        thread::sleep(Duration::from_millis(5));
    }
    controller.stop_raw_recording();
    assert!(controller.raw_status.complete && controller.raw_status.closed);
    assert_eq!(controller.raw_status.dropped, 0);
    let ui_entry_check = raw_readback(&ui_entry_raw, &manifest, None);
    let raw_session = hex_bytes(&raw_header.session_id);
    let pcap_export = output.join("history-pcap-ab.csv");
    let raw_export = output.join("history-raw-ab.csv");
    println!("S02_REAL02_HISTORY_BEGIN PCAPNG");
    let (pcap_history, pcap_export_metadata) =
        history_readback(&input, &manifest, 2141, None, Some(&pcap_export));
    println!("S02_REAL02_HISTORY_BEGIN LVDSRAW");
    let (raw_history, raw_export_metadata) = history_readback(
        &raw_path,
        &manifest,
        2142,
        Some(&raw_session),
        Some(&raw_export),
    );
    println!("S02_REAL02_HISTORY_BEGIN LPD");
    let (lpd_history, _) =
        history_readback(&aa_stem.with_extension("lpd"), &manifest, 2143, None, None);
    let history_checks = [pcap_history, raw_history, lpd_history];
    let history_exports = [pcap_export_metadata.unwrap(), raw_export_metadata.unwrap()];
    assert_eq!(
        langmuir_core::sha256_hex(&fs::read(&input).unwrap()),
        input_hash
    );
    let timed_ab_frames = observed_ab
        .iter()
        .filter(|record| record.frame.interval_us > 0)
        .count();
    let zero_time_ab_frames = observed_ab.len() - timed_ab_frames;
    let summary = json!({"schema_version":2,"status":"PASS","input":manifest["input"],
        "metadata":metadata,"files":{"raw":raw_path,"science_csv":aa_stem.with_extension("csv"),
            "science_lpd":aa_stem.with_extension("lpd"),"excitation_csv":ab_path,"ui_entry_raw":ui_entry_raw},
        "writers_ready_before_first_frame":true,
        "paced_replay":{"pid":std::process::id(),"wall_seconds":wall_seconds,"capture_span_ns":u64::try_from(capture_span.as_nanos()).unwrap(),
            "normal_pace":true,"source":"production PcapReplay / Controller::drain_replay"},
        "stats":counters(&stats),"counter_scope":{"all_observed_UDP":expected_count("datagrams"),
            "udp_packets_means_source_admitted":true,"source_admitted_UDP":matched_udp,
            "source_rejected_UDP":stats.source_rejected_packets},
        "outer_u32":{"gap_candidates":outer_metrics.gap_candidates,
            "observed_frames":outer_metrics.observed_frames,"sequence_anomalies":outer_metrics.sequence_anomalies},
        "AA":{"all_code10_and_times_equal":true,"frames":expected_count("aa"),"csv_rows":science_status.csv_rows_written,
            "enqueued":science_status.enqueued,"sink_written":science_status.sink_written,"closed":science_status.closed,"error":science_status.error},
        "AB":{"all_fields_100_samples_each_equal":true,"frames":expected_count("ab"),"csv_rows":excitation_status.rows_written,
            "timed_frames":timed_ab_frames,"no_sample_time_frames":zero_time_ab_frames,
            "timed_slots":timed_ab_frames*100,"no_sample_time_slots":zero_time_ab_frames*100,"csv_version":2,
            "enqueued":excitation_status.enqueued,"written":excitation_status.written,"dropped":excitation_status.dropped,"closed":excitation_status.closed,"error":excitation_status.error},
        "raw":raw_check,"additional_Controller_start_raw_recording_full_export":ui_entry_check,
        "history":history_checks,"history_exports":history_exports,"display":{"events_consumed":0,"queue_retained":display.len(),"dropped":display_dropped,
            "business_storage_depends_on_display":false},"input_sha256_after":input_hash,
        "windows_native_GUI":"NOT_RUN_BY_THIS_TEST","real_hardware_live":"NOT_RUN"});
    let summary_path = output.join("production-summary.json");
    let mut summary_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&summary_path)
        .unwrap();
    serde_json::to_writer_pretty(&mut summary_file, &summary).unwrap();
    summary_file.write_all(b"\n").unwrap();
    summary_file.flush().unwrap();
    println!("S02_REAL02_PRODUCTION_SUMMARY {}", summary_path.display());
}
