//! Bounded history I/O. Raw/PCAP seek safely rescans from the beginning.
use super::{DEPLOYED_LVDS_UDP_SOURCE, UdpLvdsIngress};
use crate::bridge::{
    BackendEvent, HistoryFileKind, HistoryFrameInfo, HistoryPoint, HistoryStatus, HistoryWindow,
};
use crossbeam_channel::{Receiver, Sender, bounded};
use langmuir_core::{FrameQuality, ScientificFrame, reconstruct_adc16};
use langmuir_storage::{
    BatchSink, BinaryV3Reader, RawReader, ScientificCsvConfig, ScientificCsvSink,
};
use langmuir_transport::PcapNgReader;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::{
    fs::{File, OpenOptions},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    thread::JoinHandle,
    time::{Duration, Instant},
};

const MAX_WINDOW_POINTS: usize = 120_100;
const MAX_INDEX: usize = 4096;
const MAX_ANOMALIES: usize = 128;

#[derive(Clone)]
struct Entry {
    frame: Option<ScientificFrame>,
    info: HistoryFrameInfo,
    shift: u8,
    validation: String,
}

enum Input {
    Lpd(BinaryV3Reader<BufReader<File>>),
    Raw(RawReader<BufReader<File>>),
    Pcap(PcapNgReader<BufReader<File>>),
}

struct HistoryReader {
    input: Input,
    kind: HistoryFileKind,
    ingress: UdpLvdsIngress,
    records: u64,
    epoch: Option<Duration>,
    previous_capture: Option<Duration>,
    clock: Instant,
    source: String,
    warning: Option<String>,
    ended: bool,
}

impl HistoryReader {
    fn open(path: &Path, recovery: bool) -> Result<Self, String> {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let (input, kind, expected) = match extension.as_str() {
            "lpd" => (
                Input::Lpd(BinaryV3Reader::open(path).map_err(|e| e.to_string())?),
                HistoryFileKind::LpdV3,
                DEPLOYED_LVDS_UDP_SOURCE.parse().expect("endpoint"),
            ),
            "lvdsraw" => {
                let reader = RawReader::open(path).map_err(|e| e.to_string())?;
                let expected = reader.header().expected_source;
                (Input::Raw(reader), HistoryFileKind::LvdsRaw, expected)
            }
            "pcapng" => (
                Input::Pcap(PcapNgReader::new(BufReader::new(
                    File::open(path).map_err(|e| e.to_string())?,
                ))),
                HistoryFileKind::PcapNg,
                DEPLOYED_LVDS_UDP_SOURCE.parse().expect("endpoint"),
            ),
            _ => {
                return Err("Only LPD v3 / LVDSRAW / PCAPNG history files are supported".to_owned());
            }
        };
        let mut ingress = UdpLvdsIngress::new(expected);
        ingress.recovery_enabled = recovery;
        ingress.audit_context["source_file"] = serde_json::json!(path.to_string_lossy());
        if kind == HistoryFileKind::LvdsRaw {
            ingress.audit_context["raw_archive"] = serde_json::json!(path.to_string_lossy());
            if let Input::Raw(reader) = &input {
                ingress.audit_context["raw_session_id"] =
                    serde_json::json!(super::hex_bytes(&reader.header().session_id));
            }
        }
        Ok(Self {
            input,
            kind,
            ingress,
            records: 0,
            epoch: None,
            previous_capture: None,
            clock: Instant::now(),
            source: path.to_string_lossy().into_owned(),
            warning: None,
            ended: false,
        })
    }

    fn next(&mut self) -> Result<Option<Vec<Entry>>, String> {
        if self.ended {
            return Ok(None);
        }
        match self.next_entries() {
            Ok(None) => {
                self.ended = true;
                Ok(None)
            }
            Err(error) if self.records > 0 => {
                self.warning = Some(error);
                self.ended = true;
                Ok(None)
            }
            result => result,
        }
    }

    // Keep source admission and provenance construction together for each record.
    #[allow(clippy::too_many_lines)]
    fn next_entries(&mut self) -> Result<Option<Vec<Entry>>, String> {
        let datagram = match &mut self.input {
            Input::Lpd(reader) => {
                let Some(frame) = reader.next_frame().map_err(|e| e.to_string())? else {
                    return Ok(None);
                };
                self.records += 1;
                return Ok(Some(vec![Entry {
                    info: HistoryFrameInfo {
                        record_id: self.records,
                        outer_sequence: None,
                        device_time_seconds: frame.channel_1_sample_time(0),
                        frame_seq_u8: Some(frame.frame_seq_u8),
                        quality: FrameQuality::Native,
                        reason: "LPD v3 native record; UDP sequence and original CRC are absent"
                            .to_owned(),
                        source: self.source.clone(),
                    },
                    frame: Some(frame),
                    shift: 0,
                    validation: "LPD_V3_METADATA_VALID".to_owned(),
                }]));
            }
            Input::Raw(reader) => {
                let Some(record) = reader.next_record().map_err(|e| e.to_string())? else {
                    if let Some(summary) = reader.summary()
                        && !summary.complete
                    {
                        self.warning = Some(format!(
                            "Raw archive is incomplete: observed={}, written={}, dropped={}, error={}",
                            summary.observed,
                            summary.written,
                            summary.dropped,
                            summary.error.as_deref().unwrap_or("unspecified")
                        ));
                    }
                    return Ok(None);
                };
                self.records += 1;
                Some((
                    record.id,
                    record.timestamp,
                    record.source,
                    record.destination,
                    record.payload,
                ))
            }
            Input::Pcap(reader) => {
                let Some(packet) = reader.next_packet().map_err(|e| e.to_string())? else {
                    return Ok(None);
                };
                self.records += 1;
                packet.udp.map(|udp| {
                    self.ingress.record_id = self.ingress.record_id.saturating_add(1);
                    (
                        self.ingress.record_id,
                        packet.timestamp,
                        udp.source,
                        langmuir_storage::RawDestination::Captured(udp.destination),
                        udp.payload,
                    )
                })
            }
        };
        let Some((id, timestamp, source, destination, payload)) = datagram else {
            return Ok(Some(Vec::new()));
        };
        if self
            .previous_capture
            .is_some_and(|previous| timestamp < previous)
        {
            return Err(
                "Capture time moved backwards; stopped without modifying timestamps".to_owned(),
            );
        }
        self.previous_capture = Some(timestamp);
        let epoch = *self.epoch.get_or_insert(timestamp);
        let at = self
            .clock
            .checked_add(timestamp.saturating_sub(epoch))
            .ok_or("History timestamp exceeds the supported range")?;
        self.ingress
            .push_observed_at(source, destination, &payload, at, id, |_| {});
        let mut entries = Vec::new();
        for observation in self.ingress.observations.drain(..) {
            let result = observation.result;
            let accepted =
                observation.delivered || result.quality == FrameQuality::RecoveryCandidate;
            let frame = if accepted { result.frame } else { None };
            entries.push(Entry {
                info: HistoryFrameInfo {
                    record_id: observation.record_id,
                    outer_sequence: observation.outer_sequence,
                    device_time_seconds: frame.as_ref().and_then(|f| f.channel_1_sample_time(0)),
                    frame_seq_u8: frame.as_ref().map(|f| f.frame_seq_u8),
                    quality: result.quality,
                    reason: if accepted {
                        result.reason
                    } else {
                        format!("{}; rejected or duplicate suppressed", result.reason)
                    },
                    source: self.source.clone(),
                },
                frame,
                shift: result.shift,
                validation: serde_json::to_string(&result.validation).map_err(|e| e.to_string())?,
            });
        }
        Ok(Some(entries))
    }
}

pub(super) enum HistoryCommand {
    Play(bool),
    Seek(f64),
    Window(f64, f64),
    Export {
        path: String,
        recovered: bool,
        candidates: bool,
        range: Option<[f64; 2]>,
    },
}

pub(super) struct HistorySession {
    pub(super) path: PathBuf,
    pub(super) events: Receiver<BackendEvent>,
    commands: Sender<(u64, HistoryCommand)>,
    selection_epoch: Arc<AtomicU64>,
    cancel: Sender<()>,
    join: Option<JoinHandle<()>>,
}

impl HistorySession {
    pub(super) fn start(path: PathBuf, generation: u64, recovery: bool) -> std::io::Result<Self> {
        let (tx, events) = bounded(64);
        let (commands, requests) = bounded(64);
        let (cancel, cancelled) = bounded(1);
        let worker_path = path.clone();
        let selection_epoch = Arc::new(AtomicU64::new(0));
        let worker_epoch = selection_epoch.clone();
        let join = std::thread::Builder::new()
            .name("lvds-history".to_owned())
            .spawn(move || {
                worker(
                    &worker_path,
                    generation,
                    recovery,
                    &tx,
                    &requests,
                    &cancelled,
                    &worker_epoch,
                );
            })?;
        Ok(Self {
            path,
            events,
            commands,
            selection_epoch,
            cancel,
            join: Some(join),
        })
    }
    pub(super) fn command(&self, command: HistoryCommand) -> Result<(), String> {
        let revision = if matches!(&command, HistoryCommand::Export { .. }) {
            self.selection_epoch.load(Ordering::Acquire)
        } else {
            self.selection_epoch.fetch_add(1, Ordering::AcqRel) + 1
        };
        self.commands
            .try_send((revision, command))
            .map_err(|e| format!("History request queue: {e}"))
    }
}
impl Drop for HistorySession {
    fn drop(&mut self) {
        let _ = self.cancel.try_send(());
        if self.join.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(join) = self.join.take()
        {
            let _ = join.join();
        }
    }
}

struct HistoryExport {
    path: String,
    quality: bool,
    result: Receiver<Result<u64, String>>,
    cancel: Sender<()>,
    join: Option<JoinHandle<()>>,
}
impl HistoryExport {
    fn start(
        input: &Path,
        path: String,
        recovery: bool,
        recovered: bool,
        candidates: bool,
        range: Option<[f64; 2]>,
    ) -> std::io::Result<Self> {
        let input = input.to_owned();
        let output = PathBuf::from(&path);
        let (completed, result) = bounded(1);
        let (cancel, cancelled) = bounded(1);
        let join = std::thread::Builder::new()
            .name("lvds-history-csv-export".to_owned())
            .spawn(move || {
                let outcome = export(
                    &input, &output, recovery, recovered, candidates, range, &cancelled,
                );
                let _ = completed.try_send(outcome);
            })?;
        Ok(Self {
            path,
            quality: recovered || candidates,
            result,
            cancel,
            join: Some(join),
        })
    }
}
impl Drop for HistoryExport {
    fn drop(&mut self) {
        let _ = self.cancel.try_send(());
        if self.join.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(join) = self.join.take()
        {
            let _ = join.join();
        }
    }
}

fn send(tx: &Sender<BackendEvent>, cancel: &Receiver<()>, mut event: BackendEvent) -> bool {
    while cancel.is_empty() {
        match tx.send_timeout(event, Duration::from_millis(20)) {
            Ok(()) => return true,
            Err(crossbeam_channel::SendTimeoutError::Timeout(value)) => event = value,
            Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => return false,
        }
    }
    false
}

// This loop owns one history session's cancellable scan, navigation and export.
#[allow(clippy::too_many_lines)]
fn worker(
    path: &Path,
    generation: u64,
    recovery: bool,
    tx: &Sender<BackendEvent>,
    requests: &Receiver<(u64, HistoryCommand)>,
    cancel: &Receiver<()>,
    selection_epoch: &AtomicU64,
) {
    let mut status = HistoryStatus {
        generation,
        active: true,
        path: path.to_string_lossy().into_owned(),
        loading: true,
        ..HistoryStatus::default()
    };
    let _ = send(
        tx,
        cancel,
        BackendEvent::HistoryStatus(Box::new(status.clone())),
    );
    let run = (|| -> Result<(), String> {
        let mut reader = HistoryReader::open(path, recovery)?;
        status.format = Some(reader.kind);
        let mut sparse = Vec::<(u64, Option<f64>)>::new();
        let mut stride = 1_u64;
        let mut previous_device = None;
        let mut first_time_segment = true;
        let mut initial = HistoryWindow {
            generation,
            revision: 1,
            range: [0.0, 0.0],
            ..HistoryWindow::default()
        };
        let mut audit_path = std::env::var_os("LOCALAPPDATA")
            .map_or_else(std::env::temp_dir, PathBuf::from)
            .join("Langmuier")
            .join("audits");
        std::fs::create_dir_all(&audit_path).map_err(|e| e.to_string())?;
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        audit_path.push(format!("history-{suffix}.audit.jsonl"));
        let mut audit = super::recovery_audit::RecoveryAudit::start(&audit_path)
            .map_err(|e| format!("Could not create history audit: {e}"))?;
        loop {
            let entries = match reader.next() {
                Ok(Some(entries)) => entries,
                Ok(None) => break,
                Err(error) => {
                    status.error = Some(error);
                    break;
                }
            };
            if !cancel.is_empty() {
                return Ok(());
            }
            for row in reader.ingress.audit_rows.drain(..) {
                if !audit.record_wait(row, cancel) {
                    return Err("History audit was cancelled or failed".to_owned());
                }
            }
            status.read_records = reader.records;
            for entry in entries {
                count(&mut status, &entry);
                if let Some(time) = entry.info.device_time_seconds {
                    if reliable(&entry) && first_time_segment {
                        if previous_device.is_some_and(|previous| time < previous) {
                            status.error=Some("Device time moved backwards; file order is preserved and playback is limited to one time segment".to_owned());
                            first_time_segment = false;
                        } else {
                            status.first_device_time.get_or_insert(time);
                            status.last_device_time = Some(time);
                            previous_device = Some(time);
                            if initial.channel_1.is_empty() {
                                initial.range = [time, time + 60.0];
                            }
                        }
                    }
                    if status.first_device_time.is_some()
                        && time >= initial.range[0]
                        && time < initial.range[1]
                        && status.error.is_none()
                    {
                        append_window(&mut initial, &entry);
                    }
                }

                if entry.info.quality != FrameQuality::Native
                    && status.anomalies.len() < MAX_ANOMALIES
                {
                    status.anomalies.push(entry.info.clone());
                }
            }
            if reader.records % stride == 0 {
                sparse.push((reader.records, previous_device));
                if sparse.len() >= MAX_INDEX {
                    sparse = sparse.into_iter().step_by(2).collect();
                    stride = stride.saturating_mul(2);
                }
            }
            if reader.records % 1024 == 0 {
                status.index_entries = sparse.len();
                if !send(
                    tx,
                    cancel,
                    BackendEvent::HistoryStatus(Box::new(status.clone())),
                ) {
                    return Ok(());
                }
            }
        }
        if let Some(warning) = reader.warning {
            status.error.get_or_insert(warning);
        }
        if let Some(error) = &status.error {
            audit.mark_failed(error.clone());
        }
        let audit_final = audit.stop();
        let _ = send(
            tx,
            cancel,
            BackendEvent::RecoveryStatus(Box::new(crate::bridge::RecoveryStatus {
                enabled: recovery,
                native: status.native,
                recovered: status.recovered,
                candidates: status.candidates,
                rejected: status.rejected,
                audit_path: Some(audit_path.to_string_lossy().into_owned()),
                audit_dropped: audit_final.dropped,
                error: audit_final.error,
            })),
        );
        status.total_records = Some(reader.records);
        status.index_entries = sparse.len();
        status.loading = false;
        status.current_device_time = status.first_device_time;
        status.current_seq8 = initial.frames.first().and_then(|f| f.frame_seq_u8);
        status.current_outer_seq = initial.frames.first().and_then(|f| f.outer_sequence);
        let _ = send(tx, cancel, BackendEvent::HistoryWindow(Box::new(initial)));
        let _ = send(
            tx,
            cancel,
            BackendEvent::HistoryStatus(Box::new(status.clone())),
        );
        let mut revision = 1_u64;
        let mut last_tick = Instant::now();
        let mut pending_export: Option<HistoryExport> = None;
        while cancel.is_empty() {
            let command = match requests.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => Some(command),
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
            };
            let read_revision = command.as_ref().map_or_else(
                || selection_epoch.load(Ordering::Acquire),
                |(revision, _)| *revision,
            );
            let command = command.map(|(_, command)| command);
            let mut range = None;
            let had_command = command.is_some();
            match command {
                Some(HistoryCommand::Play(playing)) => {
                    status.playing = playing;
                    status.finished = false;
                    last_tick = Instant::now();
                }
                Some(HistoryCommand::Seek(fraction)) => {
                    last_tick = Instant::now();
                    status.position_fraction = fraction.clamp(0.0, 1.0);
                    status.finished = false;
                    let first = status.first_device_time.unwrap_or(0.0);
                    let last = status.last_device_time.unwrap_or(first);
                    let time = first + (last - first).max(0.0) * status.position_fraction;
                    status.current_device_time = Some(time);
                    range = Some([time, time + 60.0]);
                }
                Some(HistoryCommand::Window(start, end)) => {
                    if start.is_finite() && end.is_finite() && end > start {
                        range = Some([start, end.min(start + 60.0)]);
                    }
                }
                Some(HistoryCommand::Export {
                    path: output,
                    recovered,
                    candidates,
                    range,
                }) => {
                    if pending_export.is_some() {
                        status.export_error = Some("A history export is already active".to_owned());
                    } else {
                        match HistoryExport::start(
                            path,
                            output.clone(),
                            recovery,
                            recovered,
                            candidates,
                            range,
                        ) {
                            Ok(task) => {
                                status.exporting = true;
                                status.export_path = Some(output);
                                status.export_error = None;
                                pending_export = Some(task);
                            }
                            Err(error) => status.export_error = Some(error.to_string()),
                        }
                    }
                }
                None => {}
            }
            if status.playing && last_tick.elapsed() >= Duration::from_millis(500) {
                let first = status.first_device_time.unwrap_or(0.0);
                let last = status.last_device_time.unwrap_or(first);
                let time = (status.current_device_time.unwrap_or(first)
                    + last_tick.elapsed().as_secs_f64())
                .min(last);
                last_tick = Instant::now();
                status.current_device_time = Some(time);
                status.position_fraction = if last > first {
                    (time - first) / (last - first)
                } else {
                    1.0
                };
                status.finished = time >= last;
                if status.finished {
                    status.playing = false;
                }
                range = Some([(time - 60.0).max(first), time + 0.05]);
            }
            if let Some(range) = range {
                revision += 1;
                status.loading = true;
                let _ = send(
                    tx,
                    cancel,
                    BackendEvent::HistoryStatus(Box::new(status.clone())),
                );
                let request = WindowRequest {
                    generation,
                    revision,
                    range,
                    selection: read_revision,
                    monotonic: status.error.is_none(),
                };
                let window = read_window(path, recovery, request, cancel, selection_epoch)?;
                status.loading = false;
                if let Some(window) = window {
                    let target = status.current_device_time.unwrap_or(range[0]);
                    if let Some(info) = window
                        .frames
                        .iter()
                        .filter(|info| {
                            info.quality != FrameQuality::RecoveryCandidate
                                && info.quality != FrameQuality::Rejected
                        })
                        .min_by(|left, right| {
                            let left = (left.device_time_seconds.unwrap_or(target) - target).abs();
                            let right =
                                (right.device_time_seconds.unwrap_or(target) - target).abs();
                            left.total_cmp(&right)
                        })
                    {
                        status.current_seq8 = info.frame_seq_u8;
                        status.current_outer_seq = info.outer_sequence;
                    }
                    if selection_epoch.load(Ordering::Acquire) == read_revision
                        && !send(tx, cancel, BackendEvent::HistoryWindow(Box::new(window)))
                    {
                        break;
                    }
                }
            }
            if let Some(result) = pending_export
                .as_ref()
                .and_then(|task| task.result.try_recv().ok())
            {
                let task = pending_export.take().expect("export task exists");
                status.exporting = false;
                match result {
                    Ok(rows) => {
                        let _ = send(
                            tx,
                            cancel,
                            BackendEvent::HistoryCsvExported {
                                path: task.path.clone(),
                                rows,
                                quality_format: task.quality,
                            },
                        );
                    }
                    Err(error) => status.export_error = Some(error),
                }
                let _ = send(
                    tx,
                    cancel,
                    BackendEvent::HistoryStatus(Box::new(status.clone())),
                );
            }

            if had_command || range.is_some() {
                let _ = send(
                    tx,
                    cancel,
                    BackendEvent::HistoryStatus(Box::new(status.clone())),
                );
            }
        }
        Ok(())
    })();
    if let Err(error) = run {
        status.loading = false;
        status.playing = false;
        status.error = Some(error);
        let _ = send(tx, cancel, BackendEvent::HistoryStatus(Box::new(status)));
    }
}

fn reliable(entry: &Entry) -> bool {
    entry.frame.is_some()
        && matches!(
            entry.info.quality,
            FrameQuality::Native | FrameQuality::Recovered1Bit | FrameQuality::Recovered4Bit
        )
}

fn count(status: &mut HistoryStatus, entry: &Entry) {
    match entry.info.quality {
        FrameQuality::Native => status.native += 1,
        FrameQuality::Recovered1Bit | FrameQuality::Recovered4Bit => status.recovered += 1,
        FrameQuality::RecoveryCandidate => status.candidates += 1,
        FrameQuality::Rejected => status.rejected += 1,
    }
}
fn append_window(window: &mut HistoryWindow, entry: &Entry) {
    let Some(frame) = entry.frame.as_ref() else {
        return;
    };
    if frame
        .channel_1_sample_time(99)
        .is_none_or(|end| end < window.range[0])
        || frame
            .channel_1_sample_time(0)
            .is_none_or(|start| start > window.range[1])
    {
        return;
    }
    if window.frames.len() < 4096 {
        window.frames.push(entry.info.clone());
    }
    for index in 0..100 {
        if window.channel_1.len() >= MAX_WINDOW_POINTS {
            break;
        }
        let Some(time) = frame.channel_1_sample_time(index) else {
            continue;
        };
        if time < window.range[0] || time > window.range[1] {
            continue;
        }
        for (output, code) in [
            (&mut window.channel_1, frame.channel_1[index]),
            (&mut window.channel_2, frame.channel_2[index]),
        ] {
            output.push(HistoryPoint {
                time_seconds: time,
                value: f64::from(reconstruct_adc16(code)),
                quality: entry.info.quality,
                record_id: entry.info.record_id,
            });
        }
    }
}
#[derive(Clone, Copy)]
struct WindowRequest {
    generation: u64,
    revision: u64,
    range: [f64; 2],
    selection: u64,
    monotonic: bool,
}
fn read_window(
    path: &Path,
    recovery: bool,
    request: WindowRequest,
    cancel: &Receiver<()>,
    selection_epoch: &AtomicU64,
) -> Result<Option<HistoryWindow>, String> {
    if selection_epoch.load(Ordering::Acquire) != request.selection {
        return Ok(None);
    }
    let mut reader = HistoryReader::open(path, recovery)?;
    let mut window = HistoryWindow {
        generation: request.generation,
        revision: request.revision,
        range: request.range,
        ..HistoryWindow::default()
    };
    let mut previous = None;
    while let Some(entries) = reader.next()? {
        if !cancel.is_empty() || selection_epoch.load(Ordering::Acquire) != request.selection {
            return Ok(None);
        }
        reader.ingress.audit_rows.clear();
        for entry in entries {
            if let Some(time) = entry.info.device_time_seconds {
                if reliable(&entry) {
                    if previous.is_some_and(|last| time < last) {
                        return Ok(Some(window));
                    }
                    previous = Some(time);
                    if request.monotonic && time > request.range[1] {
                        return Ok(Some(window));
                    }
                }
                append_window(&mut window, &entry);
            }
        }
    }
    Ok(Some(window))
}

// Native and quality CSVs share one ordered reader and sample-range filter.
#[allow(clippy::too_many_lines)]
fn export(
    input: &Path,
    output: &Path,
    recovery: bool,
    include_recovered: bool,
    include_candidates: bool,
    range: Option<[f64; 2]>,
    cancel: &Receiver<()>,
) -> Result<u64, String> {
    let mut reader = HistoryReader::open(input, recovery)?;
    let qualified = include_recovered || include_candidates;
    let mut native = if qualified || range.is_some() {
        None
    } else {
        Some(
            ScientificCsvSink::create(ScientificCsvConfig::new(output))
                .map_err(|e| e.to_string())?,
        )
    };
    let mut quality = if qualified {
        let mut writer = BufWriter::new(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(output)
                .map_err(|e| e.to_string())?,
        );
        writeln!(writer, "# LANGMUIR_QUALITY_CSV_VERSION=1").map_err(|e| e.to_string())?;
        writeln!(
            writer,
            "{},quality,raw_record_id,recovery_shift,recovery_validation,source",
            langmuir_storage::SCIENTIFIC_CSV_HEADER.trim_end()
        )
        .map_err(|e| e.to_string())?;
        Some(writer)
    } else {
        None
    };
    let mut ranged = if !qualified && range.is_some() {
        let mut writer = BufWriter::new(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(output)
                .map_err(|e| e.to_string())?,
        );
        writer
            .write_all(langmuir_storage::SCIENTIFIC_CSV_HEADER.as_bytes())
            .map_err(|e| e.to_string())?;
        Some(writer)
    } else {
        None
    };
    let mut rows = 0_u64;
    let mut frame_index = 0_u64;
    while let Some(entries) = reader.next()? {
        if !cancel.is_empty() {
            return Err("Export cancelled; the output is incomplete".to_owned());
        }
        reader.ingress.audit_rows.clear();
        for entry in entries {
            let Some(frame) = entry.frame else { continue };
            let selected = entry.info.quality == FrameQuality::Native
                || (include_recovered
                    && matches!(
                        entry.info.quality,
                        FrameQuality::Recovered1Bit | FrameQuality::Recovered4Bit
                    ))
                || (include_candidates && entry.info.quality == FrameQuality::RecoveryCandidate);
            if !selected {
                continue;
            }
            if let Some(range) = range
                && (frame
                    .channel_1_sample_time(99)
                    .is_none_or(|time| time < range[0])
                    || frame
                        .channel_1_sample_time(0)
                        .is_none_or(|time| time > range[1]))
            {
                continue;
            }
            if let Some(sink) = native.as_mut() {
                sink.write_batch(std::slice::from_ref(&frame))
                    .map_err(|e| e.to_string())?;
                rows += 100;
            }
            if qualified || ranged.is_some() {
                for index in 0..100 {
                    let sample_seconds = frame
                        .channel_1_sample_time(index)
                        .ok_or("sample time overflow")?;
                    if range
                        .is_some_and(|range| sample_seconds < range[0] || sample_seconds > range[1])
                    {
                        continue;
                    }
                    let time = frame.sample_time_us(index).ok_or("sample time overflow")?;
                    if let Some(writer) = quality.as_mut() {
                        writeln!(
                            writer,
                            "{},{},{},{},{},{},{},{},{},{},CRC_NOT_CHECKED,{},{},{},{},{}",
                            frame_index,
                            frame.timestamp.utc_seconds,
                            frame.timestamp.microseconds,
                            frame.frame_seq_u8,
                            index,
                            time,
                            frame.channel_1[index],
                            frame.channel_2[index],
                            reconstruct_adc16(frame.channel_1[index]),
                            reconstruct_adc16(frame.channel_2[index]),
                            entry.info.quality.label(),
                            entry.info.record_id,
                            entry.shift,
                            csv_quote(&entry.validation),
                            csv_quote(&entry.info.source)
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    if let Some(writer) = ranged.as_mut() {
                        writeln!(
                            writer,
                            "{},{},{},{},{},{},{},{},{},{},CRC_NOT_CHECKED",
                            frame_index,
                            frame.timestamp.utc_seconds,
                            frame.timestamp.microseconds,
                            frame.frame_seq_u8,
                            index,
                            time,
                            frame.channel_1[index],
                            frame.channel_2[index],
                            reconstruct_adc16(frame.channel_1[index]),
                            reconstruct_adc16(frame.channel_2[index])
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    rows += 1;
                }
            }
            frame_index += 1;
        }
    }
    if let Some(sink) = native.as_mut() {
        sink.close().map_err(|e| e.to_string())?;
    }
    if let Some(writer) = quality.as_mut() {
        writer.flush().map_err(|e| e.to_string())?;
    }
    if let Some(writer) = ranged.as_mut() {
        writer.flush().map_err(|e| e.to_string())?;
    }
    if let Some(warning) = reader.warning {
        return Err(warning);
    }
    Ok(rows)
}
fn csv_quote(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

#[cfg(test)]
struct TestScan {
    format: HistoryFileKind,
    frames: Vec<ScientificFrame>,
    total_records: u64,
}
#[cfg(test)]
fn scan_history_for_test(path: &Path) -> Result<TestScan, String> {
    let mut reader = HistoryReader::open(path, false)?;
    let mut frames = Vec::new();
    while let Some(entries) = reader.next()? {
        for entry in entries {
            if let Some(frame) = entry.frame {
                frames.push(frame);
            }
        }
        reader.ingress.audit_rows.clear();
    }
    Ok(TestScan {
        format: reader.kind,
        frames,
        total_records: reader.records,
    })
}
#[cfg(test)]
fn export_history_for_test(input: &Path, output: &Path) -> Result<u64, String> {
    let (_tx, cancel) = bounded(1);
    export(input, output, false, false, false, None, &cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::HistoryFileKind;
    use langmuir_storage::{BatchSink, BinaryConfig, BinaryV3Sink};

    #[test]
    fn raw_history_destination_semantics_isolate_recovery_anchors() {
        use super::super::capture_history_tests::{shift_four, wire};
        use langmuir_storage::{RawArchive, RawConfig, RawDestination, RawHeader, RawOrigin};
        let directory = tempfile::tempdir().unwrap();
        let source = DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap();
        let first = "192.0.2.100:1534".parse().unwrap();
        let second = "192.0.2.101:1534".parse().unwrap();
        for (index, destinations) in [
            [
                RawDestination::Captured(first),
                RawDestination::Captured(second),
            ],
            [
                RawDestination::SocketBinding(first),
                RawDestination::SocketBinding(second),
            ],
            [
                RawDestination::Captured(first),
                RawDestination::SocketBinding(first),
            ],
        ]
        .into_iter()
        .enumerate()
        {
            let path = directory
                .path()
                .join(format!("SYNTHETIC-destination-{index}.lvdsraw"));
            let mut raw = RawArchive::start(
                RawConfig::new(&path),
                RawHeader {
                    origin: RawOrigin::PcapReplay,
                    created_at: Duration::ZERO,
                    expected_source: source,
                    binding: first,
                    session_id: [8; 16],
                },
            )
            .unwrap();
            for (ordinal, payload) in [udp(100, &wire(10, 0)), udp(101, &shift_four(&wire(11, 1)))]
                .into_iter()
                .enumerate()
            {
                raw.try_record(
                    Duration::from_millis(u64::try_from(ordinal).unwrap() * 50),
                    source,
                    destinations[ordinal],
                    true,
                    Some(u64::try_from(ordinal).unwrap()),
                    &payload,
                )
                .unwrap();
            }
            assert!(raw.stop().unwrap().complete);
            let mut reader = HistoryReader::open(&path, true).unwrap();
            assert_eq!(
                reader.next().unwrap().unwrap()[0].info.quality,
                FrameQuality::Native
            );
            assert_eq!(
                reader.next().unwrap().unwrap()[0].info.quality,
                FrameQuality::RecoveryCandidate,
                "raw history must preserve destination address and Captured/SocketBinding meaning"
            );
        }
    }

    fn udp(sequence: u32, wire: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0x55, 0xAA, 1, 0xA5];
        bytes.extend_from_slice(&sequence.to_be_bytes());
        bytes.extend_from_slice(&(u32::try_from(wire.len()).unwrap()).to_be_bytes());
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        bytes.extend_from_slice(&1_u16.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&(u16::try_from(wire.len()).unwrap()).to_be_bytes());
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        bytes.extend_from_slice(wire);
        bytes
    }

    fn synthetic_lpd(path: &Path, count: u32) {
        let mut sink = BinaryV3Sink::create(BinaryConfig::new(path)).unwrap();
        let mut batch = Vec::new();
        for ordinal in 0..count {
            let mut wire = super::super::protocol_10bit_tests::golden_frame(
                u8::try_from(ordinal % 256).unwrap(),
            );
            wire[7..11].copy_from_slice(&(1_700_000_000 + ordinal / 20).to_be_bytes());
            wire[11..15].copy_from_slice(&((ordinal % 20) * 50_000).to_be_bytes());
            batch.push(langmuir_core::decode_scientific_frame(&wire).unwrap());
            if batch.len() == 32 {
                sink.write_batch(&batch).unwrap();
                batch.clear();
            }
        }
        if !batch.is_empty() {
            sink.write_batch(&batch).unwrap();
        }
        sink.close().unwrap();
    }

    fn wait_ready(session: &HistorySession) {
        let mut received_window = false;
        loop {
            match session.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                BackendEvent::HistoryWindow(window) => {
                    assert!(window.channel_1.len() <= MAX_WINDOW_POINTS);
                    assert!(window.channel_2.len() <= MAX_WINDOW_POINTS);
                    assert!(window.range[1] - window.range[0] <= 60.0);
                    received_window = true;
                }
                BackendEvent::HistoryStatus(status) if !status.loading => {
                    assert!(status.error.is_none(), "{:?}", status.error);
                    assert!(received_window);
                    assert!(status.index_entries <= MAX_INDEX);
                    break;
                }
                _ => {}
            }
        }
    }

    #[test]
    fn a_new_seek_cancels_the_old_window_before_it_is_published() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SYNTHETIC-rapid-seek.lpd");
        synthetic_lpd(&path, 2000);
        let session = HistorySession::start(path, 31, false).unwrap();
        wait_ready(&session);
        session.command(HistoryCommand::Seek(0.2)).unwrap();
        session.command(HistoryCommand::Seek(0.8)).unwrap();
        let expected = 1_700_000_000.0 + 1999.0 * 0.05 * 0.8;
        loop {
            if let BackendEvent::HistoryWindow(window) =
                session.events.recv_timeout(Duration::from_secs(5)).unwrap()
            {
                assert!(
                    (window.range[0] - expected).abs() < 1e-5,
                    "superseded seek window was emitted: {}",
                    window.range[0]
                );
                break;
            }
        }
    }

    #[test]
    fn history_csv_export_does_not_block_a_pause_request() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SYNTHETIC-export-pause.lpd");
        synthetic_lpd(&path, 5000);
        let session = HistorySession::start(path, 32, false).unwrap();
        wait_ready(&session);
        session
            .command(HistoryCommand::Export {
                path: directory
                    .path()
                    .join("export.csv")
                    .to_string_lossy()
                    .into_owned(),
                recovered: false,
                candidates: false,
                range: None,
            })
            .unwrap();
        loop {
            if let BackendEvent::HistoryStatus(status) =
                session.events.recv_timeout(Duration::from_secs(5)).unwrap()
                && status.exporting
            {
                break;
            }
        }
        session.command(HistoryCommand::Play(false)).unwrap();
        loop {
            match session.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                BackendEvent::HistoryStatus(status) if status.exporting && !status.playing => break,
                BackendEvent::HistoryCsvExported { .. } => {
                    panic!("pause waited behind the complete export")
                }
                _ => {}
            }
        }
    }

    #[test]
    fn native_range_export_clips_samples_at_both_window_edges() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("SYNTHETIC-clipped.lpd");
        synthetic_lpd(&input, 2);
        let output = directory.path().join("clipped.csv");
        let (_tx, cancel) = bounded(1);
        let rows = export(
            &input,
            &output,
            false,
            false,
            false,
            Some([1_700_000_000.025, 1_700_000_000.075]),
            &cancel,
        )
        .unwrap();
        assert_eq!(rows, 101);
        let text = std::fs::read_to_string(output).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 102);
        assert_eq!(lines[0].split(',').count(), 11);
        assert_eq!(lines[1].split(',').nth(5).unwrap(), "1700000000025000");
        assert_eq!(
            lines.last().unwrap().split(',').nth(5).unwrap(),
            "1700000000075000"
        );
    }

    #[test]
    fn an_unreliable_candidate_clock_cannot_change_native_navigation() {
        use langmuir_storage::{RawArchive, RawConfig, RawDestination, RawHeader, RawOrigin};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SYNTHETIC-untrusted-clock.lvdsraw");
        let source = DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap();
        let binding = "0.0.0.0:1534".parse().unwrap();
        let mut raw = RawArchive::start(
            RawConfig::new(&path),
            RawHeader {
                origin: RawOrigin::LiveUdp,
                created_at: Duration::ZERO,
                expected_source: source,
                binding,
                session_id: [8; 16],
            },
        )
        .unwrap();
        for ordinal in 0..3_u8 {
            let mut wire = super::super::protocol_10bit_tests::golden_frame(ordinal);
            wire[7..11]
                .copy_from_slice(&(if ordinal == 1 { 0_u32 } else { 100_u32 }).to_be_bytes());
            wire[11..15].copy_from_slice(&(u32::from(ordinal) * 50_000).to_be_bytes());
            if ordinal == 1 {
                let mut shifted = vec![0_u8; 269];
                for (index, &byte) in wire.iter().enumerate() {
                    for bit in 0..8 {
                        let target = index * 8 + bit + 4;
                        if target < 269 * 8 && byte & (0x80 >> bit) != 0 {
                            shifted[target / 8] |= 0x80 >> (target % 8);
                        }
                    }
                }
                wire = shifted;
            }
            raw.try_record(
                Duration::from_millis(u64::from(ordinal) * 50),
                source,
                RawDestination::SocketBinding(binding),
                true,
                None,
                &udp(u32::from(ordinal), &wire),
            )
            .unwrap();
        }
        raw.stop().unwrap();
        let session = HistorySession::start(path, 41, true).unwrap();
        loop {
            if let BackendEvent::HistoryStatus(status) =
                session.events.recv_timeout(Duration::from_secs(5)).unwrap()
                && !status.loading
            {
                assert_eq!((status.native, status.candidates), (2, 1));
                assert!(
                    status.error.is_none(),
                    "candidate time poisoned native history: {:?}",
                    status.error
                );
                assert_eq!(status.first_device_time, Some(100.0));
                assert_eq!(status.last_device_time, Some(100.1));
                break;
            }
        }
    }

    #[test]
    fn a_native_clock_restart_keeps_navigation_in_the_first_file_segment() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SYNTHETIC-clock-restart.lpd");
        let mut sink = BinaryV3Sink::create(BinaryConfig::new(&path)).unwrap();
        for (sequence, seconds, micros) in [(0, 100_u32, 0_u32), (1, 100, 50_000), (2, 1, 0)] {
            let mut bytes = super::super::protocol_10bit_tests::golden_frame(sequence);
            bytes[7..11].copy_from_slice(&seconds.to_be_bytes());
            bytes[11..15].copy_from_slice(&micros.to_be_bytes());
            sink.write_batch(&[langmuir_core::decode_scientific_frame(&bytes).unwrap()])
                .unwrap();
        }
        sink.close().unwrap();
        let session = HistorySession::start(path, 42, false).unwrap();
        loop {
            if let BackendEvent::HistoryStatus(status) =
                session.events.recv_timeout(Duration::from_secs(5)).unwrap()
                && !status.loading
            {
                assert!(status.error.is_some());
                assert_eq!(status.first_device_time, Some(100.0));
                assert_eq!(
                    status.last_device_time,
                    Some(100.05),
                    "a later clock epoch must not invert the primary navigation interval"
                );
                break;
            }
        }
    }

    #[test]
    fn incomplete_raw_history_keeps_the_window_and_reports_source_loss() {
        use langmuir_storage::{RawArchive, RawConfig, RawDestination, RawHeader, RawOrigin};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SYNTHETIC-incomplete.lvdsraw");
        let source = DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap();
        let binding = "0.0.0.0:1534".parse().unwrap();
        let mut raw = RawArchive::start(
            RawConfig::new(&path),
            RawHeader {
                origin: RawOrigin::LiveUdp,
                created_at: Duration::ZERO,
                expected_source: source,
                binding,
                session_id: [9; 16],
            },
        )
        .unwrap();
        raw.try_record(
            Duration::ZERO,
            source,
            RawDestination::SocketBinding(binding),
            true,
            None,
            &udp(1, &super::super::protocol_10bit_tests::golden_frame(1)),
        )
        .unwrap();
        raw.accounting()
            .mark_failed("SYNTHETIC raw source lost a datagram");
        raw.stop().unwrap();
        let session = HistorySession::start(path.clone(), 13, false).unwrap();
        let mut got_window = false;
        loop {
            match session.events.recv_timeout(Duration::from_secs(3)).unwrap() {
                BackendEvent::RecoveryStatus(status) => {
                    let text =
                        std::fs::read_to_string(status.audit_path.as_ref().unwrap()).unwrap();
                    let footer: serde_json::Value =
                        serde_json::from_str(text.lines().last().unwrap()).unwrap();
                    assert_eq!(
                        footer["complete"], false,
                        "incomplete raw input must retain an incomplete history audit"
                    );
                }
                BackendEvent::HistoryWindow(window) => got_window = !window.channel_1.is_empty(),
                BackendEvent::HistoryStatus(status) if !status.loading => {
                    assert!(got_window, "validated frames must remain viewable");
                    assert!(
                        status.error.is_some(),
                        "an incomplete END summary must remain visible"
                    );
                    break;
                }
                _ => {}
            }
        }
        let csv = directory.path().join("incomplete.csv");
        assert!(
            export_history_for_test(&path, &csv).is_err(),
            "a damaged source must not yield a successful export result"
        );
    }

    #[test]
    fn missing_raw_footer_retains_a_stable_readable_prefix_for_window_requests() {
        use langmuir_storage::{RawArchive, RawConfig, RawDestination, RawHeader, RawOrigin};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SYNTHETIC-no-footer.lvdsraw");
        let source = DEPLOYED_LVDS_UDP_SOURCE.parse().unwrap();
        let binding = "0.0.0.0:1534".parse().unwrap();
        let mut raw = RawArchive::start(
            RawConfig::new(&path),
            RawHeader {
                origin: RawOrigin::LiveUdp,
                created_at: Duration::ZERO,
                expected_source: source,
                binding,
                session_id: [4; 16],
            },
        )
        .unwrap();
        for sequence in 0..3_u8 {
            let mut bytes = super::super::protocol_10bit_tests::golden_frame(sequence);
            bytes[7..11].copy_from_slice(&(100 + u32::from(sequence)).to_be_bytes());
            bytes[11..15].copy_from_slice(&0_u32.to_be_bytes());
            raw.try_record(
                Duration::from_secs(u64::from(sequence)),
                source,
                RawDestination::SocketBinding(binding),
                true,
                None,
                &udp(u32::from(sequence), &bytes),
            )
            .unwrap();
        }
        raw.stop().unwrap();
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(file.metadata().unwrap().len() - 92).unwrap();
        drop(file);
        let mut reader = HistoryReader::open(&path, false).unwrap();
        for _ in 0..3 {
            assert_eq!(reader.next().unwrap().unwrap().len(), 1);
        }
        assert!(
            reader.next().unwrap().is_none(),
            "a validated prefix remains readable after interrupted recording"
        );
        assert!(reader.warning.is_some());
        assert!(
            reader.next().unwrap().is_none(),
            "the damaged tail is a stable stop boundary"
        );
        let (_cancel, cancel) = bounded(1);
        let window = read_window(
            &path,
            false,
            WindowRequest {
                generation: 1,
                revision: 2,
                range: [101.0, 102.1],
                selection: 0,
                monotonic: false,
            },
            &cancel,
            &AtomicU64::new(0),
        )
        .unwrap()
        .unwrap();
        assert_eq!(window.channel_1.len(), 200);
        assert!(export_history_for_test(&path, &directory.path().join("incomplete.csv")).is_err());
    }

    #[test]
    fn pcap_audit_record_ids_count_udp_records_and_keep_all_packet_indices_separate() {
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
        fn epb(packet: Vec<u8>) -> Vec<u8> {
            let mut body = vec![0; 12];
            body.extend_from_slice(&(u32::try_from(packet.len()).unwrap()).to_le_bytes());
            body.extend_from_slice(&(u32::try_from(packet.len()).unwrap()).to_le_bytes());
            body.extend(packet);
            block(6, body)
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SYNTHETIC-nonudp-first.pcapng");
        let mut section = 0x1A2B_3C4D_u32.to_le_bytes().to_vec();
        section.extend_from_slice(&1_u16.to_le_bytes());
        section.extend_from_slice(&0_u16.to_le_bytes());
        section.extend_from_slice(&u64::MAX.to_le_bytes());
        let mut bytes = block(0x0A0D_0D0A, section);
        let mut interface = 1_u16.to_le_bytes().to_vec();
        interface.extend_from_slice(&0_u16.to_le_bytes());
        interface.extend_from_slice(&65535_u32.to_le_bytes());
        bytes.extend(block(1, interface));
        let mut arp = vec![0_u8; 14];
        arp[12..14].copy_from_slice(&0x0806_u16.to_be_bytes());
        bytes.extend(epb(arp));
        let payload = udp(8, &super::super::protocol_10bit_tests::golden_frame(1));
        let length = u16::try_from(payload.len() + 28).unwrap();
        let mut ethernet = vec![0_u8; 14];
        ethernet[12..14].copy_from_slice(&0x0800_u16.to_be_bytes());
        let mut ip = vec![0_u8; 20];
        ip[0] = 0x45;
        ip[2..4].copy_from_slice(&length.to_be_bytes());
        ip[8] = 64;
        ip[9] = 17;
        ip[12..16].copy_from_slice(&[192, 168, 1, 128]);
        ip[16..20].copy_from_slice(&[192, 168, 1, 10]);
        ethernet.extend(ip);
        ethernet.extend_from_slice(&1534_u16.to_be_bytes());
        ethernet.extend_from_slice(&1534_u16.to_be_bytes());
        ethernet.extend_from_slice(&(length - 20).to_be_bytes());
        ethernet.extend_from_slice(&0_u16.to_be_bytes());
        ethernet.extend(payload);
        bytes.extend(epb(ethernet));
        std::fs::write(&path, bytes).unwrap();
        let mut reader = HistoryReader::open(&path, false).unwrap();
        assert!(reader.next().unwrap().unwrap().is_empty());
        let entries = reader.next().unwrap().unwrap();
        assert_eq!(reader.records, 2);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].info.record_id, 1,
            "raw archive IDs count UDP records, not unrelated EPBs"
        );
    }

    #[test]
    fn lpd_history_reads_actual_records_and_exports_full_samples() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SYNTHETIC-history.lpd");
        let mut sink = BinaryV3Sink::create(BinaryConfig::new(&path)).unwrap();
        let mut expected = Vec::new();
        for sequence in 0..25_u8 {
            let mut wire = super::super::protocol_10bit_tests::golden_frame(sequence);
            wire[11..15].copy_from_slice(&(u32::from(sequence) * 50_000 % 1_000_000).to_be_bytes());
            wire[7..11]
                .copy_from_slice(&(1_700_000_000_u32 + u32::from(sequence) / 20).to_be_bytes());
            expected.push(langmuir_core::decode_scientific_frame(&wire).unwrap());
        }
        sink.write_batch(&expected).unwrap();
        sink.close().unwrap();
        let scan = scan_history_for_test(&path).unwrap();
        assert_eq!(scan.format, HistoryFileKind::LpdV3);
        assert_eq!(scan.frames, expected);
        assert_eq!(scan.total_records, 25);
        let csv = directory.path().join("science.csv");
        assert_eq!(export_history_for_test(&path, &csv).unwrap(), 2500);
        assert_eq!(std::fs::read_to_string(&csv).unwrap().lines().count(), 2501);
        assert!(
            export_history_for_test(&path, &csv).is_err(),
            "never overwrite an export"
        );
    }

    #[test]
    fn malformed_or_legacy_history_is_rejected_without_fabricated_frames() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid.lpd");
        std::fs::write(&path, [0_u8; 40]).unwrap();
        assert!(scan_history_for_test(&path).is_err());
        std::fs::write(&path, b"LMPROBE\0\x02\0").unwrap();
        assert!(scan_history_for_test(&path).is_err());
    }
}
