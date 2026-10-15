use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use chrono::{DateTime, Local, Utc};
use eframe::egui::{
    self, Align, Color32, ComboBox, Grid, Layout, PointerButton, RichText, Stroke, Ui,
};
use egui_plot::{Legend, Line, Plot, PlotPoints, VLine};
use langmuir_core::Command;

use crate::{
    bridge::{
        BackendBridge, BackendEvent, BackendStateSnapshot, LinkKind, LinkMetricsSnapshot, LogLevel,
        TELEMETRY_POLL_INTERVAL_MAX_MILLIS, TELEMETRY_POLL_INTERVAL_MIN_MILLIS,
        TelemetryPollingState, TelemetrySnapshot, UiRequest,
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
const APPLICATION_WIDTH: f32 = 1_360.0;
const APPLICATION_HEIGHT: f32 = 860.0;
const HEADER_HEIGHT: f32 = 60.0;
const LOG_HEIGHT: f32 = 128.0;
const COMMAND_SIDEBAR_WIDTH: f32 = 300.0;
const TELEMETRY_SIDEBAR_WIDTH: f32 = 300.0;
const COMMAND_EDITOR_HEIGHT: f32 = 132.0;
const ADC_ZERO_COUNTS: f64 = 32_768.0;
const ADC_COUNTS_PER_MILLIVOLT: f64 = 6.4;
const ALARM_COALESCE_WINDOW: Duration = Duration::from_secs(30);
const DATA_FRESHNESS_WINDOW: Duration = Duration::from_secs(2);
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
            Self::Configuration => "配置 (0x22)",
            Self::Lvds => "LVDS (0x33)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigurationMode {
    Frequency,
    Mode,
    Voltage,
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
    const ALL: [Self; 3] = [Self::Frequency, Self::Mode, Self::Voltage];

    const fn label(self) -> &'static str {
        match self {
            Self::Frequency => "频率 (0x55)",
            Self::Mode => "模式 (0xAA)",
            Self::Voltage => "电压 (0xBB)",
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
    mode: u8,
    voltage: u16,
    telemetry_interval: u32,
}

impl Default for ParameterStepState {
    fn default() -> Self {
        Self {
            frequency: 10,
            mode: 1,
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
    command_family: CommandFamily,
    configuration_mode: ConfigurationMode,
    frequency: u8,
    mode: u8,
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
            command_family: CommandFamily::Telemetry,
            configuration_mode: ConfigurationMode::Frequency,
            frequency: 10,
            mode: 0,
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
                self.push_log(LogLevel::Info, format!("TX {}", hex_frame(&frame)));
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
    }

    fn apply_telemetry_polling_state(&mut self, active: bool, interval_millis: u32) {
        self.telemetry_polling = TelemetryPollingState {
            active,
            interval_millis,
        };
        self.push_log(
            LogLevel::Info,
            if active {
                format!("循环遥测已启动: {interval_millis} ms")
            } else {
                "循环遥测已停止".to_owned()
            },
        );
    }

    fn command(&self) -> Command {
        match self.command_family {
            CommandFamily::Telemetry => Command::RequestTelemetry,
            CommandFamily::Configuration => match self.configuration_mode {
                ConfigurationMode::Frequency => Command::ConfigureFrequency(self.frequency),
                ConfigurationMode::Mode => Command::ConfigureMode(self.mode),
                ConfigurationMode::Voltage => Command::ConfigureVoltage(self.voltage),
            },
            CommandFamily::Lvds => Command::SetLvdsEnabled(self.lvds_enabled),
        }
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
        theme::card().show(ui, |ui| {
            ui.horizontal(|ui| {
                status_dot(ui, state.connected);
                theme::section_title(ui, &format!("{link} 串口连接"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(&state.detail)
                            .small()
                            .color(theme::TEXT_SECONDARY),
                    );
                });
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
        let configuration_enabled = self.command_family == CommandFamily::Configuration;
        let lvds_enabled = self.command_family == CommandFamily::Lvds;
        Grid::new("typed_command_controls")
            .num_columns(2)
            .min_row_height(theme::CONTROL_HEIGHT)
            .spacing([8.0, 7.0])
            .show(ui, |ui| {
                theme::form_label(ui, "类型", true);
                ComboBox::from_id_salt("command_family")
                    .selected_text(self.command_family.label())
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for family in CommandFamily::ALL {
                            ui.selectable_value(&mut self.command_family, family, family.label());
                        }
                    });
                ui.end_row();

                theme::form_label(ui, "子模式", configuration_enabled);
                ui.add_enabled_ui(configuration_enabled, |ui| {
                    ComboBox::from_id_salt("configuration_mode")
                        .selected_text(self.configuration_mode.label())
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for mode in ConfigurationMode::ALL {
                                ui.selectable_value(
                                    &mut self.configuration_mode,
                                    mode,
                                    mode.label(),
                                );
                            }
                        });
                });
                ui.end_row();

                let parameter_label = match self.configuration_mode {
                    ConfigurationMode::Frequency => "频率",
                    ConfigurationMode::Mode => "模式",
                    ConfigurationMode::Voltage => "电压",
                };
                theme::form_label(ui, parameter_label, configuration_enabled);
                ui.add_enabled_ui(configuration_enabled, |ui| match self.configuration_mode {
                    ConfigurationMode::Frequency => {
                        integer_parameter_u8(
                            ui,
                            &mut self.frequency,
                            &mut self.parameter_steps.frequency,
                            &[1, 5, 10],
                        );
                    }
                    ConfigurationMode::Mode => {
                        integer_parameter_u8(
                            ui,
                            &mut self.mode,
                            &mut self.parameter_steps.mode,
                            &[1, 16, 64],
                        );
                    }
                    ConfigurationMode::Voltage => {
                        integer_parameter_u16(
                            ui,
                            &mut self.voltage,
                            &mut self.parameter_steps.voltage,
                            &[1, 10, 100],
                        );
                    }
                });
                ui.end_row();

                theme::form_label(ui, "LVDS", lvds_enabled);
                ui.add_enabled_ui(lvds_enabled, |ui| {
                    theme::aligned_checkbox(ui, &mut self.lvds_enabled, "使能科学数据传输");
                });
                ui.end_row();
            });
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
        let typed_before_toggle = command_bytes(self.command());
        let mut raw_mode = self.command_edit_mode == CommandEditMode::Raw;
        if theme::aligned_checkbox(ui, &mut raw_mode, "原始字节编辑").changed() {
            self.command_edit_mode = if raw_mode {
                self.raw_command = typed_before_toggle;
                CommandEditMode::Raw
            } else {
                CommandEditMode::Typed
            };
        }

        theme::subtle_card().show(ui, |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), COMMAND_EDITOR_HEIGHT),
                Layout::top_down(Align::Min),
                |ui| {
                    if raw_mode {
                        let raw_frame = self.raw_command;
                        self.command_bytes_grid(ui, &raw_frame, true);
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new("原始模式：直接编辑 12 字节；不会自动修复校验和")
                                .size(9.0)
                                .color(Color32::from_rgb(220, 38, 38)),
                        );
                    } else {
                        self.typed_command_controls(ui);
                    }
                },
            );
        });

        let displayed_frame = if raw_mode {
            self.raw_command
        } else {
            command_bytes(self.command())
        };
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("待发送")
                    .size(9.0)
                    .color(theme::TEXT_SECONDARY),
            );
            let preview = hex_frame(&displayed_frame);
            ui.add(
                egui::Label::new(
                    RichText::new(&preview)
                        .monospace()
                        .size(9.0)
                        .color(theme::PRIMARY),
                )
                .truncate(),
            )
            .on_hover_text(preview);
        });
        if !raw_mode && self.command_family == CommandFamily::Telemetry {
            self.telemetry_polling_control(ui);
        }
        ui.horizontal(|ui| {
            if ui
                .add_sized([68.0, 28.0], egui::Button::new("重置"))
                .clicked()
            {
                self.command_family = CommandFamily::Telemetry;
                self.configuration_mode = ConfigurationMode::Frequency;
                self.frequency = 10;
                self.mode = 0;
                self.voltage = 5_000;
                self.lvds_enabled = true;
                self.command_edit_mode = CommandEditMode::Typed;
                self.raw_command = command_bytes(Command::RequestTelemetry);
                raw_mode = false;
            }
            if raw_mode && ui.button("重算 SUM").clicked() {
                recalculate_checksum(&mut self.raw_command);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_sized(
                        [104.0, 28.0],
                        egui::Button::new(RichText::new("发送指令").strong().color(theme::WHITE))
                            .fill(theme::PRIMARY),
                    )
                    .clicked()
                {
                    let frame = if raw_mode {
                        self.raw_command
                    } else {
                        command_bytes(self.command())
                    };
                    self.submit(UiRequest::SendCommand(frame));
                }
            });
        });
    }

    fn telemetry_polling_control(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("循环周期")
                    .size(9.0)
                    .color(theme::TEXT_SECONDARY),
            );
            ui.label(
                RichText::new("滑块粗调 / 数字精调")
                    .size(9.0)
                    .color(theme::TEXT_MUTED),
            );
        });
        let slider_changed = ui
            .add(
                egui::Slider::new(
                    &mut self.telemetry_polling.interval_millis,
                    TELEMETRY_POLL_INTERVAL_MIN_MILLIS..=TELEMETRY_POLL_INTERVAL_MAX_MILLIS,
                )
                .show_value(false)
                .logarithmic(true)
                .step_by(f64::from(self.parameter_steps.telemetry_interval)),
            )
            .changed();
        let input_changed = ui
            .horizontal(|ui| {
                let changed = ui
                    .add_sized(
                        [102.0, theme::CONTROL_HEIGHT],
                        egui::DragValue::new(&mut self.telemetry_polling.interval_millis)
                            .range(
                                TELEMETRY_POLL_INTERVAL_MIN_MILLIS
                                    ..=TELEMETRY_POLL_INTERVAL_MAX_MILLIS,
                            )
                            .speed(f64::from(self.parameter_steps.telemetry_interval))
                            .suffix(" ms"),
                    )
                    .changed();
                for preset in [100, 1_000, 5_000] {
                    if ui
                        .selectable_label(
                            self.parameter_steps.telemetry_interval == preset,
                            format!("{preset}"),
                        )
                        .clicked()
                    {
                        self.parameter_steps.telemetry_interval = preset;
                    }
                }
                changed
            })
            .inner;
        let changed = slider_changed || input_changed;
        if changed && self.telemetry_polling.active {
            self.submit(UiRequest::SetTelemetryPolling {
                enabled: true,
                interval_millis: self.telemetry_polling.interval_millis,
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
                    interval_millis: self.telemetry_polling.interval_millis,
                });
            }
        });
    }

    fn command_byte_is_active(&self, index: usize) -> bool {
        match self.command_family {
            CommandFamily::Telemetry => index == 6,
            CommandFamily::Configuration => match self.configuration_mode {
                ConfigurationMode::Frequency | ConfigurationMode::Mode => {
                    matches!(index, 6..=8)
                }
                ConfigurationMode::Voltage => matches!(index, 6..=8),
            },
            CommandFamily::Lvds => matches!(index, 6..=7),
        }
    }

    fn telemetry_panel(&self, ui: &mut Ui) {
        let Some(data) = &self.telemetry else {
            theme::card().show(ui, |ui| {
                theme::section_title(ui, "遥测状态");
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("等待遥测数据…")
                            .color(theme::TEXT_SECONDARY)
                            .size(11.0),
                    );
                });
            });
            return;
        };
        let fresh = self.telemetry_is_fresh();

        theme::card().show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::section_title(ui, "时间与工作参数");
                heartbeat_indicator(ui, fresh);
            });
            Grid::new("telemetry_parameters")
                .num_columns(2)
                .min_col_width(120.0)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    telemetry_value_cell(
                        ui,
                        "下位机时间",
                        format_timestamp(data.absolute_time_seconds),
                        fresh,
                    );
                    telemetry_value_cell(
                        ui,
                        "自守时 (s)",
                        data.local_time_seconds.to_string(),
                        fresh,
                    );
                    ui.end_row();
                    telemetry_value_cell(
                        ui,
                        "遥测计数",
                        data.telemetry_request_count.to_string(),
                        fresh,
                    );
                    telemetry_value_cell(ui, "工作模式", work_mode_name(data.work_mode), fresh);
                    ui.end_row();
                    telemetry_value_cell(ui, "扫描频率", data.scan_frequency.to_string(), fresh);
                    telemetry_value_cell(ui, "扫描电压", data.scan_voltage.to_string(), fresh);
                    ui.end_row();
                });
        });

        ui.add_space(7.0);
        theme::card().show(ui, |ui| {
            theme::section_title(ui, "温度");
            temperature_bar(ui, "CPU 温度", data.cpu_temperature_celsius, fresh);
        });

        ui.add_space(7.0);
        theme::card().show(ui, |ui| {
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
            Grid::new("device_status_grid")
                .num_columns(2)
                .min_col_width(120.0)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    protocol_status_cell(ui, "AD9833", data.ad9833_status, fresh);
                    protocol_status_cell(ui, "DAC8563", data.dac8563_status, fresh);
                    ui.end_row();
                    protocol_status_cell(ui, "ADC1", data.adc_1_status, fresh);
                    protocol_status_cell(ui, "ADC2", data.adc_2_status, fresh);
                    ui.end_row();
                    protocol_status_cell(ui, "LVDS", data.lvds_status, fresh);
                    telemetry_value_cell(
                        ui,
                        "上一条指令",
                        hex_bytes(&data.last_injected_command),
                        fresh,
                    );
                    ui.end_row();
                });
        });
    }

    fn fault_panel(&mut self, ui: &mut Ui) {
        theme::card().show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::section_title(ui, "报警");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
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
                ui.label(
                    RichText::new("该筛选下没有报警")
                        .small()
                        .color(theme::TEXT_MUTED),
                );
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
                            ui.horizontal(|ui| {
                                theme::status_badge(ui, fault.state.label(), color);
                                ui.label(
                                    RichText::new(format!(
                                        "{}  ×{}",
                                        fault.time, fault.repetitions
                                    ))
                                    .small()
                                    .color(theme::TEXT_MUTED),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
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
                            });
                            let summary = format!("{}: {}", fault.source, fault.message);
                            ui.add(
                                egui::Label::new(RichText::new(&summary).size(10.0).color(color))
                                    .truncate(),
                            )
                            .on_hover_text(summary);
                        });
                }
            }
        });
    }

    #[expect(
        clippy::too_many_lines,
        reason = "overlay and split layouts intentionally share one control header"
    )]
    fn plot_panel(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            theme::section_title(
                ui,
                match self.plot_layout {
                    PlotLayout::Overlay => "LVDS 科学数据（双通道）",
                    PlotLayout::Split => "LVDS 科学数据（上下分屏）",
                },
            );
            ui.separator();
            ui.checkbox(&mut self.show_channels[0], "通道 1");
            ui.checkbox(&mut self.show_channels[1], "通道 2");
            ui.selectable_value(&mut self.plot_layout, PlotLayout::Overlay, "叠加");
            ui.selectable_value(&mut self.plot_layout, PlotLayout::Split, "分屏");
            if self.plot_layout == PlotLayout::Overlay {
                ui.checkbox(&mut self.overlay_normalized, "叠加独立归一");
            }
            let frozen = self.operator.frozen_plot.is_some();
            let freeze_label = if frozen {
                "恢复更新"
            } else {
                "冻结画面"
            };
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
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if frozen {
                    theme::status_badge(ui, "画面冻结", theme::PRIMARY);
                }
                ui.small(format!(
                    "固定窗 {:.0}s · 缓存 {}/{} 点",
                    PLOT_WINDOW_SECONDS,
                    self.channel_1.len(),
                    self.channel_2.len()
                ));
            });
        });
        let (mut channel_1, mut channel_2) = self.display_plot_points();
        let cursor_channel_1 = channel_1.clone();
        let cursor_channel_2 = channel_2.clone();
        if let Some(cursor) = self.operator.plot_cursor_seconds {
            plot_cursor_readout(ui, cursor, &cursor_channel_1, &cursor_channel_2);
        }
        let available_plot_height = ui.available_height().max(180.0);
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
                let split_height = ((available_plot_height - 8.0) / 2.0).max(105.0);
                if self.show_channels[0] {
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
                if self.show_channels[1] {
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
        reason = "the LVDS toolbar keeps its responsive single-row controls in one render function"
    )]
    fn operations_panel(&mut self, ui: &mut Ui) {
        let mut request = None;
        theme::card().show(ui, |ui| {
            ui.horizontal(|ui| {
                status_dot(ui, self.lvds.connected);
                ui.strong("LVDS / UDP");
                ui.add_enabled_ui(!self.lvds.connected, |ui| {
                    ui.label(RichText::new("本机").small().color(theme::TEXT_SECONDARY));
                    ui.add_sized(
                        [92.0, theme::CONTROL_HEIGHT],
                        egui::TextEdit::singleline(&mut self.lvds.bind_address)
                            .hint_text("192.168.1.100"),
                    );
                    ui.label(RichText::new("端口").small().color(theme::TEXT_SECONDARY));
                    ui.add_sized(
                        [64.0, theme::CONTROL_HEIGHT],
                        egui::DragValue::new(&mut self.lvds.port).range(1..=u16::MAX),
                    );
                });

                let connection_label = if self.lvds.connected {
                    "断开"
                } else {
                    "连接"
                };
                if ui
                    .add(
                        egui::Button::new(connection_label)
                            .min_size(egui::vec2(50.0, theme::CONTROL_HEIGHT)),
                    )
                    .clicked()
                {
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
                if ui
                    .add_sized(
                        [128.0, theme::CRITICAL_CONTROL_HEIGHT],
                        egui::Button::new(RichText::new(action_text).strong().color(theme::WHITE))
                            .fill(action_color),
                    )
                    .clicked()
                {
                    request = Some(if self.acquisition_running {
                        UiRequest::StopAcquisition
                    } else {
                        UiRequest::StartAcquisition
                    });
                }
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
            });
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&self.lvds.detail)
                        .small()
                        .color(theme::TEXT_SECONDARY),
                );
                if let Some(metrics) = self.lvds.metrics {
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "重组 {} · 拒绝 {} · 显示丢弃 {}",
                            metrics.frames_decoded,
                            metrics.rejected_frames,
                            metrics.ui_display_events_dropped
                        ))
                        .small()
                        .color(theme::TEXT_MUTED),
                    );
                }
            });
            ui.label(
                RichText::new("网口协议：24B UDP 封装 + 269B LVDS； 192.168.1.128:1534")
                    .size(9.0)
                    .color(theme::TEXT_MUTED),
            );
        });
        if let Some(request) = request {
            self.submit(request);
        }
    }

    fn log_panel(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            theme::section_title(ui, "系统日志");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{}/{} 条", self.logs.len(), MAX_LOGS))
                        .small()
                        .color(theme::TEXT_MUTED),
                );
            });
        });
        for entry in self.logs.iter().rev().take(4).rev() {
            let color = match entry.level {
                LogLevel::Info => theme::TEXT,
                LogLevel::Warning => theme::WARNING,
                LogLevel::Error => theme::DANGER,
            };
            let full_text = format!("[{}] {}", entry.time, entry.message);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("[{}]", entry.time))
                        .monospace()
                        .size(10.0)
                        .color(color),
                );
                ui.add(
                    egui::Label::new(RichText::new(&entry.message).size(10.0).color(color))
                        .truncate(),
                )
                .on_hover_text(full_text);
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

        debug_assert!((ui.available_width() - APPLICATION_WIDTH).abs() < 1.0);
        debug_assert!((ui.available_height() - APPLICATION_HEIGHT).abs() < 1.0);

        egui::Panel::top("application_header")
            .exact_size(HEADER_HEIGHT)
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(14, 5)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
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
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_sized(
                                [122.0, theme::CRITICAL_CONTROL_HEIGHT],
                                egui::Button::new(
                                    RichText::new("软件急停").strong().color(theme::WHITE),
                                )
                                .fill(theme::DANGER),
                            )
                            .clicked_by(PointerButton::Primary)
                        {
                            self.operator.emergency_stop_confirmation_open = true;
                        }
                        #[cfg(feature = "mock")]
                        if self.mock_mode {
                            theme::status_badge(ui, "MOCK", theme::WARNING);
                        }
                        if let Some(warning) = self.font_warning.as_deref() {
                            ui.colored_label(theme::WARNING, "CJK FONT MISSING")
                                .on_hover_text(warning);
                        }
                        if self.operator.frozen_plot.is_some() {
                            theme::status_badge(ui, "画面冻结", theme::PRIMARY);
                        }
                        if self.telemetry_polling.active {
                            theme::status_badge(ui, "自动遥测", theme::PRIMARY);
                        }
                        header_link_status(
                            ui,
                            if self.acquisition_running {
                                "采集中"
                            } else {
                                "已停止"
                            },
                            self.acquisition_running,
                            self.scientific_data_is_fresh(),
                        );
                        header_link_status(
                            ui,
                            "LVDS",
                            self.lvds.connected,
                            self.scientific_data_is_fresh(),
                        );
                        header_link_status(
                            ui,
                            "RS422",
                            self.rs422.connected,
                            self.telemetry_is_fresh(),
                        );
                    });
                });
            });

        egui::Panel::left("command_sidebar")
            .resizable(false)
            .exact_size(COMMAND_SIDEBAR_WIDTH)
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::same(8)),
            )
            .show(ui, |ui| {
                if let Some(request) = Self::connection_card(ui, LinkKind::Rs422, &mut self.rs422) {
                    self.submit(request);
                }
                ui.add_space(6.0);
                self.command_panel(ui);
            });

        egui::Panel::right("telemetry_sidebar")
            .resizable(false)
            .exact_size(TELEMETRY_SIDEBAR_WIDTH)
            .frame(
                egui::Frame::new()
                    .fill(theme::PAGE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::same(8)),
            )
            .show(ui, |ui| {
                self.telemetry_panel(ui);
                ui.add_space(6.0);
                self.fault_panel(ui);
            });

        egui::Panel::bottom("system_log")
            .resizable(false)
            .exact_size(LOG_HEIGHT)
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE_ALT)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(10, 8)),
            )
            .show(ui, |ui| self.log_panel(ui));

        egui::CentralPanel::default().show(ui, |ui| {
            self.operations_panel(ui);
            ui.add_space(6.0);
            theme::card().show(ui, |ui| self.plot_panel(ui));
        });

        self.emergency_stop_dialog(ui.ctx());

        ui.ctx().request_repaint_after(
            if self.acquisition_running
                || self.telemetry_is_fresh()
                || self.scientific_data_is_fresh()
            {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(250)
            },
        );
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

fn header_link_status(ui: &mut Ui, label: &str, active: bool, fresh: bool) {
    let color = if !active {
        theme::OFFLINE
    } else if fresh {
        theme::SUCCESS
    } else {
        theme::WARNING
    };
    ui.horizontal(|ui| {
        heartbeat_indicator(ui, active && fresh);
        ui.label(RichText::new(label).strong().size(11.0).color(color));
    });
}

fn telemetry_value_cell(ui: &mut Ui, label: &str, value: String, fresh: bool) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(9.0).color(theme::TEXT_SECONDARY));
        heartbeat_indicator(ui, fresh);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(value)
                    .monospace()
                    .size(10.0)
                    .strong()
                    .color(theme::PRIMARY),
            );
        });
    });
}

fn integer_parameter_u8(ui: &mut Ui, value: &mut u8, step: &mut u8, presets: &[u8]) {
    ui.add(
        egui::Slider::new(value, 0..=u8::MAX)
            .show_value(false)
            .step_by(f64::from(*step)),
    );
    ui.horizontal(|ui| {
        ui.add_sized(
            [64.0, theme::CONTROL_HEIGHT],
            egui::DragValue::new(value)
                .range(0..=u8::MAX)
                .speed(f64::from(*step)),
        );
        for preset in presets {
            if ui
                .selectable_label(*step == *preset, format!("{preset}"))
                .clicked()
            {
                *step = *preset;
            }
        }
    });
}

fn integer_parameter_u16(ui: &mut Ui, value: &mut u16, step: &mut u16, presets: &[u16]) {
    ui.add(
        egui::Slider::new(value, 0..=u16::MAX)
            .show_value(false)
            .step_by(f64::from(*step)),
    );
    ui.horizontal(|ui| {
        ui.add_sized(
            [78.0, theme::CONTROL_HEIGHT],
            egui::DragValue::new(value)
                .range(0..=u16::MAX)
                .speed(f64::from(*step)),
        );
        for preset in presets {
            if ui
                .selectable_label(*step == *preset, format!("{preset}"))
                .clicked()
            {
                *step = *preset;
            }
        }
    });
}

fn protocol_status_cell(ui: &mut Ui, label: &str, status: u8, fresh: bool) {
    let value = format!("0x{status:02X}");
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).small().color(theme::TEXT_SECONDARY));
        heartbeat_indicator(ui, fresh);
        theme::status_badge(ui, &value, theme::PRIMARY);
    })
    .response
    .on_hover_text("协议仅定义状态码字段，未给出各数值的正常/故障语义");
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
        let fraction = (f32::from(temperature) / 100.0).clamp(0.0, 1.0);
        let color = threshold_color(fraction);
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_width(ui.available_width())
                .fill(color)
                .text(format!("{temperature} ℃   80% 警告 · 90% 严重")),
        );
    });
}

fn heartbeat_indicator(ui: &mut Ui, fresh: bool) {
    let time = ui.ctx().input(|input| input.time);
    let bright_phase = time.fract() < 0.5;
    let color = if fresh {
        if bright_phase {
            theme::SUCCESS
        } else {
            theme::SUCCESS.gamma_multiply(0.55)
        }
    } else {
        theme::WARNING
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(10.0, 12.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), if bright_phase { 4.0 } else { 2.5 }, color);
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
    frame[11] = frame[6..11]
        .iter()
        .fold(0_u8, |checksum, byte| checksum.wrapping_add(*byte));
}

fn format_timestamp(seconds: u32) -> String {
    DateTime::<Utc>::from_timestamp(i64::from(seconds), 0).map_or_else(
        || seconds.to_string(),
        |date_time| {
            date_time
                .with_timezone(&Local)
                .format("%H:%M:%S")
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
    use langmuir_core::Command;

    use super::{
        adc_counts_to_millivolts, command_bytes, hex_bytes, normalize_plot_points,
        recalculate_checksum, work_mode_name,
    };

    #[test]
    fn entering_raw_mode_starts_from_the_typed_command() {
        let expected = Command::ConfigureVoltage(0x1234).encode().into_bytes();
        assert_eq!(command_bytes(Command::ConfigureVoltage(0x1234)), expected);
    }

    #[test]
    fn raw_checksum_recalculation_only_updates_sum_byte() {
        let mut frame = command_bytes(Command::RequestTelemetry);
        frame[5] = 0xFE;
        let before = frame;
        recalculate_checksum(&mut frame);
        let expected = frame[6..11]
            .iter()
            .fold(0_u8, |checksum, byte| checksum.wrapping_add(*byte));
        assert_eq!(frame[11], expected);
        assert_eq!(&frame[..11], &before[..11]);
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
}
