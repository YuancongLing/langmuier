use std::{
    collections::{HashMap, VecDeque},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use chrono::{DateTime, Local, Utc};
use eframe::egui::{
    self, Align, Color32, ComboBox, Grid, Layout, PointerButton, RichText, ScrollArea, Stroke, Ui,
};
use egui_plot::{Legend, Line, Plot, PlotPoint, PlotPoints, VLine};
use langmuir_core::{
    Command, CommandError, CommandFrame, SAMPLE_COUNT, SAMPLE_INTERVAL_MICROS, ScanFrequency,
    command_checksum as core_command_checksum, validate_command_frame,
};

use crate::{
    bridge::{
        BackendBridge, BackendEvent, BackendStateSnapshot, LinkKind, LinkMetricsSnapshot, LogLevel,
        LvdsStatusSnapshot, ScientificSaveFormat, TELEMETRY_POLL_INTERVAL_DEFAULT_MILLIS,
        TELEMETRY_POLL_INTERVAL_MAX_MILLIS, TELEMETRY_POLL_INTERVAL_MIN_MILLIS,
        TelemetryPollingState, TelemetrySnapshot, UiRequest,
    },
    buffer::{SamplePoint, TimeSeriesBuffer},
    ui::{fonts, theme},
    voltage::adc16_to_volts,
};

#[path = "app_history.rs"]
mod history_ui;

// UI history: 2 kHz × 60 s, plus one complete scientific frame.
// Scientific file storage is independent of this bounded display history.
const PLOT_CAPACITY: usize = 60 * 1_000_000 / SAMPLE_INTERVAL_MICROS as usize + SAMPLE_COUNT;
const PLOT_HISTORY_SECONDS: f64 = 60.0;
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
const CENTRAL_OPERATIONS_HEIGHT: f32 = 216.0;
const CENTRAL_PLOT_TOOLBAR_HEIGHT: f32 = 86.0;
const CENTRAL_LOG_HEIGHT: f32 = 160.0;
const CENTRAL_GAP: f32 = 6.0;
const CENTRAL_CONTROL_HEIGHT: f32 = 32.0;
const CENTRAL_COMPACT_OPERATIONS_HEIGHT: f32 = 154.0;
const CENTRAL_COMPACT_LOG_HEIGHT: f32 = 80.0;
const CENTRAL_SHORT_HEIGHT: f32 = 620.0;
const UI_SCALE_OPTIONS: [f32; 6] = [0.8, 0.9, 1.0, 1.1, 1.25, 1.5];
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

#[derive(Debug, Default)]
struct LvdsView {
    status: Option<LvdsStatusSnapshot>,
    replay_dialog_open: bool,
    plotted_frames: u64,
    replay_path: String,
    replay_save_science: bool,
    save_format: ScientificSaveFormat,
    history: history_ui::HistoryView,
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
    channel_1: TimeSeriesBuffer,
    channel_2: TimeSeriesBuffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PlotViewMode {
    #[default]
    FollowLatest,
    ManualView,
}

#[derive(Debug)]
struct OperatorViewState {
    frozen_plot: Option<FrozenPlot>,
    plot_cursor_seconds: Option<f64>,
    plot_view: PlotViewMode,
    plot_x_range: Option<[f64; 2]>,
    plot_reset_pending: bool,
    plot_width_points: Option<f32>,
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
            plot_view: PlotViewMode::FollowLatest,
            plot_x_range: None,
            plot_reset_pending: true,
            plot_width_points: None,
            alarm_filter: AlarmState::Unacknowledged,
            emergency_stop_confirmation_open: false,
            last_telemetry_update: None,
            last_scientific_update: None,
        }
    }
}

#[derive(Debug)]
struct ParameterStepState {
    telemetry_interval: u32,
}

impl Default for ParameterStepState {
    fn default() -> Self {
        Self {
            telemetry_interval: 1_000,
        }
    }
}

#[derive(Debug)]
struct IntegerParameterEdit {
    text: String,
    dirty: bool,
    error: Option<String>,
}

impl IntegerParameterEdit {
    fn new(value: u16) -> Self {
        Self {
            text: value.to_string(),
            dirty: false,
            error: None,
        }
    }

    fn parsed_value(&self, maximum: u16) -> Result<u16, String> {
        if self.text.is_empty() || !self.text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!("请输入 0–{maximum} 的整数，不接受小数或其他字符"));
        }
        self.text
            .parse::<u16>()
            .ok()
            .filter(|value| *value <= maximum)
            .ok_or_else(|| format!("数值超出范围，请输入 0–{maximum}"))
    }

    fn set_confirmed(&mut self, value: u16) {
        self.text = value.to_string();
        self.dirty = false;
        self.error = None;
    }

    fn confirm(&mut self, maximum: u16) -> Option<u16> {
        match self.parsed_value(maximum) {
            Ok(value) => {
                self.set_confirmed(value);
                Some(value)
            }
            Err(error) => {
                self.error = Some(error);
                None
            }
        }
    }

    fn ensure_confirmed(&self, value: u16, maximum: u16) -> Result<(), String> {
        let parsed = self.parsed_value(maximum)?;
        if self.dirty || parsed != value {
            Err("精确输入尚未确认，请按 Enter 或移开焦点".to_owned())
        } else {
            Ok(())
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
    measurements: crate::perf::PlotMeasurements,
    bridge: Box<dyn BackendBridge>,
    rs422: ConnectionPanelState,
    lvds: LvdsUdpPanelState,
    lvds_view: LvdsView,
    acquisition_running: bool,
    telemetry_polling: TelemetryPollingState,
    telemetry_polling_edit_millis: u32,
    command_family: CommandFamily,
    configuration_mode: ConfigurationMode,
    frequency: u8,
    frequency_edit: IntegerParameterEdit,
    voltage: u16,
    voltage_edit: IntegerParameterEdit,
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
        Self::with_bridge(
            bridge,
            font_report.warning,
            #[cfg(feature = "mock")]
            mock_mode,
        )
    }

    fn with_bridge(
        bridge: Box<dyn BackendBridge>,
        font_warning: Option<String>,
        #[cfg(feature = "mock")] mock_mode: bool,
    ) -> Self {
        let mut app = Self {
            measurements: crate::perf::PlotMeasurements::default(),
            bridge,
            rs422: ConnectionPanelState::default(),
            lvds: LvdsUdpPanelState::default(),
            lvds_view: LvdsView::default(),
            acquisition_running: false,
            telemetry_polling: TelemetryPollingState::default(),
            telemetry_polling_edit_millis: TELEMETRY_POLL_INTERVAL_DEFAULT_MILLIS,
            command_family: CommandFamily::Telemetry,
            configuration_mode: ConfigurationMode::Triangle,
            frequency: 10,
            frequency_edit: IntegerParameterEdit::new(10),
            voltage: 5_000,
            voltage_edit: IntegerParameterEdit::new(5_000),
            lvds_enabled: true,
            command_edit_mode: CommandEditMode::Typed,
            raw_command: command_bytes(Command::RequestTelemetry),
            telemetry: None,
            channel_1: scientific_plot_buffer(),
            channel_2: scientific_plot_buffer(),
            show_channels: [true, true],
            plot_layout: PlotLayout::Overlay,
            overlay_normalized: false,
            logs: VecDeque::new(),
            faults: VecDeque::new(),
            operator: OperatorViewState::default(),
            parameter_steps: ParameterStepState::default(),
            font_warning,
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
            self.push_log(LogLevel::Info, "画面更新已恢复，后台接收未中断");
        } else {
            let (one, two) = self.display_histories();
            let frozen = FrozenPlot {
                channel_1: one.clone(),
                channel_2: two.clone(),
            };
            self.operator.frozen_plot = Some(FrozenPlot {
                channel_1: frozen.channel_1,
                channel_2: frozen.channel_2,
            });
            self.push_log(LogLevel::Info, "画面已冻结，后台接收继续运行");
        }
    }

    #[cfg(test)]
    fn display_plot_points(&mut self) -> (Vec<[f64; 2]>, Vec<[f64; 2]>) {
        let range = self.plot_range();
        let (channel_1, channel_2) = self.display_histories_mut();
        channel_1.prepare_visible(range, MAX_DISPLAY_POINTS, false);
        channel_2.prepare_visible(range, MAX_DISPLAY_POINTS, false);
        let coordinates = |buffer: &TimeSeriesBuffer| {
            buffer.visible_points().iter().map(|p| [p.x, p.y]).collect()
        };
        (coordinates(channel_1), coordinates(channel_2))
    }

    fn display_histories(&self) -> (&TimeSeriesBuffer, &TimeSeriesBuffer) {
        if let Some(frozen) = &self.operator.frozen_plot {
            (&frozen.channel_1, &frozen.channel_2)
        } else if self.lvds_view.history.status.active {
            (
                &self.lvds_view.history.channel_1,
                &self.lvds_view.history.channel_2,
            )
        } else if self.lvds_view.history.live_has_qualified {
            (
                &self.lvds_view.history.live_channel_1,
                &self.lvds_view.history.live_channel_2,
            )
        } else {
            (&self.channel_1, &self.channel_2)
        }
    }

    fn display_histories_mut(&mut self) -> (&mut TimeSeriesBuffer, &mut TimeSeriesBuffer) {
        if let Some(frozen) = &mut self.operator.frozen_plot {
            (&mut frozen.channel_1, &mut frozen.channel_2)
        } else if self.lvds_view.history.status.active {
            (
                &mut self.lvds_view.history.channel_1,
                &mut self.lvds_view.history.channel_2,
            )
        } else if self.lvds_view.history.live_has_qualified {
            (
                &mut self.lvds_view.history.live_channel_1,
                &mut self.lvds_view.history.live_channel_2,
            )
        } else {
            (&mut self.channel_1, &mut self.channel_2)
        }
    }

    fn plot_range(&self) -> [f64; 2] {
        if self.operator.plot_view == PlotViewMode::ManualView
            && let Some(range) = self.operator.plot_x_range
        {
            return range;
        }
        let (channel_1, channel_2) = self.display_histories();
        let latest = channel_1
            .latest_time()
            .into_iter()
            .chain(channel_2.latest_time())
            .reduce(f64::max)
            .unwrap_or(PLOT_WINDOW_SECONDS);
        [latest - PLOT_WINDOW_SECONDS, latest]
    }

    fn follow_latest(&mut self) {
        self.operator.plot_view = PlotViewMode::FollowLatest;
        self.operator.plot_x_range = None;
        self.operator.plot_reset_pending = true;
        if ui_rect_debug_enabled() {
            eprintln!(
                "PLOT_VIEW FollowLatest epoch={:.6} frozen={}",
                plot_debug_epoch_seconds(),
                self.operator.frozen_plot.is_some()
            );
        }
    }

    fn reset_plot_interaction(&mut self) {
        self.operator.frozen_plot = None;
        self.operator.plot_cursor_seconds = None;
        self.operator.plot_width_points = None;
        self.follow_latest();
    }

    fn clear_current_plot(&mut self) {
        self.clear_live_quality_history();
        self.channel_1.clear();
        self.channel_2.clear();
        self.lvds_view.plotted_frames = 0;
        self.reset_plot_interaction();
    }

    fn apply_lvds_status(&mut self, status: LvdsStatusSnapshot) {
        let source_changed = self
            .lvds_view
            .status
            .as_ref()
            .is_some_and(|previous| previous.source != status.source);
        if source_changed {
            if self.lvds_view.history.status.active {
                self.clear_live_quality_history();
                self.channel_1.clear();
                self.channel_2.clear();
                self.lvds_view.plotted_frames = 0;
            } else {
                self.clear_current_plot();
            }
            self.operator.last_scientific_update = None;
        }
        self.lvds_view.status = Some(status);
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
            event @ (BackendEvent::RawCaptureStatus(_)
            | BackendEvent::RecoveryStatus(_)
            | BackendEvent::HistoryStatus(_)
            | BackendEvent::HistoryWindow(_)
            | BackendEvent::QualifiedScientificSamples(_)
            | BackendEvent::HistoryCsvExported { .. }) => self.handle_history_event(event),
            BackendEvent::PortsUpdated { rs422 } => {
                self.rs422.ports = rs422;
                self.rs422.selected_port = 0;
                self.push_log(LogLevel::Info, "串口列表已刷新");
            }
            BackendEvent::LinkChanged {
                link,
                connected,
                detail,
            } => self.apply_link_changed(link, connected, &detail),
            BackendEvent::AcquisitionChanged { running } => {
                self.acquisition_running = running;
                if running {
                    self.restore_faults_for_source("采集");
                }
                let message = if running {
                    "正式科学保存已开始"
                } else {
                    "正式科学保存已停止"
                };
                #[cfg(feature = "mock")]
                let message = if self.mock_mode {
                    "MOCK 合成波形状态已更新；不写科学文件"
                } else {
                    message
                };
                self.push_log(LogLevel::Info, message);
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
                    self.rs422.metrics = Some(*metrics);
                }
                LinkKind::Lvds => {
                    self.lvds.connected = connected;
                    self.lvds.metrics = Some(*metrics);
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
            BackendEvent::LvdsStatus(status) => self.apply_lvds_status(*status),
            BackendEvent::CommandSent(frame) => {
                self.push_log(
                    LogLevel::Info,
                    format!("RS422 已写入（设备无控制应答）: {}", hex_frame(&frame)),
                );
            }
            BackendEvent::CsvExported { path } => {
                self.push_log(LogLevel::Info, format!("RS422 遥测 CSV 已导出: {path}"));
            }
            BackendEvent::Fault { source, message } => self.push_fault(source, message),
            BackendEvent::Log { level, message } => self.push_log(level, message),
        }
    }

    fn apply_link_changed(&mut self, link: LinkKind, connected: bool, detail: &str) {
        match link {
            LinkKind::Rs422 => {
                self.rs422.connected = connected;
                detail.clone_into(&mut self.rs422.detail);
            }
            LinkKind::Lvds => {
                self.lvds.connected = connected;
                detail.clone_into(&mut self.lvds.detail);
            }
        }
        if connected {
            self.restore_faults_for_source(&link.to_string());
        }
        let status = if connected { "已连接" } else { "已断开" };
        self.push_log(LogLevel::Info, format!("{link} {status}: {detail}"));
    }

    fn handle_scientific_samples(
        &mut self,
        channel_1_start_seconds: f64,
        channel_2_start_seconds: f64,
        sample_interval_seconds: f64,
        channel_1: Vec<f64>,
        channel_2: Vec<f64>,
    ) {
        self.remember_native_samples(
            channel_1_start_seconds,
            sample_interval_seconds,
            &channel_1,
            &channel_2,
        );
        self.operator.last_scientific_update = Some(Instant::now());
        self.lvds_view.plotted_frames = self.lvds_view.plotted_frames.saturating_add(1);
        extend_scientific_channel(
            &mut self.channel_1,
            channel_1_start_seconds,
            sample_interval_seconds,
            channel_1,
        );
        extend_scientific_channel(
            &mut self.channel_2,
            channel_2_start_seconds,
            sample_interval_seconds,
            channel_2,
        );
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

    fn typed_command_bytes(&self) -> Result<[u8; 12], String> {
        if self.command_family == CommandFamily::Configuration {
            match self.configuration_mode {
                ConfigurationMode::Triangle | ConfigurationMode::Sine => self
                    .frequency_edit
                    .ensure_confirmed(u16::from(self.frequency), u16::from(ScanFrequency::MAX))?,
                ConfigurationMode::Dc => {
                    self.voltage_edit.ensure_confirmed(self.voltage, u16::MAX)?;
                }
            }
        }
        self.command()
            .map(command_bytes)
            .map_err(|error| error.to_string())
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
                let response =
                    ui.selectable_value(&mut self.command_family, family, family.label());
                debug_ui_rect(
                    match family {
                        CommandFamily::Telemetry => "left.command_family.telemetry",
                        CommandFamily::Configuration => "left.command_family.configuration",
                        CommandFamily::Lvds => "left.command_family.lvds",
                    },
                    response.rect,
                );
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
                        let response =
                            ui.selectable_value(&mut self.configuration_mode, mode, mode.label());
                        debug_ui_rect(
                            match mode {
                                ConfigurationMode::Triangle => "left.configuration_mode.triangle",
                                ConfigurationMode::Sine => "left.configuration_mode.sine",
                                ConfigurationMode::Dc => "left.configuration_mode.dc",
                            },
                            response.rect,
                        );
                    }
                });
                ui.add_space(4.0);
                match self.configuration_mode {
                    ConfigurationMode::Triangle | ConfigurationMode::Sine => {
                        frequency_parameter(ui, &mut self.frequency, &mut self.frequency_edit);
                    }
                    ConfigurationMode::Dc => {
                        integer_parameter_u16(
                            ui,
                            &mut self.voltage,
                            &mut self.voltage_edit,
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
        self.command_preview(ui, displayed_frame, raw_mode);
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
                self.frequency_edit.set_confirmed(10);
                self.voltage = 5_000;
                self.voltage_edit.set_confirmed(5_000);
                self.lvds_enabled = true;
                self.command_edit_mode = CommandEditMode::Typed;
                self.raw_command = command_bytes(Command::RequestTelemetry);
                raw_mode = false;
            }
            if raw_mode && ui.button("重算 SUM").clicked() {
                recalculate_checksum(&mut self.raw_command);
            }
            let send_response = ui.add_enabled(
                can_send,
                egui::Button::new(RichText::new("发送指令").strong().color(theme::WHITE))
                    .min_size(egui::vec2(104.0, 28.0))
                    .fill(theme::PRIMARY),
            );
            debug_ui_rect("left.send_command", send_response.rect);
            if send_response.clicked() {
                if raw_mode {
                    match raw_frame_validation(&self.raw_command) {
                        Ok(()) => self.submit(UiRequest::SendCommand(self.raw_command)),
                        Err(reason) => {
                            self.push_fault("RS422 指令", format!("原始帧无效，未发送：{reason}"));
                        }
                    }
                } else {
                    match self.typed_command_bytes() {
                        Ok(frame) => self.submit(UiRequest::SendCommand(frame)),
                        Err(error) => {
                            self.push_fault("RS422 指令", format!("参数无效，指令未发送：{error}"));
                        }
                    }
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

    fn command_preview(&self, ui: &mut Ui, displayed_frame: Option<[u8; 12]>, raw_mode: bool) {
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
            } else if let Err(error) = self.typed_command_bytes() {
                ui.colored_label(theme::DANGER, error);
            }
        });
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

        let changed = self.telemetry_polling_interval_editor(ui);
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

    fn telemetry_polling_interval_editor(&mut self, ui: &mut Ui) -> bool {
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
                RichText::new(format!("最小 {TELEMETRY_POLL_INTERVAL_MIN_MILLIS} ms"))
                    .small()
                    .color(theme::TEXT_MUTED),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("最大 {TELEMETRY_POLL_INTERVAL_MAX_MILLIS} ms"))
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
        slider_changed || input_changed
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

    fn telemetry_empty_panel(ui: &mut Ui, #[cfg(feature = "mock")] mock_mode: bool) {
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
            if mock_mode {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("数据源：Mock 模式（等待模拟遥测）")
                        .small()
                        .color(theme::WARNING),
                );
            }
        });
    }

    fn telemetry_panel(&self, ui: &mut Ui) {
        let Some(data) = &self.telemetry else {
            Self::telemetry_empty_panel(
                ui,
                #[cfg(feature = "mock")]
                self.mock_mode,
            );
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

    fn fault_empty_notice(ui: &mut Ui) {
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
                Self::fault_empty_notice(ui);
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
        let normalized_before = self.overlay_normalized;
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
        if self.overlay_normalized != normalized_before {
            // Values change scale; fit Y again without taking away the user's
            // manually chosen time range or switching back to live follow.
            self.operator.plot_reset_pending = true;
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
                if ui.button("跟随最新").clicked() {
                    self.follow_latest();
                    ui.close();
                }
                if ui.button("重置视图").clicked() {
                    self.follow_latest();
                    ui.close();
                }
                if ui.button(freeze_label).clicked() {
                    self.toggle_display_freeze();
                    ui.close();
                }
                if ui.button("清空").clicked() {
                    self.clear_current_plot();
                    ui.close();
                }
            });
        } else {
            if ui
                .selectable_label(
                    self.operator.plot_view == PlotViewMode::FollowLatest,
                    "跟随最新",
                )
                .clicked()
            {
                self.follow_latest();
            }
            if ui.button("重置视图").clicked() {
                self.follow_latest();
            }
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
                self.clear_current_plot();
            }
        }
    }

    fn plot_toolbar(&mut self, ui: &mut Ui) {
        let width = ui.available_width();
        let compact = width < 560.0;
        let ultra_compact = width < 360.0;
        let range = self.plot_range();
        let (one, two) = self.display_histories();
        let cache = format!(
            "视图 {:.1}s / 历史 {:.0}s · {}/{} 点",
            range[1] - range[0],
            PLOT_HISTORY_SECONDS,
            one.len(),
            two.len()
        );
        central_control_style(ui);
        ui.spacing_mut().item_spacing.y = 8.0;
        central_control_row(ui, |ui| {
            if ultra_compact {
                ui.strong("波形");
                self.plot_toolbar_channels_menu(ui);
                self.plot_toolbar_modes(ui, true);
            } else {
                if compact {
                    ui.strong("波形");
                }
                self.plot_toolbar_channels(ui, compact, !compact);
                self.plot_toolbar_modes(ui, compact);
            }
        });
        central_control_row(ui, |ui| {
            self.plot_toolbar_actions(ui, ultra_compact);
            ui.add_sized(
                    [ui.available_width().max(0.0), CENTRAL_CONTROL_HEIGHT],
                    egui::Label::new(RichText::new(&cache).small().color(theme::TEXT_MUTED))
                        .truncate(),
                ).on_hover_text(format!("{cache}\n左键拖动平移 · 滚轮围绕鼠标缩放 · 移动鼠标查看电压 (V)\n手动视图不会被新数据抢回；跟随最新恢复实时窗口。"));
        });
    }

    #[expect(
        clippy::too_many_lines,
        reason = "overlay and split plots share rendering logic"
    )]
    fn plot_panel(&mut self, ui: &mut Ui) {
        let prepare_start = Instant::now();
        debug_ui_rect("central.plot_body_inner", ui.available_rect_before_wrap());
        let mut range = self.plot_range();
        let normalized = self.plot_layout == PlotLayout::Overlay && self.overlay_normalized;
        let show_channels = self.show_channels;
        let pixel_width = self
            .operator
            .plot_width_points
            .unwrap_or(ui.available_width())
            * ui.ctx().pixels_per_point();
        let point_limit =
            f32::from(u16::try_from(MAX_DISPLAY_POINTS).expect("the display point cap fits u16"));
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the integral physical pixel budget is clamped to 128..2000 before conversion"
        )]
        let budget = (pixel_width * 2.0).ceil().clamp(128.0, point_limit) as usize;
        let (channel_1, channel_2) = self.display_histories_mut();
        channel_1.prepare_visible(range, budget, normalized);
        channel_2.prepare_visible(range, budget, normalized);
        let rendered = [
            if show_channels[0] {
                channel_1.visible_points().len()
            } else {
                0
            },
            if show_channels[1] {
                channel_2.visible_points().len()
            } else {
                0
            },
        ];
        self.measurements.prepare_ms = prepare_start.elapsed().as_secs_f64() * 1000.0;
        self.measurements.rendered = rendered;

        // Reserve the hover readout row so cursor changes never move the plot
        // or the fixed log dock.
        let readout_row_height = if ui.available_height() < 100.0 {
            16.0
        } else {
            20.0
        };
        let readout_height = ui.available_height().clamp(0.0, readout_row_height);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width().max(0.0), readout_height),
            Layout::top_down(Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if let Some(cursor) = self.operator.plot_cursor_seconds {
                    let (one, two) = self.display_histories();
                    let values = [one.nearest_value(cursor), two.nearest_value(cursor)];
                    let value_text = |value: Option<f64>| {
                        value.map_or_else(|| "—".to_owned(), |v| format!("{v:.2}"))
                    };
                    ui.add_sized(
                        [ui.available_width(), readout_row_height],
                        egui::Label::new(
                            RichText::new(format!(
                                "{} · 设备时间 {} s  通道1 {} V  通道2 {} V",
                                self.displayed_quality_label(),
                                format_plot_timestamp(cursor, 0.0),
                                value_text(values[0]),
                                value_text(values[1])
                            ))
                            .monospace()
                            .small()
                            .color(theme::TEXT_SECONDARY),
                        )
                        .truncate(),
                    );
                } else {
                    ui.add_sized(
                        [ui.available_width(), readout_row_height],
                        egui::Label::new(
                            RichText::new(self.displayed_quality_label())
                                .small()
                                .color(theme::TEXT_MUTED),
                        )
                        .truncate(),
                    );
                }
            },
        );
        let height = ui.available_height().max(0.0);
        let (one, two) = self.display_histories();
        let has_history = (self.show_channels[0] && !one.is_empty())
            || (self.show_channels[1] && !two.is_empty());
        if !has_history {
            let empty_rect = ui.available_rect_before_wrap();
            debug_ui_rect("central.plot_empty_state", empty_rect);
            let hidden = !self.show_channels[0] && !self.show_channels[1];
            let state = if hidden {
                "通道 1 和通道 2 当前均已隐藏"
            } else if self.lvds_view.history.status.active {
                "可调整时间窗或数据质量筛选"
            } else {
                "请开始 UDP 监听或离线 PCAP 回放"
            };
            if empty_rect.height() < 50.0 {
                ui.add_sized(
                    empty_rect.size(),
                    egui::Label::new(RichText::new(state).small().color(theme::TEXT_MUTED))
                        .truncate(),
                );
                return;
            }
            // Align one measured two-line group within the remaining plot
            // body. No spacer widgets participate in the plot's input rect.
            let group_height = ui.text_style_height(&egui::TextStyle::Body)
                + ui.text_style_height(&egui::TextStyle::Small)
                + ui.spacing().item_spacing.y;
            let group = egui::Align2::CENTER_CENTER
                .align_size_within_rect(egui::vec2(empty_rect.width(), group_height), empty_rect);
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(group)
                    .layout(Layout::top_down(Align::Center)),
                |ui| {
                    ui.label(
                        RichText::new(if hidden {
                            "请选择显示通道"
                        } else {
                            "暂无 LVDS 数据"
                        })
                        .strong()
                        .color(theme::TEXT_SECONDARY),
                    );
                    ui.label(RichText::new(state).small().color(theme::TEXT_MUTED));
                },
            );
            ui.advance_cursor_after_rect(empty_rect);
            return;
        }

        let mut manual = self.operator.plot_view == PlotViewMode::ManualView;
        let mut cursor = self.operator.plot_cursor_seconds;
        let reset = self.operator.plot_reset_pending;
        let overlay = self.plot_layout == PlotLayout::Overlay;
        let mut return_to_follow = false;
        let mut plot_width = ui.available_width();
        let (one, two) = self.display_histories();
        if overlay {
            let names = [
                overlay_channel_name(
                    "通道 1",
                    normalized.then(|| one.visible_value_range()).flatten(),
                ),
                overlay_channel_name(
                    "通道 2",
                    normalized.then(|| two.visible_value_range()).flatten(),
                ),
            ];
            let outcome = show_scientific_plot(
                ui,
                ScientificPlotConfig {
                    id: "scientific_plot_overlay",
                    height,
                    range,
                    reset,
                    normalized,
                    y_label: if normalized {
                        "独立归一化幅值（无量纲；实际电压见图例）"
                    } else {
                        "电压 (V)"
                    },
                    channels: show_channels,
                    names: [&names[0], &names[1]],
                },
                one.visible_points(),
                two.visible_points(),
                cursor,
            );
            range = outcome.range;
            manual |= outcome.interacted;
            cursor = outcome.cursor.or(cursor);
            return_to_follow |= outcome.reset;
            plot_width = outcome.width;
        } else {
            let count = usize::from(show_channels[0]) + usize::from(show_channels[1]);
            let split_height = if count == 2 {
                ((height - ui.spacing().item_spacing.y) / 2.0).max(1.0)
            } else {
                height
            };
            for (channel, visible) in show_channels.iter().copied().enumerate() {
                if !visible {
                    continue;
                }
                let outcome = show_scientific_plot(
                    ui,
                    ScientificPlotConfig {
                        id: if channel == 0 {
                            "scientific_plot_channel_1"
                        } else {
                            "scientific_plot_channel_2"
                        },
                        height: split_height,
                        range,
                        reset,
                        normalized: false,
                        y_label: if channel == 0 {
                            "通道 1 电压 (V)"
                        } else {
                            "通道 2 电压 (V)"
                        },
                        channels: [channel == 0, channel == 1],
                        names: ["通道 1", "通道 2"],
                    },
                    one.visible_points(),
                    two.visible_points(),
                    cursor,
                );
                range = outcome.range;
                manual |= outcome.interacted;
                cursor = outcome.cursor.or(cursor);
                return_to_follow |= outcome.reset;
                plot_width = plot_width.min(outcome.width);
            }
        }
        self.operator.plot_reset_pending = false;
        self.operator.plot_cursor_seconds = cursor;
        self.operator.plot_width_points = Some(plot_width);
        if manual {
            self.operator.plot_view = PlotViewMode::ManualView;
            self.operator.plot_x_range = Some(range);
        }
        if return_to_follow {
            self.follow_latest();
        }
        self.request_visible_history_window(range);
    }

    fn operations_panel(&mut self, ui: &mut Ui) {
        let mut request = None;
        let compact = ui.available_height() < 200.0;
        let inner_size = ui.available_size() - egui::Vec2::splat(if compact { 18.0 } else { 26.0 });
        theme::card()
            .inner_margin(egui::Margin::same(if compact { 8 } else { 12 }))
            .show(ui, |ui| {
                ui.set_min_size(inner_size.max(egui::Vec2::ZERO));
                debug_ui_rect("central.operations_card", ui.available_rect_before_wrap());
                let width = ui.available_width().max(0.0);
                central_control_style(ui);
                ui.spacing_mut().item_spacing.y = if compact { 4.0 } else { 12.0 };
                if self.lvds_view.history.status.active {
                    self.history_inline_controls(ui, &mut request);
                } else if compact {
                    central_control_row(ui, |ui| self.lvds_listen_inputs(ui));
                    central_control_row(ui, |ui| {
                        if width < 320.0 {
                            ui.menu_button("操作", |ui| {
                                self.lvds_listen_buttons(ui, &mut request, false);
                                self.lvds_save_button(ui, &mut request, false);
                            });
                        } else {
                            self.lvds_listen_buttons(ui, &mut request, true);
                        }
                        if ui
                            .button(if width < 320.0 {
                                "回放"
                            } else {
                                "离线回放 PCAP"
                            })
                            .clicked()
                        {
                            self.lvds_view.replay_dialog_open = true;
                        }
                        if width >= 320.0 {
                            self.lvds_save_button(ui, &mut request, true);
                        }
                        self.history_open_button(ui, true);
                        self.lvds_more_menu(ui, &mut request);
                    });
                    self.lvds_receive_status(ui, true);
                } else {
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        central_control_row(ui, |ui| {
                            self.lvds_listen_inputs(ui);
                            if width >= 540.0 {
                                self.lvds_listen_buttons(ui, &mut request, false);
                            }
                        });
                        if width < 540.0 {
                            central_control_row(ui, |ui| {
                                self.lvds_listen_buttons(ui, &mut request, false);
                            });
                            ui.add_space(8.0);
                        }
                        self.lvds_receive_status(ui, false);
                    });
                    central_control_row(ui, |ui| {
                        if ui.button("离线回放 PCAP").clicked() {
                            self.lvds_view.replay_dialog_open = true;
                        }
                        self.history_open_button(ui, false);
                        self.raw_record_button(ui, &mut request, false);
                    });
                    central_control_row(ui, |ui| {
                        if width >= 440.0 {
                            self.scientific_save_format(ui, "capture");
                        }
                        self.lvds_save_button(ui, &mut request, false);
                        self.lvds_more_menu(ui, &mut request);
                        let text = self.lvds_save_summary();
                        ui.add_sized(
                            [ui.available_width().max(0.0), CENTRAL_CONTROL_HEIGHT],
                            egui::Label::new(RichText::new(&text).small().monospace()).truncate(),
                        )
                        .on_hover_text(format!("{text}\n显示保存进度及文件状态。"));
                    });
                }
                if !self.lvds_view.history.status.active {
                    self.lvds_quality_grid(ui);
                }
            });
        if let Some(request) = request {
            self.submit(request);
        }
    }

    fn lvds_listen_inputs(&mut self, ui: &mut Ui) {
        if ui.available_width() >= 300.0 {
            ui.strong("LVDS / UDP");
        }
        ui.add_enabled_ui(!self.lvds.connected, |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let ip = ui.add_sized(
                [124.0, CENTRAL_CONTROL_HEIGHT],
                egui::TextEdit::singleline(&mut self.lvds.bind_address)
                    .font(egui::TextStyle::Button)
                    .vertical_align(Align::Center)
                    .hint_text("192.168.1.100"),
            );
            debug_ui_rect("central.operations.ip_input", ip.rect);
            ip.on_hover_text("本地监听 IPv4：必须已配置在本机网卡上；手动填写，不会自动替换地址。");
            let port = ui.add_sized(
                [48.0, CENTRAL_CONTROL_HEIGHT],
                egui::DragValue::new(&mut self.lvds.port).range(1..=u16::MAX),
            );
            debug_ui_rect("central.operations.port_input", port.rect);
            port.on_hover_text("本地 UDP 监听端口");
        });
    }

    fn lvds_listen_buttons(&self, ui: &mut Ui, request: &mut Option<UiRequest>, short: bool) {
        let start = ui.add_enabled(
            !self.lvds.connected,
            egui::Button::new(if short { "监听" } else { "开始监听" }).min_size(egui::vec2(
                if short { 44.0 } else { 80.0 },
                CENTRAL_CONTROL_HEIGHT,
            )),
        );
        debug_ui_rect("central.operations.connection_button", start.rect);
        if start.clicked() {
            *request = Some(UiRequest::BindLvdsUdp {
                bind_address: self.lvds.bind_address.trim().to_owned(),
                port: self.lvds.port,
            });
        }
        if ui
            .add_enabled(
                self.lvds.connected,
                egui::Button::new(if short { "停止" } else { "停止监听" }).min_size(egui::vec2(
                    if short { 44.0 } else { 80.0 },
                    CENTRAL_CONTROL_HEIGHT,
                )),
            )
            .clicked()
        {
            *request = Some(UiRequest::Disconnect {
                link: LinkKind::Lvds,
            });
        }
    }

    fn lvds_save_button(&self, ui: &mut Ui, request: &mut Option<UiRequest>, short: bool) {
        let saving = self.acquisition_running;
        let label = if saving {
            "停止保存"
        } else {
            "开始保存"
        };
        let response = ui.add_sized(
            [if short { 68.0 } else { 80.0 }, CENTRAL_CONTROL_HEIGHT],
            egui::Button::new(RichText::new(label).color(theme::WHITE)).fill(if saving {
                theme::DANGER
            } else {
                theme::SUCCESS
            }),
        );
        debug_ui_rect("central.operations.acquisition_button", response.rect);
        if response.on_hover_text(self.lvds_save_summary()).clicked() {
            *request = Some(if saving {
                UiRequest::StopAcquisition
            } else {
                UiRequest::StartAcquisition {
                    format: self.lvds_view.save_format,
                }
            });
        }
    }

    fn lvds_save_summary(&self) -> String {
        let storage = self.lvds_view.status.as_ref().map(|s| &s.storage);
        let state = if storage.is_some_and(|s| s.error.is_some()) {
            "保存错误"
        } else if self.acquisition_running {
            "保存中"
        } else if storage.is_some_and(|s| s.closed) {
            "已关闭"
        } else {
            "未保存"
        };
        let summary = format!(
            "{state} · 入队 {} / 写入 {}",
            storage.map_or(0, |s| s.enqueued),
            storage.map_or(0, |s| s.sink_written)
        );
        if let Some(storage) = storage
            && matches!(
                storage.format,
                ScientificSaveFormat::Csv | ScientificSaveFormat::LpdAndCsv
            )
        {
            return format!("{summary} · CSV 行 {}", storage.csv_rows_written);
        }
        summary
    }

    fn scientific_save_format(&mut self, ui: &mut Ui, location: &'static str) {
        ui.label("保存格式");
        ui.add_enabled_ui(!self.acquisition_running, |ui| {
            let response = ComboBox::from_id_salt(("scientific_save_format", location))
                .width(100.0)
                .selected_text(self.lvds_view.save_format.label())
                .show_ui(ui, |ui| {
                    for format in [
                        ScientificSaveFormat::Csv,
                        ScientificSaveFormat::Lpd,
                        ScientificSaveFormat::LpdAndCsv,
                    ] {
                        ui.selectable_value(
                            &mut self.lvds_view.save_format,
                            format,
                            format.label(),
                        );
                    }
                    #[cfg(any(feature = "hdf5", feature = "hdf5-static"))]
                    ui.add_enabled(false, egui::Button::new("HDF5（暂不可用）"))
                        .on_hover_text("当前数据格式支持 CSV、LPD 和 LPD + CSV。");
                });
            debug_ui_rect("central.operations.save_format", response.response.rect);
            response
                .response
                .on_hover_text("选择科学数据保存格式；保存结束后可修改格式。");
        });
    }

    fn lvds_more_menu(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        ui.menu_button("更多", |ui| {
            ui.set_max_width(480.0);
            ui.strong("科学数据保存");
            ui.horizontal(|ui| self.scientific_save_format(ui, "more"));
            if let Some(status) = &self.lvds_view.status {
                scientific_storage_status(ui, status);
            } else {
                ui.label(self.lvds_save_summary());
            }
            ui.separator();
            self.raw_record_button(ui, request, true);
            self.recovery_controls(ui, request);
            if ui.button("历史与原始数据设置").clicked() {
                self.lvds_view.history.dialog_open = true;
                ui.close();
            }
            ui.separator();
            if ui.button("刷新串口").clicked() {
                *request = Some(UiRequest::RefreshPorts);
                ui.close();
            }
            if ui.button("导出 RS422 遥测 (CSV)").clicked() {
                *request = Some(UiRequest::ExportCsv {
                    suggested_name: format!(
                        "langmuier-probe-ground-control-{}.csv",
                        Local::now().format("%Y%m%d-%H%M%S")
                    ),
                });
                ui.close();
            }
            #[cfg(feature = "mock")]
            if self.mock_mode && ui.button("注入模拟异常").clicked() {
                *request = Some(UiRequest::InjectMockFault);
                ui.close();
            }
        });
    }

    fn lvds_receive_status(&self, ui: &mut Ui, compact: bool) {
        let raw = self
            .lvds_view
            .status
            .as_ref()
            .map_or(self.lvds.detail.as_str(), |s| s.receive_state.as_str());
        let mut summary = lvds_receive_summary(raw, self.lvds.connected).to_owned();
        let recording = &self.lvds_view.history.raw;
        if recording.error.is_some() || recording.dropped > 0 {
            summary = format!("原始录制异常 · 丢失 {} · {summary}", recording.dropped);
        } else if recording.path.is_some() {
            use std::fmt::Write as _;
            let _ = write!(
                summary,
                " · 原始 {} / {} · 丢失 {}{}",
                recording.written,
                recording.received,
                recording.dropped,
                if recording.error.is_some() {
                    " · 录制异常"
                } else if recording.active {
                    " · 录制中"
                } else if recording.closed {
                    " · 已关闭"
                } else {
                    ""
                }
            );
        }
        let detail = if compact {
            format!("{raw}\n{}", self.lvds_save_summary())
        } else {
            raw.to_owned()
        };
        let response = ui
            .allocate_ui_with_layout(
                egui::vec2(ui.available_width().max(0.0), 20.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.add_sized(
                        [ui.available_width().max(0.0), 20.0],
                        egui::Label::new(RichText::new(summary).small().color(
                            if recording.error.is_some()
                                || recording.dropped > 0
                                || raw.contains("异常")
                                || raw.contains("错误")
                                || raw.contains("address")
                            {
                                theme::WARNING
                            } else {
                                theme::TEXT_MUTED
                            },
                        ))
                        .truncate(),
                    )
                    .on_hover_text(&detail);
                },
            )
            .response;
        debug_ui_rect("central.operations.receive_status", response.rect);
    }

    fn lvds_quality_grid(&self, ui: &mut Ui) {
        let stats = self.lvds_view.status.as_ref().map(|s| &s.stats);
        let values = [
            stats.map_or(0, |s| s.udp_packets),
            stats.map_or(0, |s| s.parsed_frames),
            stats.map_or(0, |s| s.parse_failures),
            self.lvds_view.plotted_frames,
            self.lvds_view
                .status
                .as_ref()
                .map_or(0, |s| s.storage.sink_written),
        ];
        let labels = ["接收", "解析成功", "解析失败", "绘图", "保存"];
        let width = ui.available_width();
        ui.spacing_mut().item_spacing.y = 0.0;
        for is_value in [false, true] {
            ui.allocate_ui_with_layout(egui::vec2(width, if is_value { 22.0 } else { 16.0 }), Layout::left_to_right(Align::Center), |ui| {
                let cell = ((width - 32.0) / 5.0).max(0.0);
                for (label, value) in labels.into_iter().zip(values) {
                    let text = if is_value { RichText::new(value.to_string()).monospace().color(theme::TEXT_SECONDARY) }
                        else { RichText::new(label).small().color(theme::TEXT_MUTED) };
                    let response = ui.add_sized([cell, if is_value { 22.0 } else { 16.0 }], egui::Label::new(text).truncate());
                    response.on_hover_text(format!("{label}：{value}\n接收为匹配来源的数据包数；解析成功为解码帧数；保存为实际写入帧数。"));
                }
            });
        }
    }

    fn lvds_dialog(&mut self, context: &egui::Context) {
        if !self.lvds_view.replay_dialog_open {
            return;
        }
        let mut open = true;
        self.replay_dialog(context, &mut open);
        self.lvds_view.replay_dialog_open = open;
    }

    fn replay_dialog(&mut self, context: &egui::Context, open: &mut bool) {
        let mut request = None;
        egui::Window::new("数据回放 · PCAP / PCAPNG")
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .open(open)
            .show(context, |ui| {
                ui.label(
                    "选择 PCAP 或 PCAPNG 文件，按记录时间显示数据。开始回放将切换数据源并重新加载波形。",
                );
                ui.label("捕获文件路径");
                ui.add(
                    egui::TextEdit::singleline(&mut self.lvds_view.replay_path)
                        .desired_width(f32::INFINITY),
                );
                let replay_running = self
                    .lvds_view
                    .status
                    .as_ref()
                    .and_then(|status| status.replay.as_ref())
                    .is_some_and(|replay| !replay.finished);
                ui.add_enabled_ui(!self.acquisition_running && !replay_running, |ui| {
                    ui.checkbox(
                        &mut self.lvds_view.replay_save_science,
                        "回放时保存科学数据",
                    );
                    ui.horizontal(|ui| self.scientific_save_format(ui, "replay"));
                });
                if self.lvds_view.replay_save_science {
                    ui.label(
                        RichText::new(
                            "按所选格式保存科学数据，回放结束后自动关闭文件。",
                        )
                        .small()
                        .color(theme::TEXT_SECONDARY),
                    );
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !self.acquisition_running
                                && !replay_running
                                && !self.lvds_view.replay_path.trim().is_empty(),
                            egui::Button::new(if self.lvds_view.replay_save_science {
                                "开始回放并保存"
                            } else {
                                "开始回放"
                            }),
                        )
                        .clicked()
                    {
                        request = Some(UiRequest::StartPcapReplay {
                            path: self.lvds_view.replay_path.trim().to_owned(),
                            save: self
                                .lvds_view
                                .replay_save_science
                                .then_some(self.lvds_view.save_format),
                        });
                    }
                    if ui
                        .add_enabled(replay_running, egui::Button::new("停止回放"))
                        .clicked()
                    {
                        request = Some(UiRequest::StopPcapReplay);
                    }
                    if self.acquisition_running {
                        ui.colored_label(theme::WARNING, "请先停止保存，再切换数据源");
                    }
                });
                if let Some(status) = &self.lvds_view.status {
                    Self::replay_status(ui, status);
                }
            });
        if let Some(request) = request {
            self.submit(request);
        }
    }

    fn replay_status(ui: &mut Ui, status: &LvdsStatusSnapshot) {
        if let Some(replay) = &status.replay {
            ui.label(format!(
                "{} · 总包 {} · 目标 UDP {}",
                if replay.error.is_some() {
                    "回放失败"
                } else if replay.finished {
                    "已结束"
                } else {
                    "回放中"
                },
                replay.total_packets,
                replay.target_udp_packets
            ));
            ui.add(egui::Label::new(&replay.path).wrap());
            if let Some(error) = &replay.error {
                ui.add(egui::Label::new(RichText::new(error).color(theme::DANGER)).wrap());
            }
        } else {
            ui.label("就绪");
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
            .min_scrolled_height(0.0)
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
                        RichText::new(crate::APP_NAME)
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
                        "保存 · 运行中"
                    } else {
                        "保存 · 已停止"
                    },
                    self.acquisition_running,
                    self.scientific_data_is_fresh(),
                );
                header_link_status(
                    ui,
                    if !self.lvds.connected {
                        "LVDS · 未监听"
                    } else if self.scientific_data_is_fresh() {
                        "LVDS · 波形更新"
                    } else {
                        "LVDS · 监听中"
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
        let update_start = Instant::now();
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
                self.operations_panel(ui);
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
        self.lvds_dialog(ui.ctx());
        self.history_dialog(ui.ctx());

        ui.ctx().request_repaint_after(
            if self.acquisition_running
                || self.scientific_data_is_fresh()
                || self.lvds_view.history.status.playing
                || self.lvds_view.history.status.loading
            {
                // Drawing cadence is independent of the 20 Hz receive cadence.
                Duration::from_millis(16)
            } else {
                // Telemetry is event-driven and does not need a high-rate repaint loop.
                Duration::from_millis(250)
            },
        );
        self.measurements.record(
            update_start,
            [self.channel_1.len(), self.channel_2.len()],
            self.lvds_view.plotted_frames,
        );
    }
}

fn scientific_plot_buffer() -> TimeSeriesBuffer {
    TimeSeriesBuffer::with_history(
        PLOT_CAPACITY,
        PLOT_HISTORY_SECONDS,
        PLOT_WINDOW_SECONDS,
        MAX_DISPLAY_POINTS,
    )
}

#[derive(Clone, Copy)]
struct ScientificPlotConfig<'a> {
    id: &'static str,
    height: f32,
    range: [f64; 2],
    reset: bool,
    normalized: bool,
    y_label: &'static str,
    channels: [bool; 2],
    names: [&'a str; 2],
}

struct ScientificPlotOutcome {
    range: [f64; 2],
    cursor: Option<f64>,
    interacted: bool,
    reset: bool,
    width: f32,
}

fn show_scientific_plot(
    ui: &mut Ui,
    config: ScientificPlotConfig<'_>,
    channel_1: &[PlotPoint],
    channel_2: &[PlotPoint],
    cursor: Option<f64>,
) -> ScientificPlotOutcome {
    let plot = configured_scientific_plot(&config);
    let response = plot.show(ui, |plot_ui| {
        let hovered = plot_ui.response().contains_pointer();
        let (wheel, zoom) = plot_ui.ctx().input(|input| {
            (
                if hovered {
                    input.smooth_scroll_delta.y
                } else {
                    0.0
                },
                if hovered {
                    input.zoom_delta_2d()
                } else {
                    egui::Vec2::splat(1.0)
                },
            )
        });
        let dragging = plot_ui.response().dragged();
        let interacted = dragging || wheel != 0.0 || zoom != egui::Vec2::splat(1.0);
        // This is the shared X range for overlay and split plots. A
        // native pan or zoom runs after this base range is applied.
        plot_ui.set_plot_bounds_x(config.range[0]..=config.range[1]);
        if config.reset {
            plot_ui.set_auto_bounds([false, true]);
        }
        if wheel != 0.0 && zoom == egui::Vec2::splat(1.0) {
            let factor = (wheel * 0.005).clamp(-0.5, 0.5).exp();
            plot_ui.zoom_bounds_around_hovered(egui::Vec2::splat(factor));
        }
        let pointer = hovered
            .then(|| plot_ui.pointer_coordinate())
            .flatten()
            .map(|p| p.x);
        if let Some(position) = pointer.or(cursor) {
            plot_ui.vline(
                VLine::new("游标", position)
                    .color(theme::WARNING)
                    .width(1.0),
            );
        }
        for (index, points) in [channel_1, channel_2].into_iter().enumerate() {
            if config.channels[index] && !points.is_empty() {
                plot_ui.line(
                    Line::new(config.names[index], PlotPoints::Borrowed(points)).color(
                        if index == 0 {
                            theme::PRIMARY
                        } else {
                            theme::DANGER
                        },
                    ),
                );
            }
        }
        (pointer, interacted, plot_ui.response().double_clicked())
    });
    let bounds = response.transform.bounds();
    debug_ui_rect(config.id, response.response.rect);
    debug_ui_rect(
        if config.id == "scientific_plot_overlay" {
            "plot.overlay.frame"
        } else if config.id == "scientific_plot_channel_1" {
            "plot.channel_1.frame"
        } else {
            "plot.channel_2.frame"
        },
        *response.transform.frame(),
    );
    if ui_rect_debug_enabled() && response.inner.1 {
        eprintln!(
            "PLOT_VIEW ManualView epoch={:.6} id={} x=[{:.6},{:.6}] y=[{:.6},{:.6}]",
            plot_debug_epoch_seconds(),
            config.id,
            bounds.min()[0],
            bounds.max()[0],
            bounds.min()[1],
            bounds.max()[1]
        );
    }
    ScientificPlotOutcome {
        range: [bounds.min()[0], bounds.max()[0]],
        cursor: response.inner.0,
        interacted: response.inner.1,
        reset: response.inner.2,
        width: response.transform.frame().width(),
    }
}

fn configured_scientific_plot(config: &ScientificPlotConfig<'_>) -> Plot<'static> {
    let normalized = config.normalized;
    let plot = Plot::new(config.id)
        .min_size(egui::vec2(1.0, 1.0))
        .show_axes([config.height >= 100.0; 2])
        .x_axis_label("设备时间 (s)")
        .y_axis_label(config.y_label)
        .x_axis_formatter(|mark, range| {
            format_plot_timestamp(mark.value, range.end() - range.start())
        })
        .y_axis_formatter(|mark, _| format!("{:.2}", mark.value))
        .label_formatter(move |hover| {
            let (name, position) = match *hover {
                egui_plot::HoverPosition::NearDataPoint {
                    plot_name,
                    position,
                    ..
                } => (plot_name, position),
                egui_plot::HoverPosition::Elsewhere { position } => ("", position),
            };
            let ordinate = if normalized {
                format!("归一化幅值 {:.2}（无量纲）", position.y)
            } else {
                format!("电压 {:.2} V", position.y)
            };
            Some(format!(
                "{name}\n设备时间 {} s\n{ordinate}",
                format_plot_timestamp(position.x, 0.0)
            ))
        })
        .allow_drag(true)
        .pan_pointer_button(PointerButton::Primary)
        .allow_scroll(false)
        .allow_zoom(true)
        .allow_axis_zoom_drag(false)
        .allow_double_click_reset(false)
        .allow_boxed_zoom(true)
        .auto_bounds([false, true])
        .height(config.height);
    if config.height >= 100.0 {
        plot.legend(Legend::default())
    } else {
        plot
    }
}

fn extend_scientific_channel(
    buffer: &mut TimeSeriesBuffer,
    start_seconds: f64,
    sample_interval_seconds: f64,
    values: Vec<f64>,
) {
    buffer.extend(
        values
            .into_iter()
            .scan(start_seconds, |time_seconds, value| {
                let point = SamplePoint {
                    time_seconds: *time_seconds,
                    // Convert reconstructed ADC16 once at display ingress.
                    // Cached extrema, history and cursors now all use volts.
                    value: adc16_to_volts(value),
                };
                *time_seconds += sample_interval_seconds;
                Some(point)
            }),
    );
}

fn scientific_storage_status(ui: &mut Ui, status: &LvdsStatusSnapshot) {
    let storage = &status.storage;
    ui.label(format!(
        "LVDS 科学保存 ({})：入队 {} 帧 · 实际写入 {} 帧",
        storage.format.label(),
        storage.enqueued,
        storage.sink_written,
    ));
    if matches!(
        storage.format,
        ScientificSaveFormat::Csv | ScientificSaveFormat::LpdAndCsv
    ) {
        ui.label(format!(
            "科学 CSV：实际写入 {} 帧 · CSV 数据行 {}",
            storage.csv_frames_written, storage.csv_rows_written,
        ));
    }
    if matches!(
        storage.format,
        ScientificSaveFormat::Lpd | ScientificSaveFormat::LpdAndCsv
    ) {
        ui.label(format!("LPD：实际写入 {} 帧", storage.lpd_frames_written));
    }
    ui.label(format!(
        "文件状态：{}",
        if storage.error.is_some() {
            "错误 / 捕获不完整"
        } else if storage.closed {
            "已关闭"
        } else if storage.path.is_some() {
            "写入中"
        } else {
            "未开始"
        }
    ));
    ui.label(
        RichText::new("文件路径和保存状态如下。")
            .small()
            .color(theme::TEXT_SECONDARY),
    );
    for (label, path) in [
        ("科学 CSV", storage.csv_path.as_ref()),
        ("LPD", storage.lpd_path.as_ref()),
        (
            "文件",
            storage
                .path
                .as_ref()
                .filter(|_| storage.csv_path.is_none() && storage.lpd_path.is_none()),
        ),
    ] {
        if let Some(path) = path {
            ui.add(egui::Label::new(format!("{label}：{path}")).wrap())
                .on_hover_text(path);
        }
    }
    if let Some(error) = &storage.error {
        ui.colored_label(theme::DANGER, error);
    }
}

fn ui_rect_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os(UI_RECT_DEBUG_ENV).is_some())
}

fn plot_debug_epoch_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

#[cfg(test)]
#[path = "app_plot_tests.rs"]
mod plot_tests;

/// Logs a region once and again only when its geometry moves by more than one
/// logical point. This makes a 60-second Mock run useful for detecting layout
/// jumps without flooding the log on every repaint.
fn debug_ui_rect(label: &'static str, rect: egui::Rect) {
    static LAST: OnceLock<Mutex<HashMap<&'static str, [f32; 4]>>> = OnceLock::new();
    if !ui_rect_debug_enabled() {
        return;
    }
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

fn central_control_style(ui: &mut Ui) {
    ui.spacing_mut().interact_size.y = CENTRAL_CONTROL_HEIGHT;
    ui.spacing_mut().button_padding = egui::vec2(8.0, 5.0);
    ui.spacing_mut().item_spacing.x = 8.0;
}

fn central_control_row<R>(ui: &mut Ui, contents: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width().max(0.0), CENTRAL_CONTROL_HEIGHT),
        Layout::left_to_right(Align::Center),
        contents,
    )
    .inner
}

/// Presentation only: retain the complete socket error in the status tooltip.
fn lvds_receive_summary(raw: &str, connected: bool) -> &str {
    let address_code = if cfg!(target_os = "macos") {
        "(os error 49)"
    } else if cfg!(target_os = "linux") {
        "(os error 99)"
    } else if cfg!(windows) {
        "(os error 10049)"
    } else {
        ""
    };
    if !connected
        && (raw.contains("Can't assign requested address")
            || raw.contains("Cannot assign requested address")
            || (!address_code.is_empty() && raw.contains(address_code)))
    {
        "监听失败：本机未配置该 IP 地址"
    } else {
        raw
    }
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
    // Presets depend only on the assigned viewport, never on status strings,
    // counters or save state. The short preset keeps 32 pt controls reachable.
    let compact = root.height() < CENTRAL_SHORT_HEIGHT || root.width() < 320.0;
    let operations = if compact {
        CENTRAL_COMPACT_OPERATIONS_HEIGHT
    } else if root.width() < 566.0 {
        CENTRAL_OPERATIONS_HEIGHT + 40.0
    } else {
        CENTRAL_OPERATIONS_HEIGHT
    };
    let logs = if compact {
        CENTRAL_COMPACT_LOG_HEIGHT
    } else {
        CENTRAL_LOG_HEIGHT
    };
    let normal_fixed = operations + CENTRAL_PLOT_TOOLBAR_HEIGHT + logs;
    let scale = (available / normal_fixed).min(1.0);
    let operations_height = operations * scale;
    let toolbar_height = CENTRAL_PLOT_TOOLBAR_HEIGHT * scale;
    let logs_height = logs * scale;
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

fn frequency_parameter(ui: &mut Ui, value: &mut u8, edit: &mut IntegerParameterEdit) {
    parameter_header(ui, "扫描频率设定值", u16::from(*value));
    let response = full_width_integer_slider(
        ui,
        value,
        ScanFrequency::MIN..=ScanFrequency::MAX,
        "left.scan_frequency_slider",
    )
    .on_hover_text("扫描频率：0..=100；拖动或键盘逐整数调整，精确输入按 Enter 确认");
    if response.changed() {
        edit.set_confirmed(u16::from(*value));
    }
    parameter_limits(ui, u16::from(ScanFrequency::MAX));
    let mut edited = u16::from(*value);
    integer_parameter_actions(
        ui,
        &mut edited,
        edit,
        u16::from(ScanFrequency::MAX),
        &[1, 50, u16::from(ScanFrequency::MAX)],
        "left.scan_frequency_input",
    );
    *value = u8::try_from(edited).expect("frequency editor validates the u8 protocol range");
}

fn parameter_header(ui: &mut Ui, title: &str, value: u16) {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), theme::CONTROL_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.label(RichText::new(title).small().color(theme::TEXT_SECONDARY));
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
}

fn full_width_integer_slider<Num: egui::emath::Numeric>(
    ui: &mut Ui,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
    debug_label: &'static str,
) -> egui::Response {
    let parent_rect = ui.available_rect_before_wrap();
    let slider_width = frequency_slider_width(parent_rect.width());
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
                egui::Slider::new(value, range)
                    .show_value(false)
                    .step_by(1.0)
                    .smart_aim(false)
                    .trailing_fill(true),
            )
        })
        .inner;
    debug_ui_rect(debug_label, slider_response.rect);
    debug_slider_geometry(
        debug_label,
        parent_rect,
        slider_response.rect,
        slider_width,
        true,
    );
    slider_response
}

fn parameter_limits(ui: &mut Ui, maximum: u16) {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), 20.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.label(RichText::new("最小 0").small().color(theme::TEXT_MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("最大 {maximum}"))
                        .small()
                        .color(theme::TEXT_MUTED),
                );
            });
        },
    );
}

fn integer_parameter_actions(
    ui: &mut Ui,
    value: &mut u16,
    edit: &mut IntegerParameterEdit,
    maximum: u16,
    presets: &[u16],
    debug_label: &'static str,
) {
    let compact = ui.available_width() < 260.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.label(
            RichText::new("快速设置")
                .small()
                .color(theme::TEXT_SECONDARY),
        );
        for &preset in presets {
            if ui
                .selectable_label(*value == preset, format!("{preset}"))
                .clicked()
            {
                *value = preset;
                edit.set_confirmed(preset);
            }
        }
        if !compact {
            ui.separator();
            let _ = integer_parameter_input(ui, value, edit, maximum, debug_label);
        }
    });
    if compact {
        ui.horizontal(|ui| {
            let _ = integer_parameter_input(ui, value, edit, maximum, debug_label);
        });
    }
    let feedback = edit
        .error
        .as_deref()
        .unwrap_or("精确输入按 Enter 或移开焦点确认");
    ui.add(
        egui::Label::new(
            RichText::new(feedback)
                .size(9.0)
                .color(if edit.error.is_some() {
                    theme::DANGER
                } else {
                    theme::TEXT_MUTED
                }),
        )
        .wrap(),
    );
}

fn integer_parameter_input(
    ui: &mut Ui,
    value: &mut u16,
    edit: &mut IntegerParameterEdit,
    maximum: u16,
    debug_label: &'static str,
) -> egui::Response {
    ui.label(
        RichText::new("精确输入")
            .small()
            .color(theme::TEXT_SECONDARY),
    );
    let response = ui.add_sized(
        [64.0, theme::CONTROL_HEIGHT],
        egui::TextEdit::singleline(&mut edit.text)
            .id_salt(debug_label)
            .font(egui::TextStyle::Monospace)
            .desired_width(64.0),
    );
    debug_ui_rect(debug_label, response.rect);
    if response.changed() {
        edit.dirty = true;
        edit.error = None;
    }
    if (response.lost_focus()
        || (response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))))
        && let Some(confirmed) = edit.confirm(maximum)
    {
        *value = confirmed;
    }
    response
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
    // Geometry diagnostics should not add continuous I/O to a live plot.
    static LAST: OnceLock<Mutex<HashMap<&'static str, [u32; 5]>>> = OnceLock::new();
    if !ui_rect_debug_enabled() {
        return;
    }
    let geometry = [
        parent_rect.width().to_bits(),
        configured_width.to_bits(),
        response_rect.width().to_bits(),
        response_rect.height().to_bits(),
        u32::from(enabled),
    ];
    let Ok(mut last) = LAST.get_or_init(|| Mutex::new(HashMap::new())).lock() else {
        return;
    };
    if last.get(label) == Some(&geometry) {
        return;
    }
    last.insert(label, geometry);
    eprintln!(
        "UI_SLIDER {label}: parent_w={:.1} configured_w={:.1} response_w={:.1} response_h={:.1} rail_h=6.0 enabled={enabled}",
        parent_rect.width(),
        configured_width,
        response_rect.width(),
        response_rect.height(),
    );
}

fn integer_parameter_u16(
    ui: &mut Ui,
    value: &mut u16,
    edit: &mut IntegerParameterEdit,
    presets: &[u16],
) {
    parameter_header(ui, "原始电压设定值", *value);
    let response = full_width_integer_slider(ui, value, 0..=u16::MAX, "left.dc_voltage_slider")
        .on_hover_text("协议原始 u16：0..=65535；协议未定义物理电压单位");
    if response.changed() {
        edit.set_confirmed(*value);
    }
    parameter_limits(ui, u16::MAX);
    integer_parameter_actions(ui, value, edit, u16::MAX, presets, "left.dc_voltage_input");
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

#[cfg(test)]
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
        Some((min, max)) => format!("{name} [{min:.2}, {max:.2}] V"),
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
    // The device epoch is unconfirmed. At fine zoom (and for cursor readout)
    // show microseconds so consecutive 500 us samples remain distinguishable.
    if visible_seconds < 1.0 {
        format!("{seconds:.6}")
    } else if visible_seconds < 10.0 {
        format!("{seconds:.4}")
    } else {
        format!("{seconds:.3}")
    }
}

#[cfg(test)]
#[path = "app_parameter_tests.rs"]
mod parameter_tests;

#[cfg(test)]
mod tests {
    use eframe::egui::{
        Context, Event, FullOutput, Modifiers, PointerButton, Pos2, RawInput, Rect, Shape, pos2,
        vec2,
    };
    use langmuir_core::Command;

    use langmuir_core::SAMPLE_COUNT;

    use crate::bridge::{
        BackendEndpoint, BackendEvent, LvdsStatusSnapshot, UiRequest, channel_bridge,
    };

    use super::{
        CENTRAL_LOG_HEIGHT, CENTRAL_OPERATIONS_HEIGHT, CENTRAL_PLOT_TOOLBAR_HEIGHT,
        COMMAND_SIDEBAR_MAX_WIDTH, COMMAND_SIDEBAR_MIN_WIDTH, COMMAND_SIDEBAR_WIDTH,
        COMPACT_COMMAND_SIDEBAR_WIDTH, COMPACT_LAYOUT_BREAKPOINT, COMPACT_TELEMETRY_SIDEBAR_WIDTH,
        HEADER_COMPACT_BREAKPOINT, HEADER_COMPACT_HEIGHT, HEADER_HEIGHT, LangmuirDesktopApp,
        TELEMETRY_SIDEBAR_MAX_WIDTH, TELEMETRY_SIDEBAR_MIN_WIDTH, TELEMETRY_SIDEBAR_WIDTH,
        central_layout_rects, command_bytes, command_checksum, fixed_panel_size,
        frequency_slider_width, hex_bytes, nearest_ui_scale, normalize_plot_points,
        polling_slider_width, raw_frame_validation, recalculate_checksum, telemetry_preset_active,
        work_mode_name,
    };

    fn lvds_test_app() -> (LangmuirDesktopApp, BackendEndpoint) {
        let (bridge, endpoint) = channel_bridge(16);
        let app = LangmuirDesktopApp::with_bridge(
            Box::new(bridge),
            None,
            #[cfg(feature = "mock")]
            false,
        );
        assert_eq!(
            endpoint.request_rx.try_recv().unwrap(),
            UiRequest::RefreshPorts
        );
        (app, endpoint)
    }

    fn status(source: &str) -> LvdsStatusSnapshot {
        LvdsStatusSnapshot {
            source: source.to_owned(),
            ..LvdsStatusSnapshot::default()
        }
    }

    fn scientific_event(value: f64) -> BackendEvent {
        BackendEvent::ScientificSamples {
            channel_1_start_seconds: 100.0,
            channel_2_start_seconds: 100.0,
            sample_interval_seconds: 0.000_500,
            channel_1: vec![value; SAMPLE_COUNT],
            channel_2: vec![value + 64.0; SAMPLE_COUNT],
        }
    }

    fn render_lvds_ui(
        app: &mut LangmuirDesktopApp,
        context: &Context,
        events: Vec<Event>,
    ) -> FullOutput {
        context.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 700.0))),
                events,
                ..RawInput::default()
            },
            |ui| {
                app.operations_panel(ui);
                app.lvds_dialog(ui.ctx());
            },
        )
    }

    fn click_lvds_button(app: &mut LangmuirDesktopApp, context: &Context, text: &str) {
        let _ = render_lvds_ui(app, context, Vec::new());
        let output = render_lvds_ui(app, context, Vec::new());
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                Shape::Text(shape) if shape.galley.text() == text => {
                    Some(shape.pos + shape.galley.rect.center().to_vec2())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("button {text} was not rendered"));
        for pressed in [true, false] {
            let _ = render_lvds_ui(
                app,
                context,
                vec![
                    Event::PointerMoved(position),
                    Event::PointerButton {
                        pos: position,
                        button: PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    },
                ],
            );
        }
    }

    #[test]
    fn scientific_frames_share_one_plot_without_crc_mode() {
        let (mut app, endpoint) = lvds_test_app();
        app.handle_backend_event(scientific_event(0.0));
        assert_eq!(app.channel_1.len(), SAMPLE_COUNT);
        assert_eq!(app.channel_2.len(), SAMPLE_COUNT);
        assert_eq!(app.lvds_view.plotted_frames, 1);
        app.handle_scientific_samples(
            101.0,
            101.0,
            0.000_500,
            vec![64.0; SAMPLE_COUNT],
            vec![128.0; SAMPLE_COUNT],
        );
        assert_eq!(app.channel_1.len(), 2 * SAMPLE_COUNT);
        let (combined, _) = app.display_plot_points();
        assert_eq!(combined[0], [100.0, 0.0]);
        assert_eq!(combined.last().unwrap()[1], 0.01);
        app.apply_lvds_status(status("UDP session 1"));
        assert_eq!(app.lvds_view.plotted_frames, 2);
        assert!(endpoint.request_rx.try_recv().is_err());
    }

    #[test]
    fn source_switch_clears_both_histories_but_status_refresh_keeps_display_freeze() {
        let (mut app, _endpoint) = lvds_test_app();
        app.apply_lvds_status(status("UDP session 1"));
        app.handle_scientific_samples(
            100.0,
            100.0,
            0.000_500,
            vec![0.0; SAMPLE_COUNT],
            vec![0.0; SAMPLE_COUNT],
        );
        app.toggle_display_freeze();
        app.apply_lvds_status(status("UDP session 1"));
        assert!(app.operator.frozen_plot.is_some());
        app.apply_lvds_status(status("offline PCAP session 2"));
        app.handle_backend_event(scientific_event(0.0));
        assert_eq!(app.channel_1.len(), SAMPLE_COUNT);
        app.apply_lvds_status(status("UDP session 3"));
        assert!(app.channel_1.is_empty());
        assert!(app.channel_2.is_empty());
        assert_eq!(app.lvds_view.plotted_frames, 0);
        assert!(app.operator.frozen_plot.is_none());
        assert!(app.operator.last_scientific_update.is_none());
    }

    #[test]
    fn waveform_history_freeze_clear_and_clock_restart_remain_independent() {
        let (mut app, _endpoint) = lvds_test_app();
        app.handle_backend_event(scientific_event(0.0));
        app.handle_scientific_samples(
            101.0,
            101.0,
            0.000_500,
            vec![64.0; SAMPLE_COUNT],
            vec![128.0; SAMPLE_COUNT],
        );
        assert_eq!(app.lvds_view.plotted_frames, 2);
        assert_eq!(app.display_plot_points().0[0], [100.0, 0.0]);
        app.toggle_display_freeze();
        app.handle_scientific_samples(
            172.0,
            172.0,
            0.000_500,
            vec![64.0; SAMPLE_COUNT],
            vec![128.0; SAMPLE_COUNT],
        );
        assert_eq!(app.channel_1.len(), SAMPLE_COUNT);
        assert_eq!(app.display_plot_points().0[0], [100.0, 0.0]);
        app.toggle_display_freeze();
        assert_eq!(app.display_plot_points().0[0], [172.0, 0.01]);
        app.handle_scientific_samples(
            1.0,
            1.0,
            0.000_500,
            vec![0.0; SAMPLE_COUNT],
            vec![0.0; SAMPLE_COUNT],
        );
        assert_eq!(app.channel_1.len(), SAMPLE_COUNT);
        app.clear_current_plot();
        assert_eq!(app.lvds_view.plotted_frames, 0);
        assert!(app.channel_1.is_empty());
        assert_eq!(app.display_plot_points().0, [] as [[f64; 2]; 0]);
    }

    #[test]
    fn scientific_400_frame_stream_uses_one_bounded_dual_channel_plot() {
        let (mut app, _endpoint) = lvds_test_app();
        for index in 0..400 {
            let start = 100.0 + f64::from(index) * 0.05;
            if index % 2 == 0 {
                app.handle_scientific_samples(
                    start,
                    start,
                    0.000_500,
                    vec![0.0; SAMPLE_COUNT],
                    vec![64.0; SAMPLE_COUNT],
                );
            } else {
                app.handle_backend_event(BackendEvent::ScientificSamples {
                    channel_1_start_seconds: start,
                    channel_2_start_seconds: start,
                    sample_interval_seconds: 0.000_500,
                    channel_1: vec![0.0; SAMPLE_COUNT],
                    channel_2: vec![64.0; SAMPLE_COUNT],
                });
            }
        }
        assert_eq!(app.lvds_view.plotted_frames, 400);
        assert_eq!(
            app.channel_1.len(),
            40_000,
            "20 seconds remain in the 60 second history"
        );
        assert_eq!(app.channel_1.len(), app.channel_2.len());
        let (channel_1, channel_2) = app.display_plot_points();
        assert!(channel_1.len() <= super::MAX_DISPLAY_POINTS);
        assert!(channel_2.len() <= super::MAX_DISPLAY_POINTS);
        assert_eq!(channel_1.first().unwrap()[1], 0.0);
        assert_eq!(channel_2.last().unwrap()[1], 0.01);
    }

    #[test]
    fn real_time_science_updates_without_saving_and_preserves_device_sample_times() {
        let (mut app, _endpoint) = lvds_test_app();
        assert!(!app.acquisition_running);
        for frame in 0..400 {
            let start = 1_800_000_000.0 + f64::from(frame) * 0.05;
            app.handle_backend_event(BackendEvent::ScientificSamples {
                channel_1_start_seconds: start,
                channel_2_start_seconds: start,
                sample_interval_seconds: 0.000_500,
                channel_1: vec![0.0; SAMPLE_COUNT],
                channel_2: vec![64.0; SAMPLE_COUNT],
            });
        }
        assert!(app.scientific_data_is_fresh());
        assert_eq!(app.channel_1.len(), 40_000);
        let (channel_1, channel_2) = app.display_plot_points();
        assert!(channel_1.len() <= super::MAX_DISPLAY_POINTS);
        let last_1 = channel_1.last().unwrap();
        let last_2 = channel_2.last().unwrap();
        assert!((last_1[0] - 1_800_000_019.999_5).abs() < 0.000_05);
        assert!((last_2[0] - last_1[0]).abs() < 0.000_001);
        assert_eq!(last_1[1], 0.0);
        assert_eq!(last_2[1], 0.01);
    }

    #[test]
    #[cfg(feature = "mock")]
    fn mock_acquisition_confirmation_does_not_claim_real_scientific_files() {
        let (bridge, _endpoint) = channel_bridge(16);
        let mut app = LangmuirDesktopApp::with_bridge(Box::new(bridge), None, true);
        app.handle_backend_event(BackendEvent::AcquisitionChanged { running: true });
        assert!(app.logs.back().unwrap().message.contains("MOCK"));
        assert!(app.logs.back().unwrap().message.contains("不写科学文件"));
    }

    #[test]
    fn scientific_save_format_is_available_before_starting() {
        let (mut app, endpoint) = lvds_test_app();
        let context = Context::default();
        let output = render_lvds_ui(&mut app, &context, Vec::new());
        assert!(output.shapes.iter().any(|shape| matches!(
            &shape.shape,
            Shape::Text(shape) if shape.galley.text() == "保存格式"
        )));
        assert!(endpoint.request_rx.try_recv().is_err());
    }

    #[test]
    fn selected_scientific_format_is_sent_and_locked_during_capture() {
        use crate::bridge::ScientificSaveFormat;

        let (mut app, endpoint) = lvds_test_app();
        let context = Context::default();
        click_lvds_button(&mut app, &context, "LPD");
        click_lvds_button(&mut app, &context, "CSV");
        click_lvds_button(&mut app, &context, "开始保存");
        assert_eq!(
            endpoint.request_rx.try_recv().unwrap(),
            UiRequest::StartAcquisition {
                format: ScientificSaveFormat::Csv,
            }
        );
        assert!(!app.acquisition_running, "wait for backend confirmation");

        app.handle_backend_event(BackendEvent::AcquisitionChanged { running: true });
        click_lvds_button(&mut app, &context, "CSV");
        let output = render_lvds_ui(&mut app, &context, Vec::new());
        assert!(
            !output.shapes.iter().any(|shape| matches!(
                &shape.shape,
                Shape::Text(shape) if shape.galley.text() == "LPD + CSV"
            )),
            "the active capture format selector must stay locked"
        );
        assert!(endpoint.request_rx.try_recv().is_err());

        app.handle_backend_event(BackendEvent::AcquisitionChanged { running: false });
        click_lvds_button(&mut app, &context, "CSV");
        click_lvds_button(&mut app, &context, "LPD + CSV");
        click_lvds_button(&mut app, &context, "开始保存");
        assert_eq!(
            endpoint.request_rx.try_recv().unwrap(),
            UiRequest::StartAcquisition {
                format: ScientificSaveFormat::LpdAndCsv,
            }
        );
    }

    #[test]
    fn replay_scientific_save_is_requested_before_replay_starts() {
        use crate::bridge::ScientificSaveFormat;

        let (mut app, endpoint) = lvds_test_app();
        let context = Context::default();
        click_lvds_button(&mut app, &context, "LPD");
        click_lvds_button(&mut app, &context, "CSV");
        click_lvds_button(&mut app, &context, "离线回放 PCAP");
        app.lvds_view.replay_path = "  capture.pcapng  ".to_owned();
        click_lvds_button(&mut app, &context, "回放时保存科学数据");
        click_lvds_button(&mut app, &context, "开始回放并保存");
        assert_eq!(
            endpoint.request_rx.try_recv().unwrap(),
            UiRequest::StartPcapReplay {
                path: "capture.pcapng".to_owned(),
                save: Some(ScientificSaveFormat::Csv),
            }
        );
        assert!(endpoint.request_rx.try_recv().is_err());
        assert!(
            !app.acquisition_running,
            "only the backend confirms creation"
        );
    }

    #[test]
    fn scientific_storage_summary_uses_actual_csv_rows_and_retains_errors() {
        use crate::bridge::ScientificSaveFormat;

        let (mut app, _endpoint) = lvds_test_app();
        let mut status = status("PCAP replay");
        status.storage.format = ScientificSaveFormat::LpdAndCsv;
        status.storage.enqueued = 9;
        status.storage.sink_written = 2;
        status.storage.csv_frames_written = 2;
        status.storage.csv_rows_written = 200;
        status.storage.lpd_frames_written = 3;
        status.storage.closed = true;
        app.apply_lvds_status(status.clone());
        app.lvds_view.plotted_frames = 999;
        let summary = app.lvds_save_summary();
        assert!(summary.contains("已关闭"));
        assert!(summary.contains("入队 9"));
        assert!(summary.contains("写入 2"));
        assert!(summary.contains("CSV 行 200"));

        status.storage.error = Some("CSV disk full".to_owned());
        app.apply_lvds_status(status);
        assert!(app.lvds_save_summary().contains("保存错误"));
    }

    #[test]
    fn failed_replay_shows_the_reason_and_allows_a_new_capture_to_start() {
        use crate::bridge::ReplayProgress;

        let (mut app, endpoint) = lvds_test_app();
        let context = Context::default();
        let error = "未找到可绘制的科学帧：错误的 04/FF 地址已拒收，当前要求 55/AA";
        let mut snapshot = status("failed replay");
        snapshot.replay = Some(ReplayProgress {
            path: "wrong-04ff.pcapng".to_owned(),
            total_packets: 20,
            target_udp_packets: 20,
            finished: true,
            error: Some(error.to_owned()),
        });
        app.apply_lvds_status(snapshot);
        app.lvds_view.replay_dialog_open = true;
        app.lvds_view.replay_path = "final-55aa.pcapng".to_owned();
        let _ = render_lvds_ui(&mut app, &context, Vec::new());
        let output = render_lvds_ui(&mut app, &context, Vec::new());
        let text: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                Shape::Text(shape) => Some(shape.galley.text()),
                _ => None,
            })
            .collect();
        assert!(text.iter().any(|text| text.contains("回放失败")));
        assert!(text.contains(&error));
        click_lvds_button(&mut app, &context, "开始回放");
        assert_eq!(
            endpoint.request_rx.try_recv().unwrap(),
            UiRequest::StartPcapReplay {
                path: "final-55aa.pcapng".to_owned(),
                save: None,
            }
        );
    }

    #[test]
    fn replay_offers_saving_before_the_first_frame() {
        let (mut app, endpoint) = lvds_test_app();
        let context = Context::default();
        app.lvds_view.replay_dialog_open = true;
        let _ = render_lvds_ui(&mut app, &context, Vec::new());
        let output = render_lvds_ui(&mut app, &context, Vec::new());
        assert!(output.shapes.iter().any(|shape| matches!(
            &shape.shape,
            Shape::Text(shape) if shape.galley.text() == "回放时保存科学数据"
        )));
        assert!(endpoint.request_rx.try_recv().is_err());
    }

    #[test]
    fn replay_dialog_renders_without_starting_science_or_saving() {
        let (mut app, endpoint) = lvds_test_app();
        let context = Context::default();
        app.lvds_view.replay_dialog_open = true;
        let output = context.run_ui(RawInput::default(), |ui| {
            app.operations_panel(ui);
            app.lvds_dialog(ui.ctx());
        });
        assert_ne!(output.shapes, [] as [eframe::epaint::ClippedShape; 0]);
        assert!(endpoint.request_rx.try_recv().is_err());
        assert!(!app.acquisition_running);
        assert!(app.lvds_view.status.is_none());
        super::theme::configure(&context);
        for (width, height, sidebar_width, header_height) in [
            (800.0, 600.0, 580.0, super::HEADER_COMPACT_HEIGHT),
            (1_120.0, 720.0, 680.0, super::HEADER_COMPACT_HEIGHT),
            (1_360.0, 860.0, 680.0, super::HEADER_HEIGHT),
        ] {
            app.apply_lvds_status(status("headless UI session"));
            let root = Rect::from_min_size(
                pos2(8.0, header_height + 8.0),
                vec2(width - sidebar_width - 16.0, height - header_height - 16.0),
            );
            let dock = central_layout_rects(root).operations;
            let mut actual = Rect::NOTHING;
            let _ = context.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, height))),
                    ..RawInput::default()
                },
                |ui| {
                    ui.scope_builder(eframe::egui::UiBuilder::new().max_rect(dock), |ui| {
                        app.operations_panel(ui);
                        actual = ui.min_rect();
                    });
                },
            );
            assert!(
                actual.max.x <= dock.max.x + 1.0,
                "{width}px dock {dock:?}, widgets {actual:?}"
            );
            assert!(
                actual.max.y <= dock.max.y + 1.0,
                "{height}px dock {dock:?}, widgets {actual:?}"
            );
        }
    }

    #[test]
    fn receive_status_omits_engineering_notices_before_during_and_after_saving() {
        let (mut app, endpoint) = lvds_test_app();
        let context = Context::default();
        for running in [false, true, false] {
            app.acquisition_running = running;
            let output = render_lvds_ui(&mut app, &context, Vec::new());
            let text: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    Shape::Text(shape) => Some(shape.galley.text()),
                    _ => None,
                })
                .collect();
            for obsolete in [
                "仅显示 CRC 正确的数据",
                "CRC16 详情",
                "严格模式",
                "诊断模式",
                "科学数据未经 CRC 完整性验证",
                "未标定",
            ] {
                assert!(!text.iter().any(|label| label.contains(obsolete)));
            }
        }
        assert!(endpoint.request_rx.try_recv().is_err());
    }

    #[test]
    fn saturated_or_disconnected_request_queue_reports_error_without_false_saving() {
        let (bridge, endpoint) = channel_bridge(1);
        let mut app = LangmuirDesktopApp::with_bridge(
            Box::new(bridge),
            None,
            #[cfg(feature = "mock")]
            false,
        );
        app.submit(UiRequest::StartAcquisition {
            format: crate::bridge::ScientificSaveFormat::Lpd,
        });
        assert!(!app.acquisition_running);
        assert!(app.faults.front().is_some());
        assert_eq!(
            endpoint.request_rx.try_recv().unwrap(),
            UiRequest::RefreshPorts
        );
        app.submit(UiRequest::StartAcquisition {
            format: crate::bridge::ScientificSaveFormat::Lpd,
        });
        assert_eq!(
            endpoint.request_rx.try_recv().unwrap(),
            UiRequest::StartAcquisition {
                format: crate::bridge::ScientificSaveFormat::Lpd,
            }
        );
        assert!(!app.acquisition_running, "wait for backend confirmation");
        drop(endpoint);
        app.submit(UiRequest::StartAcquisition {
            format: crate::bridge::ScientificSaveFormat::Lpd,
        });
        assert!(!app.acquisition_running);
        assert!(app.faults.front().is_some());
    }

    #[test]
    fn pcap_replay_starts_directly_without_a_mode_or_plot_source_selection() {
        let (mut app, endpoint) = lvds_test_app();
        let context = Context::default();
        click_lvds_button(&mut app, &context, "离线回放 PCAP");
        assert!(app.lvds_view.replay_dialog_open);
        assert!(endpoint.request_rx.try_recv().is_err());
        // The old-protocol capture must not be preselected. Empty paths keep
        // replay disabled until the user supplies a new-protocol capture.
        assert_eq!(app.lvds_view.replay_path, "");
        click_lvds_button(&mut app, &context, "开始回放");
        assert!(endpoint.request_rx.try_recv().is_err());
        app.lvds_view.replay_path = "new-protocol.pcapng".to_owned();
        click_lvds_button(&mut app, &context, "开始回放");
        assert_eq!(
            endpoint.request_rx.try_recv().unwrap(),
            UiRequest::StartPcapReplay {
                path: "new-protocol.pcapng".to_owned(),
                save: None,
            }
        );
        assert!(endpoint.request_rx.try_recv().is_err());
    }

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
    fn bind_address_error_summary_preserves_other_failure_meanings() {
        for raw in [
            "接收异常：Can't assign requested address (os error 49)",
            "Cannot assign requested address (os error 99)",
            "Cannot assign requested address (os error 10049)",
        ] {
            assert_eq!(
                super::lvds_receive_summary(raw, false),
                "监听失败：本机未配置该 IP 地址"
            );
            assert_eq!(super::lvds_receive_summary(raw, true), raw);
        }
        for raw in [
            "Address already in use (os error 48)",
            "Permission denied (os error 13)",
            "LVDS 校验失败",
            "尚未监听",
            "bind failed (os error 490)",
        ] {
            assert_eq!(super::lvds_receive_summary(raw, false), raw);
        }
        #[cfg(target_os = "macos")]
        assert_eq!(
            super::lvds_receive_summary("Not a STREAM (os error 99)", false),
            "Not a STREAM (os error 99)"
        );
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
    fn ui_scale_selection_stays_within_the_supported_range() {
        assert_eq!(nearest_ui_scale(0.8), 0.8);
        assert_eq!(nearest_ui_scale(1.0), 1.0);
        assert_eq!(nearest_ui_scale(1.5), 1.5);
        assert_eq!(nearest_ui_scale(2.0), 1.5);
    }

    #[test]
    fn responsive_regions_have_safe_size_ordering() {
        const {
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

#[cfg(test)]
#[path = "app_layout_tests.rs"]
mod layout_tests;
