use super::{
    BackendEvent, ComboBox, Duration, Instant, LangmuirDesktopApp, Local, LogLevel,
    PLOT_HISTORY_SECONDS, PlotViewMode, RichText, SamplePoint, ScrollArea, TimeSeriesBuffer, Ui,
    UiRequest, VecDeque, adc16_to_volts, central_control_row, egui, extend_scientific_channel,
    scientific_plot_buffer, theme,
};
use crate::bridge::{
    HistoryPoint, HistoryStatus, HistoryWindow, QualifiedSamples, RawRecordingStatus,
    RecoveryStatus,
};
use langmuir_core::FrameQuality;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver};

#[derive(Debug, Clone, Copy)]
enum PathPurpose {
    History,
    Raw,
    Export,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum QualitySelection {
    Native,
    #[default]
    Confirmed,
    Diagnostic,
}

impl QualitySelection {
    fn includes(self, quality: FrameQuality) -> bool {
        match quality {
            FrameQuality::Native => true,
            FrameQuality::Recovered1Bit | FrameQuality::Recovered4Bit => self != Self::Native,
            FrameQuality::RecoveryCandidate => self == Self::Diagnostic,
            FrameQuality::Rejected => false,
        }
    }
}

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "dialog visibility and independent export options do not form one state machine"
)]
pub(super) struct HistoryView {
    pub(super) status: HistoryStatus,
    pub(super) raw: RawRecordingStatus,
    pub(super) recovery: RecoveryStatus,
    pub(super) window: Option<HistoryWindow>,
    pub(super) channel_1: TimeSeriesBuffer,
    pub(super) channel_2: TimeSeriesBuffer,
    pub(super) live_channel_1: TimeSeriesBuffer,
    pub(super) live_channel_2: TimeSeriesBuffer,
    pub(super) live_has_qualified: bool,
    live_frames: VecDeque<QualifiedSamples>,
    live_candidates: VecDeque<QualifiedSamples>,
    live_epoch_start: Option<f64>,
    live_reliable_latest: Option<f64>,
    pub(super) selection: QualitySelection,
    pub(super) dialog_open: bool,
    path: String,
    raw_path: String,
    export_path: String,
    picker: Option<(PathPurpose, Receiver<Option<String>>)>,
    seek: f64,
    range: [f64; 2],
    export_recovered: bool,
    export_candidates: bool,
    export_visible: bool,
    pending_range: Option<([f64; 2], Instant)>,
    navigation_fraction: Option<f64>,
}

impl Default for HistoryView {
    fn default() -> Self {
        Self {
            status: HistoryStatus::default(),
            raw: RawRecordingStatus::default(),
            recovery: RecoveryStatus::default(),
            window: None,
            channel_1: scientific_plot_buffer(),
            channel_2: scientific_plot_buffer(),
            live_channel_1: scientific_plot_buffer(),
            live_channel_2: scientific_plot_buffer(),
            live_has_qualified: false,
            live_frames: VecDeque::new(),
            live_candidates: VecDeque::new(),
            live_epoch_start: None,
            live_reliable_latest: None,
            selection: QualitySelection::default(),
            dialog_open: false,
            path: String::new(),
            raw_path: String::new(),
            export_path: String::new(),
            picker: None,
            seek: 0.0,
            range: [0.0, 10.0],
            export_recovered: false,
            export_candidates: false,
            export_visible: false,
            pending_range: None,
            navigation_fraction: None,
        }
    }
}

impl LangmuirDesktopApp {
    pub(super) fn handle_history_event(&mut self, event: BackendEvent) {
        match event {
            BackendEvent::RawCaptureStatus(status) => self.lvds_view.history.raw = *status,
            BackendEvent::RecoveryStatus(status) => self.lvds_view.history.recovery = *status,
            BackendEvent::HistoryStatus(status) => self.apply_history_status(*status),
            BackendEvent::HistoryWindow(window) => self.apply_history_window(*window),
            BackendEvent::QualifiedScientificSamples(samples) => {
                self.remember_live_samples(*samples);
            }
            BackendEvent::HistoryCsvExported {
                path,
                rows,
                quality_format,
            } => {
                self.push_log(
                    LogLevel::Info,
                    format!(
                        "历史科学 CSV 已导出：{rows} 行{} · {path}",
                        if quality_format {
                            "（含质量信息）"
                        } else {
                            ""
                        }
                    ),
                );
            }
            _ => {}
        }
    }

    fn apply_history_status(&mut self, status: HistoryStatus) {
        let history = &mut self.lvds_view.history;
        if status.generation < history.status.generation {
            return;
        }
        let changed = status.active != history.status.active
            || status.generation != history.status.generation;
        if changed {
            history.window = None;
            history.channel_1.clear();
            history.channel_2.clear();
            history.pending_range = None;
            history.navigation_fraction = None;
            history.path.clone_from(&status.path);
            history.seek = status.position_fraction;
        }
        history.status = status;
        if changed {
            self.reset_plot_interaction();
        }
    }

    fn apply_history_window(&mut self, window: HistoryWindow) {
        let history = &mut self.lvds_view.history;
        if !history.status.active
            || window.generation != history.status.generation
            || history
                .window
                .as_ref()
                .is_some_and(|previous| previous.revision >= window.revision)
        {
            return;
        }
        if let Some(fraction) = history.navigation_fraction {
            let target = history.status.first_device_time.map(|first| {
                first
                    + (history.status.last_device_time.unwrap_or(first) - first).max(0.0) * fraction
            });
            if target.is_none_or(|target| (window.range[0] - target).abs() > 0.000_001) {
                return;
            }
        }
        let reset_view = history.window.is_none() || history.navigation_fraction.is_some();
        history.navigation_fraction = None;
        let range = window.range;
        history.window = Some(window);
        history.range = range;
        self.rebuild_history_buffers();
        if reset_view && !self.lvds_view.history.status.playing && range[1] > range[0] {
            self.operator.plot_view = PlotViewMode::ManualView;
            self.operator.plot_x_range = Some(range);
            self.operator.plot_reset_pending = true;
        }
    }

    pub(super) fn remember_native_samples(
        &mut self,
        start_seconds: f64,
        sample_interval_seconds: f64,
        channel_1: &[f64],
        channel_2: &[f64],
    ) {
        self.remember_live_samples(QualifiedSamples {
            quality: FrameQuality::Native,
            raw_record_id: 0,
            outer_sequence: None,
            frame_seq_u8: 0,
            start_seconds,
            sample_interval_seconds,
            channel_1: channel_1.to_vec(),
            channel_2: channel_2.to_vec(),
            source: String::new(),
            reason: String::new(),
        });
    }

    fn remember_live_samples(&mut self, frame: QualifiedSamples) {
        if !frame.start_seconds.is_finite()
            || !frame.sample_interval_seconds.is_finite()
            || frame.sample_interval_seconds <= 0.0
        {
            return;
        }
        if frame.quality == FrameQuality::RecoveryCandidate {
            self.remember_live_candidate(frame);
            return;
        }
        if frame.quality == FrameQuality::Rejected {
            return;
        }
        let history = &mut self.lvds_view.history;
        if let Some(previous) = history.live_frames.iter_mut().find(|previous| {
            previous.start_seconds.to_bits() == frame.start_seconds.to_bits()
                && previous.sample_interval_seconds.to_bits()
                    == frame.sample_interval_seconds.to_bits()
                && previous.channel_1.len() == frame.channel_1.len()
                && previous.channel_2.len() == frame.channel_2.len()
                && (previous.quality == FrameQuality::Native)
                    != (frame.quality == FrameQuality::Native)
        }) {
            // A later native frame promotes the same recovered sample span in
            // place. Re-appending it would look like a device clock restart.
            if frame.quality == FrameQuality::Native {
                *previous = frame;
                rebuild_live_buffers(history);
            }
            self.operator.last_scientific_update = Some(Instant::now());
            return;
        }
        if history
            .live_reliable_latest
            .is_some_and(|latest| latest > frame.start_seconds)
        {
            self.clear_live_quality_history();
        }
        let history = &mut self.lvds_view.history;
        history.live_epoch_start.get_or_insert(frame.start_seconds);
        history.live_reliable_latest = Some(frame.start_seconds);
        history.live_has_qualified |= frame.quality != FrameQuality::Native;
        let rebuild = history.selection == QualitySelection::Diagnostic
            && history
                .live_channel_1
                .latest_time()
                .is_some_and(|latest| latest > frame.start_seconds);
        if !rebuild && history.selection.includes(frame.quality) {
            append_live_frame(
                &mut history.live_channel_1,
                &mut history.live_channel_2,
                &frame,
            );
        }
        history.live_frames.push_back(frame);
        let latest = history.live_reliable_latest.unwrap_or_default();
        while history.live_frames.len() > 1201
            || history
                .live_frames
                .front()
                .is_some_and(|oldest| latest - oldest.start_seconds > PLOT_HISTORY_SECONDS)
        {
            history.live_frames.pop_front();
        }
        retain_epoch_candidates(history);
        if rebuild {
            rebuild_live_buffers(history);
        }
        self.operator.last_scientific_update = Some(Instant::now());
    }

    fn remember_live_candidate(&mut self, frame: QualifiedSamples) {
        let history = &mut self.lvds_view.history;
        if let Some(latest) = history.live_reliable_latest
            && (frame.start_seconds
                < history
                    .live_epoch_start
                    .unwrap_or(latest)
                    .max(latest - PLOT_HISTORY_SECONDS)
                || frame.start_seconds > latest + PLOT_HISTORY_SECONDS)
        {
            return;
        }
        // Only diagnostic frames are ordered here. Reliable receive order alone
        // establishes the epoch, detects restart and ages the science cache.
        let position = history
            .live_candidates
            .partition_point(|old| old.start_seconds < frame.start_seconds);
        if history
            .live_candidates
            .get(position)
            .is_some_and(|old| old.start_seconds.to_bits() == frame.start_seconds.to_bits())
        {
            return;
        }
        history.live_candidates.insert(position, frame);
        while history.live_candidates.len() > 1201 {
            history.live_candidates.pop_front();
        }
        retain_epoch_candidates(history);
        if history.selection == QualitySelection::Diagnostic {
            history.live_has_qualified = true;
            rebuild_live_buffers(history);
        }
        self.operator.last_scientific_update = Some(Instant::now());
    }

    pub(super) fn set_quality_selection(&mut self, selection: QualitySelection) {
        let history = &mut self.lvds_view.history;
        history.selection = selection;
        rebuild_live_buffers(history);
        self.rebuild_history_buffers();
        self.operator.frozen_plot = None;
    }

    pub(super) fn clear_live_quality_history(&mut self) {
        let history = &mut self.lvds_view.history;
        history.live_frames.clear();
        history.live_candidates.clear();
        history.live_epoch_start = None;
        history.live_reliable_latest = None;
        history.live_channel_1.clear();
        history.live_channel_2.clear();
        history.live_has_qualified = false;
    }

    fn rebuild_history_buffers(&mut self) {
        let history = &mut self.lvds_view.history;
        history.channel_1.clear();
        history.channel_2.clear();
        if let Some(window) = &history.window {
            for (buffer, points) in [
                (&mut history.channel_1, &window.channel_1),
                (&mut history.channel_2, &window.channel_2),
            ] {
                if history.selection == QualitySelection::Diagnostic {
                    buffer.extend(diagnostic_history_points(points));
                    continue;
                }
                buffer.extend(
                    reliable_history_points(points)
                        .into_iter()
                        .filter(|point| history.selection.includes(point.quality))
                        .map(|point| SamplePoint {
                            time_seconds: point.time_seconds,
                            value: adc16_to_volts(point.value),
                        }),
                );
            }
        }
    }

    pub(super) fn history_open_button(&mut self, ui: &mut Ui, short: bool) {
        if ui
            .button(if short {
                "历史"
            } else {
                "打开历史数据"
            })
            .clicked()
        {
            self.lvds_view.history.dialog_open = true;
        }
    }

    pub(super) fn raw_record_button(
        &mut self,
        ui: &mut Ui,
        request: &mut Option<UiRequest>,
        short: bool,
    ) {
        let history = &mut self.lvds_view.history;
        let active = history.raw.active;
        let label = match (active, short) {
            (true, _) => "停止原始录制",
            (false, true) => "原始录制",
            (false, false) => "开始原始录制",
        };
        let source_available = self.lvds.connected
            || (history.status.active
                && history.status.format == Some(crate::bridge::HistoryFileKind::PcapNg))
            || self
                .lvds_view
                .status
                .as_ref()
                .is_some_and(|s| s.replay.is_some());
        if ui.add_enabled(active || source_available, egui::Button::new(label))
            .on_hover_text("独立保存收到的全部 UDP Payload；历史 PCAP 将导出整份抓包。路径可在历史与原始数据窗口设置。")
            .clicked() {
            *request = Some(if active { UiRequest::StopRawRecording } else {
                let path = if history.raw_path.trim().is_empty() {
                    format!("data/raw-{}.lvdsraw",Local::now().format("%Y%m%d-%H%M%S-%3f"))
                } else {history.raw_path.trim().to_owned()};
                UiRequest::StartRawRecording {path}
            });
        }
    }

    pub(super) fn history_inline_controls(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        central_control_row(ui, |ui| {
            ui.strong("历史模式");
            let status = &self.lvds_view.history.status;
            ui.add(
                egui::Label::new(format!(
                    "{} · {} 条",
                    status
                        .format
                        .map_or("文件", crate::bridge::HistoryFileKind::label),
                    status.total_records.map_or_else(
                        || format!("已读 {}", status.read_records),
                        |n| n.to_string()
                    )
                ))
                .truncate(),
            )
            .on_hover_text(&status.path);
        });
        central_control_row(ui, |ui| {
            self.history_play_buttons(ui, request);
            self.history_open_button(ui, true);
            ui.menu_button("数据", |ui| {
                self.raw_record_button(ui, request, true);
                self.recovery_controls(ui, request);
            });
        });
        self.history_progress(ui, request);
        let status = &self.lvds_view.history.status;
        let raw = &self.lvds_view.history.raw;
        let prefix = if raw.error.is_some() || raw.dropped > 0 {
            format!("原始录制异常 · 丢失 {} · ", raw.dropped)
        } else {
            String::new()
        };
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "{prefix}原生 {} · 恢复 {} · 候选 {} · 拒绝 {}",
                    status.native, status.recovered, status.candidates, status.rejected
                ))
                .small(),
            )
            .truncate(),
        );
    }

    fn history_play_buttons(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        let status = &self.lvds_view.history.status;
        if ui
            .add_enabled(
                status.playing || !status.loading,
                egui::Button::new(if status.playing { "暂停" } else { "播放" }),
            )
            .clicked()
        {
            *request = Some(UiRequest::SetHistoryPlaying {
                playing: !status.playing,
            });
            self.follow_latest();
        }
        if ui.button("起点").clicked() {
            self.prepare_history_navigation(0.0);
            *request = Some(UiRequest::SeekHistory { fraction: 0.0 });
        }
        if ui.button("退出历史").clicked() {
            *request = Some(UiRequest::CloseHistory);
        }
    }

    fn history_progress(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        let history = &mut self.lvds_view.history;
        let slider_id = ui.id().with("history_seek");
        if !ui.ctx().egui_is_using_pointer() {
            history.seek = history.status.position_fraction.clamp(0.0, 1.0);
        }
        ui.horizontal(|ui| {
            ui.push_id(slider_id, |ui| {
                let response =
                    ui.add(egui::Slider::new(&mut history.seek, 0.0..=1.0).show_value(false));
                if response.drag_stopped() || (response.changed() && !response.dragged()) {
                    *request = Some(UiRequest::SeekHistory {
                        fraction: history.seek,
                    });
                }
            });
            ui.label(format!(
                "{:.1}%{}",
                history.status.position_fraction * 100.0,
                if history.status.loading {
                    " · 读取中"
                } else {
                    ""
                }
            ));
        });
        if let Some(UiRequest::SeekHistory { fraction }) = request {
            self.prepare_history_navigation(*fraction);
        }
    }

    fn prepare_history_navigation(&mut self, fraction: f64) {
        self.lvds_view.history.navigation_fraction = Some(fraction.clamp(0.0, 1.0));
        self.lvds_view.history.pending_range = None;
        self.operator.frozen_plot = None;
        self.follow_latest();
    }

    pub(super) fn recovery_controls(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        let mut enabled = self.lvds_view.history.recovery.enabled;
        if ui.checkbox(&mut enabled, "尝试恢复异常帧").changed() {
            *request = Some(UiRequest::SetRecoveryEnabled { enabled });
        }
        let mut selection = self.lvds_view.history.selection;
        ComboBox::from_id_salt("frame_quality_filter")
            .selected_text(quality_label(selection))
            .show_ui(ui, |ui| {
                for option in [
                    QualitySelection::Native,
                    QualitySelection::Confirmed,
                    QualitySelection::Diagnostic,
                ] {
                    ui.selectable_value(&mut selection, option, quality_label(option));
                }
            });
        if selection != self.lvds_view.history.selection {
            self.set_quality_selection(selection);
        }
        if selection == QualitySelection::Diagnostic {
            ui.colored_label(theme::WARNING, "包含诊断候选波形（恢复证据不完整）");
        }
    }

    pub(super) fn history_dialog(&mut self, context: &egui::Context) {
        self.poll_history_picker();
        if !self.lvds_view.history.dialog_open {
            return;
        }
        let mut open = true;
        let mut request = None;
        egui::Window::new("历史与原始数据")
            .open(&mut open)
            .default_width(630.0)
            .default_height(520.0)
            .vscroll(true)
            .show(context, |ui| {
                self.history_source_controls(ui, &mut request);
                if self.lvds_view.history.status.active {
                    ui.separator();
                    ui.horizontal(|ui| self.history_play_buttons(ui, &mut request));
                    self.history_progress(ui, &mut request);
                    self.history_file_details(ui, &mut request);
                    self.history_export_controls(ui, &mut request);
                    self.history_anomalies(ui);
                }
                ui.separator();
                self.recovery_controls(ui, &mut request);
                self.history_raw_controls(ui, &mut request);
            });
        self.lvds_view.history.dialog_open = open;
        if let Some(request) = request {
            self.submit(request);
        }
    }

    fn history_source_controls(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        ui.label("打开 LVDSRAW、LPD v3、PCAPNG 查看双通道历史波形。");
        ui.horizontal(|ui| {
            ui.add_sized(
                [ui.available_width() - 125.0, 28.0],
                egui::TextEdit::singleline(&mut self.lvds_view.history.path)
                    .hint_text("历史文件完整路径"),
            );
            if ui.button("浏览…").clicked() {
                self.start_history_picker(PathPurpose::History);
            }
            if ui
                .add_enabled(
                    !self.lvds_view.history.path.trim().is_empty(),
                    egui::Button::new("打开"),
                )
                .clicked()
            {
                *request = Some(UiRequest::OpenHistory {
                    path: self.lvds_view.history.path.trim().to_owned(),
                });
            }
        });
        let status = &self.lvds_view.history.status;
        if let Some(error) = &status.error {
            ui.colored_label(theme::DANGER, error);
        }
        if status.active {
            ui.label(format!(
                "{} · {}",
                status
                    .format
                    .map_or("历史文件", crate::bridge::HistoryFileKind::label),
                status.path
            ));
        }
    }

    fn history_file_details(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        let history = &mut self.lvds_view.history;
        let status = &history.status;
        ui.label(format!(
            "记录 {} / {} · 索引 {} · 设备时间 {} s · seq8 {} · UDP {}",
            status.read_records,
            status
                .total_records
                .map_or_else(|| "扫描中".to_owned(), |n| n.to_string()),
            status.index_entries,
            status
                .current_device_time
                .map_or_else(|| "—".to_owned(), |t| format!("{t:.6}")),
            status
                .current_seq8
                .map_or_else(|| "—".to_owned(), |s| s.to_string()),
            status
                .current_outer_seq
                .map_or_else(|| "—".to_owned(), |s| s.to_string())
        ));
        ui.horizontal(|ui| {
            ui.label("时间窗 (s)");
            ui.add(egui::DragValue::new(&mut history.range[0]).speed(0.1));
            ui.label("至");
            ui.add(egui::DragValue::new(&mut history.range[1]).speed(0.1));
            if ui.button("加载时间窗").clicked() {
                let start = history.range[0].max(0.0);
                let end = history.range[1].clamp(start + 0.001, start + PLOT_HISTORY_SECONDS);
                *request = Some(UiRequest::LoadHistoryWindow {
                    start_seconds: start,
                    end_seconds: end,
                });
                self.operator.plot_view = PlotViewMode::ManualView;
                self.operator.plot_x_range = Some([start, end]);
                self.operator.plot_reset_pending = true;
            }
        });
    }

    fn history_export_controls(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        ui.separator();
        ui.strong("导出历史科学数据");
        let history = &mut self.lvds_view.history;
        ui.horizontal(|ui| {
            ui.checkbox(&mut history.export_recovered, "包含已确认恢复");
            ui.checkbox(&mut history.export_candidates, "包含诊断候选");
            ui.checkbox(&mut history.export_visible, "仅当前可见时间窗");
        });
        ui.horizontal(|ui| {
            ui.add_sized(
                [ui.available_width() - 155.0, 28.0],
                egui::TextEdit::singleline(&mut self.lvds_view.history.export_path)
                    .hint_text("导出 CSV 路径（留空自动命名）"),
            );
            if ui.button("另存为…").clicked() {
                self.start_history_picker(PathPurpose::Export);
            }
            if ui
                .add_enabled(
                    !self.lvds_view.history.status.exporting,
                    egui::Button::new("导出 CSV"),
                )
                .clicked()
            {
                self.request_history_export(request);
            }
        });
        let status = &self.lvds_view.history.status;
        if status.exporting {
            ui.label("正在导出…");
        }
        if let Some(error) = &status.export_error {
            ui.colored_label(theme::DANGER, error);
        }
        if let Some(path) = &status.export_path {
            ui.label(path);
        }
    }

    fn request_history_export(&mut self, request: &mut Option<UiRequest>) {
        let visible_range = self.plot_range();
        let history = &mut self.lvds_view.history;
        let quality = history.export_recovered || history.export_candidates;
        let path = if history.export_path.trim().is_empty() {
            format!(
                "data/history-{}{}.csv",
                Local::now().format("%Y%m%d-%H%M%S-%3f"),
                if quality { "-quality-v1" } else { "" }
            )
        } else {
            history.export_path.trim().to_owned()
        };
        *request = Some(UiRequest::ExportHistoryCsv {
            path,
            include_recovered: history.export_recovered,
            include_candidates: history.export_candidates,
            range: history.export_visible.then_some(visible_range),
        });
    }

    fn history_anomalies(&self, ui: &mut Ui) {
        ui.collapsing("异常与帧信息", |ui| {
            if let Some(window) = &self.lvds_view.history.window {
                ui.label(format!(
                    "当前窗口 {} 帧；完整来源与字节记录见审计文件。",
                    window.frames.len()
                ));
            }
            ScrollArea::vertical()
                .max_height(155.0)
                .id_salt("history_anomalies")
                .show(ui, |ui| {
                    for frame in &self.lvds_view.history.status.anomalies {
                        ui.label(format!(
                            "#{} · {} · {}",
                            frame.record_id,
                            frame.quality.label(),
                            frame.reason
                        ))
                        .on_hover_text(format!(
                            "来源 {}\n设备时间 {:?}\nseq8 {:?} · UDP {:?}",
                            frame.source,
                            frame.device_time_seconds,
                            frame.frame_seq_u8,
                            frame.outer_sequence
                        ));
                    }
                });
        });
    }

    fn history_raw_controls(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        ui.separator();
        ui.strong("原始 UDP 录制");
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.lvds_view.history.raw.active, |ui| {
                ui.add_sized(
                    [ui.available_width() - 100.0, 28.0],
                    egui::TextEdit::singleline(&mut self.lvds_view.history.raw_path)
                        .hint_text("LVDSRAW 路径（留空自动命名）"),
                );
                if ui.button("另存为…").clicked() {
                    self.start_history_picker(PathPurpose::Raw);
                }
            });
        });
        self.raw_record_button(ui, request, false);
        let raw = &self.lvds_view.history.raw;
        let state = if raw.active {
            "录制中"
        } else if raw.closed && raw.complete {
            "已关闭"
        } else if raw.closed {
            "归档不完整"
        } else {
            "未录制"
        };
        ui.label(format!(
            "{state} · 收到 {} · 入队 {} · 写入 {} · 丢失 {}",
            raw.received, raw.enqueued, raw.written, raw.dropped
        ));
        if let Some(path) = &raw.path {
            ui.label(path);
        }
        if let Some(error) = &raw.error {
            ui.colored_label(theme::DANGER, error);
        }
        let recovery = &self.lvds_view.history.recovery;
        if let Some(path) = &recovery.audit_path {
            ui.label(format!("恢复审计：{path}"));
        }
        if recovery.audit_dropped > 0 {
            ui.colored_label(
                theme::DANGER,
                format!("审计记录丢失 {}", recovery.audit_dropped),
            );
        }
        if let Some(error) = &recovery.error {
            ui.colored_label(theme::DANGER, error);
        }
    }

    fn start_history_picker(&mut self, purpose: PathPurpose) {
        if self.lvds_view.history.picker.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        self.lvds_view.history.picker = Some((purpose, receiver));
        std::thread::spawn(move || {
            #[cfg(target_os = "windows")]
            let path = match purpose {
                PathPurpose::History => rfd::FileDialog::new()
                    .add_filter("历史科学数据", &["lvdsraw", "lpd", "pcapng"])
                    .pick_file(),
                PathPurpose::Raw => rfd::FileDialog::new()
                    .add_filter("原始 UDP", &["lvdsraw"])
                    .save_file(),
                PathPurpose::Export => rfd::FileDialog::new()
                    .add_filter("科学数据 CSV", &["csv"])
                    .save_file(),
            }
            .map(|p| p.to_string_lossy().into_owned());
            #[cfg(not(target_os = "windows"))]
            let path: Option<String> = None;
            let _ = sender.send(path);
        });
    }

    fn poll_history_picker(&mut self) {
        let history = &mut self.lvds_view.history;
        let Some((purpose, receiver)) = &history.picker else {
            return;
        };
        let Ok(path) = receiver.try_recv() else {
            return;
        };
        if let Some(path) = path {
            match purpose {
                PathPurpose::History => history.path = path,
                PathPurpose::Raw => history.raw_path = path,
                PathPurpose::Export => history.export_path = path,
            }
        }
        history.picker = None;
    }

    pub(super) fn request_visible_history_window(&mut self, range: [f64; 2]) {
        let history = &mut self.lvds_view.history;
        if !history.status.active
            || history.status.playing
            || history.navigation_fraction.is_some()
            || self.operator.frozen_plot.is_some()
        {
            return;
        }
        let range = [range[0], range[1].min(range[0] + PLOT_HISTORY_SECONDS)];
        if !range.iter().all(|v| v.is_finite()) || range[1] <= range[0] {
            return;
        }
        if history
            .window
            .as_ref()
            .is_some_and(|w| range[0] >= w.range[0] && range[1] <= w.range[1])
        {
            history.pending_range = None;
            return;
        }
        match history.pending_range {
            Some((previous, since))
                if (previous[0] - range[0]).abs() < 0.001
                    && (previous[1] - range[1]).abs() < 0.001 =>
            {
                if since.elapsed() >= Duration::from_millis(150) && !history.status.loading {
                    history.pending_range = None;
                    self.submit(UiRequest::LoadHistoryWindow {
                        start_seconds: range[0],
                        end_seconds: range[1],
                    });
                }
            }
            _ => history.pending_range = Some((range, Instant::now())),
        }
    }

    pub(super) fn displayed_quality_label(&self) -> &'static str {
        match self.lvds_view.history.selection {
            QualitySelection::Native => "原生帧",
            QualitySelection::Confirmed => "原生 / 已确认恢复",
            QualitySelection::Diagnostic => "诊断候选波形（含原生 / 已确认恢复）",
        }
    }
}

fn reliable_history_points(points: &[HistoryPoint]) -> Vec<&HistoryPoint> {
    let mut output = Vec::with_capacity(points.len());
    let mut recovered = HashMap::new();
    let mut native = HashSet::new();
    let mut previous_reliable = None;
    for point in points.iter().filter(|point| {
        matches!(
            point.quality,
            FrameQuality::Native | FrameQuality::Recovered1Bit | FrameQuality::Recovered4Bit
        )
    }) {
        let timestamp = point.time_seconds.to_bits();
        if point.quality == FrameQuality::Native {
            if let Some(position) = recovered.remove(&timestamp) {
                output[position] = point;
                native.insert(timestamp);
                continue;
            }
        } else if native.contains(&timestamp) {
            continue;
        }
        // Preserve file order, including genuine clock resets. Only native
        // promotion of a recovered timestamp bypasses the rollback path.
        if previous_reliable.is_some_and(|last| point.time_seconds < last) {
            recovered.clear();
            native.clear();
        }
        if point.quality == FrameQuality::Native {
            native.insert(timestamp);
        } else {
            recovered.insert(timestamp, output.len());
        }
        output.push(point);
        previous_reliable = Some(point.time_seconds);
    }
    output
}

fn diagnostic_history_points(points: &[HistoryPoint]) -> Vec<SamplePoint> {
    let mut candidates: Vec<_> = points
        .iter()
        .filter(|point| point.quality == FrameQuality::RecoveryCandidate)
        .collect();
    // Only candidates are ordered. Reliable samples retain file order so an
    // actual native clock restart still resets TimeSeriesBuffer's epoch.
    candidates.sort_by(|left, right| left.time_seconds.total_cmp(&right.time_seconds));
    let mut candidates = candidates.into_iter().peekable();
    let mut output: Vec<SamplePoint> = Vec::with_capacity(points.len());
    let mut previous_reliable = None;
    let mut clock_restarted = false;
    for point in reliable_history_points(points) {
        if previous_reliable.is_some_and(|last| point.time_seconds < last) {
            clock_restarted = true;
        }
        if !clock_restarted {
            while let Some(candidate) =
                candidates.next_if(|candidate| candidate.time_seconds < point.time_seconds)
            {
                if output
                    .last()
                    .is_none_or(|last| candidate.time_seconds > last.time_seconds)
                {
                    output.push(SamplePoint {
                        time_seconds: candidate.time_seconds,
                        value: adc16_to_volts(candidate.value),
                    });
                }
            }
            // A reliable sample wins at the same timestamp.
            while candidates
                .next_if(|candidate| candidate.time_seconds <= point.time_seconds)
                .is_some()
            {}
        }
        output.push(SamplePoint {
            time_seconds: point.time_seconds,
            value: adc16_to_volts(point.value),
        });
        previous_reliable = Some(point.time_seconds);
    }
    if !clock_restarted {
        for candidate in candidates {
            if output
                .last()
                .is_none_or(|last| candidate.time_seconds > last.time_seconds)
            {
                output.push(SamplePoint {
                    time_seconds: candidate.time_seconds,
                    value: adc16_to_volts(candidate.value),
                });
            }
        }
    }
    output
}

fn append_live_frame(
    one: &mut TimeSeriesBuffer,
    two: &mut TimeSeriesBuffer,
    frame: &QualifiedSamples,
) {
    extend_scientific_channel(
        one,
        frame.start_seconds,
        frame.sample_interval_seconds,
        frame.channel_1.clone(),
    );
    extend_scientific_channel(
        two,
        frame.start_seconds,
        frame.sample_interval_seconds,
        frame.channel_2.clone(),
    );
}

fn retain_epoch_candidates(history: &mut HistoryView) {
    let Some(latest) = history.live_reliable_latest.or_else(|| {
        history
            .live_candidates
            .back()
            .map(|frame| frame.start_seconds)
    }) else {
        return;
    };
    let first = history
        .live_epoch_start
        .unwrap_or(latest - PLOT_HISTORY_SECONDS)
        .max(latest - PLOT_HISTORY_SECONDS);
    history.live_candidates.retain(|frame| {
        frame.start_seconds >= first && frame.start_seconds <= latest + PLOT_HISTORY_SECONDS
    });
}

fn candidate_end(frame: &QualifiedSamples) -> f64 {
    let samples = frame
        .channel_1
        .len()
        .max(frame.channel_2.len())
        .saturating_sub(1);
    frame.start_seconds
        + frame.sample_interval_seconds * f64::from(u32::try_from(samples).unwrap_or(u32::MAX))
}

fn rebuild_live_buffers(history: &mut HistoryView) {
    history.live_channel_1.clear();
    history.live_channel_2.clear();
    // Keep the qualified display path for this epoch even after every
    // recovered frame has been promoted to native in place.
    history.live_has_qualified |= history
        .live_frames
        .iter()
        .any(|frame| frame.quality != FrameQuality::Native)
        || (history.selection == QualitySelection::Diagnostic
            && !history.live_candidates.is_empty());
    let mut candidates = history.live_candidates.iter().peekable();
    for frame in &history.live_frames {
        while let Some(candidate) =
            candidates.next_if(|candidate| candidate.start_seconds <= frame.start_seconds)
        {
            if history.selection == QualitySelection::Diagnostic
                && candidate_end(candidate) < frame.start_seconds
                && history
                    .live_channel_1
                    .latest_time()
                    .is_none_or(|last| candidate.start_seconds > last)
            {
                append_live_frame(
                    &mut history.live_channel_1,
                    &mut history.live_channel_2,
                    candidate,
                );
            }
        }
        if history.selection.includes(frame.quality) {
            append_live_frame(
                &mut history.live_channel_1,
                &mut history.live_channel_2,
                frame,
            );
        }
    }
    if history.selection == QualitySelection::Diagnostic {
        for candidate in candidates {
            if history
                .live_channel_1
                .latest_time()
                .is_none_or(|last| candidate.start_seconds > last)
            {
                append_live_frame(
                    &mut history.live_channel_1,
                    &mut history.live_channel_2,
                    candidate,
                );
            }
        }
    }
}

fn quality_label(selection: QualitySelection) -> &'static str {
    match selection {
        QualitySelection::Native => "原生帧",
        QualitySelection::Confirmed => "原生 + 已确认恢复",
        QualitySelection::Diagnostic => "包含诊断候选",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{HistoryPoint, LvdsStatusSnapshot, channel_bridge};
    use langmuir_core::FrameQuality;

    fn app() -> LangmuirDesktopApp {
        let (bridge, _endpoint) = channel_bridge(16);
        LangmuirDesktopApp::with_bridge(
            Box::new(bridge),
            None,
            #[cfg(feature = "mock")]
            false,
        )
    }

    #[test]
    fn history_events_keep_file_mode_separate_from_live_connection() {
        let mut app = app();
        app.handle_backend_event(BackendEvent::HistoryStatus(Box::new(HistoryStatus {
            generation: 7,
            active: true,
            path: "user.lpd".to_owned(),
            ..HistoryStatus::default()
        })));
        assert!(app.lvds_view.history.status.active);
        assert!(!app.lvds.connected);
        assert!(!app.lvds_view.history.recovery.enabled);
    }

    #[test]
    fn raw_loss_and_close_status_remain_independent_of_scientific_saving() {
        let mut app = app();
        app.acquisition_running = true;
        app.handle_backend_event(BackendEvent::RawCaptureStatus(Box::new(
            RawRecordingStatus {
                active: false,
                received: 20,
                written: 19,
                dropped: 1,
                closed: true,
                complete: false,
                error: Some("queue overflow".to_owned()),
                ..RawRecordingStatus::default()
            },
        )));
        assert!(app.acquisition_running);
        assert_eq!(app.lvds_view.history.raw.dropped, 1);
        assert!(!app.lvds_view.history.raw.complete);
    }

    #[test]
    fn historical_window_rejects_stale_file_generations() {
        let mut app = app();
        app.handle_backend_event(BackendEvent::HistoryStatus(Box::new(HistoryStatus {
            generation: 3,
            active: true,
            ..HistoryStatus::default()
        })));
        let window = |generation| HistoryWindow {
            generation,
            revision: 1,
            range: [1.0, 2.0],
            channel_1: vec![HistoryPoint {
                time_seconds: 1.0,
                value: 64.0,
                quality: FrameQuality::Native,
                record_id: 0,
            }],
            channel_2: Vec::new(),
            frames: Vec::new(),
        };
        app.handle_backend_event(BackendEvent::HistoryWindow(Box::new(window(3))));
        app.handle_backend_event(BackendEvent::HistoryWindow(Box::new(window(2))));
        assert_eq!(app.lvds_view.history.window.as_ref().unwrap().generation, 3);
    }

    #[test]
    fn operators_can_open_history_and_record_raw_without_enabling_scientific_save() {
        let mut app = app();
        let context = egui::Context::default();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 1000.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                app.operations_panel(ui);
            },
        );
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    Some(text.galley.text().to_owned())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            labels.iter().any(|text| text == "打开历史数据"),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|text| text == "开始原始录制"),
            "{labels:?}"
        );
        assert!(!app.acquisition_running);
    }

    #[test]
    fn clicking_start_of_history_moves_the_view_to_the_returned_window() {
        let mut app = app();
        app.apply_history_status(HistoryStatus {
            generation: 1,
            active: true,
            first_device_time: Some(0.0),
            last_device_time: Some(1000.0),
            ..HistoryStatus::default()
        });
        let window = |revision, range| HistoryWindow {
            generation: 1,
            revision,
            range,
            ..HistoryWindow::default()
        };
        app.apply_history_window(window(1, [600.0, 660.0]));
        let context = egui::Context::default();
        let mut request = None;
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            ui.horizontal(|ui| app.history_play_buttons(ui, &mut request));
        });
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "起点" => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            })
            .unwrap();
        for pressed in [true, false] {
            let _ = context.run_ui(
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..egui::RawInput::default()
                },
                |ui| {
                    ui.horizontal(|ui| app.history_play_buttons(ui, &mut request));
                },
            );
        }
        assert_eq!(request, Some(UiRequest::SeekHistory { fraction: 0.0 }));
        app.apply_history_window(window(2, [0.0, 60.0]));
        assert_eq!(app.plot_range(), [0.0, 60.0]);
    }

    #[test]
    fn diagnostic_quality_stays_visible_when_the_settings_window_is_closed() {
        let mut app = app();
        app.set_quality_selection(QualitySelection::Diagnostic);
        let output =
            egui::Context::default().run_ui(egui::RawInput::default(), |ui| app.plot_panel(ui));
        assert!(output.shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text().contains("诊断候选"))));
    }

    #[test]
    fn selected_quality_and_freeze_preserve_both_historical_channels() {
        let mut app = app();
        app.apply_history_status(HistoryStatus {
            generation: 1,
            active: true,
            ..HistoryStatus::default()
        });
        let points = || {
            [
                FrameQuality::Native,
                FrameQuality::Recovered4Bit,
                FrameQuality::RecoveryCandidate,
            ]
            .into_iter()
            .enumerate()
            .map(|(i, quality)| HistoryPoint {
                time_seconds: f64::from(u32::try_from(i).unwrap()),
                value: 64.0,
                quality,
                record_id: u64::try_from(i).unwrap(),
            })
            .collect()
        };
        app.apply_history_window(HistoryWindow {
            generation: 1,
            revision: 1,
            range: [0.0, 3.0],
            channel_1: points(),
            channel_2: points(),
            frames: Vec::new(),
        });
        for (selection, count) in [
            (QualitySelection::Native, 1),
            (QualitySelection::Confirmed, 2),
            (QualitySelection::Diagnostic, 3),
        ] {
            app.set_quality_selection(selection);
            let (one, two) = app.display_histories();
            assert_eq!((one.len(), two.len()), (count, count));
        }
        app.toggle_display_freeze();
        app.apply_history_window(HistoryWindow {
            generation: 1,
            revision: 2,
            range: [3.0, 6.0],
            ..HistoryWindow::default()
        });
        assert_eq!(app.display_histories().0.len(), 3);
        app.apply_history_status(HistoryStatus {
            generation: 2,
            ..HistoryStatus::default()
        });
        assert!(app.display_histories().0.is_empty());
    }

    #[test]
    fn live_source_changes_do_not_reset_the_historical_view_or_freeze() {
        let mut app = app();
        app.apply_lvds_status(LvdsStatusSnapshot {
            source: "live-session-a".to_owned(),
            ..LvdsStatusSnapshot::default()
        });
        app.apply_history_status(HistoryStatus {
            generation: 1,
            active: true,
            ..HistoryStatus::default()
        });
        app.apply_history_window(HistoryWindow {
            generation: 1,
            revision: 1,
            range: [30.0, 40.0],
            ..HistoryWindow::default()
        });
        app.toggle_display_freeze();
        app.apply_lvds_status(LvdsStatusSnapshot {
            source: "live-session-b".to_owned(),
            ..LvdsStatusSnapshot::default()
        });
        assert!(app.operator.frozen_plot.is_some());
        assert_eq!(app.plot_range(), [30.0, 40.0]);
    }

    fn live_frame(quality: FrameQuality, start_seconds: f64, value: f64) -> BackendEvent {
        if quality == FrameQuality::Native {
            return BackendEvent::ScientificSamples {
                channel_1_start_seconds: start_seconds,
                channel_2_start_seconds: start_seconds,
                sample_interval_seconds: 0.0005,
                channel_1: vec![value],
                channel_2: vec![-value],
            };
        }
        BackendEvent::QualifiedScientificSamples(Box::new(QualifiedSamples {
            quality,
            raw_record_id: 1,
            outer_sequence: Some(1),
            frame_seq_u8: 1,
            start_seconds,
            sample_interval_seconds: 0.0005,
            channel_1: vec![value],
            channel_2: vec![-value],
            source: "test session".to_owned(),
            reason: String::new(),
        }))
    }

    fn full_live_frame(quality: FrameQuality, start_seconds: f64, value: f64) -> BackendEvent {
        let mut event = live_frame(quality, start_seconds, value);
        match &mut event {
            BackendEvent::ScientificSamples {
                channel_1,
                channel_2,
                ..
            } => {
                *channel_1 = vec![value; 100];
                *channel_2 = vec![-value; 100];
            }
            BackendEvent::QualifiedScientificSamples(frame) => {
                frame.channel_1 = vec![value; 100];
                frame.channel_2 = vec![-value; 100];
            }
            _ => unreachable!(),
        }
        event
    }

    #[test]
    fn overlap_live_native_promotion_preserves_prior_and_later_frames_after_rebuild() {
        for selection in [
            QualitySelection::Native,
            QualitySelection::Confirmed,
            QualitySelection::Diagnostic,
        ] {
            for delayed in [false, true] {
                let mut app = app();
                app.set_quality_selection(selection);
                app.handle_backend_event(full_live_frame(FrameQuality::Native, 100.0, 1.0));
                app.handle_backend_event(full_live_frame(FrameQuality::Recovered4Bit, 100.05, 2.0));
                assert_eq!(
                    app.display_plot_points().0.len(),
                    if selection == QualitySelection::Native {
                        100
                    } else {
                        200
                    },
                    "native-only selection must filter recovered samples"
                );
                if delayed {
                    app.handle_backend_event(full_live_frame(FrameQuality::Native, 100.1, 4.0));
                }
                app.set_quality_selection(selection);
                app.handle_backend_event(full_live_frame(FrameQuality::Native, 100.05, 3.0));
                let expected = if delayed { 300 } else { 200 };
                for next_selection in [
                    selection,
                    QualitySelection::Native,
                    QualitySelection::Confirmed,
                    QualitySelection::Diagnostic,
                    selection,
                ] {
                    app.set_quality_selection(next_selection);
                    let (one, two) = app.display_plot_points();
                    assert_eq!(
                        one.len(),
                        expected,
                        "Native promotion erased prior science: {selection:?} -> {next_selection:?}, delayed={delayed}"
                    );
                    assert_eq!(two.len(), expected);
                    assert_eq!(one[0], [100.0, 0.000_156_25]);
                    assert_eq!(two[0], [100.0, -0.000_156_25]);
                    assert_eq!(one[100], [100.05, 0.000_468_75]);
                    assert_eq!(two[100], [100.05, -0.000_468_75]);
                    assert!(one.windows(2).all(|pair| pair[0][0] < pair[1][0]));
                    let (one, two) = app.display_histories();
                    assert_eq!((one.len(), two.len()), (expected, expected));
                }
                app.handle_backend_event(full_live_frame(FrameQuality::Native, 1.0, 9.0));
                assert_eq!(
                    app.display_plot_points().0.len(),
                    100,
                    "a true native clock restart must still clear the old epoch"
                );
                assert_eq!(app.display_plot_points().0[0], [1.0, 0.001_406_25]);
            }
        }
    }

    #[test]
    fn overlap_history_native_promotion_wins_at_each_sample_without_sorting_clock_restarts() {
        for selection in [
            QualitySelection::Native,
            QualitySelection::Confirmed,
            QualitySelection::Diagnostic,
        ] {
            let mut app = app();
            app.apply_history_status(HistoryStatus {
                generation: 1,
                active: true,
                ..HistoryStatus::default()
            });
            let mut points = Vec::new();
            for (record_id, quality, start, value) in [
                (1, FrameQuality::Native, 100.0, 1.0),
                (2, FrameQuality::Recovered1Bit, 100.05, 2.0),
                (3, FrameQuality::Native, 100.1, 4.0),
                (4, FrameQuality::Native, 100.05, 3.0),
            ] {
                for index in 0..100 {
                    points.push(HistoryPoint {
                        time_seconds: start + f64::from(index) * 0.0005,
                        value,
                        quality,
                        record_id,
                    });
                }
            }
            app.apply_history_window(HistoryWindow {
                generation: 1,
                revision: 1,
                range: [100.0, 100.2],
                channel_1: points.clone(),
                channel_2: points,
                ..HistoryWindow::default()
            });
            app.set_quality_selection(selection);
            let (one, two) = app.display_plot_points();
            assert_eq!(
                one.len(),
                300,
                "{selection:?} lost points while promoting a historical native frame"
            );
            assert_eq!(two, one);
            assert_eq!(one[100], [100.05, 0.000_468_75]);
            assert_eq!(one[200], [100.1, 0.000_625]);
            assert!(one.windows(2).all(|pair| pair[0][0] < pair[1][0]));
        }
    }

    #[test]
    fn candidate_clock_cannot_clear_or_age_out_reliable_live_samples() {
        for bad_time in [0.0, 10_000.0] {
            let mut app = app();
            app.handle_backend_event(live_frame(FrameQuality::Native, 100.0, 1.0));
            for _ in 0..1205 {
                app.handle_backend_event(live_frame(
                    FrameQuality::RecoveryCandidate,
                    bad_time,
                    900.0,
                ));
            }
            for index in 1_u32..=1205 {
                app.handle_backend_event(live_frame(
                    FrameQuality::RecoveryCandidate,
                    100.0 + f64::from(index) * 0.001,
                    800.0,
                ));
            }
            app.handle_backend_event(live_frame(FrameQuality::Recovered1Bit, 100.05, 2.0));
            let (one, two) = app.display_plot_points();
            assert_eq!(
                one,
                vec![[100.0, 0.000_156_25], [100.05, 0.000_312_5]],
                "candidate time {bad_time}"
            );
            assert_eq!(two, vec![[100.0, -0.000_156_25], [100.05, -0.000_312_5]]);
        }
    }

    #[test]
    fn confirmed_recovery_refreshes_the_scientific_display_cadence() {
        let mut app = app();
        app.handle_backend_event(live_frame(FrameQuality::Recovered1Bit, 100.0, 1.0));
        assert!(app.operator.last_scientific_update.is_some());
    }

    #[test]
    fn late_same_epoch_candidate_remains_diagnostic_without_reordering_reliable_frames() {
        let mut app = app();
        app.handle_backend_event(live_frame(FrameQuality::Native, 100.0, 1.0));
        app.handle_backend_event(live_frame(FrameQuality::Recovered4Bit, 100.1, 3.0));
        app.handle_backend_event(live_frame(FrameQuality::RecoveryCandidate, 100.05, 2.0));
        app.set_quality_selection(QualitySelection::Diagnostic);
        assert_eq!(
            app.display_plot_points().0,
            vec![
                [100.0, 0.000_156_25],
                [100.05, 0.000_312_5],
                [100.1, 0.000_468_75]
            ]
        );
        app.set_quality_selection(QualitySelection::Confirmed);
        assert_eq!(
            app.display_plot_points().0,
            vec![[100.0, 0.000_156_25], [100.1, 0.000_468_75]]
        );
        app.handle_backend_event(live_frame(FrameQuality::Native, 1.0, 4.0));
        assert_eq!(
            app.display_plot_points().0,
            vec![[1.0, 0.000_625]],
            "native rollback must still start a new epoch"
        );
    }

    #[test]
    fn historical_diagnostic_candidates_fill_gaps_without_erasing_reliable_points() {
        let mut app = app();
        app.apply_history_status(HistoryStatus {
            generation: 1,
            active: true,
            ..HistoryStatus::default()
        });
        let point = |time_seconds, value, quality| HistoryPoint {
            time_seconds,
            value,
            quality,
            record_id: 1,
        };
        let points = vec![
            point(100.0, 1.0, FrameQuality::Native),
            point(100.1, 3.0, FrameQuality::Recovered1Bit),
            point(100.05, 2.0, FrameQuality::RecoveryCandidate),
            point(100.1, 900.0, FrameQuality::RecoveryCandidate),
        ];
        app.apply_history_window(HistoryWindow {
            generation: 1,
            revision: 1,
            range: [100.0, 100.2],
            channel_1: points.clone(),
            channel_2: points,
            ..HistoryWindow::default()
        });
        app.set_quality_selection(QualitySelection::Diagnostic);
        let (one, two) = app.display_plot_points();
        assert_eq!(
            one,
            vec![
                [100.0, 0.000_156_25],
                [100.05, 0.000_312_5],
                [100.1, 0.000_468_75]
            ]
        );
        assert_eq!(two, one);
        app.set_quality_selection(QualitySelection::Confirmed);
        assert_eq!(
            app.display_plot_points().0,
            vec![[100.0, 0.000_156_25], [100.1, 0.000_468_75]]
        );
    }

    #[test]
    fn historical_diagnostic_merge_keeps_a_real_native_clock_restart_visible() {
        let mut app = app();
        app.apply_history_status(HistoryStatus {
            generation: 1,
            active: true,
            ..HistoryStatus::default()
        });
        let point = |time_seconds, value, quality| HistoryPoint {
            time_seconds,
            value,
            quality,
            record_id: 1,
        };
        app.apply_history_window(HistoryWindow {
            generation: 1,
            revision: 1,
            range: [0.0, 110.0],
            channel_1: vec![
                point(100.0, 1.0, FrameQuality::Native),
                point(1.0, 2.0, FrameQuality::Native),
                point(100.05, 900.0, FrameQuality::RecoveryCandidate),
            ],
            ..HistoryWindow::default()
        });
        app.set_quality_selection(QualitySelection::Diagnostic);
        assert_eq!(app.display_plot_points().0, vec![[1.0, 0.000_312_5]]);
    }

    #[test]
    fn an_already_queued_window_cannot_consume_a_new_seek_target() {
        let mut app = app();
        app.apply_history_status(HistoryStatus {
            generation: 1,
            active: true,
            first_device_time: Some(0.0),
            last_device_time: Some(1000.0),
            ..HistoryStatus::default()
        });
        let window = |revision, range| HistoryWindow {
            generation: 1,
            revision,
            range,
            ..HistoryWindow::default()
        };
        app.apply_history_window(window(1, [600.0, 660.0]));
        let context = egui::Context::default();
        let mut request = None;
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            ui.horizontal(|ui| app.history_play_buttons(ui, &mut request));
        });
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "起点" => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            })
            .unwrap();
        for pressed in [true, false] {
            let _ = context.run_ui(
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..egui::RawInput::default()
                },
                |ui| {
                    ui.horizontal(|ui| app.history_play_buttons(ui, &mut request));
                },
            );
        }
        assert_eq!(request, Some(UiRequest::SeekHistory { fraction: 0.0 }));
        app.apply_history_window(window(2, [600.5, 660.5]));
        assert_eq!(
            app.lvds_view.history.window.as_ref().unwrap().revision,
            1,
            "old queued window must not consume navigation"
        );
        app.apply_history_window(window(3, [0.0, 60.0]));
        assert_eq!(app.plot_range(), [0.0, 60.0]);
        app.apply_history_window(window(2, [600.5, 660.5]));
        assert_eq!(app.plot_range(), [0.0, 60.0]);
        app.prepare_history_navigation(0.8);
        app.apply_history_window(window(4, [0.0, 60.0]));
        app.apply_history_window(window(5, [800.0, 860.0]));
        assert_eq!(app.plot_range(), [800.0, 860.0]);
    }
}
