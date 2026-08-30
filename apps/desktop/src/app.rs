use std::{collections::VecDeque, time::Duration};

use chrono::{DateTime, Local, Utc};
use eframe::egui::{
    self, Align, Color32, ComboBox, Grid, Layout, RichText, ScrollArea, Stroke, Ui,
};
use egui_plot::{Legend, Line, Plot, PlotPoints};
use langmuir_core::Command;

use crate::{
    bridge::{
        BackendBridge, BackendEvent, BackendStateSnapshot, LinkKind, LinkMetricsSnapshot, LogLevel,
        TelemetrySnapshot, UiRequest,
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
const EVENT_BUDGET_PER_FRAME: usize = 4_096;
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

#[derive(Debug)]
struct FaultEntry {
    time: String,
    source: String,
    message: String,
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
    lvds: ConnectionPanelState,
    acquisition_running: bool,
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
            lvds: ConnectionPanelState {
                baud_rate: 8_000_000,
                ..ConnectionPanelState::default()
            },
            acquisition_running: false,
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
            overlay_normalized: true,
            logs: VecDeque::new(),
            faults: VecDeque::new(),
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
        self.faults.push_front(FaultEntry {
            time: Self::now(),
            source: source.clone(),
            message: message.clone(),
        });
        while self.faults.len() > MAX_FAULTS {
            self.faults.pop_back();
        }
        self.push_log(LogLevel::Error, format!("{source}: {message}"));
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
            BackendEvent::PortsUpdated { rs422, lvds } => {
                self.rs422.ports = rs422;
                self.lvds.ports = lvds;
                self.rs422.selected_port = 0;
                self.lvds.selected_port = 0;
                self.push_log(LogLevel::Info, "串口列表已刷新");
            }
            BackendEvent::LinkChanged {
                link,
                connected,
                detail,
            } => {
                let state = match link {
                    LinkKind::Rs422 => &mut self.rs422,
                    LinkKind::Lvds => &mut self.lvds,
                };
                state.connected = connected;
                state.detail.clone_from(&detail);
                let status = if connected { "已连接" } else { "已断开" };
                self.push_log(LogLevel::Info, format!("{link} {status}: {detail}"));
            }
            BackendEvent::AcquisitionChanged { running } => {
                self.acquisition_running = running;
                self.push_log(
                    LogLevel::Info,
                    if running {
                        "采集已开始"
                    } else {
                        "采集已停止"
                    },
                );
            }
            BackendEvent::StateSnapshot(snapshot) => self.apply_state_snapshot(snapshot),
            BackendEvent::Telemetry(snapshot) => {
                self.record_telemetry_anomalies(&snapshot);
                self.telemetry = Some(snapshot);
            }
            BackendEvent::LinkMetrics {
                link,
                connected,
                metrics,
            } => {
                let state = match link {
                    LinkKind::Rs422 => &mut self.rs422,
                    LinkKind::Lvds => &mut self.lvds,
                };
                state.connected = connected;
                state.metrics = Some(metrics);
            }
            BackendEvent::ScientificSamples {
                channel_1_start_seconds,
                channel_2_start_seconds,
                sample_interval_seconds,
                channel_1,
                channel_2,
            } => {
                self.channel_1.extend(channel_1.into_iter().scan(
                    channel_1_start_seconds,
                    |time_seconds, value| {
                        let point = SamplePoint {
                            time_seconds: *time_seconds,
                            value,
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
                            value,
                        };
                        *time_seconds += sample_interval_seconds;
                        Some(point)
                    },
                ));
            }
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

    fn apply_state_snapshot(&mut self, snapshot: BackendStateSnapshot) {
        self.rs422.connected = snapshot.rs422_connected;
        self.rs422.detail = snapshot.rs422_detail;
        self.lvds.connected = snapshot.lvds_connected;
        self.lvds.detail = snapshot.lvds_detail;
        self.acquisition_running = snapshot.acquisition_running;
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
                "DDS1",
                snapshot.dds1_status,
                previous.map(|item| item.dds1_status),
            ),
            (
                "DDS2",
                snapshot.dds2_status,
                previous.map(|item| item.dds2_status),
            ),
            (
                "DAC",
                snapshot.dac_status,
                previous.map(|item| item.dac_status),
            ),
            (
                "ADC1",
                snapshot.adc1_status,
                previous.map(|item| item.adc1_status),
            ),
            (
                "ADC2",
                snapshot.adc2_status,
                previous.map(|item| item.adc2_status),
            ),
            (
                "辅助",
                snapshot.auxiliary_status,
                previous.map(|item| item.auxiliary_status),
            ),
        ];
        for (name, status, previous_status) in statuses {
            let is_dds = matches!(name, "DDS1" | "DDS2");
            if previous_status == Some(status) {
                continue;
            }
            if is_dds && status == 0 {
                self.push_log(LogLevel::Warning, format!("{name}: 停止 (0x00)"));
            } else if (is_dds && status > 1) || (!is_dds && status != 1) {
                let detail = if is_dds && status == 2 {
                    "故障"
                } else {
                    "状态异常"
                };
                self.push_fault(name, format!("{detail} (0x{status:02X})"));
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the connection card keeps selection, actions, and optional diagnostics together"
    )]
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
                egui::CollapsingHeader::new("链路指标")
                    .default_open(false)
                    .show(ui, |ui| {
                        Grid::new(("link_metrics", link.to_string()))
                            .num_columns(2)
                            .striped(true)
                            .show(ui, |ui| {
                                value_cell(ui, "接收字节", metrics.bytes_read.to_string());
                                value_cell(ui, "解码帧", metrics.frames_decoded.to_string());
                                ui.end_row();
                                value_cell(ui, "拒绝帧", metrics.rejected_frames.to_string());
                                value_cell(ui, "传输丢弃", metrics.events_dropped.to_string());
                                ui.end_row();
                                value_cell(
                                    ui,
                                    "显示丢弃",
                                    metrics.ui_display_events_dropped.to_string(),
                                );
                                value_cell(
                                    ui,
                                    "队列峰值",
                                    metrics.max_event_queue_depth.to_string(),
                                );
                                ui.end_row();
                            });
                        if metrics.checksum_failures > 0
                            || metrics.length_failures > 0
                            || metrics.queue_overflows > 0
                            || metrics.io_errors > 0
                        {
                            ui.colored_label(
                                theme::WARNING,
                                format!(
                                    "校验 {} · 长度 {} · 溢出 {} · I/O {}",
                                    metrics.checksum_failures,
                                    metrics.length_failures,
                                    metrics.queue_overflows,
                                    metrics.io_errors,
                                ),
                            );
                        }
                    });
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
                        ui.add_sized(
                            [ui.available_width(), theme::CONTROL_HEIGHT],
                            egui::DragValue::new(&mut self.frequency).range(0..=u8::MAX),
                        );
                    }
                    ConfigurationMode::Mode => {
                        ui.add_sized(
                            [ui.available_width(), theme::CONTROL_HEIGHT],
                            egui::DragValue::new(&mut self.mode).range(0..=u8::MAX),
                        );
                    }
                    ConfigurationMode::Voltage => {
                        ui.add_sized(
                            [ui.available_width(), theme::CONTROL_HEIGHT],
                            egui::DragValue::new(&mut self.voltage).range(0..=u16::MAX),
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
        let cell_width = ((ui.available_width() - 15.0) / 4.0).max(50.0);
        Grid::new("command_bytes")
            .num_columns(4)
            .min_col_width(cell_width)
            .max_col_width(cell_width)
            .spacing([5.0, 5.0])
            .show(ui, |ui| {
                for (index, byte) in frame.iter().enumerate() {
                    let active = !raw_mode && self.command_byte_is_active(index);
                    let (fill, stroke, text_color) = match index {
                        6 => (theme::PRIMARY, theme::PRIMARY, theme::WHITE),
                        11 => (theme::DANGER, theme::DANGER, theme::WHITE),
                        _ if active => (
                            Color32::from_rgb(255, 247, 237),
                            Color32::from_rgb(249, 115, 22),
                            Color32::from_rgb(194, 65, 12),
                        ),
                        _ => (theme::SURFACE_ALT, theme::BORDER, theme::TEXT),
                    };
                    egui::Frame::new()
                        .fill(fill)
                        .stroke(Stroke::new(if active { 2.0 } else { 1.0 }, stroke))
                        .corner_radius(10.0)
                        .inner_margin(5.0)
                        .show(ui, |ui| {
                            ui.set_width((cell_width - 10.0).max(40.0));
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    RichText::new(labels[index])
                                        .size(9.0)
                                        .strong()
                                        .color(text_color),
                                );
                                if raw_mode {
                                    ui.scope(|ui| {
                                        ui.visuals_mut().override_text_color = Some(text_color);
                                        ui.add_sized(
                                            [42.0, 22.0],
                                            egui::DragValue::new(&mut self.raw_command[index])
                                                .range(0..=u8::MAX)
                                                .hexadecimal(2, false, true),
                                        );
                                    });
                                } else {
                                    ui.label(
                                        RichText::new(format!("{byte:02X}"))
                                            .monospace()
                                            .size(15.0)
                                            .strong()
                                            .color(text_color),
                                    );
                                }
                                ui.label(
                                    RichText::new(format!("dec: {byte}"))
                                        .size(8.0)
                                        .color(text_color.gamma_multiply(0.78)),
                                );
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
            ui.add_enabled_ui(!raw_mode, |ui| self.typed_command_controls(ui));
        });
        if raw_mode {
            ui.colored_label(
                Color32::from_rgb(220, 38, 38),
                "原始模式：发送前不会自动修复帧头、长度或校验和",
            );
        }

        let typed_frame = command_bytes(self.command());
        let frame = if raw_mode {
            self.raw_command
        } else {
            typed_frame
        };
        self.command_bytes_grid(ui, &frame, raw_mode);

        let displayed_frame = if raw_mode {
            self.raw_command
        } else {
            typed_frame
        };
        theme::subtle_card().show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(
                    RichText::new(hex_frame(&displayed_frame))
                        .monospace()
                        .size(10.0)
                        .color(theme::PRIMARY),
                );
            });
        });
        ui.horizontal(|ui| {
            if ui
                .add_sized([82.0, 32.0], egui::Button::new("重置"))
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
            let send_width = ui.available_width();
            if ui
                .add_sized(
                    [send_width, 32.0],
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

    #[expect(
        clippy::too_many_lines,
        reason = "the compact telemetry sidebar mirrors the four visual groups in the Python UI"
    )]
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

        theme::card().show(ui, |ui| {
            theme::section_title(ui, "时钟系统");
            ui.columns(2, |columns| {
                theme::metric_box(
                    &mut columns[0],
                    "下位机时间",
                    format_timestamp(data.absolute_time_seconds),
                    theme::PRIMARY,
                );
                theme::metric_box(
                    &mut columns[1],
                    "本地系统时间",
                    Local::now().format("%H:%M:%S").to_string(),
                    theme::TEXT,
                );
            });
            ui.add_space(5.0);
            theme::metric_box(
                ui,
                "自守时计数 (ms)",
                format!("{:010}", data.local_time_seconds),
                theme::TEXT,
            );
        });

        ui.add_space(7.0);
        theme::card().show(ui, |ui| {
            theme::section_title(ui, "环境温度与磁场");
            temperature_bar(ui, "CPU 温度", data.cpu_temperature_celsius, theme::PRIMARY);
            temperature_bar(
                ui,
                "PCB 温度",
                data.pcb_temperature_celsius,
                Color32::from_rgb(20, 184, 166),
            );
            ui.label(
                RichText::new("空间磁场 (nT)")
                    .small()
                    .color(theme::TEXT_SECONDARY),
            );
            ui.columns(3, |columns| {
                theme::metric_box(
                    &mut columns[0],
                    "X",
                    data.magnetic_x.to_string(),
                    theme::PRIMARY,
                );
                theme::metric_box(
                    &mut columns[1],
                    "Y",
                    data.magnetic_y.to_string(),
                    theme::PRIMARY,
                );
                theme::metric_box(
                    &mut columns[2],
                    "Z",
                    data.magnetic_z.to_string(),
                    theme::PRIMARY,
                );
            });
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
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    dds_status_cell(ui, "DDS1", data.dds1_status);
                    dds_status_cell(ui, "DDS2", data.dds2_status);
                    ui.end_row();
                    binary_status_cell(ui, "DAC", data.dac_status);
                    binary_status_cell(ui, "辅助", data.auxiliary_status);
                    ui.end_row();
                    binary_status_cell(ui, "ADC1", data.adc1_status);
                    binary_status_cell(ui, "ADC2", data.adc2_status);
                    ui.end_row();
                });
        });
    }

    fn fault_panel(&self, ui: &mut Ui) {
        theme::card().show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::section_title(ui, "异常指令记录");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{}/{}", self.faults.len(), MAX_FAULTS))
                            .small()
                            .color(theme::TEXT_MUTED),
                    );
                });
            });
            Grid::new("fault_header")
                .num_columns(3)
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("时间");
                    ui.strong("类型");
                    ui.strong("错误信息");
                    ui.end_row();
                });
            ScrollArea::vertical()
                .id_salt("faults")
                .max_height(150.0)
                .show(ui, |ui| {
                    if self.faults.is_empty() {
                        ui.label(RichText::new("暂无异常").small().color(theme::TEXT_MUTED));
                    }
                    for fault in &self.faults {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&fault.time).monospace().size(10.0));
                            ui.label(
                                RichText::new(&fault.source)
                                    .strong()
                                    .size(10.0)
                                    .color(theme::DANGER),
                            );
                            ui.label(RichText::new(&fault.message).size(10.0));
                        });
                        ui.separator();
                    }
                });
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
            if ui.button("清空").clicked() {
                self.channel_1.clear();
                self.channel_2.clear();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.small(format!(
                    "固定窗 {:.0}s · 缓存 {}/{} 点",
                    PLOT_WINDOW_SECONDS,
                    self.channel_1.len(),
                    self.channel_2.len()
                ));
            });
        });
        let mut channel_1 = self.channel_1.decimated(MAX_DISPLAY_POINTS);
        let mut channel_2 = self.channel_2.decimated(MAX_DISPLAY_POINTS);
        let available_plot_height = ui.available_height().max(220.0);
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
                        "采样值（共享 Y 轴）"
                    })
                    .x_axis_formatter(|mark, range| {
                        format_plot_timestamp(mark.value, range.end() - range.start())
                    })
                    .allow_scroll(true)
                    .allow_zoom(true)
                    .allow_boxed_zoom(true)
                    .show_background(true)
                    .height(available_plot_height)
                    .show(ui, |plot_ui| {
                        if self.show_channels[0] && !channel_1.is_empty() {
                            plot_ui.line(
                                Line::new(channel_1_name, PlotPoints::from(channel_1))
                                    .color(Color32::from_rgb(37, 99, 235)),
                            );
                        }
                        if self.show_channels[1] && !channel_2.is_empty() {
                            plot_ui.line(
                                Line::new(channel_2_name, PlotPoints::from(channel_2))
                                    .color(Color32::from_rgb(220, 38, 38)),
                            );
                        }
                    });
            }
            PlotLayout::Split => {
                let split_height = ((available_plot_height - 8.0) / 2.0).max(105.0);
                if self.show_channels[0] {
                    Plot::new("scientific_plot_channel_1")
                        .show_axes([true, true])
                        .x_axis_label("通道 1 时间")
                        .y_axis_label("通道 1")
                        .x_axis_formatter(|mark, range| {
                            format_plot_timestamp(mark.value, range.end() - range.start())
                        })
                        .allow_scroll(true)
                        .allow_zoom(true)
                        .allow_boxed_zoom(true)
                        .show_background(true)
                        .height(split_height)
                        .show(ui, |plot_ui| {
                            if !channel_1.is_empty() {
                                plot_ui.line(
                                    Line::new("通道 1", PlotPoints::from(channel_1))
                                        .color(Color32::from_rgb(37, 99, 235)),
                                );
                            }
                        });
                }
                if self.show_channels[1] {
                    Plot::new("scientific_plot_channel_2")
                        .show_axes([true, true])
                        .x_axis_label("通道 2 时间")
                        .y_axis_label("通道 2")
                        .x_axis_formatter(|mark, range| {
                            format_plot_timestamp(mark.value, range.end() - range.start())
                        })
                        .allow_scroll(true)
                        .allow_zoom(true)
                        .allow_boxed_zoom(true)
                        .show_background(true)
                        .height(split_height)
                        .show(ui, |plot_ui| {
                            if !channel_2.is_empty() {
                                plot_ui.line(
                                    Line::new("通道 2", PlotPoints::from(channel_2))
                                        .color(Color32::from_rgb(220, 38, 38)),
                                );
                            }
                        });
                }
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the LVDS toolbar keeps its responsive single-row controls in one render function"
    )]
    fn operations_panel(&mut self, ui: &mut Ui) {
        let mut request = None;
        theme::card().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                status_dot(ui, self.lvds.connected);
                ui.strong("LVDS");
                let selected_port = self
                    .lvds
                    .ports
                    .get(self.lvds.selected_port)
                    .map_or("无可用串口", String::as_str);
                ui.add_enabled_ui(!self.lvds.connected, |ui| {
                    ComboBox::from_id_salt("lvds_toolbar_port")
                        .selected_text(selected_port)
                        .width(125.0)
                        .show_ui(ui, |ui| {
                            for (index, port) in self.lvds.ports.iter().enumerate() {
                                ui.selectable_value(&mut self.lvds.selected_port, index, port);
                            }
                        });
                    ComboBox::from_id_salt("lvds_toolbar_baud")
                        .selected_text(self.lvds.baud_rate.to_string())
                        .width(95.0)
                        .show_ui(ui, |ui| {
                            for baud in BAUD_RATES {
                                ui.selectable_value(
                                    &mut self.lvds.baud_rate,
                                    baud,
                                    baud.to_string(),
                                );
                            }
                        });
                });

                let connection_label = if self.lvds.connected {
                    "断开"
                } else {
                    "连接"
                };
                let connection_enabled = self.lvds.connected || !self.lvds.ports.is_empty();
                if ui
                    .add_enabled(
                        connection_enabled,
                        egui::Button::new(connection_label).min_size(egui::vec2(58.0, 30.0)),
                    )
                    .clicked()
                {
                    request = if self.lvds.connected {
                        Some(UiRequest::Disconnect {
                            link: LinkKind::Lvds,
                        })
                    } else {
                        self.lvds.ports.get(self.lvds.selected_port).map(|port| {
                            UiRequest::Connect {
                                link: LinkKind::Lvds,
                                port: port.clone(),
                                baud: self.lvds.baud_rate,
                            }
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
                        [82.0, 30.0],
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
                if ui.button("刷新").clicked() {
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
                            "解码 {} · 拒绝 {} · 显示丢弃 {}",
                            metrics.frames_decoded,
                            metrics.rejected_frames,
                            metrics.ui_display_events_dropped
                        ))
                        .small()
                        .color(theme::TEXT_MUTED),
                    );
                }
            });
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
        ScrollArea::vertical()
            .id_salt("logs")
            .stick_to_bottom(true)
            .show_rows(ui, 16.0, self.logs.len(), |ui, row_range| {
                for row in row_range {
                    let entry = &self.logs[row];
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
                                .size(11.0)
                                .color(color),
                        );
                        ui.add(
                            egui::Label::new(RichText::new(&entry.message).size(11.0).color(color))
                                .truncate(),
                        )
                        .on_hover_text(full_text);
                    });
                }
            });
    }
}

impl eframe::App for LangmuirDesktopApp {
    #[expect(
        clippy::too_many_lines,
        reason = "the top-level method declares the complete five-region instrument layout"
    )]
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.drain_backend();

        egui::Panel::top("application_header")
            .exact_size(56.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::WHITE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("航天设备集成控制中心")
                                .strong()
                                .size(18.0)
                                .color(theme::TEXT),
                        );
                        ui.label(
                            RichText::new("Langmuir Probe Ground Control")
                                .size(10.0)
                                .color(theme::TEXT_MUTED),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        #[cfg(feature = "mock")]
                        if self.mock_mode {
                            theme::status_badge(ui, "MOCK", theme::WARNING);
                        }
                        if let Some(warning) = self.font_warning.as_deref() {
                            ui.colored_label(theme::WARNING, "CJK FONT MISSING")
                                .on_hover_text(warning);
                        }
                        header_link_status(
                            ui,
                            if self.acquisition_running {
                                "采集中"
                            } else {
                                "已停止"
                            },
                            self.acquisition_running,
                        );
                        header_link_status(ui, "LVDS", self.lvds.connected);
                        header_link_status(ui, "RS422", self.rs422.connected);
                    });
                });
            });

        egui::Panel::left("command_sidebar")
            .resizable(true)
            .default_size(292.0)
            .size_range(275.0..=320.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::WHITE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::same(8)),
            )
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .id_salt("command_sidebar_scroll")
                    .show(ui, |ui| {
                        if let Some(request) =
                            Self::connection_card(ui, LinkKind::Rs422, &mut self.rs422)
                        {
                            self.submit(request);
                        }
                        ui.add_space(8.0);
                        self.command_panel(ui);
                    });
            });

        egui::Panel::right("telemetry_sidebar")
            .resizable(true)
            .default_size(260.0)
            .size_range(230.0..=290.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::PAGE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::same(8)),
            )
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .id_salt("telemetry_sidebar_scroll")
                    .show(ui, |ui| {
                        self.telemetry_panel(ui);
                        ui.add_space(7.0);
                        self.fault_panel(ui);
                    });
            });

        egui::Panel::bottom("system_log")
            .resizable(true)
            .default_size(150.0)
            .size_range(110.0..=240.0)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(249, 250, 251))
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(10, 8)),
            )
            .show(ui, |ui| self.log_panel(ui));

        egui::CentralPanel::default().show(ui, |ui| {
            self.operations_panel(ui);
            ui.add_space(8.0);
            theme::card().show(ui, |ui| self.plot_panel(ui));
        });

        ui.ctx().request_repaint_after(if self.acquisition_running {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(250)
        });
    }
}

fn status_dot(ui: &mut Ui, active: bool) {
    let color = if active {
        theme::SUCCESS
    } else {
        theme::DANGER
    };
    ui.colored_label(color, "●");
}

fn header_link_status(ui: &mut Ui, label: &str, active: bool) {
    let color = if active {
        theme::SUCCESS
    } else {
        theme::DANGER
    };
    ui.label(
        RichText::new(format!("● {label}"))
            .strong()
            .size(11.0)
            .color(color),
    );
}

fn value_cell(ui: &mut Ui, label: &str, value: String) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).small().color(theme::TEXT_SECONDARY));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(value)
                    .monospace()
                    .strong()
                    .color(theme::PRIMARY),
            );
        });
    });
}

fn dds_status_cell(ui: &mut Ui, label: &str, status: u8) {
    let (text, color) = match status {
        0 => ("停止", theme::TEXT_SECONDARY),
        1 => ("运行", theme::SUCCESS),
        2 => ("故障", theme::DANGER),
        _ => ("未知", theme::WARNING),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).small().color(theme::TEXT_SECONDARY));
        theme::status_badge(ui, text, color);
    })
    .response
    .on_hover_text(format!("状态码 0x{status:02X}"));
}

fn binary_status_cell(ui: &mut Ui, label: &str, status: u8) {
    let (text, color) = if status == 1 {
        ("正常", theme::SUCCESS)
    } else {
        ("异常", theme::DANGER)
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).small().color(theme::TEXT_SECONDARY));
        theme::status_badge(ui, text, color);
    })
    .response
    .on_hover_text(format!("状态码 0x{status:02X}"));
}

fn temperature_bar(ui: &mut Ui, label: &str, temperature: u8, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .size(10.0)
                .strong()
                .color(theme::TEXT_SECONDARY),
        );
        let fraction = (f32::from(temperature) / 100.0).clamp(0.0, 1.0);
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_width(ui.available_width())
                .fill(color)
                .text(format!("{temperature} ℃")),
        );
    });
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

    use super::{command_bytes, normalize_plot_points, recalculate_checksum};

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
    fn overlay_normalization_keeps_each_channel_in_its_own_scale() {
        let mut points = [[0.0, 100.0], [1.0, 200.0], [2.0, 150.0]];
        assert_eq!(normalize_plot_points(&mut points), Some((100.0, 200.0)));
        assert_eq!(points, [[0.0, 0.0], [1.0, 1.0], [2.0, 0.5]]);
    }
}
