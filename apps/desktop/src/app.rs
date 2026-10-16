use std::{
    collections::{HashMap, VecDeque},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use chrono::{DateTime, Local, Utc};
use eframe::egui::{
    self, Align, Color32, ComboBox, Grid, Layout, PointerButton, RichText, ScrollArea, Stroke, Ui,
};
use egui_plot::{Legend, Line, Plot, PlotPoints, VLine};
use langmuir_core::{
    Command, CommandError, CommandFrame, ScanFrequency, command_checksum as core_command_checksum,
    validate_command_frame,
};

use crate::{
    bridge::{
        BackendBridge, BackendEvent, BackendStateSnapshot, LinkKind, LinkMetricsSnapshot, LogLevel,
        TELEMETRY_POLL_INTERVAL_DEFAULT_MILLIS, TELEMETRY_POLL_INTERVAL_MAX_MILLIS,
        TELEMETRY_POLL_INTERVAL_MIN_MILLIS, TelemetryPollingState, TelemetrySnapshot, UiRequest,
    },
    buffer::{SamplePoint, TimeSeriesBuffer},
    ui::{fonts, theme},
};

// 20 frames/s × 125 samples/frame × 10 s, plus modest scheduling headroom.
const PLOT_CAPACITY: usize = 1_100_000;
const PLOT_WINDOW_SECONDS: f64 = 10.0;
const MAX_DISPLAY_POINTS: usize = 2_000;
const MAX_LOGS: usize = 500;
const MAX_FAULTS: usize = 50;
const MAX_VISIBLE_FAULTS: usize = 3;
const EVENT_BUDGET_PER_FRAME: usize = 4_096;
const HEADER_HEIGHT: f32 = 60.0;
const HEADER_COMPACT_HEIGHT: f32 = 104.0;
const HEADER_COMPACT_BREAKPOINT: f32 = 1_280.0;
const HEADER_TITLE_WIDTH: f32 = 218.0;
const HEADER_TITLE_HEIGHT: f32 = 40.0;
const HEADER_DEVICE_GROUP_WIDTH: f32 = 450.0;
const HEADER_POLLING_WIDTH: f32 = 180.0;
#[cfg(feature = "mock")]
const HEADER_MOCK_WIDTH: f32 = 58.0;
const HEADER_FONT_WARNING_WIDTH: f32 = 118.0;
const HEADER_SCALE_WIDTH: f32 = 96.0;
const HEADER_EMERGENCY_WIDTH: f32 = 122.0;
const HEADER_CONTROLS_WIDTH: f32 = 598.0;
const POLLING_SLIDER_MIN_WIDTH: f32 = 180.0;
// Fixed industrial-console columns.  The panel widths are outer widths
// (including the panel frame); cards fill the remaining inner content rect.
const COMMAND_SIDEBAR_WIDTH: f32 = 360.0;
const COMMAND_SIDEBAR_MIN_WIDTH: f32 = 260.0;
const COMMAND_SIDEBAR_MAX_WIDTH: f32 = 420.0;
const TELEMETRY_SIDEBAR_WIDTH: f32 = 320.0;
const TELEMETRY_SIDEBAR_MIN_WIDTH: f32 = 240.0;
const TELEMETRY_SIDEBAR_MAX_WIDTH: f32 = 380.0;
const COMPACT_LAYOUT_BREAKPOINT: f32 = 1_120.0;
const COMPACT_COMMAND_SIDEBAR_WIDTH: f32 = 300.0;
const COMPACT_TELEMETRY_SIDEBAR_WIDTH: f32 = 280.0;
const MONITOR_LABEL_WIDTH: f32 = 94.0;
// The central work area is deliberately partitioned into fixed docking
// regions.  Only the plot body receives elastic height; logs scroll inside
// their own region and never change the height of the central work area.
const CENTRAL_OPERATIONS_HEIGHT: f32 = 136.0;
const CENTRAL_PLOT_TOOLBAR_HEIGHT: f32 = 68.0;
const CENTRAL_LOG_HEIGHT: f32 = 160.0;
const CENTRAL_GAP: f32 = 6.0;
const UI_SCALE_OPTIONS: [f32; 6] = [0.8, 0.9, 1.0, 1.1, 1.25, 1.5];
const ADC_ZERO_COUNTS: f64 = 32_768.0;
const ADC_COUNTS_PER_MILLIVOLT: f64 = 6.4;
const ALARM_COALESCE_WINDOW: Duration = Duration::from_secs(30);
const DATA_FRESHNESS_WINDOW: Duration = Duration::from_secs(2);
const UI_RECT_DEBUG_ENV: &str = "LANGMUIR_UI_DEBUG_RECTS";
const BAUD_RATES: [u32; 14] = [
    8_000_000, 6_000_000, 4_000_000, 3_000_000, 2_000_000, 1_000_000, 921_600, 460_800, 230_400,
    115_200, 57_600, 38_400, 19_200, 9_600,
];

#[derive(Debug)]
struct ConnectionPanelState {
    ports: Vec<String>,
    selected_port: usize,
    baud_rate: u32,
    connected: bool,
    detail: String,
    metrics: Option<LinkMetricsSnapshot>,
}

#[derive(Debug)]
struct LvdsUdpPanelState {
    bind_address: String,
    port: u16,
    connected: bool,
    detail: String,
    metrics: Option<LinkMetricsSnapshot>,
}

impl Default for LvdsUdpPanelState {
    fn default() -> Self {
        Self {
            bind_address: "192.168.1.100".to_owned(),
            port: 1_534,
            connected: false,
            detail: "未监听".to_owned(),
            metrics: None,
        }
    }
}

impl Default for ConnectionPanelState {
    fn default() -> Self {
        Self {
            ports: Vec::new(),
            selected_port: 0,
            baud_rate: 115_200,
            connected: false,
            detail: "未连接".to_owned(),
            metrics: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandFamily {
    Telemetry,
    Configuration,
    Lvds,
}

impl CommandFamily {
    const ALL: [Self; 3] = [Self::Telemetry, Self::Configuration, Self::Lvds];

    const fn label(self) -> &'static str {
        match self {
            Self::Telemetry => "遥测 (0x11)",
            Self::Configuration => "波形 / DC (0x21)",
            Self::Lvds => "LVDS (0x31)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigurationMode {
    Triangle,
    Sine,
    Dc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlotLayout {
    Overlay,
    Split,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandEditMode {
    Typed,
    Raw,
}

impl ConfigurationMode {
    const ALL: [Self; 3] = [Self::Triangle, Self::Sine, Self::Dc];

    const fn label(self) -> &'static str {
        match self {
            Self::Triangle => "三角波 (DATA1 0x55)",
            Self::Sine => "正弦波 (DATA1 0xAA)",
            Self::Dc => "直流 DC (DATA1 0xBB)",
        }
    }
}

#[derive(Debug)]
struct LogEntry {
    time: String,
    level: LogLevel,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlarmState {
    Unacknowledged,
    AcknowledgedUnresolved,
    Recovered,
}

impl AlarmState {
    const ALL: [Self; 3] = [
        Self::Unacknowledged,
        Self::AcknowledgedUnresolved,
        Self::Recovered,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Unacknowledged => "未确认",
            Self::AcknowledgedUnresolved => "已确认未恢复",
            Self::Recovered => "已恢复",
        }
    }

    const fn color(self) -> Color32 {
        match self {
            Self::Unacknowledged => theme::DANGER,
            Self::AcknowledgedUnresolved => theme::WARNING,
            Self::Recovered => theme::SUCCESS,
        }
    }
}

#[derive(Debug)]
struct FaultEntry {
    time: String,
    source: String,
    message: String,
    state: AlarmState,
    repetitions: u32,
    last_seen: Instant,
}

#[derive(Debug)]
struct FrozenPlot {
    channel_1: Vec<[f64; 2]>,
    channel_2: Vec<[f64; 2]>,
}

#[derive(Debug)]
struct OperatorViewState {
    frozen_plot: Option<FrozenPlot>,
    plot_cursor_seconds: Option<f64>,
    alarm_filter: AlarmState,
    emergency_stop_confirmation_open: bool,
    last_telemetry_update: Option<Instant>,
    last_scientific_update: Option<Instant>,
}

impl Default for OperatorViewState {
    fn default() -> Self {
        Self {
            frozen_plot: None,
            plot_cursor_seconds: None,
            alarm_filter: AlarmState::Unacknowledged,
            emergency_stop_confirmation_open: false,
            last_telemetry_update: None,
            last_scientific_update: None,
        }
    }
}

#[derive(Debug)]
struct ParameterStepState {
    frequency: u8,
    voltage: u16,
    telemetry_interval: u32,
}

impl Default for ParameterStepState {
    fn default() -> Self {
        Self {
            frequency: 10,
            voltage: 100,
            telemetry_interval: 1_000,
        }
    }
}

#[cfg_attr(
    feature = "mock",
    expect(
        clippy::struct_excessive_bools,
        reason = "the feature-gated mock selector adds one independent UI flag"
    )
)]
pub struct LangmuirDesktopApp {
    bridge: Box<dyn BackendBridge>,
    rs422: ConnectionPanelState,
    lvds: LvdsUdpPanelState,
    acquisition_running: bool,
    telemetry_polling: TelemetryPollingState,
    telemetry_polling_edit_millis: u32,
    command_family: CommandFamily,
    configuration_mode: ConfigurationMode,
    frequency: u8,
    voltage: u16,
    lvds_enabled: bool,
    command_edit_mode: CommandEditMode,
    raw_command: [u8; 12],
    telemetry: Option<TelemetrySnapshot>,
    channel_1: TimeSeriesBuffer,
    channel_2: TimeSeriesBuffer,
    show_channels: [bool; 2],
    plot_layout: PlotLayout,
    overlay_normalized: bool,
    logs: VecDeque<LogEntry>,
    faults: VecDeque<FaultEntry>,
    operator: OperatorViewState,
    parameter_steps: ParameterStepState,
    font_warning: Option<String>,
    #[cfg(feature = "mock")]
    mock_mode: bool,
}

impl LangmuirDesktopApp {
    #[must_use]
    pub fn new(
        context: &eframe::CreationContext<'_>,
        bridge: Box<dyn BackendBridge>,
        #[cfg(feature = "mock")] mock_mode: bool,
    ) -> Self {
        let font_report = fonts::install_cjk_fallback(&context.egui_ctx);
        if let Some(warning) = font_report.warning.as_deref() {
            eprintln!("CJK FONT LOAD WARNING: {warning}");
        }
        theme::configure(&context.egui_ctx);
        let mut app = Self {
            bridge,
            rs422: ConnectionPanelState::default(),
            lvds: LvdsUdpPanelState::default(),
            acquisition_running: false,
            telemetry_polling: TelemetryPollingState::default(),
            telemetry_polling_edit_millis: TELEMETRY_POLL_INTERVAL_DEFAULT_MILLIS,
            command_family: CommandFamily::Telemetry,
            configuration_mode: ConfigurationMode::Triangle,
            frequency: 10,
            voltage: 5_000,
            lvds_enabled: true,
            command_edit_mode: CommandEditMode::Typed,
            raw_command: command_bytes(Command::RequestTelemetry),
            telemetry: None,
            channel_1: TimeSeriesBuffer::with_display_cache(
                PLOT_CAPACITY,
                PLOT_WINDOW_SECONDS,
                MAX_DISPLAY_POINTS,
            ),
            channel_2: TimeSeriesBuffer::with_display_cache(
                PLOT_CAPACITY,
                PLOT_WINDOW_SECONDS,
                MAX_DISPLAY_POINTS,
            ),
            show_channels: [true, true],
            plot_layout: PlotLayout::Overlay,
            overlay_normalized: false,
            logs: VecDeque::new(),
            faults: VecDeque::new(),
            operator: OperatorViewState::default(),
            parameter_steps: ParameterStepState::default(),
            font_warning: font_report.warning,
            #[cfg(feature = "mock")]
            mock_mode,
        };
        app.submit(UiRequest::RefreshPorts);
        app
    }

    fn now() -> String {
        Local::now().format("%H:%M:%S%.3f").to_string()
    }

    fn push_log(&mut self, level: LogLevel, message: impl Into<String>) {
        self.logs.push_back(LogEntry {
            time: Self::now(),
            level,
            message: message.into(),
        });
        while self.logs.len() > MAX_LOGS {
            self.logs.pop_front();
        }
    }

    fn push_fault(&mut self, source: impl Into<String>, message: impl Into<String>) {
        let source = source.into();
        let message = message.into();
        let now = Instant::now();
        let timestamp = Self::now();
        if let Some(existing) = self.faults.iter_mut().find(|fault| {
            fault.source == source
                && fault.message == message
                && now.saturating_duration_since(fault.last_seen) <= ALARM_COALESCE_WINDOW
        }) {
            existing.time = timestamp;
            existing.last_seen = now;
            existing.repetitions = existing.repetitions.saturating_add(1);
            existing.state = AlarmState::Unacknowledged;
            return;
        }
        self.faults.push_front(FaultEntry {
            time: timestamp,
            source: source.clone(),
            message: message.clone(),
            state: AlarmState::Unacknowledged,
            repetitions: 1,
            last_seen: now,
        });
        while self.faults.len() > MAX_FAULTS {
            self.faults.pop_back();
        }
        self.push_log(LogLevel::Error, format!("{source}: {message}"));
    }

    fn restore_faults_for_source(&mut self, source: &str) {
        for fault in &mut self.faults {
            if fault.state != AlarmState::Recovered && fault.source.contains(source) {
                fault.state = AlarmState::Recovered;
            }
        }
    }

    fn telemetry_is_fresh(&self) -> bool {
        self.operator
            .last_telemetry_update
            .is_some_and(|updated| updated.elapsed() <= DATA_FRESHNESS_WINDOW)
    }

    fn scientific_data_is_fresh(&self) -> bool {
        self.operator
            .last_scientific_update
            .is_some_and(|updated| updated.elapsed() <= DATA_FRESHNESS_WINDOW)
    }

    fn unacknowledged_fault_count(&self) -> usize {
        self.faults
            .iter()
            .filter(|fault| fault.state == AlarmState::Unacknowledged)
            .count()
    }

    fn toggle_display_freeze(&mut self) {
        if self.operator.frozen_plot.is_some() {
            self.operator.frozen_plot = None;
            self.push_log(LogLevel::Info, "画面更新已恢复，后台采集未中断");
        } else {
            self.operator.frozen_plot = Some(FrozenPlot {
                channel_1: self.channel_1.decimated(MAX_DISPLAY_POINTS),
                channel_2: self.channel_2.decimated(MAX_DISPLAY_POINTS),
            });
            self.push_log(LogLevel::Info, "画面已冻结，后台采集继续运行");
        }
    }

    fn display_plot_points(&mut self) -> (Vec<[f64; 2]>, Vec<[f64; 2]>) {
        if let Some(frozen) = &self.operator.frozen_plot {
            return (frozen.channel_1.clone(), frozen.channel_2.clone());
        }
        (
            self.channel_1.decimated(MAX_DISPLAY_POINTS),
            self.channel_2.decimated(MAX_DISPLAY_POINTS),
        )
    }

    fn emergency_stop_dialog(&mut self, context: &egui::Context) {
        if !self.operator.emergency_stop_confirmation_open {
            return;
        }
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new("软件急停确认")
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .open(&mut open)
            .show(context, |ui| {
                ui.label(
                    RichText::new("停止上位机采集和循环遥测；RS422 已连接时会发送禁用 LVDS 指令。")
                        .color(theme::TEXT),
                );
                ui.label(
                    RichText::new("该操作是软件急停，不替代硬件急停回路。")
                        .small()
                        .color(theme::WARNING),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() {
                        cancelled = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_sized(
                                [144.0, theme::CRITICAL_CONTROL_HEIGHT],
                                egui::Button::new(
                                    RichText::new("确认软件急停").strong().color(theme::WHITE),
                                )
                                .fill(theme::DANGER),
                            )
                            .clicked_by(PointerButton::Primary)
                        {
                            confirmed = true;
                        }
                    });
                });
            });
        self.operator.emergency_stop_confirmation_open = open && !confirmed && !cancelled;
        if confirmed {
            self.submit(UiRequest::StopAcquisition);
            self.submit(UiRequest::SetTelemetryPolling {
                enabled: false,
                interval_millis: self.telemetry_polling.interval_millis,
            });
            if self.rs422.connected {
                self.submit(UiRequest::SendCommand(command_bytes(
                    Command::SetLvdsEnabled(false),
                )));
            }
            self.push_log(LogLevel::Warning, "已执行软件急停");
        }
    }

    fn submit(&mut self, request: UiRequest) {
        if let Err(error) = self.bridge.request(request) {
            self.push_fault("前后端桥接", error.to_string());
        }
    }

    fn drain_backend(&mut self) {
        for _ in 0..EVENT_BUDGET_PER_FRAME {
            let Some(event) = self.bridge.try_next_event() else {
                break;
            };
            self.handle_backend_event(event);
        }
    }

    fn handle_backend_event(&mut self, event: BackendEvent) {
        match event {
            BackendEvent::PortsUpdated { rs422 } => {
                self.rs422.ports = rs422;
                self.rs422.selected_port = 0;
                self.push_log(LogLevel::Info, "串口列表已刷新");
            }
            BackendEvent::LinkChanged {
                link,
                connected,
                detail,
            } => {
                match link {
                    LinkKind::Rs422 => {
                        self.rs422.connected = connected;
                        self.rs422.detail.clone_from(&detail);
                    }
                    LinkKind::Lvds => {
                        self.lvds.connected = connected;
                        self.lvds.detail.clone_from(&detail);
                    }
                }
                if connected {
                    self.restore_faults_for_source(&link.to_string());
                }
                let status = if connected { "已连接" } else { "已断开" };
                self.push_log(LogLevel::Info, format!("{link} {status}: {detail}"));
            }
            BackendEvent::AcquisitionChanged { running } => {
                self.acquisition_running = running;
                if running {
                    self.restore_faults_for_source("采集");
                }
                self.push_log(
                    LogLevel::Info,
                    if running {
                        "采集已开始"
                    } else {
                        "采集已停止"
                    },
                );
            }
            BackendEvent::TelemetryPollingChanged {
                active,
                interval_millis,
            } => self.apply_telemetry_polling_state(active, interval_millis),
            BackendEvent::StateSnapshot(snapshot) => self.apply_state_snapshot(snapshot),
            BackendEvent::Telemetry(snapshot) => {
                self.operator.last_telemetry_update = Some(Instant::now());
                self.record_telemetry_anomalies(&snapshot);
                self.telemetry = Some(snapshot);
            }
            BackendEvent::LinkMetrics {
                link,
                connected,
                metrics,
            } => match link {
                LinkKind::Rs422 => {
                    self.rs422.connected = connected;
                    self.rs422.metrics = Some(metrics);
                }
                LinkKind::Lvds => {
                    self.lvds.connected = connected;
                    self.lvds.metrics = Some(metrics);
                }
            },
            BackendEvent::ScientificSamples {
                channel_1_start_seconds,
                channel_2_start_seconds,
                sample_interval_seconds,
                channel_1,
                channel_2,
            } => self.handle_scientific_samples(
                channel_1_start_seconds,
                channel_2_start_seconds,
                sample_interval_seconds,
                channel_1,
                channel_2,
            ),
            BackendEvent::CommandSent(frame) => {
                self.push_log(
                    LogLevel::Info,
                    format!("RS422 已写入（设备无控制应答）: {}", hex_frame(&frame)),
                );
            }
            BackendEvent::CsvExported { path } => {
                self.push_log(LogLevel::Info, format!("CSV 已导出: {path}"));
            }
            BackendEvent::Fault { source, message } => self.push_fault(source, message),
            BackendEvent::Log { level, message } => self.push_log(level, message),
        }
    }

    fn handle_scientific_samples(
        &mut self,
        channel_1_start_seconds: f64,
        channel_2_start_seconds: f64,
        sample_interval_seconds: f64,
        channel_1: Vec<f64>,
        channel_2: Vec<f64>,
    ) {
        self.operator.last_scientific_update = Some(Instant::now());
        self.channel_1.extend(channel_1.into_iter().scan(
            channel_1_start_seconds,
            |time_seconds, value| {
                let point = SamplePoint {
                    time_seconds: *time_seconds,
                    value: adc_counts_to_millivolts(value),
                };
                *time_seconds += sample_interval_seconds;
                Some(point)
            },
        ));
        self.channel_2.extend(channel_2.into_iter().scan(
            channel_2_start_seconds,
            |time_seconds, value| {
                let point = SamplePoint {
                    time_seconds: *time_seconds,
                    value: adc_counts_to_millivolts(value),
                };
                *time_seconds += sample_interval_seconds;
                Some(point)
            },
        ));
    }

    fn apply_state_snapshot(&mut self, snapshot: BackendStateSnapshot) {
        self.rs422.connected = snapshot.rs422_connected;
        self.rs422.detail = snapshot.rs422_detail;
        self.lvds.connected = snapshot.lvds_connected;
        self.lvds.detail = snapshot.lvds_detail;
        self.acquisition_running = snapshot.acquisition_running;
        self.telemetry_polling = snapshot.telemetry_polling;
        self.telemetry_polling_edit_millis = snapshot.telemetry_polling.interval_millis;
    }

    fn apply_telemetry_polling_state(&mut self, active: bool, interval_millis: u32) {
        self.telemetry_polling = TelemetryPollingState {
            active,
            interval_millis,
        };
        self.telemetry_polling_edit_millis = interval_millis;
        self.push_log(
            LogLevel::Info,
            if active {
                format!("循环遥测已启动: {interval_millis} ms")
            } else {
                "循环遥测已停止".to_owned()
            },
        );
    }

    fn command(&self) -> Result<Command, CommandError> {
        match self.command_family {
            CommandFamily::Telemetry => Ok(Command::RequestTelemetry),
            CommandFamily::Configuration => match self.configuration_mode {
                ConfigurationMode::Triangle => Command::configure_triangle(self.frequency),
                ConfigurationMode::Sine => Command::configure_sine(self.frequency),
                ConfigurationMode::Dc => Ok(Command::ConfigureDc(self.voltage)),
            },
            CommandFamily::Lvds => Ok(Command::SetLvdsEnabled(self.lvds_enabled)),
        }
    }

    fn typed_command_bytes(&self) -> Result<[u8; 12], CommandError> {
        self.command().map(command_bytes)
    }

    fn record_telemetry_anomalies(&mut self, snapshot: &TelemetrySnapshot) {
        let previous = self.telemetry.as_ref();
        let statuses = [
            (
                "AD9833",
                snapshot.ad9833_status,
                previous.map(|item| item.ad9833_status),
            ),
            (
                "DAC8563",
                snapshot.dac8563_status,
                previous.map(|item| item.dac8563_status),
            ),
            (
                "ADC1",
                snapshot.adc_1_status,
                previous.map(|item| item.adc_1_status),
            ),
            (
                "ADC2",
                snapshot.adc_2_status,
                previous.map(|item| item.adc_2_status),
            ),
            (
                "LVDS",
                snapshot.lvds_status,
                previous.map(|item| item.lvds_status),
            ),
        ];
        for (name, status, previous_status) in statuses {
            if previous_status == Some(status) {
                continue;
            }
            self.push_log(LogLevel::Info, format!("{name} 状态码: 0x{status:02X}"));
        }
    }

    fn connection_card(
        ui: &mut Ui,
        link: LinkKind,
        state: &mut ConnectionPanelState,
    ) -> Option<UiRequest> {
        let mut request = None;
        full_width_card(ui, |ui| {
            debug_ui_rect("left.rs422_card", ui.available_rect_before_wrap());
            ui.horizontal_wrapped(|ui| {
                status_dot(ui, state.connected);
                theme::section_title(ui, &format!("{link} 串口连接"));
                ui.add(
                    egui::Label::new(
                        RichText::new(&state.detail)
                            .small()
                            .color(theme::TEXT_SECONDARY),
                    )
                    .truncate(),
                )
                .on_hover_text(&state.detail);
            });
            let selected_port = state
                .ports
                .get(state.selected_port)
                .map_or("无可用串口", String::as_str);
            ui.add_enabled_ui(!state.connected, |ui| {
                ui.label(RichText::new("串口").small().color(theme::TEXT_SECONDARY));
                ComboBox::from_id_salt(("port", link.to_string()))
                    .selected_text(selected_port)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for (index, port) in state.ports.iter().enumerate() {
                            ui.selectable_value(&mut state.selected_port, index, port);
                        }
                    });
                ui.label(RichText::new("波特率").small().color(theme::TEXT_SECONDARY));
                ComboBox::from_id_salt(("baud", link.to_string()))
                    .selected_text(state.baud_rate.to_string())
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for baud in BAUD_RATES {
                            ui.selectable_value(&mut state.baud_rate, baud, baud.to_string());
                        }
                    });
                ui.horizontal(|ui| {
                    ui.small("自定义波特率");
                    ui.add(
                        egui::DragValue::new(&mut state.baud_rate)
                            .range(1..=20_000_000)
                            .speed(100_000),
                    );
                });
            });

            let label = if state.connected { "断开" } else { "连接" };
            let enabled = state.connected || !state.ports.is_empty();
            let color = if state.connected {
                theme::DANGER
            } else {
                theme::PRIMARY
            };
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::new(RichText::new(label).color(theme::WHITE).strong())
                        .fill(color)
                        .min_size(egui::vec2(ui.available_width(), 32.0)),
                )
                .clicked()
            {
                request = if state.connected {
                    Some(UiRequest::Disconnect { link })
                } else {
                    state
                        .ports
                        .get(state.selected_port)
                        .map(|port| UiRequest::Connect {
                            link,
                            port: port.clone(),
                            baud: state.baud_rate,
                        })
                };
            }

            if let Some(metrics) = state.metrics {
                let summary = format!(
                    "RX {} · 帧 {} · 拒绝 {} · 丢弃 {}",
                    metrics.bytes_read,
                    metrics.frames_decoded,
                    metrics.rejected_frames,
                    metrics.events_dropped,
                );
                ui.add(
                    egui::Label::new(RichText::new(&summary).size(9.0).color(theme::TEXT_MUTED))
                        .truncate(),
                )
                .on_hover_text(format!(
                    "{summary}\n校验失败: {}  长度失败: {}  队列溢出: {}  I/O 错误: {}",
                    metrics.checksum_failures,
                    metrics.length_failures,
                    metrics.queue_overflows,
                    metrics.io_errors,
                ));
            }
        });
        request
    }

    fn typed_command_controls(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new("指令类型")
                .small()
                .color(theme::TEXT_SECONDARY),
        );
        ui.horizontal_wrapped(|ui| {
            for family in CommandFamily::ALL {
                ui.selectable_value(&mut self.command_family, family, family.label());
            }
        });

        match self.command_family {
            CommandFamily::Telemetry => {
                ui.add_space(6.0);
                full_width_subtle_card(ui, |ui| {
                    ui.label(
                        RichText::new("固定遥测请求")
                            .strong()
                            .color(theme::PRIMARY_DARK),
                    );
                    ui.label(
                        RichText::new("TYPE=0x11，数据域固定为 9B 9B 9B 9B。")
                            .small()
                            .color(theme::TEXT_SECONDARY),
                    );
                });
            }
            CommandFamily::Configuration => {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("工作模式")
                        .small()
                        .color(theme::TEXT_SECONDARY),
                );
                ui.horizontal_wrapped(|ui| {
                    for mode in ConfigurationMode::ALL {
                        ui.selectable_value(&mut self.configuration_mode, mode, mode.label());
                    }
                });
                ui.add_space(4.0);
                match self.configuration_mode {
                    ConfigurationMode::Triangle | ConfigurationMode::Sine => {
                        frequency_parameter(
                            ui,
                            &mut self.frequency,
                            &mut self.parameter_steps.frequency,
                        );
                    }
                    ConfigurationMode::Dc => {
                        ui.label(
                            RichText::new("原始电压设定值（协议未定义物理单位）")
                                .small()
                                .color(theme::TEXT_SECONDARY),
                        );
                        integer_parameter_u16(
                            ui,
                            &mut self.voltage,
                            &mut self.parameter_steps.voltage,
                            &[1, 10, 100],
                        );
                    }
                }
            }
            CommandFamily::Lvds => {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("LVDS 发送目标状态")
                        .small()
                        .color(theme::TEXT_SECONDARY),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut self.lvds_enabled, true, "发送开启");
                    ui.selectable_value(&mut self.lvds_enabled, false, "发送关闭");
                });
                ui.label(
                    RichText::new("目标状态来自当前待发送指令；实际设备状态以遥测确认。")
                        .small()
                        .color(theme::TEXT_MUTED),
                );
            }
        }
    }

    fn command_bytes_grid(&mut self, ui: &mut Ui, frame: &[u8; 12], raw_mode: bool) {
        let labels = [
            "H1", "H2", "SA", "RA", "L1", "L2", "CMD", "D1", "D2", "D3", "D4", "SUM",
        ];
        let cell_width = ((ui.available_width() - 12.0) / 4.0).max(42.0);
        Grid::new("command_bytes")
            .num_columns(4)
            .min_col_width(cell_width)
            .max_col_width(cell_width)
            .spacing([4.0, 4.0])
            .show(ui, |ui| {
                for (index, byte) in frame.iter().enumerate() {
                    let active = !raw_mode && self.command_byte_is_active(index);
                    let (fill, stroke, text_color) = match index {
                        6 => (theme::PRIMARY, theme::PRIMARY, theme::WHITE),
                        11 => (theme::DANGER, theme::DANGER, theme::WHITE),
                        _ if active => (theme::SURFACE_ALT, theme::WARNING, theme::WARNING),
                        _ => (theme::SURFACE_ALT, theme::BORDER, theme::TEXT),
                    };
                    egui::Frame::new()
                        .fill(fill)
                        .stroke(Stroke::new(if active { 2.0 } else { 1.0 }, stroke))
                        .corner_radius(6.0)
                        .inner_margin(3.0)
                        .show(ui, |ui| {
                            ui.set_width((cell_width - 6.0).max(36.0));
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    RichText::new(labels[index])
                                        .size(8.0)
                                        .strong()
                                        .color(text_color),
                                );
                                if raw_mode {
                                    ui.scope(|ui| {
                                        ui.visuals_mut().override_text_color = Some(text_color);
                                        ui.add_sized(
                                            [36.0, 20.0],
                                            egui::DragValue::new(&mut self.raw_command[index])
                                                .range(0..=u8::MAX)
                                                .hexadecimal(2, false, true),
                                        );
                                    });
                                } else {
                                    ui.label(
                                        RichText::new(format!("{byte:02X}"))
                                            .monospace()
                                            .size(12.0)
                                            .strong()
                                            .color(text_color),
                                    );
                                }
                            });
                        });
                    if index % 4 == 3 {
                        ui.end_row();
                    }
                }
            });
    }

    fn command_panel(&mut self, ui: &mut Ui) {
        theme::section_title(ui, "12B 指令构造");
        let typed_before_toggle = self.typed_command_bytes();
        let mut raw_mode = self.command_edit_mode == CommandEditMode::Raw;
        if theme::aligned_checkbox(ui, &mut raw_mode, "原始字节编辑").changed() {
            self.command_edit_mode = if raw_mode {
                if let Ok(frame) = typed_before_toggle {
                    self.raw_command = frame;
                }
                CommandEditMode::Raw
            } else {
                CommandEditMode::Typed
            };
        }

        full_width_subtle_card(ui, |ui| {
            debug_ui_rect("left.command_card", ui.available_rect_before_wrap());
            if raw_mode {
                let raw_frame = self.raw_command;
                self.command_bytes_grid(ui, &raw_frame, true);
                ui.add_space(2.0);
                ui.label(
                    RichText::new("原始模式：直接编辑 HEADER | SRC | DST | LEN | TYPE | DATA | SUM；不会自动修复校验和")
                        .size(9.0)
                        .color(theme::WARNING),
                );
            } else {
                self.typed_command_controls(ui);
            }
        });

        let displayed_frame = if raw_mode {
            Some(self.raw_command)
        } else {
            self.typed_command_bytes().ok()
        };
        full_width_subtle_card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                theme::section_title(ui, "指令预览与校验");
                ui.label(
                    RichText::new("固定 12B · 大端序")
                        .small()
                        .color(theme::TEXT_MUTED),
                );
            });
            if let Some(frame) = displayed_frame {
                let preview = hex_frame(&frame);
                ui.add(
                    egui::Label::new(
                        RichText::new(&preview)
                            .monospace()
                            .size(10.0)
                            .color(theme::PRIMARY),
                    )
                    .wrap(),
                )
                .on_hover_text(&preview);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("地址").small().color(theme::TEXT_SECONDARY));
                    ui.label(RichText::new("F0 → 55").monospace());
                    ui.label(RichText::new("TYPE").small().color(theme::TEXT_SECONDARY));
                    ui.label(RichText::new(format!("0x{:02X}", frame[6])).monospace());
                    ui.label(RichText::new("SUM").small().color(theme::TEXT_SECONDARY));
                    ui.label(RichText::new(format!("0x{:02X}", frame[11])).monospace());
                    if raw_mode {
                        ui.label(
                            RichText::new(format!("预期 0x{:02X}", command_checksum(&frame)))
                                .small()
                                .color(theme::TEXT_SECONDARY),
                        );
                        match raw_frame_validation(&frame) {
                            Ok(()) => {
                                theme::status_badge(ui, "校验正确", theme::SUCCESS);
                            }
                            Err(reason) => {
                                theme::status_badge(ui, "校验错误", theme::DANGER);
                                ui.label(RichText::new(reason).small().color(theme::DANGER));
                            }
                        }
                    } else {
                        theme::status_badge(ui, "自动生成·校验正确", theme::SUCCESS);
                    }
                    if ui.button("复制完整帧").clicked() {
                        ui.ctx().copy_text(preview.clone());
                    }
                });
            } else {
                ui.colored_label(theme::DANGER, "频率超出协议范围 0x00..=0x64，无法生成指令");
            }
        });
        let can_send = if raw_mode {
            raw_frame_validation(&self.raw_command).is_ok()
        } else {
            self.typed_command_bytes().is_ok()
        };
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_sized([68.0, 28.0], egui::Button::new("重置"))
                .clicked()
            {
                self.command_family = CommandFamily::Telemetry;
                self.configuration_mode = ConfigurationMode::Triangle;
                self.frequency = 10;
                self.voltage = 5_000;
                self.lvds_enabled = true;
                self.command_edit_mode = CommandEditMode::Typed;
                self.raw_command = command_bytes(Command::RequestTelemetry);
                raw_mode = false;
            }
            if raw_mode && ui.button("重算 SUM").clicked() {
                recalculate_checksum(&mut self.raw_command);
            }
            if ui
                .add_enabled(
                    can_send,
                    egui::Button::new(RichText::new("发送指令").strong().color(theme::WHITE))
                        .min_size(egui::vec2(104.0, 28.0))
                        .fill(theme::PRIMARY),
                )
                .clicked()
            {
                if raw_mode {
                    match raw_frame_validation(&self.raw_command) {
                        Ok(()) => self.submit(UiRequest::SendCommand(self.raw_command)),
                        Err(reason) => {
                            self.push_fault("RS422 指令", format!("原始帧无效，未发送：{reason}"))
                        }
                    }
                } else if let Some(frame) = self.typed_command_bytes().ok() {
                    self.submit(UiRequest::SendCommand(frame));
                } else {
                    self.push_fault("RS422 指令", "频率超出协议范围 0x00..=0x64，指令未发送");
                }
            }
        });
        if !raw_mode && self.command_family == CommandFamily::Telemetry {
            ui.add_space(6.0);
            full_width_subtle_card(ui, |ui| {
                theme::section_title(ui, "自动遥测轮询");
                self.telemetry_polling_control(ui);
            });
        }
    }

    fn telemetry_polling_control(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("当前执行周期")
                    .size(9.0)
                    .color(theme::TEXT_SECONDARY),
            );
            let current = if self.telemetry_polling.active {
                format!("{} ms", self.telemetry_polling.interval_millis)
            } else {
                "未运行".to_owned()
            };
            ui.label(
                RichText::new(current)
                    .monospace()
                    .strong()
                    .color(theme::PRIMARY),
            );
            let edit_state =
                if self.telemetry_polling_edit_millis == self.telemetry_polling.interval_millis {
                    "已同步"
                } else {
                    "待应用"
                };
            theme::status_badge(
                ui,
                edit_state,
                if edit_state == "已同步" {
                    theme::SUCCESS
                } else {
                    theme::WARNING
                },
            );
        });
        ui.label(
            RichText::new(format!(
                "编辑周期：{} ms（范围 {}–{} ms）",
                self.telemetry_polling_edit_millis,
                TELEMETRY_POLL_INTERVAL_MIN_MILLIS,
                TELEMETRY_POLL_INTERVAL_MAX_MILLIS
            ))
            .small()
            .color(theme::TEXT_MUTED),
        );

        let slider_width = polling_slider_width(ui.available_width());
        let slider_response = ui
            .scope(|ui| {
                let visuals = ui.visuals_mut();
                visuals.selection.bg_fill = theme::PRIMARY;
                visuals.widgets.inactive.bg_fill = theme::SLIDER_TRACK;
                visuals.widgets.inactive.bg_stroke = Stroke::new(1.5, theme::PRIMARY_DARK);
                visuals.widgets.hovered.bg_fill = theme::PRIMARY_SOFT;
                visuals.widgets.hovered.bg_stroke = Stroke::new(2.0, theme::PRIMARY);
                visuals.widgets.active.bg_fill = theme::PRIMARY;
                visuals.widgets.active.fg_stroke = Stroke::new(2.0, theme::WHITE);
                ui.spacing_mut().slider_width = slider_width;
                ui.spacing_mut().slider_rail_height = 6.0;
                ui.add_sized(
                    [slider_width, theme::CONTROL_HEIGHT],
                    egui::Slider::new(
                        &mut self.telemetry_polling_edit_millis,
                        TELEMETRY_POLL_INTERVAL_MIN_MILLIS..=TELEMETRY_POLL_INTERVAL_MAX_MILLIS,
                    )
                    .show_value(false)
                    .logarithmic(true)
                    .trailing_fill(true)
                    .step_by(f64::from(self.parameter_steps.telemetry_interval)),
                )
            })
            .inner;
        debug_ui_rect("left.telemetry_slider", slider_response.rect);
        let slider_changed = slider_response.changed();

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("最小 {} ms", TELEMETRY_POLL_INTERVAL_MIN_MILLIS))
                    .small()
                    .color(theme::TEXT_MUTED),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("最大 {} ms", TELEMETRY_POLL_INTERVAL_MAX_MILLIS))
                        .small()
                        .color(theme::TEXT_MUTED),
                );
            });
        });

        let input_changed = ui
            .horizontal(|ui| {
                let mut changed = ui
                    .add_sized(
                        [102.0, theme::CONTROL_HEIGHT],
                        egui::DragValue::new(&mut self.telemetry_polling_edit_millis)
                            .range(
                                TELEMETRY_POLL_INTERVAL_MIN_MILLIS
                                    ..=TELEMETRY_POLL_INTERVAL_MAX_MILLIS,
                            )
                            .speed(f64::from(self.parameter_steps.telemetry_interval))
                            .suffix(" ms"),
                    )
                    .changed();
                for preset in [100, 1_000, 5_000, 60_000] {
                    if ui
                        .selectable_label(
                            telemetry_preset_active(self.telemetry_polling_edit_millis, preset),
                            format!("{preset}"),
                        )
                        .clicked()
                    {
                        self.telemetry_polling_edit_millis = preset;
                        changed = true;
                    }
                }
                changed
            })
            .inner;
        let changed = slider_changed || input_changed;
        if changed && self.telemetry_polling.active {
            self.submit(UiRequest::SetTelemetryPolling {
                enabled: true,
                interval_millis: self.telemetry_polling_edit_millis,
            });
        }
        ui.horizontal(|ui| {
            let (label, fill) = if self.telemetry_polling.active {
                ("停止循环", theme::DANGER)
            } else {
                ("开始循环", theme::SUCCESS)
            };
            if ui
                .add_enabled(
                    self.rs422.connected,
                    egui::Button::new(RichText::new(label).color(theme::WHITE)).fill(fill),
                )
                .clicked()
            {
                self.submit(UiRequest::SetTelemetryPolling {
                    enabled: !self.telemetry_polling.active,
                    interval_millis: self.telemetry_polling_edit_millis,
                });
            }
        });
    }

    fn command_byte_is_active(&self, index: usize) -> bool {
        match self.command_family {
            CommandFamily::Telemetry => index == 6,
            CommandFamily::Configuration => match self.configuration_mode {
                ConfigurationMode::Triangle | ConfigurationMode::Sine => matches!(index, 6..=8),
                ConfigurationMode::Dc => matches!(index, 6..=10),
            },
            CommandFamily::Lvds => matches!(index, 6..=7),
        }
    }

    fn telemetry_panel(&self, ui: &mut Ui) {
        let Some(data) = &self.telemetry else {
            full_width_card(ui, |ui| {
                debug_ui_rect(
                    "right.telemetry_empty_card",
                    ui.available_rect_before_wrap(),
                );
                ui.horizontal_wrapped(|ui| {
                    theme::section_title(ui, "遥测状态");
                    theme::status_badge(ui, "等待数据", theme::WARNING);
                });
                ui.add_space(6.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("●").color(theme::WARNING).size(16.0));
                    ui.label(
                        RichText::new("尚未收到下位机遥测响应")
                            .color(theme::TEXT_SECONDARY)
                            .size(11.0),
                    );
                    ui.label(
                        RichText::new("时间、工作模式和状态将在响应后显示")
                            .small()
                            .color(theme::TEXT_MUTED),
                    );
                });
                #[cfg(feature = "mock")]
                if self.mock_mode {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("数据源：Mock 模式（等待模拟遥测）")
                            .small()
                            .color(theme::WARNING),
                    );
                }
            });
            return;
        };
        let fresh = self.telemetry_is_fresh();

        full_width_card(ui, |ui| {
            debug_ui_rect("right.telemetry_card", ui.available_rect_before_wrap());
            ui.horizontal_wrapped(|ui| {
                theme::section_title(ui, "时间与工作参数");
                heartbeat_indicator(ui, fresh);
                theme::status_badge(
                    ui,
                    if fresh { "实时" } else { "数据超时" },
                    if fresh {
                        theme::SUCCESS
                    } else {
                        theme::WARNING
                    },
                );
            });
            telemetry_detail_row(
                ui,
                "下位机时间",
                format_timestamp(data.absolute_time_seconds),
                fresh,
            );
            telemetry_detail_row(ui, "自守时 (s)", data.local_time_seconds.to_string(), fresh);
            telemetry_detail_row(
                ui,
                "遥测计数",
                data.telemetry_request_count.to_string(),
                fresh,
            );
            telemetry_detail_row(ui, "工作模式", work_mode_name(data.work_mode), fresh);
            telemetry_detail_row(ui, "扫描频率", data.scan_frequency.to_string(), fresh);
            telemetry_detail_row(ui, "扫描电压", data.scan_voltage.to_string(), fresh);
            #[cfg(feature = "mock")]
            if self.mock_mode {
                ui.label(
                    RichText::new("数据源：Mock 模式")
                        .small()
                        .color(theme::WARNING),
                );
            }
        });

        ui.add_space(7.0);
        full_width_card(ui, |ui| {
            debug_ui_rect("right.temperature_card", ui.available_rect_before_wrap());
            theme::section_title(ui, "温度");
            temperature_bar(ui, "CPU 温度", data.cpu_temperature_celsius, fresh);
        });

        ui.add_space(7.0);
        full_width_card(ui, |ui| {
            debug_ui_rect("right.device_card", ui.available_rect_before_wrap());
            theme::section_title(ui, "设备状态");
            ui.columns(2, |columns| {
                command_counter(
                    &mut columns[0],
                    "指令正确",
                    data.accepted_commands,
                    theme::SUCCESS,
                    theme::SUCCESS_SOFT,
                );
                command_counter(
                    &mut columns[1],
                    "指令错误",
                    data.rejected_commands,
                    theme::DANGER,
                    theme::DANGER_SOFT,
                );
            });
            ui.separator();
            protocol_status_row(ui, "AD9833", data.ad9833_status, fresh);
            protocol_status_row(ui, "DAC8563", data.dac8563_status, fresh);
            protocol_status_row(ui, "ADC1", data.adc_1_status, fresh);
            protocol_status_row(ui, "ADC2", data.adc_2_status, fresh);
            protocol_status_row(ui, "LVDS", data.lvds_status, fresh);
            telemetry_detail_row(
                ui,
                "上一条指令数据域（5B）",
                hex_bytes(&data.last_injected_command),
                fresh,
            );
            ui.label(
                RichText::new("遥测应答仅包含 TYPE + DATA1..DATA4；完整发送帧请查看左侧预览。")
                    .small()
                    .color(theme::TEXT_MUTED),
            );
        });
    }

    fn fault_panel(&mut self, ui: &mut Ui) {
        full_width_card(ui, |ui| {
            debug_ui_rect("right.alarm_card", ui.available_rect_before_wrap());
            ui.horizontal_wrapped(|ui| {
                theme::section_title(ui, "报警");
                ui.label(
                    RichText::new(format!(
                        "未确认 {} · {}/{}",
                        self.unacknowledged_fault_count(),
                        self.faults.len(),
                        MAX_FAULTS
                    ))
                    .small()
                    .color(theme::TEXT_MUTED),
                );
            });
            ui.horizontal_wrapped(|ui| {
                for state in AlarmState::ALL {
                    ui.selectable_value(&mut self.operator.alarm_filter, state, state.label());
                }
            });
            let visible: Vec<_> = self
                .faults
                .iter()
                .enumerate()
                .filter_map(|(index, fault)| {
                    (fault.state == self.operator.alarm_filter).then_some(index)
                })
                .take(MAX_VISIBLE_FAULTS)
                .collect();
            if visible.is_empty() {
                egui::Frame::new()
                    .fill(theme::SUCCESS_SOFT)
                    .stroke(Stroke::new(1.0, theme::SUCCESS.gamma_multiply(0.45)))
                    .corner_radius(5.0)
                    .inner_margin(egui::Margin::same(7))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            theme::status_badge(ui, "正常", theme::SUCCESS);
                            ui.label(
                                RichText::new("该筛选下没有报警记录")
                                    .small()
                                    .color(theme::TEXT_SECONDARY),
                            );
                        });
                        ui.label(
                            RichText::new("收到设备异常或通信故障后，记录会显示在这里。")
                                .small()
                                .color(theme::TEXT_MUTED)
                                .italics(),
                        );
                    });
            } else {
                for index in visible {
                    let fault = self
                        .faults
                        .get_mut(index)
                        .expect("filtered fault index remains valid");
                    let color = if fault.state == AlarmState::Unacknowledged {
                        flashing_alarm_color(ui, fault.state)
                    } else {
                        fault.state.color()
                    };
                    egui::Frame::new()
                        .fill(color.gamma_multiply(0.16))
                        .stroke(Stroke::new(1.0, color))
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::same(5))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                theme::status_badge(ui, fault.state.label(), color);
                                ui.label(
                                    RichText::new(format!(
                                        "{}  ×{}",
                                        fault.time, fault.repetitions
                                    ))
                                    .small()
                                    .color(theme::TEXT_MUTED),
                                );
                                match fault.state {
                                    AlarmState::Unacknowledged => {
                                        if ui.button("确认").clicked() {
                                            fault.state = AlarmState::AcknowledgedUnresolved;
                                        }
                                    }
                                    AlarmState::AcknowledgedUnresolved => {
                                        if ui.button("标记恢复").clicked() {
                                            fault.state = AlarmState::Recovered;
                                        }
                                    }
                                    AlarmState::Recovered => {}
                                }
                            });
                            let summary = format!("{}: {}", fault.source, fault.message);
                            ui.add(
                                egui::Label::new(RichText::new(&summary).size(10.0).color(color))
                                    .wrap(),
                            )
                            .on_hover_text(summary);
                        });
                }
            }
        });
    }

    fn plot_toolbar_channels(&mut self, ui: &mut Ui, short: bool, show_title: bool) {
        if show_title {
            theme::section_title(ui, "LVDS 科学数据");
            ui.separator();
        }
        ui.checkbox(
            &mut self.show_channels[0],
            if short { "通1" } else { "通道 1" },
        );
        ui.checkbox(
            &mut self.show_channels[1],
            if short { "通2" } else { "通道 2" },
        );
    }

    fn plot_toolbar_modes(&mut self, ui: &mut Ui, menu: bool) {
        if menu {
            ui.menu_button("显示模式", |ui| {
                ui.selectable_value(&mut self.plot_layout, PlotLayout::Overlay, "叠加");
                ui.selectable_value(&mut self.plot_layout, PlotLayout::Split, "分屏");
                if self.plot_layout == PlotLayout::Overlay {
                    ui.checkbox(&mut self.overlay_normalized, "独立归一化");
                }
            });
        } else {
            ui.selectable_value(&mut self.plot_layout, PlotLayout::Overlay, "叠加");
            ui.selectable_value(&mut self.plot_layout, PlotLayout::Split, "分屏");
            if self.plot_layout == PlotLayout::Overlay {
                ui.checkbox(&mut self.overlay_normalized, "独立归一化");
            }
        }
    }

    fn plot_toolbar_channels_menu(&mut self, ui: &mut Ui) {
        ui.menu_button("通道", |ui| {
            ui.checkbox(&mut self.show_channels[0], "通道 1");
            ui.checkbox(&mut self.show_channels[1], "通道 2");
        });
    }

    fn plot_toolbar_actions(&mut self, ui: &mut Ui, menu: bool) {
        let frozen = self.operator.frozen_plot.is_some();
        let freeze_label = if frozen {
            "恢复更新"
        } else {
            "冻结画面"
        };
        if menu {
            ui.menu_button("图表操作", |ui| {
                if ui.button(freeze_label).clicked() {
                    self.toggle_display_freeze();
                    ui.close();
                }
                if ui.button("清空").clicked() {
                    self.channel_1.clear();
                    self.channel_2.clear();
                    self.operator.plot_cursor_seconds = None;
                    ui.close();
                }
            });
        } else {
            if ui
                .add(egui::Button::new(freeze_label).fill(if frozen {
                    theme::PRIMARY
                } else {
                    theme::SURFACE_ALT
                }))
                .clicked()
            {
                self.toggle_display_freeze();
            }
            if ui.button("清空").clicked() {
                self.channel_1.clear();
                self.channel_2.clear();
                self.operator.plot_cursor_seconds = None;
            }
        }
    }

    fn plot_toolbar(&mut self, ui: &mut Ui) {
        let width = ui.available_width();
        let compact = width < 560.0;
        let ultra_compact = width < 360.0;
        let frozen = self.operator.frozen_plot.is_some();
        let cache = format!(
            "固定 {:.0}s · {}/{} 点",
            PLOT_WINDOW_SECONDS,
            self.channel_1.len(),
            self.channel_2.len()
        );

        if !compact {
            ui.allocate_ui_with_layout(
                egui::vec2(width, 50.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    self.plot_toolbar_channels(ui, false, true);
                    ui.add_space(6.0);
                    self.plot_toolbar_modes(ui, false);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&cache).small().color(theme::TEXT_MUTED),
                            )
                            .truncate(),
                        );
                        self.plot_toolbar_actions(ui, false);
                    });
                },
            );
        } else {
            ui.allocate_ui_with_layout(
                egui::vec2(width, 25.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if ultra_compact {
                        // At the minimum window width the title and two
                        // checkboxes cannot share a row with the mode menu.
                        // Keep both channel controls accessible through an
                        // explicit menu instead of allowing an accidental
                        // third toolbar row.
                        self.plot_toolbar_channels_menu(ui);
                        self.plot_toolbar_modes(ui, true);
                    } else {
                        self.plot_toolbar_channels(ui, true, true);
                        ui.add_space(6.0);
                        self.plot_toolbar_modes(ui, false);
                    }
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(width, 25.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    self.plot_toolbar_actions(ui, ultra_compact);
                    if frozen && !ultra_compact {
                        theme::status_badge(ui, "画面冻结", theme::PRIMARY);
                    }
                    ui.add_sized(
                        [ui.available_width().max(0.0), 25.0],
                        egui::Label::new(RichText::new(&cache).small().color(theme::TEXT_MUTED))
                            .truncate(),
                    );
                },
            );
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "overlay and split plots share rendering logic"
    )]
    fn plot_panel(&mut self, ui: &mut Ui) {
        debug_ui_rect("central.plot_body_inner", ui.available_rect_before_wrap());
        let (mut channel_1, mut channel_2) = self.display_plot_points();
        let cursor_channel_1 = channel_1.clone();
        let cursor_channel_2 = channel_2.clone();
        // Reserve the readout row even when no cursor is active.  This keeps
        // the actual plot rect stable when a pointer interaction creates or
        // clears the cursor.
        let readout_height = ui.available_height().min(20.0).max(0.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width().max(0.0), readout_height),
            Layout::left_to_right(Align::Center),
            |ui| {
                if let Some(cursor) = self.operator.plot_cursor_seconds {
                    plot_cursor_readout(ui, cursor, &cursor_channel_1, &cursor_channel_2);
                }
            },
        );
        // The parent central layout owns the plot height.  Never grow this
        // region to a minimum that could overflow the fixed log dock.
        let available_plot_height = ui.available_height().max(0.0);
        let has_visible_samples = (self.show_channels[0] && !channel_1.is_empty())
            || (self.show_channels[1] && !channel_2.is_empty());
        if !has_visible_samples {
            let empty_rect = ui.available_rect_before_wrap();
            debug_ui_rect("central.plot_empty_state", empty_rect);
            let channels_hidden = !self.show_channels[0] && !self.show_channels[1];
            let state = if channels_hidden {
                "通道 1 和通道 2 当前均已隐藏"
            } else if !self.lvds.connected {
                "LVDS / UDP 未连接"
            } else if !self.acquisition_running {
                "采集未启动，点击“开始采集”后等待样本"
            } else if self.operator.frozen_plot.is_some() {
                "画面已冻结，恢复更新后等待新样本"
            } else {
                "采集已启动，等待 LVDS 样本"
            };
            ui.allocate_ui_with_layout(
                empty_rect.size(),
                Layout::centered_and_justified(egui::Direction::TopDown),
                |ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(if channels_hidden {
                                "请选择显示通道"
                            } else {
                                "暂无 LVDS 数据"
                            })
                            .strong()
                            .size(16.0)
                            .color(if channels_hidden {
                                theme::WARNING
                            } else {
                                theme::TEXT_SECONDARY
                            }),
                        );
                        ui.label(RichText::new(state).small().color(theme::TEXT_SECONDARY));
                        ui.label(
                            RichText::new("接收到数据后，图表会自动显示最近 10 秒")
                                .small()
                                .color(theme::TEXT_MUTED),
                        );
                    });
                },
            );
            return;
        }
        let cursor = self.operator.plot_cursor_seconds;
        let mut cursor_update = None;
        match self.plot_layout {
            PlotLayout::Overlay => {
                let channel_1_range = if self.overlay_normalized {
                    normalize_plot_points(&mut channel_1)
                } else {
                    None
                };
                let channel_2_range = if self.overlay_normalized {
                    normalize_plot_points(&mut channel_2)
                } else {
                    None
                };
                let channel_1_name = overlay_channel_name("通道 1", channel_1_range);
                let channel_2_name = overlay_channel_name("通道 2", channel_2_range);
                Plot::new("scientific_plot_overlay")
                    .legend(Legend::default())
                    .show_axes([true, true])
                    .x_axis_label("时间")
                    .y_axis_label(if self.overlay_normalized {
                        "独立归一化幅值（实际量程见图例）"
                    } else {
                        "电压 (mV)"
                    })
                    .x_axis_formatter(|mark, range| {
                        format_plot_timestamp(mark.value, range.end() - range.start())
                    })
                    .allow_scroll(false)
                    .allow_drag(false)
                    .allow_zoom(true)
                    .allow_boxed_zoom(true)
                    .show_background(true)
                    .height(available_plot_height)
                    .show(ui, |plot_ui| {
                        if let Some(cursor) = cursor {
                            plot_ui
                                .vline(VLine::new("游标", cursor).color(theme::WARNING).width(1.5));
                        }
                        if self.show_channels[0] && !channel_1.is_empty() {
                            plot_ui.line(
                                Line::new(channel_1_name, PlotPoints::from(channel_1.clone()))
                                    .color(theme::PRIMARY),
                            );
                        }
                        if self.show_channels[1] && !channel_2.is_empty() {
                            plot_ui.line(
                                Line::new(channel_2_name, PlotPoints::from(channel_2.clone()))
                                    .color(theme::DANGER),
                            );
                        }
                        update_plot_cursor_from_pointer(plot_ui, &mut cursor_update);
                    });
            }
            PlotLayout::Split => {
                let show_channel_1 = self.show_channels[0];
                let show_channel_2 = self.show_channels[1];
                let visible_channel_count =
                    usize::from(show_channel_1) + usize::from(show_channel_2);
                let split_height = if visible_channel_count == 2 {
                    ((available_plot_height - 8.0) / 2.0).max(1.0)
                } else {
                    // A single visible channel owns the whole elastic plot
                    // region; do not leave an unexplained empty half below it.
                    available_plot_height.max(1.0)
                };
                if show_channel_1 {
                    Plot::new("scientific_plot_channel_1")
                        .show_axes([true, true])
                        .x_axis_label("通道 1 时间")
                        .y_axis_label("通道 1 电压 (mV)")
                        .x_axis_formatter(|mark, range| {
                            format_plot_timestamp(mark.value, range.end() - range.start())
                        })
                        .allow_scroll(false)
                        .allow_drag(false)
                        .allow_zoom(true)
                        .allow_boxed_zoom(true)
                        .show_background(true)
                        .height(split_height)
                        .show(ui, |plot_ui| {
                            if let Some(cursor) = cursor {
                                plot_ui.vline(
                                    VLine::new("游标", cursor).color(theme::WARNING).width(1.5),
                                );
                            }
                            if !channel_1.is_empty() {
                                plot_ui.line(
                                    Line::new("通道 1", PlotPoints::from(channel_1.clone()))
                                        .color(theme::PRIMARY),
                                );
                            }
                            update_plot_cursor_from_pointer(plot_ui, &mut cursor_update);
                        });
                }
                if show_channel_2 {
                    Plot::new("scientific_plot_channel_2")
                        .show_axes([true, true])
                        .x_axis_label("通道 2 时间")
                        .y_axis_label("通道 2 电压 (mV)")
                        .x_axis_formatter(|mark, range| {
                            format_plot_timestamp(mark.value, range.end() - range.start())
                        })
                        .allow_scroll(false)
                        .allow_drag(false)
                        .allow_zoom(true)
                        .allow_boxed_zoom(true)
                        .show_background(true)
                        .height(split_height)
                        .show(ui, |plot_ui| {
                            if let Some(cursor) = cursor {
                                plot_ui.vline(
                                    VLine::new("游标", cursor).color(theme::WARNING).width(1.5),
                                );
                            }
                            if !channel_2.is_empty() {
                                plot_ui.line(
                                    Line::new("通道 2", PlotPoints::from(channel_2.clone()))
                                        .color(theme::DANGER),
                                );
                            }
                            update_plot_cursor_from_pointer(plot_ui, &mut cursor_update);
                        });
                }
            }
        }
        if let Some(cursor) = cursor_update {
            self.operator.plot_cursor_seconds = Some(cursor);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the LVDS toolbar keeps its responsive controls in one render function"
    )]
    fn operations_panel(&mut self, ui: &mut Ui) {
        let mut request = None;
        full_width_card(ui, |ui| {
            debug_ui_rect("central.operations_card", ui.available_rect_before_wrap());
            let row_width = ui.available_width().max(0.0);
            let compact = row_width < 560.0;
            let ultra_compact = row_width < 360.0;
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.allocate_ui_with_layout(
                egui::vec2(row_width, theme::CRITICAL_CONTROL_HEIGHT),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    status_dot(ui, self.lvds.connected);
                    ui.strong("LVDS / UDP");
                    if !compact {
                        ui.label(RichText::new("本机").small().color(theme::TEXT_SECONDARY));
                    }
                    let ip_response = ui
                        .add_enabled_ui(!self.lvds.connected, |ui| {
                            ui.add_sized(
                                [
                                    if ultra_compact {
                                        82.0
                                    } else if compact {
                                        96.0
                                    } else {
                                        150.0
                                    },
                                    theme::CONTROL_HEIGHT,
                                ],
                                egui::TextEdit::singleline(&mut self.lvds.bind_address)
                                    .hint_text("192.168.1.100"),
                            )
                        })
                        .inner;
                    debug_ui_rect("central.operations.ip_input", ip_response.rect);
                    if !ultra_compact {
                        ui.label(
                            RichText::new(if compact { "端" } else { "端口" })
                                .small()
                                .color(theme::TEXT_SECONDARY),
                        );
                    }
                    let port_response = ui.add_sized(
                        [
                            if ultra_compact {
                                34.0
                            } else if compact {
                                42.0
                            } else {
                                64.0
                            },
                            theme::CONTROL_HEIGHT,
                        ],
                        egui::DragValue::new(&mut self.lvds.port).range(1..=u16::MAX),
                    );
                    debug_ui_rect("central.operations.port_input", port_response.rect);
                    if !compact {
                        let connection_label = if self.lvds.connected {
                            "断开"
                        } else {
                            "连接"
                        };
                        let response = ui.add_sized(
                            [56.0, theme::CONTROL_HEIGHT],
                            egui::Button::new(connection_label),
                        );
                        debug_ui_rect("central.operations.connection_button", response.rect);
                        if response.clicked() {
                            request = if self.lvds.connected {
                                Some(UiRequest::Disconnect {
                                    link: LinkKind::Lvds,
                                })
                            } else {
                                Some(UiRequest::BindLvdsUdp {
                                    bind_address: self.lvds.bind_address.trim().to_owned(),
                                    port: self.lvds.port,
                                })
                            };
                        }

                        let (action_text, action_color) = if self.acquisition_running {
                            ("停止采集", theme::DANGER)
                        } else {
                            ("开始采集", theme::SUCCESS)
                        };
                        let response = ui.add_sized(
                            [112.0, theme::CRITICAL_CONTROL_HEIGHT],
                            egui::Button::new(
                                RichText::new(action_text).strong().color(theme::WHITE),
                            )
                            .fill(action_color),
                        );
                        debug_ui_rect("central.operations.acquisition_button", response.rect);
                        if response.clicked() {
                            request = Some(if self.acquisition_running {
                                UiRequest::StopAcquisition
                            } else {
                                UiRequest::StartAcquisition
                            });
                        }
                    }
                },
            );

            let row_two_height = if compact {
                theme::CRITICAL_CONTROL_HEIGHT
            } else {
                theme::CONTROL_HEIGHT
            };
            ui.allocate_ui_with_layout(
                egui::vec2(row_width, row_two_height),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if compact {
                        let connection_label = if self.lvds.connected {
                            "断开"
                        } else {
                            "连接"
                        };
                        let response = ui.add_sized(
                            [44.0, theme::CONTROL_HEIGHT],
                            egui::Button::new(connection_label),
                        );
                        debug_ui_rect("central.operations.connection_button", response.rect);
                        if response.clicked() {
                            request = if self.lvds.connected {
                                Some(UiRequest::Disconnect {
                                    link: LinkKind::Lvds,
                                })
                            } else {
                                Some(UiRequest::BindLvdsUdp {
                                    bind_address: self.lvds.bind_address.trim().to_owned(),
                                    port: self.lvds.port,
                                })
                            };
                        }

                        let (action_text, action_color) = if self.acquisition_running {
                            ("停止采集", theme::DANGER)
                        } else {
                            ("开始采集", theme::SUCCESS)
                        };
                        let response = ui.add_sized(
                            [70.0, theme::CRITICAL_CONTROL_HEIGHT],
                            egui::Button::new(
                                RichText::new(action_text).strong().color(theme::WHITE),
                            )
                            .fill(action_color),
                        );
                        debug_ui_rect("central.operations.acquisition_button", response.rect);
                        if response.clicked() {
                            request = Some(if self.acquisition_running {
                                UiRequest::StopAcquisition
                            } else {
                                UiRequest::StartAcquisition
                            });
                        }

                        ui.menu_button("更多操作", |ui| {
                            if ui.button("刷新串口").clicked() {
                                request = Some(UiRequest::RefreshPorts);
                                ui.close();
                            }
                            if ui.button("导出遥测 (CSV)").clicked() {
                                request = Some(UiRequest::ExportCsv {
                                    suggested_name: format!(
                                        "langmuir-{}.csv",
                                        Local::now().format("%Y%m%d-%H%M%S")
                                    ),
                                });
                                ui.close();
                            }
                            #[cfg(feature = "mock")]
                            if self.mock_mode && ui.button("注入模拟异常").clicked() {
                                request = Some(UiRequest::InjectMockFault);
                                ui.close();
                            }
                        });
                    } else {
                        if ui.button("刷新串口").clicked() {
                            request = Some(UiRequest::RefreshPorts);
                        }
                        if ui.button("导出遥测 (CSV)").clicked() {
                            request = Some(UiRequest::ExportCsv {
                                suggested_name: format!(
                                    "langmuir-{}.csv",
                                    Local::now().format("%Y%m%d-%H%M%S")
                                ),
                            });
                        }
                        #[cfg(feature = "mock")]
                        if self.mock_mode && ui.button("注入模拟异常").clicked() {
                            request = Some(UiRequest::InjectMockFault);
                        }
                    }
                    let listener_status = if self.lvds.connected {
                        format!(
                            "监听 {}:{} · {}",
                            self.lvds.bind_address, self.lvds.port, self.lvds.detail
                        )
                    } else {
                        format!("未监听 · {}:{}", self.lvds.bind_address, self.lvds.port)
                    };
                    ui.add_space(4.0);
                    ui.add_sized(
                        [ui.available_width().max(0.0), row_two_height],
                        egui::Label::new(
                            RichText::new(listener_status.clone())
                                .small()
                                .color(theme::TEXT_SECONDARY),
                        )
                        .truncate(),
                    )
                    .on_hover_text(listener_status);
                },
            );

            ui.allocate_ui_with_layout(
                egui::vec2(row_width, 22.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    let protocol =
                        "UDP 24B + LVDS 269B · 源端 192.168.1.128:1534 · 详细信息悬停查看";
                    ui.add_sized(
                        [row_width, 22.0],
                        egui::Label::new(
                            RichText::new(protocol).size(9.0).color(theme::TEXT_MUTED),
                        )
                        .truncate(),
                    )
                    .on_hover_text("网口协议：24B UDP 封装 + 269B LVDS；源端 192.168.1.128:1534");
                },
            );
        });
        if let Some(request) = request {
            self.submit(request);
        }
    }

    fn log_panel(&self, ui: &mut Ui) {
        let compact = ui.available_width() < 420.0;
        ui.horizontal(|ui| {
            theme::section_title(ui, "系统日志");
            if !compact {
                ui.label(
                    RichText::new("自动跟随最新消息；向上滚动可查看历史")
                        .small()
                        .color(theme::TEXT_MUTED),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{}/{} 条", self.logs.len(), MAX_LOGS))
                        .small()
                        .color(theme::TEXT_MUTED),
                );
            });
        });
        egui::ScrollArea::vertical()
            .id_salt("system_log_scroll")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .max_height(ui.available_height().max(0.0))
            .show(ui, |ui| {
                for entry in &self.logs {
                    let color = match entry.level {
                        LogLevel::Info => theme::TEXT,
                        LogLevel::Warning => theme::WARNING,
                        LogLevel::Error => theme::DANGER,
                    };
                    let full_text = format!("[{}] {}", entry.time, entry.message);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!("[{}]", entry.time))
                                .monospace()
                                .size(10.0)
                                .color(color),
                        );
                        ui.add(
                            egui::Label::new(RichText::new(&entry.message).size(10.0).color(color))
                                .wrap(),
                        )
                        .on_hover_text(full_text);
                    });
                }
            });
    }

    fn header_title(ui: &mut Ui) {
        ui.allocate_ui_with_layout(
            egui::vec2(HEADER_TITLE_WIDTH, HEADER_TITLE_HEIGHT),
            Layout::left_to_right(Align::Center),
            |ui| {
                debug_ui_rect("header.title", ui.available_rect_before_wrap());
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("航天设备集成控制中心")
                            .strong()
                            .size(16.0)
                            .color(theme::TEXT),
                    );
                    ui.label(
                        RichText::new("Langmuir Probe Ground Control")
                            .size(10.0)
                            .color(theme::TEXT_MUTED),
                    );
                });
            },
        );
    }

    fn header_device_group(&self, ui: &mut Ui) {
        ui.allocate_ui_with_layout(
            egui::vec2(HEADER_DEVICE_GROUP_WIDTH, theme::CONTROL_HEIGHT),
            Layout::left_to_right(Align::Center),
            |ui| {
                debug_ui_rect("header.device_group", ui.available_rect_before_wrap());
                header_link_status(
                    ui,
                    if self.acquisition_running {
                        "采集 · 运行中"
                    } else {
                        "采集 · 已停止"
                    },
                    self.acquisition_running,
                    self.scientific_data_is_fresh(),
                );
                header_link_status(
                    ui,
                    if self.lvds.connected {
                        "LVDS · 已连接"
                    } else {
                        "LVDS · 未连接"
                    },
                    self.lvds.connected,
                    self.scientific_data_is_fresh(),
                );
                header_link_status(
                    ui,
                    if self.rs422.connected {
                        "RS422 · 已连接"
                    } else {
                        "RS422 · 未连接"
                    },
                    self.rs422.connected,
                    self.telemetry_is_fresh(),
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(82.0, theme::CONTROL_HEIGHT),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        if self.operator.frozen_plot.is_some() {
                            theme::status_badge(ui, "画面冻结", theme::PRIMARY);
                        }
                    },
                );
            },
        );
    }

    fn header_controls(&mut self, ui: &mut Ui) {
        let remaining_width = ui.available_width() - HEADER_CONTROLS_WIDTH;
        if remaining_width > 0.0 {
            ui.allocate_space(egui::vec2(remaining_width, 0.0));
        }
        let mut emergency_clicked = false;
        ui.allocate_ui_with_layout(
            egui::vec2(HEADER_CONTROLS_WIDTH, theme::CONTROL_HEIGHT),
            Layout::right_to_left(Align::Center),
            |ui| {
                debug_ui_rect("header.controls", ui.available_rect_before_wrap());
                if ui
                    .add_sized(
                        [HEADER_EMERGENCY_WIDTH, theme::CONTROL_HEIGHT],
                        egui::Button::new(RichText::new("软件急停").strong().color(theme::WHITE))
                            .fill(theme::DANGER),
                    )
                    .clicked_by(PointerButton::Primary)
                {
                    emergency_clicked = true;
                }
                ui_scale_control(ui);
                ui.allocate_ui_with_layout(
                    egui::vec2(HEADER_FONT_WARNING_WIDTH, theme::CONTROL_HEIGHT),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        if let Some(warning) = self.font_warning.as_deref() {
                            ui.colored_label(theme::WARNING, "CJK FONT MISSING")
                                .on_hover_text(warning);
                        }
                    },
                );
                #[cfg(feature = "mock")]
                ui.allocate_ui_with_layout(
                    egui::vec2(HEADER_MOCK_WIDTH, theme::CONTROL_HEIGHT),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        if self.mock_mode {
                            theme::status_badge(ui, "MOCK", theme::WARNING);
                        }
                    },
                );
                let polling_label = if self.telemetry_polling.active {
                    format!("遥测轮询 · {} ms", self.telemetry_polling.interval_millis)
                } else {
                    "遥测轮询 · 停止".to_owned()
                };
                ui.allocate_ui_with_layout(
                    egui::vec2(HEADER_POLLING_WIDTH, theme::CONTROL_HEIGHT),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        theme::status_badge(
                            ui,
                            &polling_label,
                            if self.telemetry_polling.active {
                                theme::PRIMARY
                            } else {
                                theme::OFFLINE
                            },
                        );
                    },
                );
            },
        );
        if emergency_clicked {
            self.operator.emergency_stop_confirmation_open = true;
        }
    }

    fn header_content(&mut self, ui: &mut Ui, compact: bool) {
        if compact {
            ui.vertical(|ui| {
                let row_width = ui.available_width();
                ui.allocate_ui_with_layout(
                    egui::vec2(row_width, 40.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        Self::header_title(ui);
                        self.header_device_group(ui);
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(row_width, 40.0),
                    Layout::left_to_right(Align::Center),
                    |ui| self.header_controls(ui),
                );
            });
        } else {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                Self::header_title(ui);
                self.header_device_group(ui);
                self.header_controls(ui);
            });
        }
    }
}

impl eframe::App for LangmuirDesktopApp {
    #[expect(
        clippy::too_many_lines,
        reason = "the desktop shell keeps the fixed industrial console regions together"
    )]
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.drain_backend();
        let compact_header = ui.available_width() < HEADER_COMPACT_BREAKPOINT;

        egui::Panel::top("application_header")
            .resizable(false)
            .exact_size(if compact_header {
                HEADER_COMPACT_HEIGHT
            } else {
                HEADER_HEIGHT
            })
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(14, 5)),
            )
            .show(ui, |ui| {
                debug_ui_rect("top_status", ui.available_rect_before_wrap());
                self.header_content(ui, compact_header);
            });

        // The compact mode changes only at a window-width threshold, never in
        // response to telemetry content. This keeps the three docked regions
        // stable during a run while leaving a usable central strip at 800px.
        let compact_layout = ui.available_width() < COMPACT_LAYOUT_BREAKPOINT;
        egui::Panel::left("command_sidebar")
            .resizable(false)
            .exact_size(fixed_panel_size(
                if compact_layout {
                    COMPACT_COMMAND_SIDEBAR_WIDTH
                } else {
                    COMMAND_SIDEBAR_WIDTH
                },
                COMMAND_SIDEBAR_MIN_WIDTH,
                COMMAND_SIDEBAR_MAX_WIDTH,
            ))
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::same(8)),
            )
            .show(ui, |ui| {
                debug_ui_rect("left_sidebar", ui.available_rect_before_wrap());
                ScrollArea::vertical()
                    .id_salt("command_sidebar_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        if let Some(request) =
                            Self::connection_card(ui, LinkKind::Rs422, &mut self.rs422)
                        {
                            self.submit(request);
                        }
                        ui.add_space(6.0);
                        self.command_panel(ui);
                    });
            });

        egui::Panel::right("telemetry_sidebar")
            .resizable(false)
            .exact_size(fixed_panel_size(
                if compact_layout {
                    COMPACT_TELEMETRY_SIDEBAR_WIDTH
                } else {
                    TELEMETRY_SIDEBAR_WIDTH
                },
                TELEMETRY_SIDEBAR_MIN_WIDTH,
                TELEMETRY_SIDEBAR_MAX_WIDTH,
            ))
            .frame(
                egui::Frame::new()
                    .fill(theme::PAGE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::same(8)),
            )
            .show(ui, |ui| {
                debug_ui_rect("right_sidebar", ui.available_rect_before_wrap());
                ScrollArea::vertical()
                    .id_salt("telemetry_sidebar_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        self.telemetry_panel(ui);
                        ui.add_space(6.0);
                        self.fault_panel(ui);
                    });
            });

        egui::CentralPanel::default().show(ui, |ui| {
            let regions = central_layout_rects(ui.available_rect_before_wrap());
            debug_ui_rect("central_data", regions.root);
            debug_ui_rect("central.operations", regions.operations);
            debug_ui_rect("central.plot_toolbar", regions.plot_toolbar);
            debug_ui_rect("central.plot_body", regions.plot_body);
            debug_ui_rect("central.logs", regions.logs);

            ui.scope_builder(egui::UiBuilder::new().max_rect(regions.operations), |ui| {
                self.operations_panel(ui)
            });
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(regions.plot_toolbar),
                |ui| {
                    full_width_subtle_card(ui, |ui| self.plot_toolbar(ui));
                },
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(regions.plot_body), |ui| {
                full_width_card(ui, |ui| self.plot_panel(ui));
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(regions.logs), |ui| {
                full_width_card(ui, |ui| self.log_panel(ui));
            });
        });

        self.emergency_stop_dialog(ui.ctx());

        ui.ctx().request_repaint_after(
            if self.acquisition_running || self.scientific_data_is_fresh() {
                // Scientific samples are time-sensitive; keep the plot responsive.
                Duration::from_millis(50)
            } else {
                // Telemetry is event-driven and does not need a high-rate repaint loop.
                Duration::from_millis(250)
            },
        );
    }
}

fn ui_rect_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os(UI_RECT_DEBUG_ENV).is_some())
}

/// Logs a region once and again only when its geometry moves by more than one
/// logical point. This makes a 60-second Mock run useful for detecting layout
/// jumps without flooding the log on every repaint.
fn debug_ui_rect(label: &'static str, rect: egui::Rect) {
    if !ui_rect_debug_enabled() {
        return;
    }
    static LAST: OnceLock<Mutex<HashMap<&'static str, [f32; 4]>>> = OnceLock::new();
    let snapshot = [rect.min.x, rect.min.y, rect.width(), rect.height()];
    let last = LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut last) = last.lock() else {
        return;
    };
    let changed = last.get(label).is_none_or(|previous| {
        previous
            .iter()
            .zip(snapshot)
            .any(|(old, new)| (old - new).abs() > 1.0)
    });
    if changed {
        eprintln!(
            "UI_RECT {label}: x={:.1} y={:.1} w={:.1} h={:.1}",
            snapshot[0], snapshot[1], snapshot[2], snapshot[3]
        );
        last.insert(label, snapshot);
    }
}

fn adc_counts_to_millivolts(previous_sample: f64) -> f64 {
    (previous_sample - ADC_ZERO_COUNTS) / ADC_COUNTS_PER_MILLIVOLT
}

fn status_dot(ui: &mut Ui, active: bool) {
    let color = if active {
        theme::SUCCESS
    } else {
        theme::OFFLINE
    };
    ui.colored_label(color, "●");
}

/// Render a card at the full width assigned by its parent panel.
///
/// `Frame::show` sizes the outer frame from the child UI's minimum rect.  A
/// short label can therefore make a card shrink to its contents, which is
/// especially visible in the telemetry sidebar.  Expanding the child UI
/// before drawing its contents keeps every card aligned to the panel's
/// content rect while preserving the panel's own padding.
fn full_width_card<R>(
    ui: &mut Ui,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    theme::card().show(ui, |ui| {
        ui.take_available_width();
        add_contents(ui)
    })
}

fn fixed_panel_size(default: f32, minimum: f32, maximum: f32) -> f32 {
    default.clamp(minimum, maximum)
}

#[derive(Debug, Clone, Copy)]
struct CentralLayoutRects {
    root: egui::Rect,
    operations: egui::Rect,
    plot_toolbar: egui::Rect,
    plot_body: egui::Rect,
    logs: egui::Rect,
}

/// Split the central panel without relying on child content's natural height.
///
/// The operations card, plot toolbar, and log card retain stable heights on a
/// normal window.  If the available height is exceptionally small, those
/// fixed portions are scaled together so every returned rectangle still fits
/// inside `root`; the plot body is the only elastic region in the normal case.
fn central_layout_rects(root: egui::Rect) -> CentralLayoutRects {
    // Three gaps are present between A, toolbar, plot body, and C.  Scale the
    // gap itself for pathological tiny roots so the returned rectangles never
    // extend outside the central panel.
    let gap = CENTRAL_GAP.min(root.height().max(0.0) / 3.0);
    let available = (root.height() - gap * 3.0).max(0.0);
    let normal_fixed = CENTRAL_OPERATIONS_HEIGHT + CENTRAL_PLOT_TOOLBAR_HEIGHT + CENTRAL_LOG_HEIGHT;
    let scale = (available / normal_fixed).min(1.0);
    let operations_height = CENTRAL_OPERATIONS_HEIGHT * scale;
    let toolbar_height = CENTRAL_PLOT_TOOLBAR_HEIGHT * scale;
    let logs_height = CENTRAL_LOG_HEIGHT * scale;
    let plot_height = (available - operations_height - toolbar_height - logs_height).max(0.0);

    let x = root.min.x;
    let width = root.width().max(0.0);
    let operations = egui::Rect::from_min_size(
        egui::pos2(x, root.min.y),
        egui::vec2(width, operations_height),
    );
    let toolbar_y = operations.max.y + gap;
    let plot_toolbar =
        egui::Rect::from_min_size(egui::pos2(x, toolbar_y), egui::vec2(width, toolbar_height));
    let plot_y = plot_toolbar.max.y + gap;
    let plot_body =
        egui::Rect::from_min_size(egui::pos2(x, plot_y), egui::vec2(width, plot_height));
    let logs_y = plot_body.max.y + gap;
    let logs = egui::Rect::from_min_size(egui::pos2(x, logs_y), egui::vec2(width, logs_height));
    CentralLayoutRects {
        root,
        operations,
        plot_toolbar,
        plot_body,
        logs,
    }
}

fn full_width_subtle_card<R>(
    ui: &mut Ui,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    theme::subtle_card().show(ui, |ui| {
        ui.take_available_width();
        add_contents(ui)
    })
}

fn header_link_status(ui: &mut Ui, label: &str, active: bool, fresh: bool) {
    let color = if !active {
        theme::OFFLINE
    } else if fresh {
        theme::SUCCESS
    } else {
        theme::WARNING
    };
    ui.allocate_ui_with_layout(
        egui::vec2(112.0, theme::CONTROL_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            heartbeat_indicator(ui, active && fresh);
            ui.add(
                egui::Label::new(RichText::new(label).strong().size(11.0).color(color)).truncate(),
            );
        },
    );
}

fn ui_scale_control(ui: &mut Ui) {
    let current_scale = ui.ctx().zoom_factor();
    let mut selected_scale = nearest_ui_scale(current_scale);
    ui.allocate_ui_with_layout(
        egui::vec2(HEADER_SCALE_WIDTH, theme::CONTROL_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.label(RichText::new("界面").small().color(theme::TEXT_SECONDARY));
            ComboBox::from_id_salt("ui_scale")
                .selected_text(format!("{:.0}%", selected_scale * 100.0))
                .width(64.0)
                .show_ui(ui, |ui| {
                    for scale in UI_SCALE_OPTIONS {
                        ui.selectable_value(
                            &mut selected_scale,
                            scale,
                            format!("{:.0}%", scale * 100.0),
                        );
                    }
                });
        },
    );
    if (selected_scale - current_scale).abs() > f32::EPSILON {
        // egui's zoom factor is relative to native_pixels_per_point, so the
        // operating-system Retina/HiDPI scale is preserved instead of doubled.
        ui.ctx().set_zoom_factor(selected_scale);
    }
}

fn nearest_ui_scale(value: f32) -> f32 {
    let mut nearest = UI_SCALE_OPTIONS[0];
    let mut nearest_distance = (value - nearest).abs();
    for scale in UI_SCALE_OPTIONS.into_iter().skip(1) {
        let distance = (value - scale).abs();
        if distance < nearest_distance {
            nearest = scale;
            nearest_distance = distance;
        }
    }
    nearest
}

fn telemetry_detail_row(ui: &mut Ui, label: &str, value: String, fresh: bool) {
    let row_width = ui.available_width().max(MONITOR_LABEL_WIDTH + 42.0);
    ui.allocate_ui_with_layout(
        egui::vec2(row_width, theme::CONTROL_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_sized(
                [MONITOR_LABEL_WIDTH, theme::CONTROL_HEIGHT],
                egui::Label::new(RichText::new(label).small().color(theme::TEXT_SECONDARY)),
            );
            heartbeat_indicator(ui, fresh);
            let value_width = ui.available_width().max(0.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_sized(
                    [value_width, theme::CONTROL_HEIGHT],
                    egui::Label::new(
                        RichText::new(&value)
                            .monospace()
                            .size(10.0)
                            .strong()
                            .color(theme::PRIMARY),
                    )
                    .truncate(),
                )
                .on_hover_text(value);
            });
        },
    );
    ui.add_space(2.0);
}

fn protocol_status_row(ui: &mut Ui, label: &str, status: u8, fresh: bool) {
    let value = format!("0x{status:02X}");
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), theme::CONTROL_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_sized(
                [MONITOR_LABEL_WIDTH, theme::CONTROL_HEIGHT],
                egui::Label::new(RichText::new(label).small().color(theme::TEXT_SECONDARY)),
            );
            heartbeat_indicator(ui, fresh);
            theme::status_badge(ui, &value, theme::PRIMARY);
        },
    );
    ui.add_space(2.0);
}

fn frequency_parameter(ui: &mut Ui, value: &mut u8, step: &mut u8) {
    let parent_rect = ui.available_rect_before_wrap();
    debug_ui_rect("left.scan_frequency_parameter", parent_rect);
    let slider_width = frequency_slider_width(parent_rect.width());

    ui.allocate_ui_with_layout(
        egui::vec2(parent_rect.width().max(0.0), theme::CONTROL_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.label(
                RichText::new("扫描频率设定值")
                    .small()
                    .color(theme::TEXT_SECONDARY),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("当前值 {value}"))
                        .monospace()
                        .strong()
                        .color(theme::PRIMARY),
                );
            });
        },
    );

    let slider_response = ui
        .scope(|ui| {
            let visuals = ui.visuals_mut();
            visuals.selection.bg_fill = theme::PRIMARY;
            visuals.widgets.noninteractive.bg_fill = theme::SLIDER_TRACK;
            visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.5, theme::PRIMARY_DARK);
            visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.5, theme::WHITE);
            visuals.widgets.inactive.bg_fill = theme::SLIDER_TRACK;
            visuals.widgets.inactive.bg_stroke = Stroke::new(1.5, theme::PRIMARY_DARK);
            visuals.widgets.inactive.fg_stroke = Stroke::new(2.0, theme::WHITE);
            visuals.widgets.hovered.bg_fill = theme::PRIMARY_SOFT;
            visuals.widgets.hovered.bg_stroke = Stroke::new(2.0, theme::PRIMARY);
            visuals.widgets.hovered.fg_stroke = Stroke::new(2.0, theme::WHITE);
            visuals.widgets.active.bg_fill = theme::PRIMARY;
            visuals.widgets.active.fg_stroke = Stroke::new(2.0, theme::WHITE);
            ui.spacing_mut().slider_width = slider_width;
            ui.spacing_mut().slider_rail_height = 6.0;
            ui.add_sized(
                [slider_width, theme::CONTROL_HEIGHT],
                egui::Slider::new(value, ScanFrequency::MIN..=ScanFrequency::MAX)
                    .show_value(false)
                    .step_by(f64::from(*step))
                    .trailing_fill(true),
            )
            .on_hover_text("扫描频率：0..=100；可拖动、键盘调整或精确输入")
        })
        .inner;
    debug_ui_rect("left.scan_frequency_slider", slider_response.rect);
    debug_slider_geometry(
        "left.scan_frequency_slider",
        parent_rect,
        slider_response.rect,
        slider_width,
        true,
    );

    ui.allocate_ui_with_layout(
        egui::vec2(slider_width, 20.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.label(RichText::new("最小 0").small().color(theme::TEXT_MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("最大 {}", ScanFrequency::MAX))
                        .small()
                        .color(theme::TEXT_MUTED),
                );
            });
        },
    );

    ui.allocate_ui_with_layout(
        egui::vec2(parent_rect.width().max(0.0), theme::CONTROL_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.label(
                RichText::new("快速设置")
                    .small()
                    .color(theme::TEXT_SECONDARY),
            );
            for preset in [1, 50, ScanFrequency::MAX] {
                if ui
                    .selectable_label(*value == preset, format!("{preset}"))
                    .clicked()
                {
                    *value = preset;
                }
            }
            ui.separator();
            ui.label(
                RichText::new("精确输入")
                    .small()
                    .color(theme::TEXT_SECONDARY),
            );
            ui.add_sized(
                [64.0, theme::CONTROL_HEIGHT],
                egui::DragValue::new(value)
                    .range(ScanFrequency::MIN..=ScanFrequency::MAX)
                    .speed(f64::from(*step)),
            );
        },
    );
}

fn frequency_slider_width(available_width: f32) -> f32 {
    // Use the complete parent width.  Unlike the polling control, frequency
    // editing must not force a minimum that could overflow a narrow command
    // card; 180pt is the normal target whenever the parent can provide it.
    available_width.max(1.0)
}

fn debug_slider_geometry(
    label: &'static str,
    parent_rect: egui::Rect,
    response_rect: egui::Rect,
    configured_width: f32,
    enabled: bool,
) {
    if !ui_rect_debug_enabled() {
        return;
    }
    eprintln!(
        "UI_SLIDER {label}: parent_w={:.1} configured_w={:.1} response_w={:.1} response_h={:.1} rail_h=6.0 enabled={enabled}",
        parent_rect.width(),
        configured_width,
        response_rect.width(),
        response_rect.height(),
    );
}

fn integer_parameter_u16(ui: &mut Ui, value: &mut u16, step: &mut u16, presets: &[u16]) {
    let slider_width = ui.available_width().max(140.0);
    ui.scope(|ui| {
        let visuals = ui.visuals_mut();
        visuals.selection.bg_fill = theme::PRIMARY;
        visuals.widgets.inactive.bg_fill = theme::SURFACE_ALT;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.5, theme::PRIMARY);
        visuals.widgets.hovered.bg_fill = theme::PRIMARY_SOFT;
        visuals.widgets.hovered.bg_stroke = Stroke::new(2.0, theme::PRIMARY);
        visuals.widgets.active.bg_fill = theme::PRIMARY;
        visuals.widgets.active.fg_stroke = Stroke::new(1.5, theme::WHITE);
        ui.add_sized(
            [slider_width, theme::CONTROL_HEIGHT],
            egui::Slider::new(value, 0..=u16::MAX)
                .show_value(false)
                .step_by(f64::from(*step)),
        )
        .on_hover_text("原始电压设定值：0x0000..=0xFFFF");
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("0x0000").small().color(theme::TEXT_MUTED));
        ui.add_sized(
            [78.0, theme::CONTROL_HEIGHT],
            egui::DragValue::new(value)
                .range(0..=u16::MAX)
                .speed(f64::from(*step)),
        );
        ui.label(
            RichText::new(format!("当前 {value} / 最大 {}", u16::MAX))
                .small()
                .color(theme::TEXT_SECONDARY),
        );
        for preset in presets {
            if ui
                .selectable_label(*value == *preset, format!("{preset}"))
                .clicked()
            {
                *value = *preset;
            }
        }
    });
}

fn work_mode_name(mode: u8) -> String {
    match mode {
        0x55 => "三角波 (0x55)".to_owned(),
        0xAA => "正弦波 (0xAA)".to_owned(),
        0xBB => "直流 DC (0xBB)".to_owned(),
        _ => format!("未知 (0x{mode:02X})"),
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn temperature_bar(ui: &mut Ui, label: &str, temperature: u8, fresh: bool) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .size(10.0)
                .strong()
                .color(theme::TEXT_SECONDARY),
        );
        heartbeat_indicator(ui, fresh);
        ui.label(
            RichText::new(format!("{temperature} ℃"))
                .monospace()
                .strong()
                .color(theme::PRIMARY),
        );
    });
    let fraction = (f32::from(temperature) / 100.0).clamp(0.0, 1.0);
    let color = threshold_color(fraction);
    ui.add(
        egui::ProgressBar::new(fraction)
            .desired_width(ui.available_width())
            .fill(color)
            .text(format!("{temperature}%")),
    );
    ui.label(
        RichText::new("阈值：80% 警告 · 90% 严重")
            .small()
            .color(theme::TEXT_MUTED),
    );
}

fn heartbeat_indicator(ui: &mut Ui, fresh: bool) {
    // A status indicator must communicate state without causing a repaint loop.
    // The previous time-based pulse made every telemetry update look like a UI
    // flicker. Alarm emphasis remains handled separately by the alarm panel.
    let color = if fresh {
        theme::SUCCESS
    } else {
        theme::WARNING
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(10.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
    response.on_hover_text(if fresh {
        "实时数据正在更新"
    } else {
        "数据更新已超时"
    });
}

fn flashing_alarm_color(ui: &Ui, state: AlarmState) -> Color32 {
    let phase = ui.ctx().input(|input| input.time).fract();
    if state == AlarmState::Unacknowledged && phase >= 0.5 {
        theme::DANGER.gamma_multiply(0.45)
    } else {
        state.color()
    }
}

fn threshold_color(fraction: f32) -> Color32 {
    if fraction <= 0.8 {
        blend_color(theme::SUCCESS, theme::WARNING, fraction / 0.8)
    } else {
        blend_color(theme::WARNING, theme::DANGER, (fraction - 0.8) / 0.2)
    }
}

fn blend_color(start: Color32, end: Color32, amount: f32) -> Color32 {
    let start = egui::Rgba::from(start);
    let end = egui::Rgba::from(end);
    egui::lerp(start..=end, amount.clamp(0.0, 1.0)).into()
}

fn command_counter(ui: &mut Ui, title: &str, value: u8, color: Color32, background: Color32) {
    egui::Frame::new()
        .fill(background)
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.35)))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::same(7))
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(title)
                        .small()
                        .strong()
                        .color(theme::TEXT_SECONDARY),
                );
                ui.label(
                    RichText::new(value.to_string())
                        .monospace()
                        .strong()
                        .size(20.0)
                        .color(color),
                );
            });
        });
}

fn update_plot_cursor_from_pointer(plot_ui: &egui_plot::PlotUi<'_>, cursor: &mut Option<f64>) {
    let response = plot_ui.response();
    if (response.clicked_by(PointerButton::Primary) || response.dragged_by(PointerButton::Primary))
        && let Some(pointer) = plot_ui.pointer_coordinate()
    {
        *cursor = Some(pointer.x);
    }
}

fn plot_cursor_readout(
    ui: &mut Ui,
    cursor_seconds: f64,
    channel_1: &[[f64; 2]],
    channel_2: &[[f64; 2]],
) {
    let channel_1_value = closest_plot_value(channel_1, cursor_seconds);
    let channel_2_value = closest_plot_value(channel_2, cursor_seconds);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "游标 {}",
                format_plot_timestamp(cursor_seconds, 0.0)
            ))
            .small()
            .color(theme::WARNING),
        );
        if let Some(value) = channel_1_value {
            ui.label(
                RichText::new(format!("通道 1 {value:.2} mV"))
                    .monospace()
                    .small()
                    .color(theme::PRIMARY),
            );
        }
        if let Some(value) = channel_2_value {
            ui.label(
                RichText::new(format!("通道 2 {value:.2} mV"))
                    .monospace()
                    .small()
                    .color(theme::DANGER),
            );
        }
    });
}

fn closest_plot_value(points: &[[f64; 2]], cursor_seconds: f64) -> Option<f64> {
    let mut closest = None;
    let mut distance = f64::INFINITY;
    for point in points {
        let candidate_distance = (point[0] - cursor_seconds).abs();
        if candidate_distance < distance {
            distance = candidate_distance;
            closest = Some(point[1]);
        }
    }
    closest
}

fn normalize_plot_points(points: &mut [[f64; 2]]) -> Option<(f64, f64)> {
    let mut values = points.iter().map(|point| point[1]);
    let first = values.next()?;
    let (min, max) = values.fold((first, first), |(min, max), value| {
        (min.min(value), max.max(value))
    });
    let span = max - min;
    for point in points {
        point[1] = if span > f64::EPSILON {
            (point[1] - min) / span
        } else {
            0.5
        };
    }
    Some((min, max))
}

fn overlay_channel_name(name: &str, range: Option<(f64, f64)>) -> String {
    match range {
        Some((min, max)) => format!("{name} [{min:.0}, {max:.0}]"),
        None => name.to_owned(),
    }
}

fn hex_frame(frame: &[u8; 12]) -> String {
    frame
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn command_bytes(command: Command) -> [u8; 12] {
    command.encode().into_bytes()
}

fn recalculate_checksum(frame: &mut [u8; 12]) {
    CommandFrame::recalculate_checksum(frame);
}

fn command_checksum(frame: &[u8; 12]) -> u8 {
    core_command_checksum(frame)
}

fn telemetry_preset_active(interval_millis: u32, preset_millis: u32) -> bool {
    interval_millis == preset_millis
}

fn polling_slider_width(available_width: f32) -> f32 {
    available_width.max(POLLING_SLIDER_MIN_WIDTH)
}

/// Validates a manually edited command frame before it can reach the serial
/// backend. Typed commands are generated by `langmuir_core`; this stricter
/// check protects the expert raw editor from sending malformed frames.
fn raw_frame_validation(frame: &[u8; 12]) -> Result<(), String> {
    validate_command_frame(frame).map_err(|error| error.to_string())
}

fn format_timestamp(seconds: u32) -> String {
    DateTime::<Utc>::from_timestamp(i64::from(seconds), 0).map_or_else(
        || seconds.to_string(),
        |date_time| {
            date_time
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        },
    )
}

fn format_plot_timestamp(seconds: f64, visible_seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return String::new();
    }
    let Ok(duration) = Duration::try_from_secs_f64(seconds) else {
        return format!("{seconds:.3}");
    };
    let Ok(whole_seconds) = i64::try_from(duration.as_secs()) else {
        return format!("{seconds:.3}");
    };
    let nanoseconds = duration.subsec_nanos();
    DateTime::<Utc>::from_timestamp(whole_seconds, nanoseconds).map_or_else(
        || format!("{seconds:.3}"),
        |date_time| {
            let local = date_time.with_timezone(&Local);
            if visible_seconds < 1.0 {
                local.format("%H:%M:%S%.3f").to_string()
            } else if visible_seconds < 3_600.0 {
                local.format("%H:%M:%S").to_string()
            } else {
                local.format("%m-%d %H:%M").to_string()
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use eframe::egui::{Rect, pos2, vec2};
    use langmuir_core::Command;

    use super::{
        CENTRAL_LOG_HEIGHT, CENTRAL_OPERATIONS_HEIGHT, CENTRAL_PLOT_TOOLBAR_HEIGHT,
        COMMAND_SIDEBAR_MAX_WIDTH, COMMAND_SIDEBAR_MIN_WIDTH, COMMAND_SIDEBAR_WIDTH,
        COMPACT_COMMAND_SIDEBAR_WIDTH, COMPACT_LAYOUT_BREAKPOINT, COMPACT_TELEMETRY_SIDEBAR_WIDTH,
        HEADER_COMPACT_BREAKPOINT, HEADER_COMPACT_HEIGHT, HEADER_HEIGHT,
        TELEMETRY_SIDEBAR_MAX_WIDTH, TELEMETRY_SIDEBAR_MIN_WIDTH, TELEMETRY_SIDEBAR_WIDTH,
        adc_counts_to_millivolts, central_layout_rects, command_bytes, command_checksum,
        fixed_panel_size, frequency_slider_width, hex_bytes, nearest_ui_scale,
        normalize_plot_points, polling_slider_width, raw_frame_validation, recalculate_checksum,
        telemetry_preset_active, work_mode_name,
    };

    #[test]
    fn entering_raw_mode_starts_from_the_typed_command() {
        let expected = Command::ConfigureDc(0x1234).encode().into_bytes();
        assert_eq!(command_bytes(Command::ConfigureDc(0x1234)), expected);
    }

    #[test]
    fn raw_checksum_recalculation_only_updates_sum_byte() {
        let mut frame = command_bytes(Command::RequestTelemetry);
        frame[5] = 0xFE;
        let before = frame;
        recalculate_checksum(&mut frame);
        assert_eq!(frame[11], command_checksum(&frame));
        assert_eq!(&frame[..11], &before[..11]);
    }

    #[test]
    fn raw_validation_rejects_bad_sum_and_accepts_recalculated_frame() {
        let mut frame = command_bytes(Command::RequestTelemetry);
        frame[11] ^= 0x01;
        let error = raw_frame_validation(&frame).expect_err("bad SUM must be rejected");
        assert!(error.contains("SUM mismatch"));
        assert_ne!(frame[11], command_checksum(&frame));
        recalculate_checksum(&mut frame);
        assert_eq!(raw_frame_validation(&frame), Ok(()));
    }

    #[test]
    fn raw_validation_checks_frequency_range_and_protocol_fields() {
        let mut frame = command_bytes(Command::configure_triangle(50).unwrap());
        frame[8] = 0x65;
        recalculate_checksum(&mut frame);
        let error = raw_frame_validation(&frame).expect_err("out-of-range frequency must fail");
        assert!(error.contains("0x00..=0x64"));
        frame = command_bytes(Command::SetLvdsEnabled(false));
        frame[7] = 0x55;
        recalculate_checksum(&mut frame);
        assert_eq!(raw_frame_validation(&frame), Ok(()));
    }

    #[test]
    fn telemetry_preset_highlight_tracks_the_actual_interval() {
        assert!(telemetry_preset_active(1_000, 1_000));
        assert!(!telemetry_preset_active(1_100, 1_000));
        assert!(!telemetry_preset_active(1_000, 5_000));
        assert!(telemetry_preset_active(60_000, 60_000));
    }

    #[test]
    fn polling_slider_has_a_visible_minimum_track_width() {
        assert_eq!(polling_slider_width(120.0), 180.0);
        assert_eq!(polling_slider_width(284.0), 284.0);
    }

    #[test]
    fn frequency_slider_uses_the_parent_width_without_forcing_overflow() {
        assert_eq!(frequency_slider_width(284.0), 284.0);
        assert_eq!(frequency_slider_width(180.0), 180.0);
        assert_eq!(frequency_slider_width(120.0), 120.0);
        assert_eq!(frequency_slider_width(0.0), 1.0);
    }

    #[test]
    fn work_modes_use_protocol_values() {
        assert_eq!(work_mode_name(0x55), "三角波 (0x55)");
        assert_eq!(work_mode_name(0xAA), "正弦波 (0xAA)");
        assert_eq!(work_mode_name(0xBB), "直流 DC (0xBB)");
        assert_eq!(work_mode_name(0), "未知 (0x00)");
    }

    #[test]
    fn injected_command_is_rendered_as_hex_bytes() {
        assert_eq!(hex_bytes(&[0x21, 0x55, 0x64, 0x9B, 0x9B]), "21 55 64 9B 9B");
    }

    #[test]
    fn overlay_normalization_keeps_each_channel_in_its_own_scale() {
        let mut points = [[0.0, 100.0], [1.0, 200.0], [2.0, 150.0]];
        assert_eq!(normalize_plot_points(&mut points), Some((100.0, 200.0)));
        assert_eq!(points, [[0.0, 0.0], [1.0, 1.0], [2.0, 0.5]]);
    }

    #[test]
    fn plot_vertical_axis_uses_the_adc_to_millivolt_formula() {
        assert!(adc_counts_to_millivolts(32_768.0).abs() < 1.0e-12);
        assert!((adc_counts_to_millivolts(32_774.4) - 1.0).abs() < 1.0e-12);
        assert!((adc_counts_to_millivolts(0.0) + 5_120.0).abs() < 1.0e-12);
    }

    #[test]
    fn ui_scale_selection_stays_within_the_supported_range() {
        assert_eq!(nearest_ui_scale(0.8), 0.8);
        assert_eq!(nearest_ui_scale(1.0), 1.0);
        assert_eq!(nearest_ui_scale(1.5), 1.5);
        assert_eq!(nearest_ui_scale(2.0), 1.5);
    }

    #[test]
    fn responsive_regions_have_safe_size_ordering() {
        assert!(COMMAND_SIDEBAR_MIN_WIDTH < COMMAND_SIDEBAR_WIDTH);
        assert!(COMMAND_SIDEBAR_WIDTH < COMMAND_SIDEBAR_MAX_WIDTH);
        assert!(TELEMETRY_SIDEBAR_MIN_WIDTH < TELEMETRY_SIDEBAR_WIDTH);
        assert!(TELEMETRY_SIDEBAR_WIDTH < TELEMETRY_SIDEBAR_MAX_WIDTH);
        assert!(CENTRAL_PLOT_TOOLBAR_HEIGHT > 0.0);
        assert!(CENTRAL_OPERATIONS_HEIGHT > CENTRAL_PLOT_TOOLBAR_HEIGHT);
        assert!(CENTRAL_LOG_HEIGHT > CENTRAL_PLOT_TOOLBAR_HEIGHT);
        assert!(HEADER_HEIGHT < HEADER_COMPACT_HEIGHT);
        assert!(HEADER_COMPACT_BREAKPOINT > 1_024.0);
    }

    #[test]
    fn compact_layout_keeps_a_usable_central_region() {
        let compact_total = COMPACT_COMMAND_SIDEBAR_WIDTH + COMPACT_TELEMETRY_SIDEBAR_WIDTH;
        let normal_total = COMMAND_SIDEBAR_WIDTH + TELEMETRY_SIDEBAR_WIDTH;
        assert!(compact_total < COMPACT_LAYOUT_BREAKPOINT);
        assert!(normal_total < 1_360.0);
        assert_eq!(
            fixed_panel_size(
                COMPACT_COMMAND_SIDEBAR_WIDTH,
                COMMAND_SIDEBAR_MIN_WIDTH,
                COMMAND_SIDEBAR_MAX_WIDTH,
            ),
            COMPACT_COMMAND_SIDEBAR_WIDTH
        );
    }

    #[test]
    fn central_layout_rects_partition_normal_workspace_without_overlap() {
        let root = Rect::from_min_size(pos2(12.0, 24.0), vec2(920.0, 640.0));
        let regions = central_layout_rects(root);
        let ordered = [
            regions.operations,
            regions.plot_toolbar,
            regions.plot_body,
            regions.logs,
        ];
        for region in ordered {
            assert!(root.contains(region.min));
            assert!(root.contains(region.max));
            assert!((region.width() - root.width()).abs() <= f32::EPSILON);
        }
        for pair in ordered.windows(2) {
            assert!(!pair[0].intersects(pair[1]));
            assert!(pair[1].min.y >= pair[0].max.y);
        }
        assert!(regions.plot_body.height() > 0.0);
        assert_eq!(regions.logs.max.y, root.max.y);
    }

    #[test]
    fn central_layout_rects_remain_inside_tiny_workspace() {
        // A high UI scale or a very short window can leave less than the
        // nominal three inter-region gaps.  The allocator must still return
        // in-bounds rectangles instead of allowing the final log panel to
        // overflow the central panel.
        for height in [0.0, 1.0, 10.0, 17.9] {
            let root = Rect::from_min_size(pos2(0.0, 0.0), vec2(320.0, height));
            let regions = central_layout_rects(root);
            for region in [
                regions.operations,
                regions.plot_toolbar,
                regions.plot_body,
                regions.logs,
            ] {
                assert!(region.min.y >= root.min.y - f32::EPSILON);
                assert!(region.max.y <= root.max.y + f32::EPSILON);
            }
        }
    }

    #[test]
    fn central_layout_rects_geometry_is_content_independent() {
        let root = Rect::from_min_size(pos2(8.0, 16.0), vec2(600.0, 500.0));
        let first = central_layout_rects(root);
        let second = central_layout_rects(root);
        assert_eq!(first.operations, second.operations);
        assert_eq!(first.plot_toolbar, second.plot_toolbar);
        assert_eq!(first.plot_body, second.plot_body);
        assert_eq!(first.logs, second.logs);
    }

    #[test]
    fn central_layout_rects_cover_requested_viewport_matrix() {
        // These are the central work-area sizes left by the fixed sidebars and
        // header at the requested desktop window sizes.  The test intentionally
        // checks geometry only: content, telemetry, and sample counts cannot
        // change any of the returned rectangles.
        for (width, height) in [
            (680.0, 800.0),   // 1360×860, normal dock sizes
            (920.0, 840.0),   // 1600×900
            (1240.0, 1020.0), // 1920×1080
            (600.0, 660.0),   // 1280×720
            (444.0, 664.0),   // 1024×768, compact header and sidebars
        ] {
            let root = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, height));
            let regions = central_layout_rects(root);
            let ordered = [
                regions.operations,
                regions.plot_toolbar,
                regions.plot_body,
                regions.logs,
            ];
            for region in ordered {
                assert!(root.contains(region.min));
                assert!(root.contains(region.max));
                assert_eq!(region.width(), root.width());
            }
            for pair in ordered.windows(2) {
                assert!(!pair[0].intersects(pair[1]));
            }
            assert_eq!(regions.logs.max, root.max);
            assert!(regions.plot_body.height() > 0.0);
        }
    }
}
