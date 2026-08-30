use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
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
    LvdsStreamDecoder, Rs422StreamDecoder, Rs422Telemetry, SAMPLE_INTERVAL_MICROS, ScientificFrame,
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

use crate::bridge::{
    BackendBridge, BackendEvent, BackendStateSnapshot, BridgeError, LinkKind, LinkMetricsSnapshot,
    LogLevel, TelemetrySnapshot, UiRequest,
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
        let dropped_critical = Arc::new(AtomicU64::new(0));
        let dropped_critical_for_controller = Arc::clone(&dropped_critical);
        let (completion_tx, completion) = bounded(1);
        let latest_state = Arc::new(Mutex::new(BackendStateSnapshot::default()));
        let latest_state_for_controller = Arc::clone(&latest_state);
        let join = thread::Builder::new()
            .name("langmuir-backend".to_owned())
            .spawn(move || {
                Controller::new(
                    critical_event_tx,
                    critical_event_reclaim,
                    display_event_tx,
                    dropped_critical_for_controller,
                    latest_state_for_controller,
                )
                .run(&request_rx);
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
    dropped_critical: Arc<AtomicU64>,
    latest_state: Arc<Mutex<BackendStateSnapshot>>,
    rs422_display_dropped: AtomicU64,
    lvds_display_dropped: AtomicU64,
    rs422: Option<ReaderWorker<Rs422Telemetry>>,
    lvds: Option<ReaderWorker<ScientificFrame>>,
    rs422_connected: bool,
    lvds_connected: bool,
    rs422_endpoint: String,
    lvds_endpoint: String,
    last_rs422_fault: Option<String>,
    last_lvds_fault: Option<String>,
    last_storage_fault: Option<String>,
    scientific_writer: Option<BatchWriter<ScientificFrame>>,
    acquisition_running: bool,
    telemetry_rows: VecDeque<TelemetrySnapshot>,
    pending_commands: BTreeMap<u64, (WriteTicket, [u8; 12])>,
    last_rs422_metrics: Option<(bool, ReaderMetricsSnapshot)>,
    last_lvds_metrics: Option<(bool, ReaderMetricsSnapshot)>,
    last_rs422_metrics_emitted: Option<(bool, ReaderMetricsSnapshot, u64)>,
    last_lvds_metrics_emitted: Option<(bool, ReaderMetricsSnapshot, u64)>,
    last_metrics_sample: Instant,
    csv_export: Option<CsvExportTask>,
    capture_path: Option<PathBuf>,
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
            dropped_critical,
            latest_state,
            rs422_display_dropped: AtomicU64::new(0),
            lvds_display_dropped: AtomicU64::new(0),
            rs422: None,
            lvds: None,
            rs422_connected: false,
            lvds_connected: false,
            rs422_endpoint: String::new(),
            lvds_endpoint: String::new(),
            last_rs422_fault: None,
            last_lvds_fault: None,
            last_storage_fault: None,
            scientific_writer: None,
            acquisition_running: false,
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
            self.drain_rs422();
            self.drain_lvds();
            self.poll_reader_statuses();
            self.sample_reader_metrics();
            self.drain_command_acks();
            self.drain_storage();
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
                    self.emit_critical(BackendEvent::PortsUpdated {
                        rs422: ports.clone(),
                        lvds: ports,
                    });
                }
                Err(error) => self.fault("串口枚举", error.to_string()),
            },
            UiRequest::Connect { link, port, baud } => self.connect(link, &port, baud),
            UiRequest::Disconnect { link } => {
                self.disconnect(link);
            }
            UiRequest::StartAcquisition => self.start_acquisition(),
            UiRequest::StopAcquisition => self.stop_acquisition_at_ui_boundary(),
            UiRequest::SendCommand(frame) => self.send_command(frame),
            UiRequest::ExportCsv { suggested_name } => self.export_csv(&suggested_name),
            #[cfg(feature = "mock")]
            UiRequest::InjectMockFault => {
                self.fault("配置", "生产后端拒绝模拟异常请求");
            }
        }
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
                    self.last_lvds_fault = None;
                }
            }
            self.log(LogLevel::Info, format!("正在连接 {link}: {port} @ {baud}"));
        }
    }

    fn disconnect(&mut self, link: LinkKind) -> bool {
        let result = match link {
            LinkKind::Rs422 => {
                self.rs422_connected = false;
                self.rs422_endpoint.clear();
                self.last_rs422_fault = None;
                self.rs422.take().map(ReaderWorker::shutdown)
            }
            LinkKind::Lvds => {
                let result = self.stop_lvds_reader_and_drain();
                if self.acquisition_running || self.scientific_writer.is_some() {
                    self.stop_acquisition();
                }
                self.lvds_connected = false;
                self.lvds_endpoint.clear();
                self.last_lvds_fault = None;
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
        if let Some(writer) = self.scientific_writer.take()
            && let Err(error) = writer.shutdown_timeout(STORAGE_SHUTDOWN_TIMEOUT)
        {
            let message = error.to_string();
            self.fault("科学数据存储", &message);
            if incomplete_reason.is_none() {
                incomplete_reason = Some(format!("存储关闭失败: {message}"));
            }
        }
        if let (Some(path), Some(reason)) = (capture_path.as_deref(), incomplete_reason.as_deref())
        {
            self.mark_capture_incomplete(path, reason);
        }
        self.capture_path = None;
        self.emit_critical(BackendEvent::AcquisitionChanged { running: false });
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

    fn send_command(&mut self, frame: [u8; 12]) {
        let Some(worker) = self.rs422.as_ref().filter(|_| self.rs422_connected) else {
            self.fault("RS422", "未连接，指令未发送");
            return;
        };
        match worker.try_send_bytes(frame.to_vec()) {
            Ok(ticket) => {
                self.pending_commands.insert(ticket.id(), (ticket, frame));
            }
            Err(error) => self.fault("RS422 指令队列", error.to_string()),
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
        if !self.acquisition_running {
            return;
        }
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
        if let Some(display) = display {
            self.emit_display(LinkKind::Lvds, display);
        }
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
            self.report_metric_growth(LinkKind::Rs422, previous, metrics);
            self.last_rs422_metrics = Some((connected, metrics));
            if self.last_rs422_metrics_emitted != Some((connected, metrics, display_dropped))
                && self.emit_critical(BackendEvent::LinkMetrics {
                    link: LinkKind::Rs422,
                    connected,
                    metrics: ui_metrics(metrics, display_dropped),
                })
            {
                self.last_rs422_metrics_emitted = Some((connected, metrics, display_dropped));
            }
        }

        if let Some(metrics) = self.lvds.as_ref().map(ReaderWorker::metrics) {
            let connected = self.lvds_connected;
            let display_dropped = self.lvds_display_dropped.load(Ordering::Relaxed);
            let previous = self
                .last_lvds_metrics
                .map_or_else(ReaderMetricsSnapshot::default, |(_, snapshot)| snapshot);
            self.report_metric_growth(LinkKind::Lvds, previous, metrics);
            self.last_lvds_metrics = Some((connected, metrics));
            if self.last_lvds_metrics_emitted != Some((connected, metrics, display_dropped))
                && self.emit_critical(BackendEvent::LinkMetrics {
                    link: LinkKind::Lvds,
                    connected,
                    metrics: ui_metrics(metrics, display_dropped),
                })
            {
                self.last_lvds_metrics_emitted = Some((connected, metrics, display_dropped));
            }
        }
    }

    fn report_metric_growth(
        &self,
        link: LinkKind,
        previous: ReaderMetricsSnapshot,
        current: ReaderMetricsSnapshot,
    ) {
        let counters = [
            ("拒绝帧", previous.rejected_frames, current.rejected_frames),
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
            ("事件丢弃", previous.events_dropped, current.events_dropped),
            (
                "队列溢出",
                previous.queue_overflows,
                current.queue_overflows,
            ),
        ];
        let growth: Vec<_> = counters
            .into_iter()
            .filter(|(_, old, new)| new > old)
            .map(|(name, _, new)| format!("{name}={new}"))
            .collect();
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
        pcb_temperature_celsius: telemetry.pcb_temperature,
        magnetic_x: telemetry.magnetic_field.x,
        magnetic_y: telemetry.magnetic_field.y,
        magnetic_z: telemetry.magnetic_field.z,
        accepted_commands: telemetry.accepted_command_count,
        rejected_commands: telemetry.rejected_command_count,
        dds1_status: telemetry.dds1_status,
        dds2_status: telemetry.dds2_status,
        dac_status: telemetry.dac_status,
        auxiliary_status: telemetry.auxiliary_status,
        adc1_status: telemetry.adc1_status,
        adc2_status: telemetry.adc2_status,
    }
}

fn ui_metrics(
    metrics: ReaderMetricsSnapshot,
    ui_display_events_dropped: u64,
) -> LinkMetricsSnapshot {
    LinkMetricsSnapshot {
        bytes_read: metrics.bytes_read,
        frames_decoded: metrics.frames_decoded,
        rejected_frames: metrics.rejected_frames,
        checksum_failures: metrics.checksum_failures,
        length_failures: metrics.length_failures,
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
        "absolute_time_seconds,local_time_seconds,cpu_temperature_celsius,pcb_temperature_celsius,magnetic_x,magnetic_y,magnetic_z,accepted_commands,rejected_commands,dds1_status,dds2_status,dac_status,auxiliary_status,adc1_status,adc2_status"
    )?;
    for row in rows {
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            row.absolute_time_seconds,
            row.local_time_seconds,
            row.cpu_temperature_celsius,
            row.pcb_temperature_celsius,
            row.magnetic_x,
            row.magnetic_y,
            row.magnetic_z,
            row.accepted_commands,
            row.rejected_commands,
            row.dds1_status,
            row.dds2_status,
            row.dac_status,
            row.auxiliary_status,
            row.adc1_status,
            row.adc2_status,
        )?;
    }
    writer.flush()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        sync::{Arc, Mutex, atomic::Ordering},
        time::Duration,
    };

    use crossbeam_channel::bounded;
    use tempfile::tempdir;

    use crate::bridge::{
        BackendEvent, BackendStateSnapshot, LinkKind, LogLevel, TelemetrySnapshot,
    };

    use super::{Controller, CsvExportTask, collect_available, sanitize_file_name};

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
            pcb_temperature_celsius: 4,
            magnetic_x: 5,
            magnetic_y: 6,
            magnetic_z: 7,
            accepted_commands: 8,
            rejected_commands: 9,
            dds1_status: 1,
            dds2_status: 1,
            dac_status: 1,
            auxiliary_status: 1,
            adc1_status: 1,
            adc2_status: 1,
        }]);
        let task = CsvExportTask::spawn(path.clone(), rows).unwrap();
        let exported = task.result.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(exported.unwrap(), path);
        assert!(path.exists());
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
