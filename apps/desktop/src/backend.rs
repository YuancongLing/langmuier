use std::{
    collections::{BTreeMap, VecDeque},
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{BufWriter, ErrorKind, Write},
    net::{SocketAddr, UdpSocket},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use chrono::Local;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};
use langmuir_core::{
    Command, DEPLOYED_LVDS_RAW_FRAME_LEN, FrameError, LvdsStreamDecoder, Rs422StreamDecoder,
    Rs422Telemetry, SAMPLE_INTERVAL_MICROS, ScientificFrame, UdpLvdsDisposition, UdpLvdsError,
    UdpLvdsReassembler, UdpLvdsScienceGate, UdpLvdsSequenceMetrics, UdpLvdsSequenceMonitor,
    parse_udp_lvds_datagram,
};
use langmuir_storage::{BatchWriter, StorageEvent, StorageServiceConfig};
#[cfg(not(any(feature = "hdf5", feature = "hdf5-static")))]
use langmuir_storage::{BinaryConfig, BinarySink};
#[cfg(any(feature = "hdf5", feature = "hdf5-static"))]
use langmuir_storage::{Hdf5Config, Hdf5Sink};
use langmuir_transport::ReaderMetricsSnapshot;
use langmuir_transport::{
    BackpressurePolicy, Connector, ReaderConfig, ReaderEvent, ReaderWorker, SerialConfig,
    SerialConnector, WriteTicket, available_serial_ports,
};
use langmuir_transport::{CapturePacket, PcapNgReader};

use crate::bridge::{
    BackendBridge, BackendEvent, BackendStateSnapshot, BridgeError, LinkKind, LinkMetricsSnapshot,
    LogLevel, LvdsReceiveStats, LvdsStatusSnapshot, ReplayProgress, ScientificStorageStatus,
    TELEMETRY_POLL_INTERVAL_MAX_MILLIS, TELEMETRY_POLL_INTERVAL_MIN_MILLIS, TelemetrySnapshot,
    UiRequest,
};

const BRIDGE_REQUEST_CAPACITY: usize = 128;
const CRITICAL_EVENT_CAPACITY: usize = 1_024;
const DISPLAY_EVENT_CAPACITY: usize = 8;
const CONTROLLER_TICK: Duration = Duration::from_millis(5);
const METRICS_INTERVAL: Duration = Duration::from_millis(500);
const STORAGE_SUBMIT_TIMEOUT: Duration = Duration::from_millis(250);
const BACKEND_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const READER_STOP_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
const STORAGE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const CSV_TASK_DROP_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_TELEMETRY_ROWS: usize = 100_000;
const MAX_READER_EVENTS_PER_TICK: usize = 1_024;
const DEPLOYED_LVDS_UDP_SOURCE: &str = "192.168.1.128:1534";
static NEXT_LVDS_SOURCE: AtomicU64 = AtomicU64::new(1);

fn source_identity(label: &str) -> String {
    let mut id = NEXT_LVDS_SOURCE.load(Ordering::Relaxed);
    loop {
        let next = id
            .checked_add(1)
            .expect("process-local LVDS source identity exhausted");
        match NEXT_LVDS_SOURCE.compare_exchange_weak(id, next, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return format!("{label} · 本机接收会话 {id}"),
            Err(current) => id = current,
        }
    }
}

/// File I/O stays on this bounded worker; pacing and protocol state stay in
/// the controller. Cancellation never sends a network packet.
struct PcapReplay {
    packets: Receiver<Result<Option<CapturePacket>, String>>,
    cancel: Sender<()>,
    join: Option<JoinHandle<()>>,
    ingress: UdpLvdsIngress,
    progress: ReplayProgress,
    pending: Option<CapturePacket>,
    first_timestamp: Option<Duration>,
    last_timestamp: Option<Duration>,
    started: Instant,
}

impl PcapReplay {
    fn start(path: String) -> std::io::Result<Self> {
        let (tx, packets) = bounded(64);
        let (cancel, cancelled) = bounded(1);
        let worker_path = path.clone();
        let join = thread::Builder::new()
            .name("lvds-pcap-reader".to_owned())
            .spawn(move || {
                let run = || -> Result<(), String> {
                    let file = fs::File::open(&worker_path).map_err(|e| e.to_string())?;
                    let mut reader = PcapNgReader::new(std::io::BufReader::new(file));
                    while cancelled.is_empty() {
                        let Some(packet) = reader.next_packet().map_err(|e| e.to_string())? else {
                            return Ok(());
                        };
                        let mut item = Ok(Some(packet));
                        loop {
                            match tx.send_timeout(item, Duration::from_millis(20)) {
                                Ok(()) => break,
                                Err(crossbeam_channel::SendTimeoutError::Timeout(value)) => {
                                    if !cancelled.is_empty() {
                                        return Ok(());
                                    }
                                    item = value;
                                }
                                Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                                    return Ok(());
                                }
                            }
                        }
                    }
                    Ok(())
                };
                {
                    let mut item = run().map(|()| None);
                    while cancelled.is_empty() {
                        match tx.send_timeout(item, Duration::from_millis(20)) {
                            Ok(()) | Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                                break;
                            }
                            Err(crossbeam_channel::SendTimeoutError::Timeout(value)) => {
                                item = value
                            }
                        }
                    }
                }
            })?;
        let ingress = UdpLvdsIngress::new(
            DEPLOYED_LVDS_UDP_SOURCE
                .parse()
                .expect("fixed source endpoint"),
        );
        Ok(Self {
            packets,
            cancel,
            join: Some(join),
            ingress,
            progress: ReplayProgress {
                path,
                total_packets: 0,
                target_udp_packets: 0,
                finished: false,
                error: None,
            },
            pending: None,
            first_timestamp: None,
            last_timestamp: None,
            started: Instant::now(),
        })
    }

    fn drain_deliveries(&mut self, now: Instant, deliveries: &mut Vec<ScientificFrame>) {
        if self.progress.finished {
            return;
        }
        for _ in 0..64 {
            if self.pending.is_none() {
                match self.packets.try_recv() {
                    Ok(Ok(Some(packet))) => self.pending = Some(packet),
                    Ok(Ok(None)) => {
                        self.progress.finished = true;
                        break;
                    }
                    Ok(Err(error)) => {
                        self.progress.error = Some(error);
                        self.progress.finished = true;
                        break;
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        self.progress.error = Some("PCAP reader 在完成信号前异常终止".to_owned());
                        self.progress.finished = true;
                        break;
                    }
                }
            }
            let packet = self.pending.as_ref().expect("pending replay packet");
            let first = *self.first_timestamp.get_or_insert(packet.timestamp);
            if self
                .last_timestamp
                .is_some_and(|last| packet.timestamp < last)
                || packet.timestamp < first
            {
                self.progress.error = Some("抓包时间倒退，已停止，未改写时间或跳过报文".to_owned());
                self.progress.finished = true;
                break;
            }
            let offset = packet.timestamp.saturating_sub(first);
            if offset > now.saturating_duration_since(self.started) {
                break;
            }
            let packet = self.pending.take().expect("pending replay packet");
            self.last_timestamp = Some(packet.timestamp);
            self.progress.total_packets = self.progress.total_packets.saturating_add(1);
            if let Some(udp) = packet.udp
                && udp.source == self.ingress.expected_source
            {
                self.progress.target_udp_packets =
                    self.progress.target_udp_packets.saturating_add(1);
                // Protocol TTL/dedup use capture-relative time, not file read speed.
                let Some(protocol_now) = self.started.checked_add(offset) else {
                    self.progress.error = Some("抓包时间超出可表示范围".to_owned());
                    self.progress.finished = true;
                    break;
                };
                self.ingress
                    .push_at(udp.source, &udp.payload, protocol_now, |delivery| {
                        deliveries.push(delivery)
                    });
            }
        }
        if let Some(first) = self.first_timestamp {
            let last = self.last_timestamp.unwrap_or(first).saturating_sub(first);
            let offset = self.pending.as_ref().map_or(last, |packet| {
                now.saturating_duration_since(self.started)
                    .min(packet.timestamp.saturating_sub(first))
            });
            if let Some(time) = self.started.checked_add(offset) {
                self.ingress.expire_at(time);
            }
        }
    }
}

impl Drop for PcapReplay {
    fn drop(&mut self) {
        let _ = self.cancel.try_send(());
        if self.join.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(join) = self.join.take()
        {
            let _ = join.join();
        }
    }
}

/// Headless, read-only audit using the exact live protocol ingress. Capture
/// timestamps drive protocol deadlines; no wall-clock sleeping or network I/O.
pub fn audit_pcap(path: &str) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut reader = PcapNgReader::new(std::io::BufReader::new(file));
    let source = DEPLOYED_LVDS_UDP_SOURCE
        .parse()
        .map_err(|e: std::net::AddrParseError| e.to_string())?;
    let mut ingress = UdpLvdsIngress::new(source);
    let start = Instant::now();
    let mut first = None;
    let mut last = None;
    let mut total = 0_u64;
    let mut target = 0_u64;
    let mut formal = 0_u64;
    while let Some(packet) = reader.next_packet().map_err(|e| e.to_string())? {
        let epoch = *first.get_or_insert(packet.timestamp);
        if packet.timestamp < epoch || last.is_some_and(|last| packet.timestamp < last) {
            return Err("抓包时间倒退".to_owned());
        }
        last = Some(packet.timestamp);
        total = total.saturating_add(1);
        let at = start
            .checked_add(packet.timestamp.saturating_sub(epoch))
            .ok_or("抓包时间溢出")?;
        if let Some(udp) = packet.udp
            && udp.source == source
        {
            target = target.saturating_add(1);
            ingress.push_at(udp.source, &udp.payload, at, |_| {
                formal = formal.saturating_add(1)
            });
        }
        ingress.expire_at(at);
    }
    Ok(format!(
        "crc_validation=disabled total_packets={total} target_udp_packets={target} formal_science={formal} file_frames=NOT_RUN\nstats={:?}\nprotocol={:?}",
        ingress.receive_stats, ingress.metrics,
    ))
}

#[cfg(test)]
#[path = "backend_e2e_tests.rs"]
mod e2e_tests;

#[cfg(test)]
#[path = "backend_replay_tests.rs"]
mod replay_tests;

#[derive(Debug, Clone, Copy)]
struct TelemetryPolling {
    interval: Duration,
    next_due: Instant,
}

impl TelemetryPolling {
    fn new(interval: Duration, now: Instant) -> Self {
        Self {
            interval,
            next_due: now,
        }
    }

    fn is_due(self, now: Instant) -> bool {
        now >= self.next_due
    }

    fn record_send(&mut self, now: Instant) {
        // A paused controller must resume at one interval from the actual send,
        // rather than burst a backlog of stale telemetry requests.
        self.next_due = now.checked_add(self.interval).unwrap_or(now);
    }

    fn interval_millis(self) -> u32 {
        u32::try_from(self.interval.as_millis()).unwrap_or(u32::MAX)
    }
}

/// Datagram-aware receiver for the deployed LVDS-to-UDP converter. The desktop
/// controller owns it directly so neither packet boundaries nor source addresses
/// are lost before LVDS fragment reassembly.
struct UdpLvdsReceiver {
    socket: UdpSocket,
    ingress: UdpLvdsIngress,
    buffer: Vec<u8>,
}

/// The production datagram validation path without socket I/O. Protocol tests
/// use this same gate before invoking the controller's storage/display handler.
struct UdpLvdsIngress {
    expected_source: SocketAddr,
    reassembler: UdpLvdsReassembler,
    science_gate: UdpLvdsScienceGate,
    sequence_monitor: UdpLvdsSequenceMonitor,
    metrics: ReaderMetricsSnapshot,
    receive_stats: LvdsReceiveStats,
    last_parse_error: Option<FrameError>,
    last_datagram: Option<Instant>,
    last_science: Option<Instant>,
}

impl UdpLvdsReceiver {
    fn bind(bind_address: SocketAddr, expected_source: SocketAddr) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(bind_address)?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            ingress: UdpLvdsIngress::new(expected_source),
            buffer: vec![0_u8; 65_535],
        })
    }

    fn drain(
        &mut self,
        limit: usize,
        mut on_delivery: impl FnMut(ScientificFrame),
    ) -> std::io::Result<()> {
        // The controller drains on every tick, including quiet sockets. Keep
        // incomplete-frame lifetime and visible gauges independent of traffic.
        self.ingress.expire();
        for _ in 0..limit {
            let (bytes, source) = match self.socket.recv_from(&mut self.buffer) {
                Ok(result) => result,
                Err(error) if matches!(error.kind(), ErrorKind::WouldBlock) => break,
                Err(error) => {
                    self.ingress.metrics.io_errors =
                        self.ingress.metrics.io_errors.saturating_add(1);
                    return Err(error);
                }
            };
            self.ingress.push_at(
                source,
                &self.buffer[..bytes],
                Instant::now(),
                &mut on_delivery,
            );
        }
        Ok(())
    }

    #[must_use]
    const fn metrics(&self) -> ReaderMetricsSnapshot {
        self.ingress.metrics
    }
}

impl UdpLvdsIngress {
    fn new(expected_source: SocketAddr) -> Self {
        Self {
            expected_source,
            reassembler: UdpLvdsReassembler::default(),
            science_gate: UdpLvdsScienceGate::default(),
            sequence_monitor: UdpLvdsSequenceMonitor::default(),
            metrics: ReaderMetricsSnapshot::default(),
            receive_stats: LvdsReceiveStats::default(),
            last_parse_error: None,
            last_datagram: None,
            last_science: None,
        }
    }

    #[cfg(test)]
    fn push(&mut self, source: SocketAddr, bytes: &[u8], on_frame: impl FnMut(ScientificFrame)) {
        self.push_at(source, bytes, Instant::now(), on_frame);
    }

    fn push_at(
        &mut self,
        source: SocketAddr,
        bytes: &[u8],
        now: Instant,
        mut on_frame: impl FnMut(ScientificFrame),
    ) {
        self.expire_at(now);
        if source != self.expected_source {
            return;
        }
        self.last_datagram = Some(now);
        self.receive_stats.udp_packets = self.receive_stats.udp_packets.saturating_add(1);
        self.metrics.read_calls = self.metrics.read_calls.saturating_add(1);
        self.metrics.bytes_read = self
            .metrics
            .bytes_read
            .saturating_add(u64::try_from(bytes.len()).expect("UDP datagram length fits in u64"));

        let Ok(datagram) = parse_udp_lvds_datagram(bytes) else {
            self.record_udp_fragment_error();
            return;
        };
        self.receive_stats.outer_valid = self.receive_stats.outer_valid.saturating_add(1);
        let frame_length = usize::try_from(datagram.header.frame_length).unwrap_or(usize::MAX);
        if frame_length != DEPLOYED_LVDS_RAW_FRAME_LEN {
            // This length is declared by the UDP envelope, before an inner
            // scientific-frame candidate exists; keep its category separate.
            self.record_udp_fragment_error();
            return;
        }
        let result = self.reassembler.push_at(bytes, now);
        self.sync_reassembly_metrics();
        match result {
            Ok(Some(frame)) => {
                let result = self.science_gate.accept_at(&frame, now);
                self.record_complete_result(result.as_ref().err());
                self.sync_dedup_metrics();
                match result {
                    Ok(outcome) => {
                        if outcome.disposition() == UdpLvdsDisposition::SequenceConflict {
                            self.metrics.rejected_frames =
                                self.metrics.rejected_frames.saturating_add(1);
                        }
                        if let Some(scientific) = outcome.into_frame() {
                            // Observe only Accepted science, before the existing
                            // callback, while the complete UDP u32 is available.
                            self.sequence_monitor.observe_at(frame.frame_sequence, now);
                            self.metrics.udp_sequence = self.sequence_monitor.metrics();
                            self.metrics.scientific_frames_delivered =
                                self.metrics.scientific_frames_delivered.saturating_add(1);
                            self.last_science = Some(now);
                            on_frame(scientific);
                        }
                    }
                    Err(error) => self.metrics.record_frame_error(&error),
                }
            }
            Ok(None) => {}
            Err(UdpLvdsError::TooManyInFlightFrames { .. }) => {
                self.metrics.rejected_frames = self.metrics.rejected_frames.saturating_add(1);
            }
            Err(_) => self.record_udp_fragment_error(),
        }
    }

    fn expire(&mut self) -> usize {
        self.expire_at(Instant::now())
    }

    fn expire_at(&mut self, now: Instant) -> usize {
        let expired = self.reassembler.expire_at(now);
        self.science_gate.expire_at(now);
        self.sequence_monitor.expire_at(now);
        self.sync_reassembly_metrics();
        self.sync_dedup_metrics();
        self.metrics.udp_sequence = self.sequence_monitor.metrics();
        expired
    }

    fn sync_reassembly_metrics(&mut self) {
        let metrics = self.reassembler.metrics();
        self.metrics.reassembly_timeouts = metrics.expired_frames;
        self.metrics.reassembly_capacity_rejections = metrics.capacity_rejections;
        self.metrics.reassembled_frames = metrics.completed_frames;
        self.metrics.reassembly_pending_frames =
            u64::try_from(metrics.in_flight_frames).unwrap_or(u64::MAX);
        self.metrics.reassembly_buffered_bytes =
            u64::try_from(metrics.buffered_payload_bytes).unwrap_or(u64::MAX);
    }

    fn record_udp_fragment_error(&mut self) {
        self.metrics.rejected_frames = self.metrics.rejected_frames.saturating_add(1);
        self.metrics.udp_fragment_errors = self.metrics.udp_fragment_errors.saturating_add(1);
    }

    fn sync_dedup_metrics(&mut self) {
        let metrics = self.science_gate.metrics();
        self.metrics.frames_decoded = metrics.validated_frames;
        self.metrics.duplicate_frames_suppressed = metrics.duplicate_frames;
        self.metrics.sequence_conflicts = metrics.sequence_conflicts;
        self.metrics.dedup_cached_frames = u64::try_from(metrics.cached_frames).unwrap_or(u64::MAX);
        self.metrics.dedup_cached_bytes =
            u64::try_from(metrics.cached_payload_bytes).unwrap_or(u64::MAX);
        self.receive_stats.accepted_frames = metrics.accepted_frames;
        self.receive_stats.duplicates = metrics.duplicate_frames;
        self.receive_stats.conflicts = metrics.sequence_conflicts;
    }

    /// Reuses the gate's parse result: no second decoder, CRC calculation, or
    /// raw-byte modification is involved in receive/display/storage admission.
    fn record_complete_result(&mut self, error: Option<&FrameError>) {
        self.last_parse_error = error.cloned();
        let stats = &mut self.receive_stats;
        stats.complete_frames = stats.complete_frames.saturating_add(1);
        if !matches!(
            error,
            Some(
                FrameError::InvalidHeader { .. }
                    | FrameError::InvalidTail { .. }
                    | FrameError::UnexpectedLength { .. }
                    | FrameError::InvalidDeclaredLength { .. }
                    | FrameError::UnexpectedField { .. }
            )
        ) {
            stats.structure_valid_frames = stats.structure_valid_frames.saturating_add(1);
        }
        if let Some(error) = error {
            stats.parse_failures = stats.parse_failures.saturating_add(1);
            let counter = match error {
                FrameError::InvalidHeader { .. } | FrameError::InvalidTail { .. } => {
                    &mut stats.header_errors
                }
                FrameError::UnexpectedLength { .. } | FrameError::InvalidDeclaredLength { .. } => {
                    &mut stats.length_errors
                }
                FrameError::UnexpectedField { .. } => &mut stats.field_errors,
                FrameError::FieldOutOfRange { .. } => &mut stats.timestamp_errors,
                FrameError::SampleOutOfRange { .. } => &mut stats.adc_errors,
                // Shared error enum includes RS422 checksum errors; the LVDS
                // production decoder no longer produces this variant.
                FrameError::ChecksumMismatch { .. } => return,
            };
            *counter = counter.saturating_add(1);
        } else {
            stats.parsed_frames = stats.parsed_frames.saturating_add(1);
        }
    }

    fn receive_state(&self, now: Instant) -> &'static str {
        let Some(last) = self.last_datagram else {
            return "已绑定，等待数据";
        };
        if now.saturating_duration_since(last) >= Duration::from_secs(2) {
            return "数据流超时 / 中断";
        }
        if let Some(error) = &self.last_parse_error {
            return if matches!(error, FrameError::FieldOutOfRange { .. }) {
                "科学帧解析失败：无法建立可信设备时间"
            } else {
                "收到 LVDS，解析失败"
            };
        }
        if self
            .last_science
            .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(2))
        {
            return "正在收到有效科学帧";
        }
        "正在收到 UDP 数据 / 等待完整 LVDS"
    }
}

enum ControllerRequest {
    Ui(UiRequest),
    Shutdown,
}

struct CsvExportTask {
    result: Receiver<Result<PathBuf, String>>,
    completion: Receiver<()>,
    join: Option<JoinHandle<()>>,
}

impl CsvExportTask {
    fn spawn(path: PathBuf, rows: VecDeque<TelemetrySnapshot>) -> std::io::Result<Self> {
        let (result_tx, result) = bounded(1);
        let (completion_tx, completion) = bounded(1);
        let join = thread::Builder::new()
            .name("langmuir-csv-export".to_owned())
            .spawn(move || {
                let export_result = write_telemetry_csv(&path, &rows)
                    .map(|()| path)
                    .map_err(|error| error.to_string());
                let _ = result_tx.send(export_result);
                let _ = completion_tx.send(());
            })?;
        Ok(Self {
            result,
            completion,
            join: Some(join),
        })
    }

    fn try_result(&self) -> Option<Result<PathBuf, String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(crossbeam_channel::TryRecvError::Empty) => None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                Some(Err("CSV 导出线程异常终止".to_owned()))
            }
        }
    }
}

impl Drop for CsvExportTask {
    fn drop(&mut self) {
        match self.completion.recv_timeout(CSV_TASK_DROP_TIMEOUT) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(join) = self.join.take() {
                    let _ = join.join();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // The export owns only its snapshot and output file. Detaching
                // after a bounded wait keeps application shutdown responsive.
                self.join.take();
            }
        }
    }
}

pub struct RealBackendBridge {
    requests: Sender<ControllerRequest>,
    critical_events: Receiver<BackendEvent>,
    display_events: Receiver<BackendEvent>,
    latest_state: Arc<Mutex<BackendStateSnapshot>>,
    last_state_revision: u64,
    completion: Receiver<()>,
    join: Option<JoinHandle<()>>,
}

impl RealBackendBridge {
    pub fn spawn() -> Result<Self, BridgeError> {
        let (requests, request_rx) = bounded(BRIDGE_REQUEST_CAPACITY);
        let (critical_event_tx, critical_events) = bounded(CRITICAL_EVENT_CAPACITY);
        let critical_event_reclaim = critical_events.clone();
        let (display_event_tx, display_events) = bounded(DISPLAY_EVENT_CAPACITY);
        let display_event_reclaim = display_events.clone();
        let dropped_critical = Arc::new(AtomicU64::new(0));
        let dropped_critical_for_controller = Arc::clone(&dropped_critical);
        let (completion_tx, completion) = bounded(1);
        let latest_state = Arc::new(Mutex::new(BackendStateSnapshot::default()));
        let latest_state_for_controller = Arc::clone(&latest_state);
        let join = thread::Builder::new()
            .name("langmuir-backend".to_owned())
            .spawn(move || {
                let mut controller = Controller::new(
                    critical_event_tx,
                    critical_event_reclaim,
                    display_event_tx,
                    dropped_critical_for_controller,
                    latest_state_for_controller,
                );
                controller.display_event_reclaim = Some(display_event_reclaim);
                controller.run(&request_rx);
                let _ = completion_tx.send(());
            })
            .map_err(|error| BridgeError(format!("无法启动生产后端: {error}")))?;
        Ok(Self {
            requests,
            critical_events,
            display_events,
            latest_state,
            last_state_revision: 0,
            completion,
            join: Some(join),
        })
    }
}

impl BackendBridge for RealBackendBridge {
    fn request(&mut self, request: UiRequest) -> Result<(), BridgeError> {
        self.requests
            .try_send(ControllerRequest::Ui(request))
            .map_err(|error| match error {
                TrySendError::Full(_) => BridgeError("生产后端请求队列已满".to_owned()),
                TrySendError::Disconnected(_) => BridgeError("生产后端已停止".to_owned()),
            })
    }

    fn try_next_event(&mut self) -> Option<BackendEvent> {
        self.critical_events.try_recv().ok().or_else(|| {
            let snapshot = lock_unpoisoned(&self.latest_state).clone();
            if snapshot.revision == self.last_state_revision {
                self.display_events.try_recv().ok()
            } else {
                self.last_state_revision = snapshot.revision;
                Some(BackendEvent::StateSnapshot(snapshot))
            }
        })
    }
}

impl Drop for RealBackendBridge {
    fn drop(&mut self) {
        let _ = self
            .requests
            .send_timeout(ControllerRequest::Shutdown, Duration::from_secs(1));
        match self.completion.recv_timeout(BACKEND_SHUTDOWN_TIMEOUT) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(join) = self.join.take() {
                    let _ = join.join();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // Dropping a JoinHandle detaches the still-owned resources. The
                // UI can exit even if an OS driver or sink violates its timeout.
                self.join.take();
            }
        }
    }
}

struct Controller {
    critical_events: Sender<BackendEvent>,
    critical_event_reclaim: Receiver<BackendEvent>,
    display_events: Sender<BackendEvent>,
    display_event_reclaim: Option<Receiver<BackendEvent>>,
    last_lvds_view: Option<String>,
    dropped_critical: Arc<AtomicU64>,
    latest_state: Arc<Mutex<BackendStateSnapshot>>,
    rs422_display_dropped: AtomicU64,
    lvds_display_dropped: AtomicU64,
    rs422: Option<ReaderWorker<Rs422Telemetry>>,
    lvds: Option<ReaderWorker<ScientificFrame>>,
    lvds_udp: Option<UdpLvdsReceiver>,
    rs422_connected: bool,
    lvds_connected: bool,
    rs422_endpoint: String,
    lvds_endpoint: String,
    last_rs422_fault: Option<String>,
    last_lvds_fault: Option<String>,
    last_storage_fault: Option<String>,
    scientific_writer: Option<BatchWriter<ScientificFrame>>,
    acquisition_running: bool,
    telemetry_polling: Option<TelemetryPolling>,
    telemetry_rows: VecDeque<TelemetrySnapshot>,
    pending_commands: BTreeMap<u64, (WriteTicket, [u8; 12])>,
    last_rs422_metrics: Option<(bool, ReaderMetricsSnapshot)>,
    last_lvds_metrics: Option<(bool, ReaderMetricsSnapshot)>,
    last_rs422_metrics_emitted: Option<(bool, ReaderMetricsSnapshot, u64)>,
    last_lvds_metrics_emitted: Option<(bool, ReaderMetricsSnapshot, u64)>,
    last_metrics_sample: Instant,
    csv_export: Option<CsvExportTask>,
    capture_path: Option<PathBuf>,
    replay: Option<PcapReplay>,
    lvds_source: String,
    storage_status: ScientificStorageStatus,
    last_lvds_status: Instant,
}

impl Controller {
    fn new(
        critical_events: Sender<BackendEvent>,
        critical_event_reclaim: Receiver<BackendEvent>,
        display_events: Sender<BackendEvent>,
        dropped_critical: Arc<AtomicU64>,
        latest_state: Arc<Mutex<BackendStateSnapshot>>,
    ) -> Self {
        Self {
            critical_events,
            critical_event_reclaim,
            display_events,
            display_event_reclaim: None,
            last_lvds_view: None,
            dropped_critical,
            latest_state,
            rs422_display_dropped: AtomicU64::new(0),
            lvds_display_dropped: AtomicU64::new(0),
            rs422: None,
            lvds: None,
            lvds_udp: None,
            rs422_connected: false,
            lvds_connected: false,
            rs422_endpoint: String::new(),
            lvds_endpoint: String::new(),
            last_rs422_fault: None,
            last_lvds_fault: None,
            last_storage_fault: None,
            scientific_writer: None,
            acquisition_running: false,
            telemetry_polling: None,
            telemetry_rows: VecDeque::new(),
            pending_commands: BTreeMap::new(),
            last_rs422_metrics: None,
            last_lvds_metrics: None,
            last_rs422_metrics_emitted: None,
            last_lvds_metrics_emitted: None,
            last_metrics_sample: Instant::now()
                .checked_sub(METRICS_INTERVAL)
                .unwrap_or_else(Instant::now),
            csv_export: None,
            capture_path: None,
            replay: None,
            lvds_source: String::new(),
            storage_status: ScientificStorageStatus::default(),
            last_lvds_status: Instant::now(),
        }
    }

    fn run(mut self, requests: &Receiver<ControllerRequest>) {
        let mut stopping = false;
        while !stopping {
            match requests.recv_timeout(CONTROLLER_TICK) {
                Ok(ControllerRequest::Ui(request)) => self.handle_request(request),
                Ok(ControllerRequest::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                    stopping = true;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            while let Ok(request) = requests.try_recv() {
                match request {
                    ControllerRequest::Ui(request) => self.handle_request(request),
                    ControllerRequest::Shutdown => {
                        stopping = true;
                        break;
                    }
                }
            }
            self.poll_telemetry_request(Instant::now());
            self.drain_rs422();
            self.drain_lvds();
            self.drain_udp_lvds();
            self.drain_replay();
            self.poll_reader_statuses();
            self.sample_reader_metrics();
            self.drain_command_acks();
            self.drain_storage();
            if self.last_lvds_status.elapsed() >= METRICS_INTERVAL {
                self.publish_lvds_status();
            }
            self.poll_csv_export();
            self.flush_critical_drop_summary();
        }
        self.disconnect(LinkKind::Lvds);
        self.disconnect(LinkKind::Rs422);
        self.stop_acquisition();
    }

    fn handle_request(&mut self, request: UiRequest) {
        match request {
            UiRequest::RefreshPorts => match available_serial_ports() {
                Ok(ports) => {
                    if ports.is_empty() {
                        self.log(LogLevel::Warning, "未发现可用串口");
                    }
                    self.emit_critical(BackendEvent::PortsUpdated { rs422: ports });
                }
                Err(error) => self.fault("串口枚举", error.to_string()),
            },
            UiRequest::Connect { link, port, baud } => self.connect(link, &port, baud),
            UiRequest::BindLvdsUdp { bind_address, port } => {
                self.bind_lvds_udp(&bind_address, port);
            }
            UiRequest::Disconnect { link } => {
                self.disconnect(link);
            }
            UiRequest::StartAcquisition => self.start_acquisition(),
            UiRequest::StopAcquisition => self.stop_acquisition_at_ui_boundary(),
            UiRequest::StartPcapReplay { path } => self.start_pcap_replay(&path),
            UiRequest::StopPcapReplay => {
                if self.replay.is_some() {
                    self.disconnect(LinkKind::Lvds);
                }
                self.publish_lvds_status();
            }
            UiRequest::SetTelemetryPolling {
                enabled,
                interval_millis,
            } => self.set_telemetry_polling(enabled, interval_millis),
            UiRequest::SendCommand(frame) => {
                let _ = self.send_command(frame);
            }
            UiRequest::ExportCsv { suggested_name } => self.export_csv(&suggested_name),
            #[cfg(feature = "mock")]
            UiRequest::InjectMockFault => {
                self.fault("配置", "生产后端拒绝模拟异常请求");
            }
        }
    }

    fn start_pcap_replay(&mut self, path: &str) {
        if self.acquisition_running {
            self.fault("PCAP", "请先停止正式保存，再切换数据来源");
            return;
        }
        if !self.disconnect(LinkKind::Lvds) {
            return;
        }
        match PcapReplay::start(path.to_owned()) {
            Ok(replay) => {
                self.replay = Some(replay);
                self.lvds_connected = true;
                self.lvds_source = source_identity(&format!("离线 PCAP {path}"));
                self.lvds_endpoint = format!("离线回放 {path}");
                self.emit_critical(BackendEvent::LinkChanged {
                    link: LinkKind::Lvds,
                    connected: true,
                    detail: self.lvds_endpoint.clone(),
                });
                self.log(
                    LogLevel::Info,
                    "开始只读 PCAP 回放，按抓包时间推进；不会发送网络报文",
                );
            }
            Err(error) => self.fault("PCAP", error.to_string()),
        }
        self.publish_lvds_status();
    }

    fn drain_replay(&mut self) {
        let mut deliveries = Vec::new();
        let was_finished = self.replay.as_ref().is_none_or(|r| r.progress.finished);
        if let Some(replay) = &mut self.replay {
            replay.drain_deliveries(Instant::now(), &mut deliveries);
        }
        for delivery in deliveries {
            self.handle_scientific_frame(delivery);
        }
        if !was_finished && self.replay.as_ref().is_some_and(|r| r.progress.finished) {
            let error = self.replay.as_ref().and_then(|r| r.progress.error.clone());
            if let Some(error) = error {
                self.fault("PCAP", &error);
                if self.acquisition_running {
                    self.stop_acquisition_incomplete(error);
                }
            } else {
                if self.acquisition_running {
                    self.stop_acquisition();
                }
                self.log(LogLevel::Info, "离线回放完成");
            }
            self.lvds_connected = false;
            self.emit_critical(BackendEvent::LinkChanged {
                link: LinkKind::Lvds,
                connected: false,
                detail: "离线回放已结束".to_owned(),
            });
            self.publish_lvds_status();
        }
    }

    fn lvds_status(&self) -> LvdsStatusSnapshot {
        let ingress = self
            .lvds_udp
            .as_ref()
            .map(|r| &r.ingress)
            .or_else(|| self.replay.as_ref().map(|r| &r.ingress));
        let receive_state = if let Some(error) = &self.last_lvds_fault {
            format!("接收异常：{error}")
        } else if let Some(replay) = &self.replay {
            if let Some(error) = &replay.progress.error {
                format!("离线回放错误：{error}")
            } else if replay.progress.finished {
                "离线回放完成".to_owned()
            } else {
                format!(
                    "离线回放 · {}",
                    replay.ingress.receive_state(Instant::now())
                )
            }
        } else if let Some(ingress) = ingress {
            ingress.receive_state(Instant::now()).to_owned()
        } else if self.lvds.is_some() && self.lvds_connected {
            let metrics = self
                .lvds
                .as_ref()
                .map(ReaderWorker::metrics)
                .unwrap_or_default();
            format!(
                "原始 LVDS 字节流 · 已解码 {} 帧（无 UDP 外层序号）",
                metrics.frames_decoded
            )
        } else {
            "未监听".to_owned()
        };
        LvdsStatusSnapshot {
            source: self.lvds_source.clone(),
            receive_state,
            stats: ingress.map_or_else(LvdsReceiveStats::default, |i| i.receive_stats),
            replay: self.replay.as_ref().map(|r| r.progress.clone()),
            storage: self.storage_status.clone(),
        }
    }

    fn publish_lvds_status(&mut self) {
        self.last_lvds_status = Instant::now();
        let status = self.lvds_status();
        let view = status.source.clone();
        if self.last_lvds_view.as_ref() != Some(&view) {
            // Clear stale visuals at the producer boundary. Clearing later in
            // the bridge could also discard new-source frames already queued
            // after this status was published.
            if let Some(events) = &self.display_event_reclaim {
                while events.try_recv().is_ok() {}
            }
            self.last_lvds_view = Some(view);
        }
        self.emit_critical(BackendEvent::LvdsStatus(Box::new(status)));
    }

    fn connect(&mut self, link: LinkKind, port: &str, baud: u32) {
        if !self.disconnect(link) {
            self.fault(link.to_string(), "旧连接未能在期限内停止，已取消重连");
            return;
        }
        let connector = SerialConnector::new(SerialConfig::new(port.to_owned(), baud));
        let transport = match connector.connect() {
            Ok(transport) => transport,
            Err(error) => {
                self.fault(link.to_string(), error.to_string());
                self.emit_critical(BackendEvent::LinkChanged {
                    link,
                    connected: false,
                    detail: error.to_string(),
                });
                return;
            }
        };

        let result = match link {
            LinkKind::Rs422 => ReaderWorker::spawn(
                transport,
                Rs422StreamDecoder::new(),
                ReaderConfig {
                    backpressure: BackpressurePolicy::DropNewest,
                    ..ReaderConfig::default()
                },
            )
            .map(|worker| self.rs422 = Some(worker)),
            LinkKind::Lvds => ReaderWorker::spawn(
                transport,
                LvdsStreamDecoder::new(),
                ReaderConfig {
                    event_capacity: 2_048,
                    backpressure: BackpressurePolicy::Wait(Duration::from_millis(250)),
                    ..ReaderConfig::default()
                },
            )
            .map(|worker| self.lvds = Some(worker)),
        };
        if let Err(error) = result {
            self.fault(link.to_string(), error.to_string());
            self.emit_critical(BackendEvent::LinkChanged {
                link,
                connected: false,
                detail: error.to_string(),
            });
        } else {
            match link {
                LinkKind::Rs422 => {
                    self.rs422_endpoint = format!("{port} @ {baud}");
                    self.last_rs422_fault = None;
                }
                LinkKind::Lvds => {
                    self.lvds_endpoint = format!("{port} @ {baud}");
                    self.lvds_source =
                        source_identity(&format!("原始 LVDS 字节流 {port}（外层序号未知）"));
                    self.last_lvds_fault = None;
                }
            }
            self.log(LogLevel::Info, format!("正在连接 {link}: {port} @ {baud}"));
            if link == LinkKind::Lvds {
                self.publish_lvds_status();
            }
        }
    }

    fn bind_lvds_udp(&mut self, bind_address: &str, port: u16) {
        if !self.disconnect(LinkKind::Lvds) {
            self.fault("LVDS / UDP", "旧监听未能在期限内停止，已取消重新监听");
            return;
        }
        let endpoint = format!("{bind_address}:{port}");
        let socket_address = match endpoint.parse() {
            Ok(address) => address,
            Err(error) => {
                self.fault("LVDS / UDP", format!("监听地址无效 {endpoint}: {error}"));
                return;
            }
        };
        let expected_source = match DEPLOYED_LVDS_UDP_SOURCE.parse() {
            Ok(address) => address,
            Err(error) => {
                self.fault("LVDS / UDP", format!("固定源地址无效: {error}"));
                return;
            }
        };
        let receiver = match UdpLvdsReceiver::bind(socket_address, expected_source) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.fault("LVDS / UDP", error.to_string());
                self.last_lvds_fault = Some(error.to_string());
                self.emit_critical(BackendEvent::LinkChanged {
                    link: LinkKind::Lvds,
                    connected: false,
                    detail: error.to_string(),
                });
                self.publish_lvds_status();
                return;
            }
        };
        self.lvds_source = source_identity(&format!("实时 UDP {endpoint} <- {expected_source}"));
        self.lvds_udp = Some(receiver);
        self.lvds_connected = true;
        self.lvds_endpoint = format!("UDP {endpoint} <- {expected_source}");
        self.last_lvds_fault = None;
        self.last_lvds_metrics = None;
        self.last_lvds_metrics_emitted = None;
        self.emit_critical(BackendEvent::LinkChanged {
            link: LinkKind::Lvds,
            connected: true,
            detail: self.lvds_endpoint.clone(),
        });
        self.log(
            LogLevel::Info,
            format!("正在监听 LVDS / UDP: {endpoint}，仅接收 {expected_source}"),
        );
        self.publish_lvds_status();
    }

    fn disconnect(&mut self, link: LinkKind) -> bool {
        let result = match link {
            LinkKind::Rs422 => {
                self.stop_telemetry_polling();
                self.rs422_connected = false;
                self.rs422_endpoint.clear();
                self.last_rs422_fault = None;
                self.rs422.take().map(ReaderWorker::shutdown)
            }
            LinkKind::Lvds => {
                self.replay.take();
                self.lvds_udp.take();
                let result = self.stop_lvds_reader_and_drain();
                if self.acquisition_running || self.scientific_writer.is_some() {
                    self.stop_acquisition();
                }
                self.lvds_connected = false;
                self.lvds_endpoint.clear();
                self.last_lvds_fault = None;
                self.last_lvds_metrics = None;
                self.last_lvds_metrics_emitted = None;
                self.lvds_source.clear();
                result
            }
        };
        if let Some(Err(error)) = &result {
            self.fault(link.to_string(), error.to_string());
        }
        self.emit_critical(BackendEvent::LinkChanged {
            link,
            connected: false,
            detail: "未连接".to_owned(),
        });
        if link == LinkKind::Lvds {
            self.publish_lvds_status();
        }
        result.is_none_or(|value| value.is_ok())
    }

    fn start_acquisition(&mut self) {
        if self.acquisition_running {
            return;
        }
        if !self.lvds_connected {
            self.fault("采集", "请先连接 LVDS");
            return;
        }
        self.last_storage_fault = None;

        #[cfg(any(feature = "hdf5", feature = "hdf5-static"))]
        {
            let path = data_path("scientific", "h5");
            let sink = match Hdf5Sink::create(Hdf5Config::new(&path)) {
                Ok(sink) => sink,
                Err(error) => {
                    self.fault("HDF5", error.to_string());
                    return;
                }
            };
            match BatchWriter::start(
                sink,
                StorageServiceConfig {
                    control_timeout: STORAGE_SHUTDOWN_TIMEOUT,
                    ..StorageServiceConfig::default()
                },
            ) {
                Ok(writer) => {
                    self.scientific_writer = Some(writer);
                    self.capture_path = Some(path.clone());
                }
                Err(error) => {
                    self.fault("HDF5", error.to_string());
                    return;
                }
            }
            self.log(LogLevel::Info, format!("科学数据存储: {}", path.display()));
            self.acquisition_running = true;
            self.emit_critical(BackendEvent::AcquisitionChanged { running: true });
        }
        #[cfg(not(any(feature = "hdf5", feature = "hdf5-static")))]
        {
            let path = data_path("scientific", "lpd");
            let sink = match BinarySink::create(BinaryConfig::new(&path)) {
                Ok(sink) => sink,
                Err(error) => {
                    self.fault("二进制存储", error.to_string());
                    return;
                }
            };
            match BatchWriter::start(
                sink,
                StorageServiceConfig {
                    control_timeout: STORAGE_SHUTDOWN_TIMEOUT,
                    ..StorageServiceConfig::default()
                },
            ) {
                Ok(writer) => {
                    self.scientific_writer = Some(writer);
                    self.capture_path = Some(path.clone());
                }
                Err(error) => {
                    self.fault("二进制存储", error.to_string());
                    return;
                }
            }
            self.log(LogLevel::Info, format!("科学数据存储: {}", path.display()));
            self.acquisition_running = true;
            self.emit_critical(BackendEvent::AcquisitionChanged { running: true });
        }
        if self.acquisition_running {
            self.storage_status = ScientificStorageStatus {
                path: self.capture_path.as_ref().map(|p| p.display().to_string()),
                ..ScientificStorageStatus::default()
            };
            self.publish_lvds_status();
        }
    }

    fn stop_acquisition(&mut self) {
        self.stop_acquisition_with_reason(None);
    }

    fn stop_acquisition_incomplete(&mut self, reason: impl Into<String>) {
        self.stop_acquisition_with_reason(Some(reason.into()));
    }

    fn stop_acquisition_with_reason(&mut self, reason: Option<String>) {
        self.acquisition_running = false;
        let capture_path = self.capture_path.clone();
        let mut incomplete_reason = reason;
        if let Some(writer) = self.scientific_writer.take() {
            let metrics = writer.metrics();
            self.storage_status.enqueued = metrics.submitted_items;
            match writer.shutdown_timeout(STORAGE_SHUTDOWN_TIMEOUT) {
                Ok(exit) => {
                    self.storage_status.sink_written = exit.metrics.written_items;
                    self.storage_status.closed = true;
                }
                Err(error) => {
                    let message = error.to_string();
                    self.storage_status.error = Some(message.clone());
                    self.fault("科学数据存储", &message);
                    if incomplete_reason.is_none() {
                        incomplete_reason = Some(format!("存储关闭失败: {message}"));
                    }
                }
            }
        }
        if let Some(reason) = &incomplete_reason {
            self.storage_status.error = Some(reason.clone());
        }
        if let (Some(path), Some(reason)) = (capture_path.as_deref(), incomplete_reason.as_deref())
        {
            self.mark_capture_incomplete(path, reason);
        }
        self.capture_path = None;
        self.emit_critical(BackendEvent::AcquisitionChanged { running: false });
        self.publish_lvds_status();
    }

    /// Defines the manual stop boundary as all events already accepted by the
    /// LVDS event receiver when the UI request is handled. The reader remains
    /// connected; frames accepted after this snapshot are display-discarded.
    fn stop_acquisition_at_ui_boundary(&mut self) {
        let accepted = self.lvds.as_ref().map_or(0, |worker| worker.events().len());
        self.drain_lvds_limit(accepted);
        self.stop_acquisition();
    }

    fn stop_lvds_reader_and_drain(
        &mut self,
    ) -> Option<Result<langmuir_transport::WorkerExit, langmuir_transport::ReaderError>> {
        let active_capture = (self.acquisition_running || self.scientific_writer.is_some())
            .then(|| self.capture_path.clone())
            .flatten();
        self.lvds.as_ref()?.request_stop();
        let deadline = Instant::now()
            .checked_add(READER_STOP_DRAIN_TIMEOUT)
            .unwrap_or_else(Instant::now);
        loop {
            if Instant::now() >= deadline {
                break;
            }
            // Bound each pass. A single storage submission can wait 250 ms;
            // draining the complete 2,048-frame queue before checking the
            // deadline would make the deadline ineffective.
            self.drain_lvds_limit(1);
            let stopped = self.lvds.as_ref().is_none_or(|worker| {
                let status = worker.status();
                status.terminal.is_some() || status.panicked
            });
            if stopped {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        let join_wait = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        let result = self
            .lvds
            .as_mut()
            .map(|worker| worker.join_timeout(join_wait));
        let joined = result.as_ref().is_some_and(Result::is_ok);

        // Only a successful join establishes a producer boundary. Drain within
        // the same bounded deadline; otherwise mark the file explicitly as an
        // interrupted capture instead of claiming completeness.
        if joined {
            while self
                .lvds
                .as_ref()
                .is_some_and(|worker| !worker.events().is_empty())
                && Instant::now() < deadline
            {
                self.drain_lvds_limit(1);
            }
        }
        let queue_empty = self
            .lvds
            .as_ref()
            .is_none_or(|worker| worker.events().is_empty());
        if !joined || !queue_empty {
            let reason = if joined {
                "LVDS 已停止，但事件队列未在停机期限内排空；capture 可能缺少尾部帧"
            } else {
                "LVDS reader 未在停机期限内完成；capture 可能缺少尾部帧"
            };
            if let Some(path) = active_capture {
                self.mark_capture_incomplete(&path, reason);
            }
            self.fault("LVDS 停机", reason);
        }
        self.lvds.take();
        result
    }

    fn mark_capture_incomplete(&self, capture_path: &Path, reason: &str) {
        let mut marker_name = capture_path.as_os_str().to_os_string();
        marker_name.push(".incomplete");
        let marker_path = PathBuf::from(marker_name);
        let result = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&marker_path)
            .and_then(|file| {
                let mut writer = BufWriter::new(file);
                writeln!(writer, "capture={}", capture_path.display())?;
                writeln!(writer, "interrupted_at={}", Local::now().to_rfc3339())?;
                writeln!(writer, "reason={reason}")?;
                writer.flush()
            });
        match result {
            Ok(()) => self.log(
                LogLevel::Warning,
                format!("已写入不完整采集标记: {}", marker_path.display()),
            ),
            Err(error) => self.fault(
                "不完整采集标记",
                format!("无法写入 {}: {error}", marker_path.display()),
            ),
        }
    }

    fn set_telemetry_polling(&mut self, enabled: bool, interval_millis: u32) {
        if !enabled {
            self.stop_telemetry_polling();
            return;
        }
        if !(TELEMETRY_POLL_INTERVAL_MIN_MILLIS..=TELEMETRY_POLL_INTERVAL_MAX_MILLIS)
            .contains(&interval_millis)
        {
            self.fault(
                "循环遥测",
                format!(
                    "循环周期必须在 {TELEMETRY_POLL_INTERVAL_MIN_MILLIS}..={TELEMETRY_POLL_INTERVAL_MAX_MILLIS} ms"
                ),
            );
            return;
        }
        if !self.rs422_connected {
            self.fault("循环遥测", "RS422 未连接，不能启动循环请求");
            self.emit_critical(BackendEvent::TelemetryPollingChanged {
                active: false,
                interval_millis,
            });
            return;
        }

        let now = Instant::now();
        self.telemetry_polling = Some(TelemetryPolling::new(
            Duration::from_millis(u64::from(interval_millis)),
            now,
        ));
        self.poll_telemetry_request(now);
        if self.telemetry_polling.is_some() {
            self.emit_critical(BackendEvent::TelemetryPollingChanged {
                active: true,
                interval_millis,
            });
        }
    }

    fn stop_telemetry_polling(&mut self) {
        let Some(polling) = self.telemetry_polling.take() else {
            return;
        };
        self.emit_critical(BackendEvent::TelemetryPollingChanged {
            active: false,
            interval_millis: polling.interval_millis(),
        });
    }

    fn poll_telemetry_request(&mut self, now: Instant) {
        let Some(mut polling) = self.telemetry_polling else {
            return;
        };
        if !polling.is_due(now) {
            return;
        }
        if !self.send_command(Command::RequestTelemetry.encode().into_bytes()) {
            self.stop_telemetry_polling();
            return;
        }
        polling.record_send(now);
        self.telemetry_polling = Some(polling);
    }

    fn send_command(&mut self, frame: [u8; 12]) -> bool {
        let Some(worker) = self.rs422.as_ref().filter(|_| self.rs422_connected) else {
            self.fault("RS422", "未连接，指令未发送");
            return false;
        };
        match worker.try_send_bytes(frame.to_vec()) {
            Ok(ticket) => {
                self.pending_commands.insert(ticket.id(), (ticket, frame));
                true
            }
            Err(error) => {
                self.fault("RS422 指令队列", error.to_string());
                false
            }
        }
    }

    fn drain_rs422(&mut self) {
        let Some(worker) = &self.rs422 else {
            return;
        };
        let events: Vec<_> = worker
            .events()
            .try_iter()
            .take(MAX_READER_EVENTS_PER_TICK)
            .collect();
        for event in events {
            match event {
                ReaderEvent::Started { endpoint } => {
                    self.rs422_connected = true;
                    self.emit_critical(BackendEvent::LinkChanged {
                        link: LinkKind::Rs422,
                        connected: true,
                        detail: endpoint,
                    });
                }
                ReaderEvent::Frame(telemetry) => {
                    let snapshot = telemetry_snapshot(&telemetry);
                    self.telemetry_rows.push_back(snapshot.clone());
                    while self.telemetry_rows.len() > MAX_TELEMETRY_ROWS {
                        self.telemetry_rows.pop_front();
                    }
                    self.emit_display(LinkKind::Rs422, BackendEvent::Telemetry(snapshot));
                }
                ReaderEvent::BytesWritten { .. }
                | ReaderEvent::WriteFailed { .. }
                | ReaderEvent::Fault(_) => {}
                ReaderEvent::Stopped { reason } => {
                    self.stop_telemetry_polling();
                    self.rs422_connected = false;
                    self.emit_critical(BackendEvent::LinkChanged {
                        link: LinkKind::Rs422,
                        connected: false,
                        detail: format!("停止: {reason:?}"),
                    });
                }
            }
        }
    }

    fn drain_lvds(&mut self) {
        self.drain_lvds_limit(MAX_READER_EVENTS_PER_TICK);
        if self.acquisition_running && !self.lvds_connected {
            self.stop_acquisition_incomplete("LVDS reader stopped before acquisition completed");
        }
    }

    fn drain_udp_lvds(&mut self) {
        let mut deliveries = Vec::new();
        let result = self.lvds_udp.as_mut().map(|receiver| {
            receiver.drain(MAX_READER_EVENTS_PER_TICK, |delivery| {
                deliveries.push(delivery);
            })
        });
        if let Some(Err(error)) = result {
            let message = error.to_string();
            if self.last_lvds_fault.as_ref() != Some(&message) {
                self.last_lvds_fault = Some(message.clone());
                self.fault("LVDS / UDP", message);
            }
        }
        for delivery in deliveries {
            self.handle_scientific_frame(delivery);
        }
    }

    fn drain_lvds_all(&mut self) {
        self.drain_lvds_limit(usize::MAX);
    }

    fn drain_lvds_limit(&mut self, limit: usize) {
        let Some(worker) = &self.lvds else {
            return;
        };
        let events = collect_available(worker.events(), limit);
        for event in events {
            match event {
                ReaderEvent::Started { endpoint } => {
                    self.lvds_connected = true;
                    self.emit_critical(BackendEvent::LinkChanged {
                        link: LinkKind::Lvds,
                        connected: true,
                        detail: endpoint,
                    });
                }
                ReaderEvent::Frame(frame) => self.handle_scientific_frame(frame),
                ReaderEvent::Fault(_)
                | ReaderEvent::WriteFailed { .. }
                | ReaderEvent::BytesWritten { .. } => {}
                ReaderEvent::Stopped { reason } => {
                    self.lvds_connected = false;
                    self.emit_critical(BackendEvent::LinkChanged {
                        link: LinkKind::Lvds,
                        connected: false,
                        detail: format!("停止: {reason:?}"),
                    });
                }
            }
        }
    }

    fn handle_scientific_frame(&mut self, frame: ScientificFrame) {
        let display = if self.display_events.is_full() {
            self.lvds_display_dropped.fetch_add(1, Ordering::Relaxed);
            None
        } else {
            Some(BackendEvent::ScientificSamples {
                channel_1_start_seconds: frame.channel_1_base_seconds(),
                channel_2_start_seconds: frame.channel_2_base_seconds(),
                sample_interval_seconds: f64::from(SAMPLE_INTERVAL_MICROS) / 1_000_000.0,
                channel_1: frame
                    .channel_1
                    .iter()
                    .map(|&value| f64::from(value))
                    .collect(),
                channel_2: frame
                    .channel_2
                    .iter()
                    .map(|&value| f64::from(value))
                    .collect(),
            })
        };
        if let Some(display) = display {
            self.emit_display(LinkKind::Lvds, display);
        }
        if !self.acquisition_running {
            return;
        }
        let Some(writer) = &self.scientific_writer else {
            self.fault("科学数据存储", "采集中但存储 writer 不存在");
            self.acquisition_running = false;
            self.emit_critical(BackendEvent::AcquisitionChanged { running: false });
            return;
        };
        let submit_result = writer.submit_timeout(frame, STORAGE_SUBMIT_TIMEOUT);
        if let Err(error) = submit_result {
            let message = error.to_string();
            self.fault("科学数据存储", &message);
            self.stop_acquisition_incomplete(format!("科学数据未能写入: {message}"));
            return;
        }
        self.storage_status.enqueued = self.storage_status.enqueued.saturating_add(1);
    }

    fn poll_reader_statuses(&mut self) {
        let rs422_status = self.rs422.as_ref().map(ReaderWorker::status);
        if let Some(status) = rs422_status {
            if status.started && status.terminal.is_none() && !self.rs422_connected {
                self.rs422_connected = true;
                self.emit_critical(BackendEvent::LinkChanged {
                    link: LinkKind::Rs422,
                    connected: true,
                    detail: self.rs422_endpoint.clone(),
                });
            }
            if let Some(fault) = status.last_fault
                && self.last_rs422_fault.as_ref() != Some(&fault.message)
            {
                self.last_rs422_fault = Some(fault.message.clone());
                self.fault("RS422", fault.message);
            }
            if let Some(exit) = status.terminal
                && self.rs422_connected
            {
                self.rs422_connected = false;
                self.emit_critical(BackendEvent::LinkChanged {
                    link: LinkKind::Rs422,
                    connected: false,
                    detail: format!("停止: {:?}", exit.reason),
                });
            }
            if status.panicked && self.last_rs422_fault.as_deref() != Some("worker panic") {
                self.last_rs422_fault = Some("worker panic".to_owned());
                self.fault("RS422", "读取线程异常终止");
            }
        }

        let lvds_status = self.lvds.as_ref().map(ReaderWorker::status);
        if let Some(status) = lvds_status {
            if status.started && status.terminal.is_none() && !self.lvds_connected {
                self.lvds_connected = true;
                self.emit_critical(BackendEvent::LinkChanged {
                    link: LinkKind::Lvds,
                    connected: true,
                    detail: self.lvds_endpoint.clone(),
                });
            }
            if let Some(fault) = status.last_fault
                && self.last_lvds_fault.as_ref() != Some(&fault.message)
            {
                self.last_lvds_fault = Some(fault.message.clone());
                self.fault("LVDS", fault.message);
            }
            if let Some(exit) = status.terminal
                && self.lvds_connected
            {
                self.drain_lvds_all();
                self.lvds_connected = false;
                if self.acquisition_running {
                    self.stop_acquisition_incomplete(format!(
                        "LVDS reader stopped: {:?}",
                        exit.reason
                    ));
                }
                self.emit_critical(BackendEvent::LinkChanged {
                    link: LinkKind::Lvds,
                    connected: false,
                    detail: format!("停止: {:?}", exit.reason),
                });
            }
            if status.panicked && self.last_lvds_fault.as_deref() != Some("worker panic") {
                self.drain_lvds_all();
                if self.acquisition_running {
                    self.stop_acquisition_incomplete("LVDS reader panicked");
                }
                self.last_lvds_fault = Some("worker panic".to_owned());
                self.fault("LVDS", "读取线程异常终止");
            }
        }
    }

    fn drain_command_acks(&mut self) {
        let completed: Vec<_> = self
            .pending_commands
            .iter()
            .filter_map(|(&id, (ticket, frame))| {
                ticket.try_ack().map(|result| (id, *frame, result))
            })
            .collect();
        for (id, frame, result) in completed {
            self.pending_commands.remove(&id);
            match result {
                Ok(_) => {
                    self.emit_critical(BackendEvent::CommandSent(frame));
                }
                Err(error) => self.fault("RS422 写入", error.to_string()),
            }
        }
    }

    fn drain_storage(&mut self) {
        let Some(writer) = &self.scientific_writer else {
            return;
        };
        let events: Vec<_> = writer.events().try_iter().collect();
        let status = writer.status();
        let metrics = writer.metrics();
        self.storage_status.enqueued = metrics.submitted_items;
        self.storage_status.sink_written = metrics.written_items;
        let mut storage_failed = false;
        for event in events {
            match event {
                StorageEvent::Fault(error) => {
                    storage_failed = true;
                    let message = error.to_string();
                    if self.last_storage_fault.as_ref() != Some(&message) {
                        self.last_storage_fault = Some(message.clone());
                        self.fault("科学数据存储", message);
                    }
                }
                StorageEvent::Stopped { reason } => {
                    self.log(LogLevel::Info, format!("科学数据 writer 停止: {reason:?}"));
                }
                StorageEvent::BatchWritten { .. } | StorageEvent::Flushed => {}
            }
        }
        if let Some(error) = status.last_error {
            let message = error.to_string();
            if self.last_storage_fault.as_ref() != Some(&message) {
                self.last_storage_fault = Some(message.clone());
                self.fault("科学数据存储", message);
            }
            storage_failed = true;
        }
        if status.panicked && self.last_storage_fault.as_deref() != Some("worker panic") {
            self.last_storage_fault = Some("worker panic".to_owned());
            self.fault("科学数据存储", "存储线程异常终止");
            storage_failed = true;
        }
        if status.terminal.is_some() && self.acquisition_running {
            storage_failed = true;
        }
        if storage_failed && self.acquisition_running {
            self.stop_acquisition_incomplete("科学数据 writer 异常停止");
        }
    }

    fn sample_reader_metrics(&mut self) {
        if self.last_metrics_sample.elapsed() < METRICS_INTERVAL {
            return;
        }
        self.last_metrics_sample = Instant::now();

        if let Some(metrics) = self.rs422.as_ref().map(ReaderWorker::metrics) {
            let connected = self.rs422_connected;
            let display_dropped = self.rs422_display_dropped.load(Ordering::Relaxed);
            let previous = self
                .last_rs422_metrics
                .map_or_else(ReaderMetricsSnapshot::default, |(_, snapshot)| snapshot);
            self.report_metric_growth(LinkKind::Rs422, &previous, &metrics);
            self.last_rs422_metrics = Some((connected, metrics));
            if self.last_rs422_metrics_emitted != Some((connected, metrics, display_dropped))
                && self.emit_critical(BackendEvent::LinkMetrics {
                    link: LinkKind::Rs422,
                    connected,
                    metrics: Box::new(ui_metrics(&metrics, display_dropped)),
                })
            {
                self.last_rs422_metrics_emitted = Some((connected, metrics, display_dropped));
            }
        }

        let lvds_metrics = self
            .lvds
            .as_ref()
            .map(ReaderWorker::metrics)
            .or_else(|| self.lvds_udp.as_ref().map(UdpLvdsReceiver::metrics));
        if let Some(metrics) = lvds_metrics {
            let connected = self.lvds_connected;
            let display_dropped = self.lvds_display_dropped.load(Ordering::Relaxed);
            let previous = self
                .last_lvds_metrics
                .map_or_else(ReaderMetricsSnapshot::default, |(_, snapshot)| snapshot);
            self.report_metric_growth(LinkKind::Lvds, &previous, &metrics);
            self.last_lvds_metrics = Some((connected, metrics));
            if self.last_lvds_metrics_emitted != Some((connected, metrics, display_dropped))
                && self.emit_critical(BackendEvent::LinkMetrics {
                    link: LinkKind::Lvds,
                    connected,
                    metrics: Box::new(ui_metrics(&metrics, display_dropped)),
                })
            {
                self.last_lvds_metrics_emitted = Some((connected, metrics, display_dropped));
            }
        }
    }

    fn report_metric_growth(
        &self,
        link: LinkKind,
        previous: &ReaderMetricsSnapshot,
        current: &ReaderMetricsSnapshot,
    ) {
        let counters = [
            ("拒绝帧", previous.rejected_frames, current.rejected_frames),
            (
                "UDP分片错误",
                previous.udp_fragment_errors,
                current.udp_fragment_errors,
            ),
            (
                "重组超时",
                previous.reassembly_timeouts,
                current.reassembly_timeouts,
            ),
            (
                "重组容量拒绝",
                previous.reassembly_capacity_rejections,
                current.reassembly_capacity_rejections,
            ),
            (
                "重复帧抑制",
                previous.duplicate_frames_suppressed,
                current.duplicate_frames_suppressed,
            ),
            (
                "序号冲突",
                previous.sequence_conflicts,
                current.sequence_conflicts,
            ),
            (
                "校验失败",
                previous.checksum_failures,
                current.checksum_failures,
            ),
            (
                "长度失败",
                previous.length_failures,
                current.length_failures,
            ),
            (
                "帧头失败",
                previous.header_failures,
                current.header_failures,
            ),
            (
                "地址/类型失败",
                previous.field_failures,
                current.field_failures,
            ),
            (
                "时间失败",
                previous.timestamp_failures,
                current.timestamp_failures,
            ),
            ("ADC失败", previous.sample_failures, current.sample_failures),
            ("事件丢弃", previous.events_dropped, current.events_dropped),
            (
                "队列溢出",
                previous.queue_overflows,
                current.queue_overflows,
            ),
        ];
        let mut growth: Vec<_> = counters
            .into_iter()
            .filter(|(_, old, new)| new > old)
            .map(|(name, _, new)| format!("{name}={new}"))
            .collect();
        growth.extend(sequence_metric_growth(
            &previous.udp_sequence,
            &current.udp_sequence,
        ));
        if current.checksum_failures > previous.checksum_failures
            && let Some((expected, actual)) = current.last_checksum_mismatch
        {
            growth.push(format!(
                "最近校验计算值=0x{expected:04X}, 收到值=0x{actual:04X}"
            ));
        }
        if !growth.is_empty() {
            self.log(
                LogLevel::Warning,
                format!("{link} 数据质量计数增长: {}", growth.join(", ")),
            );
        }
        if current.io_errors > previous.io_errors {
            self.fault(
                link.to_string(),
                format!("I/O 错误累计={}", current.io_errors),
            );
        }
    }

    fn export_csv(&mut self, suggested_name: &str) {
        self.poll_csv_export();
        if self.csv_export.is_some() {
            self.log(LogLevel::Warning, "已有 CSV 导出任务正在运行");
            return;
        }
        let path = Path::new("data").join(sanitize_file_name(suggested_name));
        let rows = self.telemetry_rows.clone();
        match CsvExportTask::spawn(path, rows) {
            Ok(task) => {
                self.csv_export = Some(task);
                self.log(LogLevel::Info, "CSV 导出任务已提交");
            }
            Err(error) => self.fault("CSV 导出", error.to_string()),
        }
    }

    fn poll_csv_export(&mut self) {
        let Some(result) = self.csv_export.as_ref().and_then(CsvExportTask::try_result) else {
            return;
        };
        self.csv_export.take();
        match result {
            Ok(path) => {
                self.emit_critical(BackendEvent::CsvExported {
                    path: path.display().to_string(),
                });
            }
            Err(error) => self.fault("CSV 导出", error),
        }
    }

    fn fault(&self, source: impl Into<String>, message: impl Into<String>) {
        self.emit_critical(BackendEvent::Fault {
            source: source.into(),
            message: message.into(),
        });
    }

    fn log(&self, level: LogLevel, message: impl Into<String>) {
        self.emit_critical(BackendEvent::Log {
            level,
            message: message.into(),
        });
    }

    fn emit_display(&self, link: LinkKind, event: BackendEvent) {
        if self.display_events.try_send(event).is_err() {
            match link {
                LinkKind::Rs422 => &self.rs422_display_dropped,
                LinkKind::Lvds => &self.lvds_display_dropped,
            }
            .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn emit_critical(&self, event: BackendEvent) -> bool {
        match &event {
            BackendEvent::LinkChanged {
                link,
                connected,
                detail,
            } => {
                let mut state = lock_unpoisoned(&self.latest_state);
                match link {
                    LinkKind::Rs422 => {
                        state.rs422_connected = *connected;
                        state.rs422_detail.clone_from(detail);
                    }
                    LinkKind::Lvds => {
                        state.lvds_connected = *connected;
                        state.lvds_detail.clone_from(detail);
                    }
                }
                state.revision = state.revision.wrapping_add(1);
            }
            BackendEvent::AcquisitionChanged { running } => {
                let mut state = lock_unpoisoned(&self.latest_state);
                state.acquisition_running = *running;
                state.revision = state.revision.wrapping_add(1);
            }
            BackendEvent::TelemetryPollingChanged {
                active,
                interval_millis,
            } => {
                let mut state = lock_unpoisoned(&self.latest_state);
                state.telemetry_polling.active = *active;
                state.telemetry_polling.interval_millis = *interval_millis;
                state.revision = state.revision.wrapping_add(1);
            }
            _ => {}
        }
        match self.critical_events.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(event)) => {
                if self.critical_event_reclaim.try_recv().is_ok() {
                    self.dropped_critical.fetch_add(1, Ordering::Relaxed);
                    self.critical_events.try_send(event).is_ok()
                } else {
                    self.dropped_critical.fetch_add(1, Ordering::Relaxed);
                    false
                }
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    fn flush_critical_drop_summary(&self) {
        if self.critical_events.is_full() {
            return;
        }
        let dropped = self.dropped_critical.swap(0, Ordering::AcqRel);
        if dropped == 0 {
            return;
        }
        let summary = BackendEvent::Log {
            level: LogLevel::Warning,
            message: format!("UI 关键事件队列曾饱和，已淘汰 {dropped} 条旧事件"),
        };
        if self.critical_events.try_send(summary).is_err() {
            self.dropped_critical.fetch_add(dropped, Ordering::Relaxed);
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn telemetry_snapshot(telemetry: &Rs422Telemetry) -> TelemetrySnapshot {
    TelemetrySnapshot {
        absolute_time_seconds: telemetry.absolute_time_seconds,
        local_time_seconds: telemetry.local_time_seconds,
        cpu_temperature_celsius: telemetry.cpu_temperature,
        telemetry_request_count: telemetry.telemetry_request_count,
        accepted_commands: telemetry.accepted_command_count,
        rejected_commands: telemetry.rejected_command_count,
        work_mode: telemetry.work_mode,
        scan_frequency: telemetry.scan_frequency,
        scan_voltage: telemetry.scan_voltage,
        ad9833_status: telemetry.ad9833_status,
        dac8563_status: telemetry.dac8563_status,
        adc_1_status: telemetry.adc_1_status,
        adc_2_status: telemetry.adc_2_status,
        lvds_status: telemetry.lvds_status,
        last_injected_command: telemetry.last_injected_command,
    }
}

fn sequence_metric_growth(
    previous: &UdpLvdsSequenceMetrics,
    current: &UdpLvdsSequenceMetrics,
) -> Vec<String> {
    let counters = [
        ("向前跳号", previous.forward_jumps, current.forward_jumps),
        ("缺口等待到期", previous.expired_gaps, current.expired_gaps),
        ("迟到补齐", previous.late_fills, current.late_fills),
        (
            "到期后补齐",
            previous.late_fills_after_expiry,
            current.late_fills_after_expiry,
        ),
        (
            "乱序",
            previous.out_of_order_frames,
            current.out_of_order_frames,
        ),
        ("序号回绕", previous.wrap_events, current.wrap_events),
        (
            "序号异常",
            previous.sequence_anomalies,
            current.sequence_anomalies,
        ),
        (
            "缺口历史遗忘",
            previous.gap_records_discarded,
            current.gap_records_discarded,
        ),
    ];
    let mut growth: Vec<_> = counters
        .into_iter()
        .filter(|(_, old, new)| new > old)
        .map(|(name, _, new)| format!("{name}={new}"))
        .collect();
    if !growth.is_empty() {
        growth.push(format!(
            "待观察有效序号={}, 未补齐已到期={}",
            current.pending_gaps, current.unresolved_expired_gaps
        ));
    }
    growth
}

fn ui_metrics(
    metrics: &ReaderMetricsSnapshot,
    ui_display_events_dropped: u64,
) -> LinkMetricsSnapshot {
    LinkMetricsSnapshot {
        bytes_read: metrics.bytes_read,
        frames_decoded: metrics.frames_decoded,
        rejected_frames: metrics.rejected_frames,
        udp_fragment_errors: metrics.udp_fragment_errors,
        reassembly_timeouts: metrics.reassembly_timeouts,
        reassembly_capacity_rejections: metrics.reassembly_capacity_rejections,
        reassembled_frames: metrics.reassembled_frames,
        reassembly_pending_frames: metrics.reassembly_pending_frames,
        reassembly_buffered_bytes: metrics.reassembly_buffered_bytes,
        duplicate_frames_suppressed: metrics.duplicate_frames_suppressed,
        sequence_conflicts: metrics.sequence_conflicts,
        scientific_frames_delivered: metrics.scientific_frames_delivered,
        dedup_cached_frames: metrics.dedup_cached_frames,
        dedup_cached_bytes: metrics.dedup_cached_bytes,
        udp_sequence: metrics.udp_sequence,
        checksum_failures: metrics.checksum_failures,
        length_failures: metrics.length_failures,
        header_failures: metrics.header_failures,
        field_failures: metrics.field_failures,
        timestamp_failures: metrics.timestamp_failures,
        sample_failures: metrics.sample_failures,
        last_checksum_mismatch: metrics.last_checksum_mismatch,
        events_dropped: metrics.events_dropped,
        queue_overflows: metrics.queue_overflows,
        io_errors: metrics.io_errors,
        max_event_queue_depth: metrics.max_event_queue_depth,
        ui_display_events_dropped,
    }
}

fn collect_available<T>(receiver: &Receiver<T>, limit: usize) -> Vec<T> {
    receiver.try_iter().take(limit).collect()
}

fn data_path(stem: &str, extension: &str) -> PathBuf {
    Path::new("data").join(format!(
        "{stem}-{}.{}",
        Local::now().format("%Y%m%d-%H%M%S%.3f"),
        extension
    ))
}

fn sanitize_file_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.to_ascii_lowercase().ends_with(".csv") {
        sanitized
    } else {
        format!("{sanitized}.csv")
    }
}

fn write_telemetry_csv(path: &Path, rows: &VecDeque<TelemetrySnapshot>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut writer = BufWriter::new(file);
    writeln!(
        writer,
        "absolute_time_seconds,local_time_seconds,cpu_temperature_celsius,telemetry_request_count,accepted_command_count,rejected_command_count,work_mode,scan_frequency,scan_voltage,ad9833_status,dac8563_status,adc_1_status,adc_2_status,lvds_status,last_injected_command_hex"
    )?;
    for row in rows {
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            row.absolute_time_seconds,
            row.local_time_seconds,
            row.cpu_temperature_celsius,
            row.telemetry_request_count,
            row.accepted_commands,
            row.rejected_commands,
            row.work_mode,
            row.scan_frequency,
            row.scan_voltage,
            row.ad9833_status,
            row.dac8563_status,
            row.adc_1_status,
            row.adc_2_status,
            row.lvds_status,
            encode_hex_bytes(&row.last_injected_command),
        )?;
    }
    writer.flush()
}

fn encode_hex_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02X}").expect("writing into String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        net::SocketAddr,
        sync::{Arc, Mutex, atomic::Ordering},
        time::{Duration, Instant},
    };

    use crossbeam_channel::bounded;
    use langmuir_core::{DEFAULT_UDP_LVDS_REASSEMBLY_TTL, ScientificFrame};
    use langmuir_storage::{BatchSink, BatchWriter, StorageServiceConfig};
    use langmuir_transport::ReaderMetricsSnapshot;
    use tempfile::tempdir;

    use crate::bridge::{
        BackendEvent, BackendStateSnapshot, LinkKind, LogLevel, TelemetrySnapshot,
    };

    use super::{
        Controller, CsvExportTask, DEPLOYED_LVDS_RAW_FRAME_LEN, TelemetryPolling, UdpLvdsIngress,
        collect_available, sanitize_file_name,
    };

    fn udp_lvds_datagram(frame_sequence: u32, frame_length: usize, payload: &[u8]) -> Vec<u8> {
        let mut datagram = Vec::with_capacity(24 + payload.len());
        datagram.extend_from_slice(&[0x55, 0xAA, 0x01, 0xA5]);
        datagram.extend_from_slice(&frame_sequence.to_be_bytes());
        datagram.extend_from_slice(
            &u32::try_from(frame_length)
                .expect("test frame length fits in u32")
                .to_be_bytes(),
        );
        datagram.extend_from_slice(&[0, 0, 0, 1]);
        datagram.extend_from_slice(&0_u32.to_be_bytes());
        datagram.extend_from_slice(
            &u16::try_from(payload.len())
                .expect("test payload length fits in u16")
                .to_be_bytes(),
        );
        datagram.extend_from_slice(&0_u16.to_be_bytes());
        datagram.extend_from_slice(payload);
        datagram
    }

    fn deployed_lvds_payload() -> Vec<u8> {
        let mut payload = vec![0_u8; DEPLOYED_LVDS_RAW_FRAME_LEN];
        payload[..7].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA]);
        payload[7..11].copy_from_slice(&1_694_498_816_u32.to_be_bytes());
        payload[11..15].copy_from_slice(&999_800_u32.to_be_bytes());
        payload[15..17].copy_from_slice(&0x1234_u16.to_be_bytes());
        payload[17..19].copy_from_slice(&0xABCD_u16.to_be_bytes());
        for offset in (19..267).step_by(2) {
            payload[offset] = 0x01;
            payload[offset + 1] = 0xFF;
        }
        // Independently evaluated with the manufacturer's C reference over
        // bytes 7..267, not generated by the production Rust CRC function.
        payload[267..269].copy_from_slice(&0x44C8_u16.to_be_bytes());
        payload
    }

    fn next_deployed_lvds_payload() -> Vec<u8> {
        let mut payload = deployed_lvds_payload();
        payload[7..11].copy_from_slice(&1_694_498_817_u32.to_be_bytes());
        payload[11..15].copy_from_slice(&49_800_u32.to_be_bytes());
        // Independent manufacturer-C result, 50ms after the first golden frame.
        payload[267..269].copy_from_slice(&0x5EF9_u16.to_be_bytes());
        payload
    }

    fn udp_lvds_fragments(frame_sequence: u32, payload: &[u8]) -> [Vec<u8>; 2] {
        let mut prefix = udp_lvds_datagram(frame_sequence, payload.len(), &payload[..140]);
        prefix[14..16].copy_from_slice(&2_u16.to_be_bytes());
        let mut suffix = udp_lvds_datagram(frame_sequence, payload.len(), &payload[140..]);
        suffix[12..14].copy_from_slice(&1_u16.to_be_bytes());
        suffix[14..16].copy_from_slice(&2_u16.to_be_bytes());
        suffix[16..20].copy_from_slice(&140_u32.to_be_bytes());
        [prefix, suffix]
    }

    #[test]
    fn udp_lvds_ingress_accepts_only_complete_deployed_frames() {
        let source: SocketAddr = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let valid_payload = deployed_lvds_payload();
        let mut decoded = Vec::new();
        ingress.push(
            source,
            &udp_lvds_datagram(7, DEPLOYED_LVDS_RAW_FRAME_LEN, &valid_payload),
            |frame| decoded.push(frame),
        );
        ingress.push(source, &udp_lvds_datagram(8, 268, &[0xA5; 268]), |frame| {
            decoded.push(frame);
        });
        ingress.push(
            "192.0.2.2:1534".parse().unwrap(),
            &udp_lvds_datagram(9, DEPLOYED_LVDS_RAW_FRAME_LEN, &valid_payload),
            |frame| decoded.push(frame),
        );
        assert_eq!(ingress.metrics.frames_decoded, 1);
        assert_eq!(ingress.metrics.rejected_frames, 1);
        assert_eq!(ingress.metrics.length_failures, 0);
        assert_eq!(ingress.metrics.udp_fragment_errors, 1);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].sequence, 7);
        assert_eq!(decoded[0].channel_1[0], 0x1234);
        assert_eq!(
            ingress.metrics.bytes_read,
            u64::try_from(24 + valid_payload.len() + 24 + 268).unwrap()
        );
    }

    struct RecordingScienceSink(Arc<Mutex<Vec<ScientificFrame>>>);

    impl BatchSink<ScientificFrame> for RecordingScienceSink {
        type Error = std::io::Error;

        fn write_batch(&mut self, frames: &[ScientificFrame]) -> Result<(), Self::Error> {
            self.0.lock().unwrap().extend_from_slice(frames);
            Ok(())
        }

        fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn occupy_reassembly_capacity(
        ingress: &mut UdpLvdsIngress,
        source: SocketAddr,
        payload: &[u8],
        now: Instant,
    ) {
        for sequence in 100..164 {
            let mut fragment = udp_lvds_datagram(sequence, payload.len(), &payload[..140]);
            fragment[14..16].copy_from_slice(&2_u16.to_be_bytes());
            ingress.push_at(source, &fragment, now, |_| {
                panic!("an incomplete frame must not reach the controller");
            });
        }
        ingress.push_at(
            source,
            &udp_lvds_datagram(200, payload.len(), payload),
            now,
            |_| panic!("full capacity must reject a new sequence before TTL"),
        );
        assert_eq!(ingress.metrics.reassembly_pending_frames, 64);
        assert_eq!(ingress.metrics.reassembly_buffered_bytes, 64 * 269);
        assert_eq!(ingress.metrics.reassembly_capacity_rejections, 1);
        assert_eq!(ingress.metrics.reassembly_timeouts, 0);
        assert_eq!(ingress.metrics.udp_fragment_errors, 0);
    }

    #[test]
    fn protocol_mock_recovers_full_capacity_then_delivers_only_valid_frames() {
        let source: SocketAddr = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let (critical_tx, critical_rx) = bounded(8);
        let (display_tx, display_rx) = bounded(8);
        let mut controller = Controller::new(
            critical_tx,
            critical_rx.clone(),
            display_tx,
            Arc::default(),
            Arc::new(Mutex::new(BackendStateSnapshot::default())),
        );
        let stored = Arc::new(Mutex::new(Vec::new()));
        controller.scientific_writer = Some(
            BatchWriter::start(
                RecordingScienceSink(Arc::clone(&stored)),
                StorageServiceConfig::default(),
            )
            .unwrap(),
        );
        controller.acquisition_running = true;

        let first = deployed_lvds_payload();
        let second = next_deployed_lvds_payload();
        let mut corrupt = first.clone();
        corrupt[0] ^= 1;
        let now = Instant::now();
        occupy_reassembly_capacity(&mut ingress, source, &first, now);
        assert!(stored.lock().unwrap().is_empty());
        assert!(display_rx.is_empty());
        let recovered_at = now + DEFAULT_UDP_LVDS_REASSEMBLY_TTL;

        // Exercise the actual reassembler with two out-of-order fragments.
        let [prefix, suffix] = udp_lvds_fragments(7, &first);
        let second_datagram = udp_lvds_datagram(8, second.len(), &second);
        let mut normal_path_calls = 0;
        for datagram in [
            suffix.clone(),
            prefix.clone(),
            suffix,
            prefix,
            second_datagram.clone(),
            second_datagram,
            udp_lvds_datagram(9, corrupt.len(), &corrupt),
        ] {
            // The same production ingress automatically frees all 64 stale
            // slots before accepting this new traffic, with no reconnect.
            ingress.push_at(source, &datagram, recovered_at, |frame| {
                normal_path_calls += 1;
                controller.handle_scientific_frame(frame);
            });
        }

        assert_eq!(normal_path_calls, 2);
        assert_eq!(ingress.metrics.frames_decoded, 4);
        assert_eq!(ingress.metrics.duplicate_frames_suppressed, 2);
        assert_eq!(ingress.metrics.sequence_conflicts, 0);
        assert_eq!(ingress.metrics.scientific_frames_delivered, 2);
        assert_eq!(ingress.metrics.udp_sequence.observed_frames, 2);
        assert_eq!(ingress.metrics.udp_sequence.contiguous_frames, 1);
        assert_eq!(ingress.metrics.udp_sequence.gap_candidates, 0);
        assert_eq!(ingress.metrics.rejected_frames, 2);
        assert_eq!(ingress.metrics.reassembly_timeouts, 64);
        assert_eq!(ingress.metrics.reassembly_capacity_rejections, 1);
        assert_eq!(ingress.metrics.reassembled_frames, 5);
        assert_eq!(ingress.metrics.reassembly_pending_frames, 0);
        assert_eq!(ingress.metrics.reassembly_buffered_bytes, 0);
        assert_eq!(ingress.metrics.udp_fragment_errors, 0);
        assert_eq!(ingress.metrics.checksum_failures, 0);
        assert_eq!(ingress.metrics.header_failures, 1);
        assert_eq!(ingress.metrics.last_checksum_mismatch, None);
        controller
            .scientific_writer
            .take()
            .unwrap()
            .shutdown()
            .unwrap();
        assert!(critical_rx.is_empty());

        let persisted = stored.lock().unwrap();
        assert_golden_science_outputs(&persisted, display_rx.try_iter().collect());
    }

    fn assert_golden_science_outputs(persisted: &[ScientificFrame], events: Vec<BackendEvent>) {
        assert_eq!(persisted.len(), 2);
        assert_eq!((persisted[0].sequence, persisted[1].sequence), (7, 8));
        assert_eq!(persisted[0].timestamp.microseconds, 999_800);
        assert_eq!(persisted[1].timestamp.microseconds, 49_800);
        assert_eq!(
            persisted[1].timestamp.utc_seconds - persisted[0].timestamp.utc_seconds,
            1
        );
        for frame in persisted {
            for index in 0..125 {
                assert_eq!(
                    frame.channel_1[index],
                    0x1234 + u32::try_from(index).unwrap()
                );
                assert_eq!(
                    frame.channel_2[index],
                    0xABCD - u32::try_from(index).unwrap()
                );
            }
        }
        let mut start_times = Vec::new();
        for event in events {
            let BackendEvent::ScientificSamples {
                channel_1_start_seconds,
                channel_2_start_seconds,
                sample_interval_seconds,
                channel_1,
                channel_2,
            } = event
            else {
                panic!("unexpected display event");
            };
            assert_eq!(channel_1_start_seconds, channel_2_start_seconds);
            assert_eq!(sample_interval_seconds, 0.0004);
            assert_eq!((channel_1.len(), channel_2.len()), (125, 125));
            assert_eq!((channel_1[0], channel_1[124]), (4660.0, 4784.0));
            assert_eq!((channel_2[0], channel_2[124]), (43981.0, 43857.0));
            start_times.push(channel_1_start_seconds);
        }
        assert_eq!(start_times.len(), 2);
        assert!((start_times[1] - start_times[0] - 0.05).abs() < 0.000_001);
    }

    #[test]
    fn udp_science_dedup_distinguishes_invalid_duplicate_conflict_and_full_sequence() {
        let source: SocketAddr = "192.0.2.1:1534".parse().unwrap();
        let other_source: SocketAddr = "192.0.2.2:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let first = deployed_lvds_payload();
        let different = next_deployed_lvds_payload();
        let mut corrupt = first.clone();
        corrupt[0] ^= 1;
        let now = Instant::now();
        let mut delivered = Vec::new();
        ingress.push_at(
            other_source,
            &udp_lvds_datagram(8, first.len(), &first),
            now,
            |_| panic!("a different source must not affect the session"),
        );
        for (sequence, payload) in [
            (7, &corrupt),
            (7, &first),
            (7, &first),
            (7, &different),
            (65_543, &first), // Same low 16 bits as 7, different UDP sequence.
            (8, &first),
        ] {
            ingress.push_at(
                source,
                &udp_lvds_datagram(sequence, payload.len(), payload),
                now,
                |frame| delivered.push(frame),
            );
        }
        assert_eq!(delivered.len(), 3);
        assert_eq!(delivered[0].sequence, 7);
        assert_eq!(delivered[1].sequence, 7); // Storage model remains u16.
        assert_eq!(delivered[2].sequence, 8);
        assert_eq!(ingress.metrics.reassembled_frames, 6);
        assert_eq!(ingress.metrics.frames_decoded, 5);
        assert_eq!(ingress.metrics.scientific_frames_delivered, 3);
        assert_eq!(ingress.metrics.duplicate_frames_suppressed, 1);
        assert_eq!(ingress.metrics.sequence_conflicts, 1);
        assert_eq!(ingress.metrics.checksum_failures, 0);
        assert_eq!(ingress.metrics.header_failures, 1);
        assert_eq!(ingress.metrics.rejected_frames, 2);
        assert_eq!(ingress.metrics.dedup_cached_frames, 3);
        assert_eq!(ingress.metrics.dedup_cached_bytes, 3 * 269);
        assert_eq!(ingress.metrics.udp_sequence.observed_frames, 3);
        assert_eq!(ingress.metrics.udp_sequence.sequence_anomalies, 2);
        assert_eq!(ingress.metrics.udp_sequence.gap_candidates, 0);
    }

    #[test]
    fn udp_science_dedup_reconnect_wrap_and_expiry_do_not_permanently_block_frames() {
        let source: SocketAddr = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let first = deployed_lvds_payload();
        let different = next_deployed_lvds_payload();
        let now = Instant::now();
        let mut deliveries = 0;
        for (sequence, payload) in [
            (u32::MAX, &first),
            (0, &first),
            (u32::MAX, &first),
            (0, &different),
        ] {
            ingress.push_at(
                source,
                &udp_lvds_datagram(sequence, payload.len(), payload),
                now,
                |_| deliveries += 1,
            );
        }
        assert_eq!(deliveries, 2);
        assert_eq!(ingress.metrics.udp_sequence.observed_frames, 2);
        assert_eq!(ingress.metrics.udp_sequence.contiguous_frames, 1);
        assert_eq!(ingress.metrics.udp_sequence.wrap_events, 1);
        assert_eq!(ingress.metrics.duplicate_frames_suppressed, 1);
        assert_eq!(ingress.metrics.sequence_conflicts, 1);
        // Rebinding constructs this same fresh ingress and its empty session.
        let mut reconnected = UdpLvdsIngress::new(source);
        let reset_frame = udp_lvds_datagram(0, different.len(), &different);
        reconnected.push_at(source, &reset_frame, now, |_| deliveries += 1);
        assert_eq!(deliveries, 3);
        assert_eq!(reconnected.metrics.scientific_frames_delivered, 1);
        assert_eq!(reconnected.metrics.udp_sequence.observed_frames, 1);
        assert_eq!(reconnected.metrics.udp_sequence.wrap_events, 0);
        assert_eq!(reconnected.metrics.udp_sequence.sequence_anomalies, 0);
        // A reset without reconnect is bounded by the recent-frame window.
        let expired_at = now + Duration::from_secs(5);
        assert_eq!(ingress.expire_at(expired_at), 0);
        assert_eq!(ingress.metrics.dedup_cached_frames, 0);
        assert_eq!(ingress.metrics.reassembly_timeouts, 0);
        ingress.push_at(source, &reset_frame, expired_at, |_| deliveries += 1);
        assert_eq!(deliveries, 4);
        assert_eq!(ingress.metrics.scientific_frames_delivered, 3);
        assert_eq!(ingress.metrics.checksum_failures, 0);
        assert_eq!(ingress.metrics.udp_sequence.observed_frames, 3);
        assert_eq!(ingress.metrics.udp_sequence.sequence_anomalies, 1);
    }

    #[test]
    fn udp_science_dedup_twenty_hertz_frames_remain_lossless_and_bounded() {
        let source: SocketAddr = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let first = deployed_lvds_payload();
        let now = Instant::now();
        let mut delivered = 0;
        for sequence in 0..400 {
            let arrival = now + Duration::from_millis(u64::from(sequence) * 50);
            ingress.push_at(
                source,
                &udp_lvds_datagram(sequence, first.len(), &first),
                arrival,
                |_| delivered += 1,
            );
            assert!(ingress.metrics.dedup_cached_frames <= 256);
            assert!(ingress.metrics.dedup_cached_bytes <= 256 * 269);
            assert_eq!(ingress.metrics.reassembly_pending_frames, 0);
        }
        assert_eq!(delivered, 400);
        assert_eq!(ingress.metrics.frames_decoded, 400);
        assert_eq!(ingress.metrics.scientific_frames_delivered, 400);
        assert_eq!(ingress.metrics.duplicate_frames_suppressed, 0);
        assert_eq!(ingress.metrics.sequence_conflicts, 0);
        assert_eq!(ingress.metrics.rejected_frames, 0);
        assert_eq!(ingress.metrics.reassembly_timeouts, 0);
        assert_eq!(ingress.metrics.reassembly_capacity_rejections, 0);
        assert_eq!(
            ingress.metrics.udp_sequence,
            langmuir_core::UdpLvdsSequenceMetrics {
                observed_frames: 400,
                contiguous_frames: 399,
                ..langmuir_core::UdpLvdsSequenceMetrics::default()
            }
        );
    }

    #[test]
    fn udp_sequence_observes_only_accepted_science_and_retains_late_expired_fills() {
        let source: SocketAddr = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let valid = deployed_lvds_payload();
        let different = next_deployed_lvds_payload();
        let mut corrupt = valid.clone();
        corrupt[0] ^= 1;
        let now = Instant::now();
        let mut delivered = Vec::new();
        for (sequence, payload) in [
            (100, &valid),
            (101, &corrupt),
            (102, &valid),
            (102, &valid),
            (102, &different),
        ] {
            ingress.push_at(
                source,
                &udp_lvds_datagram(sequence, payload.len(), payload),
                now,
                |frame| delivered.push(frame.sequence),
            );
        }
        ingress.push_at(
            "192.0.2.2:1534".parse().unwrap(),
            &udp_lvds_datagram(103, valid.len(), &valid),
            now,
            |_| panic!("wrong source must not reach sequence observation"),
        );
        assert_eq!(delivered, [100, 102]);
        assert_eq!(ingress.metrics.udp_sequence.observed_frames, 2);
        assert_eq!(ingress.metrics.udp_sequence.forward_jumps, 1);
        assert_eq!(ingress.metrics.udp_sequence.pending_gaps, 1);
        assert_eq!(ingress.metrics.udp_sequence.expired_gaps, 0);
        assert_eq!(ingress.metrics.checksum_failures, 0);
        assert_eq!(ingress.metrics.header_failures, 1);
        assert_eq!(ingress.metrics.duplicate_frames_suppressed, 1);
        assert_eq!(ingress.metrics.sequence_conflicts, 1);
        let deadline = now + Duration::from_secs(2);
        // Idle maintenance preserves expire_at's residual-frame return value.
        assert_eq!(ingress.expire_at(deadline), 0);
        assert_eq!(ingress.metrics.udp_sequence.pending_gaps, 0);
        assert_eq!(ingress.metrics.udp_sequence.expired_gaps, 1);
        assert_eq!(ingress.metrics.udp_sequence.unresolved_expired_gaps, 1);
        ingress.push_at(
            source,
            &udp_lvds_datagram(101, valid.len(), &valid),
            deadline,
            |frame| delivered.push(frame.sequence),
        );
        assert_eq!(delivered, [100, 102, 101]);
        assert_eq!(ingress.metrics.udp_sequence.observed_frames, 3);
        assert_eq!(ingress.metrics.udp_sequence.late_fills, 1);
        assert_eq!(ingress.metrics.udp_sequence.late_fills_after_expiry, 1);
        assert_eq!(ingress.metrics.udp_sequence.out_of_order_frames, 1);
        assert_eq!(ingress.metrics.udp_sequence.unresolved_expired_gaps, 0);
        assert_eq!(ingress.metrics.udp_sequence.expired_gaps, 1);
        assert_eq!(ingress.metrics.reassembly_timeouts, 0);
        assert_eq!(ingress.metrics.rejected_frames, 2);
    }

    #[test]
    fn udp_sequence_monitor_preserves_controller_storage_and_display_arrival_order() {
        let source: SocketAddr = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let (critical_tx, critical_rx) = bounded(8);
        let (display_tx, display_rx) = bounded(8);
        let mut controller = Controller::new(
            critical_tx,
            critical_rx.clone(),
            display_tx,
            Arc::default(),
            Arc::new(Mutex::new(BackendStateSnapshot::default())),
        );
        let stored = Arc::new(Mutex::new(Vec::new()));
        controller.scientific_writer = Some(
            BatchWriter::start(
                RecordingScienceSink(Arc::clone(&stored)),
                StorageServiceConfig::default(),
            )
            .unwrap(),
        );
        controller.acquisition_running = true;
        let now = Instant::now();
        let mut callbacks = Vec::new();
        for (index, sequence) in [100_u16, 100, 101, 104, 103, 102].into_iter().enumerate() {
            let mut payload = deployed_lvds_payload();
            payload[15..17].copy_from_slice(&sequence.to_be_bytes());
            refresh_reference_crc(&mut payload);
            ingress.push_at(
                source,
                &udp_lvds_datagram(u32::from(sequence), payload.len(), &payload),
                now + Duration::from_millis(u64::try_from(index).unwrap() * 50),
                |frame| {
                    callbacks.push(frame.sequence);
                    controller.handle_scientific_frame(frame);
                },
            );
        }
        controller
            .scientific_writer
            .take()
            .unwrap()
            .shutdown()
            .unwrap();
        assert_eq!(callbacks, [100, 101, 104, 103, 102]);
        let persisted = stored.lock().unwrap();
        let stored_order: Vec<_> = persisted.iter().map(|frame| frame.sequence).collect();
        assert_eq!(stored_order, callbacks);
        let displayed: Vec<_> = display_rx
            .try_iter()
            .map(|event| {
                let BackendEvent::ScientificSamples { channel_1, .. } = event else {
                    panic!("expected scientific display event");
                };
                channel_1[0]
            })
            .collect();
        assert_eq!(displayed, [100.0, 101.0, 104.0, 103.0, 102.0]);
        assert!(critical_rx.is_empty());
        assert_eq!(ingress.metrics.scientific_frames_delivered, 5);
        assert_eq!(ingress.metrics.duplicate_frames_suppressed, 1);
        let quality = ingress.metrics.udp_sequence;
        assert_eq!(quality.observed_frames, 5);
        assert_eq!(quality.contiguous_frames, 1);
        assert_eq!(quality.forward_jumps, 1);
        assert_eq!(quality.gap_candidates, 2);
        assert_eq!(quality.pending_gaps, 0);
        assert_eq!(quality.late_fills, 2);
        assert_eq!(quality.out_of_order_frames, 2);
        assert_eq!(quality.sequence_anomalies, 0);
        assert_eq!(quality.expired_gaps, 0);
    }

    // Test-only transcription of the manufacturer reference, deliberately not
    // calling the production CRC helper when testing later validation stages.
    fn refresh_reference_crc(frame: &mut [u8]) {
        let mut crc = 0xFFFF_u16;
        for byte in &frame[7..267] {
            crc ^= u16::from(*byte) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 {
                    (crc << 1) ^ 0x1021
                } else {
                    crc << 1
                };
            }
        }
        frame[267..269].copy_from_slice(&crc.to_be_bytes());
    }

    #[test]
    fn udp_lvds_ingress_classifies_rejected_scientific_frames_without_double_counting() {
        let source: SocketAddr = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let valid = deployed_lvds_payload();
        let mut variants = Vec::new();
        let mut bad = valid.clone();
        bad[0] ^= 1;
        variants.push(bad);
        let mut bad = valid.clone();
        bad[2] = 0x04;
        variants.push(bad);
        let mut bad = valid.clone();
        bad[6] = 0xAB;
        variants.push(bad);
        let mut bad = valid.clone();
        bad[5] = 0x06;
        variants.push(bad);
        let mut bad = valid.clone();
        bad[11..15].copy_from_slice(&1_000_000_u32.to_be_bytes());
        refresh_reference_crc(&mut bad);
        variants.push(bad);
        let mut bad = valid.clone();
        bad[15..17].copy_from_slice(&0_u16.to_be_bytes());
        bad[19] = 0xFF;
        refresh_reference_crc(&mut bad);
        variants.push(bad);
        for (sequence, bad) in variants.iter().enumerate() {
            ingress.push(
                source,
                &udp_lvds_datagram(u32::try_from(sequence).unwrap(), bad.len(), bad),
                |_| panic!("invalid frame crossed the normal data gate"),
            );
        }
        ingress.push(source, &udp_lvds_datagram(8, 268, &valid[..268]), |_| {
            panic!("truncated frame crossed the normal data gate");
        });
        assert_eq!(ingress.metrics.frames_decoded, 0);
        assert_eq!(ingress.metrics.rejected_frames, 7);
        assert_eq!(ingress.metrics.length_failures, 1);
        assert_eq!(ingress.metrics.udp_fragment_errors, 1);
        assert_eq!(ingress.metrics.header_failures, 1);
        assert_eq!(ingress.metrics.field_failures, 2);
        assert_eq!(ingress.metrics.checksum_failures, 0);
        assert_eq!(ingress.metrics.timestamp_failures, 1);
        assert_eq!(ingress.metrics.sample_failures, 1);
        assert_eq!(ingress.metrics.last_checksum_mismatch, None);
        assert_eq!(ingress.receive_stats.complete_frames, 6);
        assert_eq!(ingress.receive_stats.parsed_frames, 0);
        assert_eq!(ingress.receive_stats.parse_failures, 6);
    }

    #[test]
    fn idle_expiry_releases_payload_capacity_without_rejecting_a_scientific_frame() {
        let source: SocketAddr = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let payload = deployed_lvds_payload();
        let mut fragment = udp_lvds_datagram(7, payload.len(), &payload[..140]);
        fragment[14..16].copy_from_slice(&2_u16.to_be_bytes());
        let now = Instant::now();
        ingress.push_at(source, &fragment, now, |_| {
            panic!("an incomplete frame must not reach the controller");
        });
        assert_eq!(ingress.metrics.reassembly_pending_frames, 1);
        assert_eq!(ingress.metrics.reassembly_buffered_bytes, 269);
        let deadline = now + DEFAULT_UDP_LVDS_REASSEMBLY_TTL;
        let before_deadline = deadline.checked_sub(Duration::from_nanos(1)).unwrap();
        assert_eq!(ingress.expire_at(before_deadline), 0);
        assert_eq!(ingress.expire_at(deadline), 1);
        assert_eq!(ingress.expire_at(deadline + Duration::from_secs(1)), 0);
        assert_eq!(ingress.metrics.reassembly_pending_frames, 0);
        assert_eq!(ingress.metrics.reassembly_buffered_bytes, 0);
        assert_eq!(ingress.metrics.reassembly_timeouts, 1);
        assert_eq!(ingress.metrics.rejected_frames, 0);
        assert_eq!(ingress.metrics.checksum_failures, 0);
        assert_eq!(ingress.metrics.udp_fragment_errors, 0);
        assert_eq!(ingress.metrics.reassembly_capacity_rejections, 0);
        assert_eq!(ingress.metrics.frames_decoded, 0);
    }

    #[test]
    fn udp_envelope_and_fragment_conflicts_have_their_own_error_category() {
        let source: SocketAddr = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let payload = deployed_lvds_payload();
        let mut fragment = udp_lvds_datagram(7, payload.len(), &payload[..140]);
        fragment[14..16].copy_from_slice(&2_u16.to_be_bytes());
        let now = Instant::now();
        ingress.push_at(source, &fragment, now, |_| panic!("incomplete frame"));
        fragment[24] ^= 1;
        ingress.push_at(source, &fragment, now, |_| panic!("conflicting fragment"));
        ingress.push_at(source, &[0; 24], now, |_| panic!("invalid envelope"));
        assert_eq!(ingress.metrics.rejected_frames, 2);
        assert_eq!(ingress.metrics.udp_fragment_errors, 2);
        assert_eq!(ingress.metrics.reassembly_capacity_rejections, 0);
        assert_eq!(ingress.metrics.reassembly_timeouts, 0);
        assert_eq!(ingress.metrics.reassembly_pending_frames, 0);
        assert_eq!(ingress.metrics.reassembly_buffered_bytes, 0);
        assert_eq!(ingress.metrics.header_failures, 0);
        assert_eq!(ingress.metrics.checksum_failures, 0);
    }

    #[test]
    fn udp_sequence_quality_logs_aggregate_changes_and_leave_normal_progress_silent() {
        let (critical_tx, critical_rx) = bounded(8);
        let (display_tx, _display_rx) = bounded(1);
        let controller = Controller::new(
            critical_tx,
            critical_rx.clone(),
            display_tx,
            Arc::default(),
            Arc::new(Mutex::new(BackendStateSnapshot::default())),
        );
        let current = ReaderMetricsSnapshot {
            udp_sequence: langmuir_core::UdpLvdsSequenceMetrics {
                observed_frames: 8,
                contiguous_frames: 1,
                forward_jumps: 2,
                gap_candidates: 4,
                pending_gaps: 1,
                expired_gaps: 2,
                unresolved_expired_gaps: 1,
                late_fills: 2,
                late_fills_after_expiry: 1,
                out_of_order_frames: 2,
                wrap_events: 1,
                sequence_anomalies: 1,
                gap_records_discarded: 0,
            },
            ..ReaderMetricsSnapshot::default()
        };
        controller.report_metric_growth(
            LinkKind::Lvds,
            &ReaderMetricsSnapshot::default(),
            &current,
        );
        controller.report_metric_growth(LinkKind::Lvds, &current, &current);
        let mut successful = current;
        successful.udp_sequence.observed_frames += 1;
        successful.udp_sequence.contiguous_frames += 1;
        successful.udp_sequence.pending_gaps = 0;
        controller.report_metric_growth(LinkKind::Lvds, &current, &successful);
        let messages: Vec<_> = critical_rx.try_iter().collect();
        assert_eq!(messages.len(), 1);
        let BackendEvent::Log { level, message } = &messages[0] else {
            panic!("expected one aggregated sequence-quality log");
        };
        assert_eq!(*level, LogLevel::Warning);
        for field in [
            "向前跳号=2",
            "缺口等待到期=2",
            "迟到补齐=2",
            "到期后补齐=1",
            "乱序=2",
            "序号回绕=1",
            "序号异常=1",
            "待观察有效序号=1",
            "未补齐已到期=1",
        ] {
            assert!(message.contains(field), "missing {field}: {message}");
        }
        assert!(!message.contains("丢包"));
        assert_eq!(
            super::ui_metrics(&current, 0).udp_sequence,
            current.udp_sequence
        );
    }

    #[test]
    fn reassembly_quality_logs_are_aggregated_and_success_gauges_stay_silent() {
        let (critical_tx, critical_rx) = bounded(8);
        let (display_tx, _display_rx) = bounded(1);
        let controller = Controller::new(
            critical_tx,
            critical_rx.clone(),
            display_tx,
            Arc::default(),
            Arc::new(Mutex::new(BackendStateSnapshot::default())),
        );
        let current = ReaderMetricsSnapshot {
            rejected_frames: 4,
            udp_fragment_errors: 3,
            reassembly_timeouts: 64,
            reassembly_capacity_rejections: 1,
            reassembled_frames: 2,
            reassembly_pending_frames: 1,
            reassembly_buffered_bytes: 269,
            duplicate_frames_suppressed: 512,
            sequence_conflicts: 2,
            scientific_frames_delivered: 2,
            dedup_cached_frames: 2,
            dedup_cached_bytes: 2 * 269,
            ..ReaderMetricsSnapshot::default()
        };
        controller.report_metric_growth(
            LinkKind::Lvds,
            &ReaderMetricsSnapshot::default(),
            &current,
        );
        controller.report_metric_growth(LinkKind::Lvds, &current, &current);
        let successful = ReaderMetricsSnapshot {
            reassembled_frames: 3,
            frames_decoded: 3,
            reassembly_pending_frames: 0,
            reassembly_buffered_bytes: 0,
            scientific_frames_delivered: 3,
            dedup_cached_frames: 3,
            dedup_cached_bytes: 3 * 269,
            ..current
        };
        controller.report_metric_growth(LinkKind::Lvds, &current, &successful);
        let messages: Vec<_> = critical_rx.try_iter().collect();
        assert_eq!(messages.len(), 1);
        assert!(matches!(
            &messages[0],
            BackendEvent::Log { level: LogLevel::Warning, message }
                if message.contains("UDP分片错误=3")
                    && message.contains("重组超时=64")
                    && message.contains("重组容量拒绝=1")
                    && message.contains("重复帧抑制=512")
                    && message.contains("序号冲突=2")
        ));
        let visible = super::ui_metrics(&current, 0);
        assert_eq!(visible.udp_fragment_errors, 3);
        assert_eq!(visible.reassembly_timeouts, 64);
        assert_eq!(visible.reassembly_capacity_rejections, 1);
        assert_eq!(visible.reassembled_frames, 2);
        assert_eq!(visible.reassembly_pending_frames, 1);
        assert_eq!(visible.reassembly_buffered_bytes, 269);
        assert_eq!(visible.duplicate_frames_suppressed, 512);
        assert_eq!(visible.sequence_conflicts, 2);
        assert_eq!(visible.scientific_frames_delivered, 2);
        assert_eq!(visible.dedup_cached_frames, 2);
        assert_eq!(visible.dedup_cached_bytes, 2 * 269);
    }

    #[test]
    fn repeated_time_failures_are_aggregated_into_one_quality_log_per_snapshot() {
        let source: SocketAddr = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let (critical_tx, critical_rx) = bounded(8);
        let (display_tx, _display_rx) = bounded(1);
        let controller = Controller::new(
            critical_tx,
            critical_rx.clone(),
            display_tx,
            Arc::default(),
            Arc::new(Mutex::new(BackendStateSnapshot::default())),
        );
        let mut corrupt = deployed_lvds_payload();
        corrupt[11..15].copy_from_slice(&1_000_000_u32.to_be_bytes());
        for sequence in 0..512 {
            ingress.push(
                source,
                &udp_lvds_datagram(sequence, corrupt.len(), &corrupt),
                |_| panic!("invalid device time must not invoke the data handler"),
            );
        }
        assert!(critical_rx.is_empty());
        assert_eq!(ingress.metrics.rejected_frames, 512);
        assert_eq!(ingress.metrics.checksum_failures, 0);
        assert_eq!(ingress.metrics.timestamp_failures, 512);
        assert_eq!(ingress.metrics.frames_decoded, 0);
        controller.report_metric_growth(
            LinkKind::Lvds,
            &ReaderMetricsSnapshot::default(),
            &ingress.metrics,
        );
        controller.report_metric_growth(LinkKind::Lvds, &ingress.metrics, &ingress.metrics);
        let messages: Vec<_> = critical_rx.try_iter().collect();
        assert_eq!(messages.len(), 1);
        assert!(matches!(
            &messages[0],
            BackendEvent::Log { level: LogLevel::Warning, message }
                if message.contains("时间失败=512") && !message.contains("校验失败")
        ));
        let visible = super::ui_metrics(&ingress.metrics, 0);
        assert_eq!(visible.checksum_failures, 0);
        assert_eq!(visible.timestamp_failures, 512);
        assert_eq!(visible.last_checksum_mismatch, None);
    }

    #[test]
    fn telemetry_polling_schedules_from_the_actual_send_without_a_burst() {
        let start = Instant::now();
        let interval = Duration::from_millis(200);
        let mut polling = TelemetryPolling::new(interval, start);
        assert!(polling.is_due(start));
        polling.record_send(start);
        assert!(!polling.is_due(start + Duration::from_millis(199)));
        assert!(polling.is_due(start + interval));

        let delayed_send = start + Duration::from_secs(3);
        polling.record_send(delayed_send);
        assert!(!polling.is_due(delayed_send));
        assert!(!polling.is_due(delayed_send + Duration::from_millis(199)));
        assert!(polling.is_due(delayed_send + interval));
    }

    #[test]
    fn export_name_is_confined_to_data_directory() {
        assert_eq!(sanitize_file_name("../../bad name"), ".._.._bad_name.csv");
        assert_eq!(sanitize_file_name("capture.CSV"), "capture.CSV");
    }

    #[test]
    fn drain_all_is_not_limited_to_the_per_tick_budget() {
        let (sender, receiver) = bounded(2_048);
        for value in 0..2_048 {
            sender.try_send(value).unwrap();
        }
        let drained = collect_available(&receiver, usize::MAX);
        assert_eq!(drained.len(), 2_048);
        assert!(receiver.is_empty());
    }

    #[test]
    fn csv_export_runs_in_a_bounded_background_task() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("telemetry.csv");
        let rows = VecDeque::from([TelemetrySnapshot {
            absolute_time_seconds: 1,
            local_time_seconds: 2,
            cpu_temperature_celsius: 3,
            telemetry_request_count: 7,
            accepted_commands: 8,
            rejected_commands: 9,
            work_mode: 0x55,
            scan_frequency: 100,
            scan_voltage: 5_000,
            ad9833_status: 1,
            dac8563_status: 1,
            adc_1_status: 1,
            adc_2_status: 1,
            lvds_status: 1,
            last_injected_command: [0x21, 0x55, 0x64, 0x9B, 0x9B],
        }]);
        let task = CsvExportTask::spawn(path.clone(), rows).unwrap();
        let exported = task.result.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(exported.unwrap(), path);
        assert!(path.exists());
        let csv = fs::read_to_string(path).unwrap();
        assert!(csv.starts_with(
            "absolute_time_seconds,local_time_seconds,cpu_temperature_celsius,telemetry_request_count"
        ));
        assert!(csv.contains("1,2,3,7,8,9,85,100,5000,1,1,1,1,1,2155649B9B"));
    }

    #[test]
    fn critical_queue_evicts_oldest_and_preserves_latest_event() {
        let (critical_tx, critical_rx) = bounded(2);
        let (display_tx, _display_rx) = bounded(1);
        let controller = Controller::new(
            critical_tx,
            critical_rx.clone(),
            display_tx,
            Arc::default(),
            Arc::new(Mutex::new(BackendStateSnapshot::default())),
        );
        controller.emit_critical(BackendEvent::Log {
            level: LogLevel::Info,
            message: "oldest".to_owned(),
        });
        controller.emit_critical(BackendEvent::Log {
            level: LogLevel::Info,
            message: "middle".to_owned(),
        });
        controller.emit_critical(BackendEvent::Log {
            level: LogLevel::Info,
            message: "latest".to_owned(),
        });

        let messages: Vec<_> = critical_rx
            .try_iter()
            .filter_map(|event| match event {
                BackendEvent::Log { message, .. } => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(messages, ["middle", "latest"]);
        assert_eq!(
            controller
                .dropped_critical
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        controller.flush_critical_drop_summary();
        assert!(matches!(
            critical_rx.try_recv(),
            Ok(BackendEvent::Log {
                level: LogLevel::Warning,
                message,
            }) if message.contains("1 条旧事件")
        ));
    }

    #[test]
    fn coalesced_state_survives_critical_event_eviction() {
        let (critical_tx, critical_rx) = bounded(1);
        let (display_tx, _display_rx) = bounded(1);
        let latest_state = Arc::new(Mutex::new(BackendStateSnapshot::default()));
        let controller = Controller::new(
            critical_tx,
            critical_rx.clone(),
            display_tx,
            Arc::default(),
            Arc::clone(&latest_state),
        );
        controller.emit_critical(BackendEvent::LinkChanged {
            link: LinkKind::Rs422,
            connected: true,
            detail: "rs422".to_owned(),
        });
        controller.emit_critical(BackendEvent::LinkChanged {
            link: LinkKind::Lvds,
            connected: true,
            detail: "lvds".to_owned(),
        });
        controller.emit_critical(BackendEvent::AcquisitionChanged { running: true });

        let state = super::lock_unpoisoned(&latest_state).clone();
        assert!(state.rs422_connected);
        assert!(state.lvds_connected);
        assert!(state.acquisition_running);
        assert_eq!(state.rs422_detail, "rs422");
        assert_eq!(state.lvds_detail, "lvds");
        assert!(state.revision >= 3);
    }

    #[test]
    fn full_display_queue_is_counted_separately_for_each_link() {
        let (critical_tx, critical_rx) = bounded(1);
        let (display_tx, display_rx) = bounded(1);
        let controller = Controller::new(
            critical_tx,
            critical_rx,
            display_tx,
            Arc::default(),
            Arc::new(Mutex::new(BackendStateSnapshot::default())),
        );
        controller.emit_display(
            LinkKind::Rs422,
            BackendEvent::Log {
                level: LogLevel::Info,
                message: "first".to_owned(),
            },
        );
        controller.emit_display(
            LinkKind::Lvds,
            BackendEvent::Log {
                level: LogLevel::Info,
                message: "dropped".to_owned(),
            },
        );
        assert_eq!(controller.rs422_display_dropped.load(Ordering::Relaxed), 0);
        assert_eq!(controller.lvds_display_dropped.load(Ordering::Relaxed), 1);
        assert_eq!(display_rx.len(), 1);
    }

    #[test]
    fn mixed_crc_trailers_preserve_science_order_without_coalescing() {
        let source = "192.0.2.1:1534".parse().unwrap();
        let mut ingress = UdpLvdsIngress::new(source);
        let valid = deployed_lvds_payload();
        let mut bad_crc = valid.clone();
        bad_crc[268] ^= 1;
        let mut deliveries = Vec::new();
        let now = Instant::now();
        for (sequence, bytes) in [(1, &bad_crc), (2, &valid), (3, &bad_crc)] {
            ingress.push_at(
                source,
                &udp_lvds_datagram(sequence, bytes.len(), bytes),
                now,
                |delivery| deliveries.push(delivery),
            );
        }
        assert_eq!(deliveries.len(), 3);
        assert_eq!(
            deliveries.iter().map(|f| f.sequence).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(deliveries[0].channel_1, deliveries[1].channel_1);
        assert_eq!(deliveries[1].channel_2, deliveries[2].channel_2);
        assert_eq!(ingress.receive_stats.structure_valid_frames, 3);
        assert_eq!(ingress.receive_stats.parsed_frames, 3);
        assert_eq!(ingress.receive_stats.accepted_frames, 3);
        assert_eq!(ingress.metrics.udp_sequence.observed_frames, 3);
        assert_eq!(ingress.metrics.checksum_failures, 0);
    }

    #[test]
    fn producer_boundary_clears_old_visuals_and_keeps_fresh_frames() {
        let (critical_tx, critical_rx) = bounded(8);
        let (display_tx, display_rx) = bounded(8);
        let mut controller = Controller::new(
            critical_tx,
            critical_rx.clone(),
            display_tx,
            Arc::default(),
            Arc::new(Mutex::new(BackendStateSnapshot::default())),
        );
        controller.display_event_reclaim = Some(display_rx.clone());
        controller.lvds_source = "old receiver".to_owned();
        controller.publish_lvds_status();
        controller.handle_scientific_frame(
            langmuir_core::decode_scientific_frame(&deployed_lvds_payload()).unwrap(),
        );
        assert_eq!(display_rx.len(), 1);
        controller.lvds_source = "new receiver".to_owned();
        controller.publish_lvds_status();
        assert!(display_rx.is_empty());
        controller.handle_scientific_frame(
            langmuir_core::decode_scientific_frame(&deployed_lvds_payload()).unwrap(),
        );
        // Reading the preceding status cannot later purge this fresh frame.
        assert_eq!(critical_rx.len(), 2);
        assert_eq!(display_rx.len(), 1);
        controller.publish_lvds_status();
        assert_eq!(display_rx.len(), 1);
        controller.lvds_source = "third receiver".to_owned();
        controller.publish_lvds_status();
        assert!(display_rx.is_empty());
    }

    #[test]
    fn interrupted_capture_marker_records_reason_next_to_data_file() {
        let directory = tempdir().unwrap();
        let capture_path = directory.path().join("capture.lpd");
        let (critical_tx, critical_rx) = bounded(4);
        let (display_tx, _display_rx) = bounded(1);
        let controller = Controller::new(
            critical_tx,
            critical_rx,
            display_tx,
            Arc::default(),
            Arc::new(Mutex::new(BackendStateSnapshot::default())),
        );
        controller.mark_capture_incomplete(&capture_path, "test interruption");

        let marker = directory.path().join("capture.lpd.incomplete");
        let contents = fs::read_to_string(marker).unwrap();
        assert!(contents.contains("capture.lpd"));
        assert!(contents.contains("test interruption"));
    }
}
