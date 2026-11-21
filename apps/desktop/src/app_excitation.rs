//! A bounded, single-frame AB view. Overlapping excitation frames are never joined.
use super::{
    Align, Instant, LangmuirDesktopApp, Layout, Local, LogLevel, PlotPoint, PlotViewMode, RichText,
    ScientificPlotConfig, Ui, UiRequest, central_control_row, central_control_style, debug_ui_rect,
    egui, format_plot_timestamp, show_scientific_plot, theme,
};
use crate::bridge::{ExcitationStorageStatus, HistoryStatus};
use langmuir_core::ExcitationRecord;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum WaveformView {
    #[default]
    Scientific,
    Excitation,
}

#[derive(Debug, Clone)]
pub(super) struct FrameSnapshot {
    record: Option<ExcitationRecord>,
}

#[derive(Debug)]
pub(super) struct ExcitationView {
    latest: Option<ExcitationRecord>,
    manual: Option<FrameSnapshot>,
    pub(super) frozen: Option<FrameSnapshot>,
    view: PlotViewMode,
    range: Option<[f64; 2]>,
    cursor: Option<f64>,
    reset_pending: bool,
    csv_path: String,
    export_path: String,
    history_navigation_pending: Option<u64>,
}

impl Default for ExcitationView {
    fn default() -> Self {
        Self {
            latest: None,
            manual: None,
            frozen: None,
            view: PlotViewMode::FollowLatest,
            range: None,
            cursor: None,
            reset_pending: true,
            csv_path: String::new(),
            export_path: String::new(),
            history_navigation_pending: None,
        }
    }
}

impl LangmuirDesktopApp {
    pub(super) fn accept_excitation_display_frame(&mut self, record: ExcitationRecord) {
        // Critical statuses are delivered before queued display events. Admit
        // visual data only from the session established by that status.
        if let Some(status) = &self.lvds_view.status
            && status.excitation_latest.as_ref().is_none_or(|current| {
                current.source != record.source || current.session != record.session
            })
        {
            return;
        }
        self.remember_excitation_frame(record);
    }

    pub(super) fn remember_excitation_frame(&mut self, record: ExcitationRecord) {
        let previous = self.lvds_view.excitation.latest.as_ref();
        let same_session = previous.is_some_and(|previous| {
            previous.source == record.source && previous.session == record.session
        });
        // A reliable status snapshot may already contain a more recent frame
        // than the bounded visual event queue. Record IDs describe receive order.
        if same_session
            && previous.is_some_and(|previous| previous.raw_record_id >= record.raw_record_id)
        {
            return;
        }
        if previous.is_some() && !same_session {
            self.reset_live_excitation();
        }
        self.lvds_view.excitation.latest = Some(record);
    }

    pub(super) fn reset_live_excitation(&mut self) {
        self.lvds_view.excitation.latest = None;
        if !self.lvds_view.history.status.active {
            self.reset_excitation_interaction();
        }
    }

    pub(super) fn sync_excitation_history(&mut self, previous: &HistoryStatus) {
        let current = &self.lvds_view.history.status;
        let source_changed =
            current.active != previous.active || current.generation != previous.generation;
        let navigation_completed = self
            .lvds_view
            .excitation
            .history_navigation_pending
            .is_some()
            && self.lvds_view.excitation.history_navigation_pending
                == current.excitation.selected_index
            && !current.excitation.loading;
        if source_changed || navigation_completed {
            self.lvds_view.excitation.history_navigation_pending = None;
            self.reset_excitation_interaction();
        } else if current.excitation.selected != previous.excitation.selected
            && self.lvds_view.excitation.view == PlotViewMode::FollowLatest
            && self.lvds_view.excitation.frozen.is_none()
        {
            self.lvds_view.excitation.reset_pending = true;
        }
    }

    pub(super) fn prepare_excitation_navigation(&mut self, index: u64) {
        self.lvds_view.excitation.history_navigation_pending = Some(index);
    }

    fn reset_excitation_interaction(&mut self) {
        let view = &mut self.lvds_view.excitation;
        view.frozen = None;
        view.cursor = None;
        self.excitation_follow_latest();
    }

    pub(super) fn excitation_display_record(&self) -> Option<&ExcitationRecord> {
        let view = &self.lvds_view.excitation;
        if let Some(snapshot) = &view.frozen {
            snapshot.record.as_ref()
        } else if let Some(snapshot) = &view.manual {
            snapshot.record.as_ref()
        } else if self.lvds_view.history.status.active {
            self.lvds_view.history.status.excitation.selected.as_ref()
        } else {
            view.latest.as_ref()
        }
    }

    pub(super) fn excitation_plot_range(&self) -> [f64; 2] {
        let view = &self.lvds_view.excitation;
        if view.view == PlotViewMode::ManualView
            && let Some(range) = view.range
        {
            return range;
        }
        self.excitation_display_record()
            .map_or([0.0, 1.0], |record| {
                record
                    .frame
                    .sample_time_us(0)
                    .zip(record.frame.sample_time_us(99))
                    .map_or([0.0, 1.0], |(first, last)| {
                        [micros_as_seconds(first), micros_as_seconds(last)]
                    })
            })
    }

    pub(super) fn excitation_follow_latest(&mut self) {
        let view = &mut self.lvds_view.excitation;
        view.view = PlotViewMode::FollowLatest;
        view.manual = None;
        view.range = None;
        view.reset_pending = true;
    }

    pub(super) fn excitation_enter_manual(&mut self, range: [f64; 2]) {
        if self.lvds_view.excitation.manual.is_none() {
            self.lvds_view.excitation.manual = Some(FrameSnapshot {
                record: self.excitation_display_record().cloned(),
            });
        }
        self.lvds_view.excitation.view = PlotViewMode::ManualView;
        self.lvds_view.excitation.range = Some(range);
    }

    pub(super) fn excitation_toggle_freeze(&mut self) {
        if self.lvds_view.excitation.frozen.is_some() {
            self.lvds_view.excitation.frozen = None;
            self.push_log(LogLevel::Info, "AB 画面更新已恢复，后台接收和保存继续运行");
        } else {
            self.lvds_view.excitation.frozen = Some(FrameSnapshot {
                record: self.excitation_display_record().cloned(),
            });
            self.push_log(LogLevel::Info, "AB 画面已冻结，后台接收和保存继续运行");
        }
    }

    #[cfg(test)]
    pub(super) fn excitation_display_points(&self) -> Vec<[f64; 2]> {
        self.excitation_display_record()
            .map(excitation_points)
            .unwrap_or_default()
            .into_iter()
            .map(|point| [point.x, point.y])
            .collect()
    }

    pub(super) fn waveform_selector(&mut self, ui: &mut Ui) {
        let short = ui.available_width() < 300.0;
        ui.selectable_value(
            &mut self.lvds_view.waveform,
            WaveformView::Scientific,
            if short {
                "科学 AA"
            } else {
                "科学波形 AA"
            },
        );
        ui.selectable_value(
            &mut self.lvds_view.waveform,
            WaveformView::Excitation,
            if short {
                "激励 AB"
            } else {
                "激励波形 AB"
            },
        );
    }

    pub(super) fn excitation_csv_menu(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        ui.menu_button("AB CSV", |ui| {
            ui.set_min_width(330.0);
            ui.set_max_width(520.0);
            self.excitation_csv_controls(ui, request);
        });
    }

    pub(super) fn excitation_toolbar(&mut self, ui: &mut Ui) {
        central_control_style(ui);
        ui.spacing_mut().item_spacing.y = 8.0;
        let width = ui.available_width();
        let history = self.lvds_view.history.status.active;
        let mut request = None;
        central_control_row(ui, |ui| {
            self.waveform_selector(ui);
            self.history_open_button(ui, width < 480.0);
            if width >= 340.0 {
                self.excitation_csv_menu(ui, &mut request);
            }
            if width >= 620.0 {
                let text = self.excitation_display_record().map_or_else(
                    || "等待 AB".to_owned(),
                    |record| {
                        format!(
                            "{} · {:.3} Hz",
                            mode_label(record.frame.work_mode),
                            record.frame.actual_frequency_hz()
                        )
                    },
                );
                ui.add_sized(
                    [ui.available_width().max(0.0), 32.0],
                    egui::Label::new(RichText::new(&text).small()).truncate(),
                )
                .on_hover_text(text);
            }
        });
        central_control_row(ui, |ui| {
            if width < if history { 600.0 } else { 390.0 } {
                ui.menu_button(
                    if width < 300.0 {
                        "图表"
                    } else {
                        "图表操作"
                    },
                    |ui| self.excitation_actions(ui),
                );
            } else {
                self.excitation_actions(ui);
            }
            if history && width >= 340.0 {
                self.excitation_frame_buttons(ui, &mut request);
            }
            ui.menu_button(if width < 300.0 { "详情" } else { "AB 详情" }, |ui| {
                ui.set_min_width(330.0);
                ui.set_max_width(520.0);
                self.excitation_details(ui, &mut request);
            });
            if width < 340.0 {
                self.excitation_csv_menu(ui, &mut request);
            }
            let range = self.excitation_plot_range();
            let text = if self
                .excitation_display_record()
                .is_some_and(|record| !record.frame.has_sample_timing())
            {
                "100 原始槽位 · 无采样时间".to_owned()
            } else {
                format!("单帧 100 点 · {:.6}s", range[1] - range[0])
            };
            ui.add_sized([ui.available_width().max(0.0), 32.0], egui::Label::new(RichText::new(&text).small().color(theme::TEXT_MUTED)).truncate())
                .on_hover_text(format!("{text}\n左键平移 · 滚轮缩放 · 鼠标点值\n手动/冻结保留当前帧，跟随最新显示最近完整帧。AB 相邻窗口可能重叠，不拼接。"));
        });
        if let Some(request) = request {
            self.submit(request);
        }
    }

    fn excitation_actions(&mut self, ui: &mut Ui) {
        if ui
            .selectable_label(
                self.lvds_view.excitation.view == PlotViewMode::FollowLatest,
                "跟随最新",
            )
            .clicked()
        {
            self.excitation_follow_latest();
        }
        if ui.button("重置视图").clicked() {
            self.excitation_follow_latest();
        }
        let frozen = self.lvds_view.excitation.frozen.is_some();
        if ui
            .button(if frozen {
                "恢复更新"
            } else {
                "冻结画面"
            })
            .clicked()
        {
            self.excitation_toggle_freeze();
        }
    }

    fn excitation_frame_buttons(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        let history = &self.lvds_view.history.status.excitation;
        let index = history.selected_index.unwrap_or_default();
        let loading = history.loading;
        let frames = history.frames;
        if ui
            .add_enabled(index > 0 && !loading, egui::Button::new("上一帧"))
            .clicked()
        {
            self.prepare_excitation_navigation(index - 1);
            *request = Some(UiRequest::LoadHistoryExcitationFrame { index: index - 1 });
        }
        if ui
            .add_enabled(index + 1 < frames && !loading, egui::Button::new("下一帧"))
            .clicked()
        {
            self.prepare_excitation_navigation(index + 1);
            *request = Some(UiRequest::LoadHistoryExcitationFrame { index: index + 1 });
        }
    }

    pub(super) fn excitation_details(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        if let Some(record) = self.excitation_display_record() {
            let frame = &record.frame;
            ui.strong(format!(
                "{} · 实际频率 {:.3} Hz",
                mode_label(frame.work_mode),
                frame.actual_frequency_hz()
            ));
            if frame.work_mode == 0xBB {
                ui.label(format!(
                    "DC 设定电压：{:.3} V（{} mV）",
                    f64::from(frame.scan_voltage_mv) / 1000.0,
                    frame.scan_voltage_mv
                ));
            } else {
                ui.label(format!("扫描频率设定值：{} Hz", frame.scan_freq));
            }
            ui.label(format!(
                "AD9833 0x{:02X} · DAC8563 0x{:02X}（原始状态）",
                frame.ad9833_status, frame.dac8563_status
            ));
            ui.label(format!(
                "PA0 seq8 {} · 最近科学 seq8 {}",
                frame.pa0_seq, frame.sci_frame_seq
            ));
            ui.label(format!(
                "{}：{}.{:06} s{}",
                if frame.has_sample_timing() {
                    "设备最新点"
                } else {
                    "设备包时间"
                },
                frame.timestamp.utc_seconds,
                frame.timestamp.microseconds,
                if frame.has_sample_timing() {
                    "（wave[99]）"
                } else {
                    "（逐点时间不可用）"
                }
            ));
            ui.label(format!(
                "间隔 {} µs · {} 点 · {}",
                frame.interval_us,
                frame.points,
                frame.integrity()
            ));
            if frame.is_all_zero() {
                ui.colored_label(theme::WARNING, "全零数据 / 有效性未确认");
            }
            if !frame.has_sample_timing() {
                ui.colored_label(
                    theme::WARNING,
                    "无可用逐点采样时间 · 100 个原始 wave 槽位已保留",
                );
            }
            ui.label(format!(
                "原始记录 {} · UDP u32 {}",
                record.raw_record_id, record.outer_sequence
            ));
            ui.label(format!("来源：{}", record.source));
            ui.label(format!("采集会话：{}", record.session));
            ui.label(format!(
                "AB 接收时间：{}",
                receive_time(record.received_unix_nanos)
            ));
        } else {
            ui.label("尚无已解码 AB 帧");
        }
        let stats = if self.lvds_view.history.status.active {
            self.lvds_view.history.status.receive_stats
        } else {
            self.lvds_view
                .status
                .as_ref()
                .map(|s| s.stats)
                .unwrap_or_default()
        };
        ui.separator();
        ui.label(format!(
            "AB 接收 {} · 解码 {} · 重复 {} · 冲突 {} · 错误 {}",
            stats.excitation_frames,
            stats.excitation_decoded,
            stats.excitation_duplicates,
            stats.excitation_conflicts,
            stats.excitation_decode_errors
        ));
        ui.label(format!(
            "AB 业务队列丢失 {} · 视觉队列丢失 {}",
            stats.excitation_business_dropped, stats.type_events_dropped
        ));
        if self.lvds_view.history.status.active {
            ui.separator();
            self.excitation_history_controls(ui, request);
        }
    }

    pub(super) fn excitation_history_controls(
        &mut self,
        ui: &mut Ui,
        request: &mut Option<UiRequest>,
    ) {
        let history = &self.lvds_view.history.status.excitation;
        ui.strong("激励波形 AB · 按帧查看");
        ui.label(format!(
            "共 {} 帧 · 当前 {}{}",
            history.frames,
            history
                .selected_index
                .map_or_else(|| "—".to_owned(), |i| (i + 1).to_string()),
            if history.loading { " · 读取中" } else { "" }
        ));
        let frames = history.frames;
        let mut position = history.selected_index.map_or(1, |i| i + 1);
        ui.horizontal(|ui| {
            self.excitation_frame_buttons(ui, request);
            if frames > 0 {
                ui.label("帧");
                if ui
                    .add(egui::DragValue::new(&mut position).range(1..=frames))
                    .changed()
                {
                    self.prepare_excitation_navigation(position - 1);
                    *request = Some(UiRequest::LoadHistoryExcitationFrame {
                        index: position - 1,
                    });
                }
            }
        });
        egui::ScrollArea::vertical()
            .max_height(130.0)
            .id_salt("ab_history_errors")
            .show(ui, |ui| {
                for error in &self.lvds_view.history.status.excitation.errors {
                    ui.colored_label(
                        theme::WARNING,
                        format!(
                            "AB 原始记录 {} · UDP {} · {}",
                            error.raw_record_id, error.outer_sequence, error.reason
                        ),
                    );
                }
            });
    }

    pub(super) fn excitation_csv_controls(&mut self, ui: &mut Ui, request: &mut Option<UiRequest>) {
        ui.strong("独立 AB 科学 CSV");
        let storage = self
            .lvds_view
            .status
            .as_ref()
            .map(|s| s.excitation_storage.clone())
            .unwrap_or_default();
        ui.add_enabled_ui(!storage.active, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.lvds_view.excitation.csv_path)
                    .hint_text("CSV 完整路径（留空自动命名）")
                    .desired_width(400.0),
            );
        });
        if ui
            .button(if storage.active {
                "停止 AB CSV"
            } else {
                "开始 AB CSV"
            })
            .clicked()
        {
            *request = Some(if storage.active {
                UiRequest::StopExcitationCsv
            } else {
                let path = output_path(&self.lvds_view.excitation.csv_path, "excitation");
                UiRequest::StartExcitationCsv { path }
            });
        }
        excitation_storage_status(ui, &storage);
        if self.lvds_view.history.status.active {
            ui.separator();
            ui.strong("导出整份历史 AB CSV");
            ui.add(
                egui::TextEdit::singleline(&mut self.lvds_view.excitation.export_path)
                    .hint_text("导出路径（留空自动命名）")
                    .desired_width(400.0),
            );
            let history = &self.lvds_view.history.status.excitation;
            if ui
                .add_enabled(!history.exporting, egui::Button::new("导出历史 AB CSV"))
                .clicked()
            {
                *request = Some(UiRequest::ExportHistoryExcitationCsv {
                    path: output_path(&self.lvds_view.excitation.export_path, "history-excitation"),
                });
            }
            if history.exporting {
                ui.label("AB CSV 正在后台导出…");
            }
            if let Some(path) = &history.export_path {
                ui.label(format!("输出路径：{path}"));
            }
            if let Some(error) = &history.export_error {
                ui.colored_label(theme::DANGER, error);
            }
        }
    }

    pub(super) fn excitation_plot_panel(&mut self, ui: &mut Ui) {
        let start = Instant::now();
        debug_ui_rect("central.plot_body_inner", ui.available_rect_before_wrap());
        let Some(record) = self.excitation_display_record() else {
            ui.centered_and_justified(|ui| {
                ui.label("暂无 AB 激励数据 · 接收或打开历史文件后显示最新完整帧");
            });
            return;
        };
        let points = excitation_points(record);
        let timed = record.frame.has_sample_timing();
        excitation_readout(
            ui,
            excitation_labels(record, self.lvds_view.excitation.cursor, &points),
        );
        if !timed {
            self.lvds_view.excitation.cursor = None;
            self.lvds_view.excitation.reset_pending = true;
            self.measurements.rendered = [0, 0];
            self.measurements.prepare_ms = start.elapsed().as_secs_f64() * 1000.0;
            excitation_unavailable(ui);
            return;
        }
        let outcome = show_scientific_plot(
            ui,
            ScientificPlotConfig {
                id: "excitation_plot_ab",
                height: ui.available_height().max(0.0),
                range: self.excitation_plot_range(),
                reset: self.lvds_view.excitation.reset_pending,
                normalized: false,
                y_label: "激励电压 (V)",
                channels: [true, false],
                names: ["PA0/TW_BACK 激励 AB", ""],
            },
            &points,
            &[],
            self.lvds_view.excitation.cursor,
        );
        self.lvds_view.excitation.reset_pending = false;
        self.lvds_view.excitation.cursor = outcome.cursor.or(self.lvds_view.excitation.cursor);
        if outcome.interacted {
            self.excitation_enter_manual(outcome.range);
        }
        if outcome.reset {
            self.excitation_follow_latest();
        }
        self.measurements.prepare_ms = start.elapsed().as_secs_f64() * 1000.0;
        self.measurements.rendered = [points.len(), 0];
    }
}

fn excitation_labels(
    record: &ExcitationRecord,
    cursor: Option<f64>,
    points: &[PlotPoint],
) -> [String; 2] {
    let frame = &record.frame;
    let setting = if frame.work_mode == 0xBB {
        format!("设定 {:.3} V", f64::from(frame.scan_voltage_mv) / 1000.0)
    } else {
        format!("设定 {} Hz", frame.scan_freq)
    };
    let summary = format!(
        "{} · {} · 实际 {:.3} Hz · AD9833 0x{:02X} · DAC8563 0x{:02X} · PA0 {} · AA {}",
        mode_label(frame.work_mode),
        setting,
        frame.actual_frequency_hz(),
        frame.ad9833_status,
        frame.dac8563_status,
        frame.pa0_seq,
        frame.sci_frame_seq
    );
    let detail = if !frame.has_sample_timing() {
        format!(
            "100 个原始槽位 · 无可用逐点采样时间 · 设备包时间 {}.{:06} s",
            frame.timestamp.utc_seconds, frame.timestamp.microseconds
        )
    } else if let Some(cursor) = cursor {
        points
            .iter()
            .min_by(|a, b| (a.x - cursor).abs().total_cmp(&(b.x - cursor).abs()))
            .map_or_else(String::new, |point| {
                format!(
                    "设备时间 {} s · 电压 {:.3} V",
                    format_plot_timestamp(point.x, 0.0),
                    point.y
                )
            })
    } else {
        format!(
            "100 点 · 间隔 {} µs · 设备最新点 {}.{:06} s",
            frame.interval_us, frame.timestamp.utc_seconds, frame.timestamp.microseconds
        )
    };
    let detail = if frame.has_sample_timing() && frame.is_all_zero() {
        format!("全零数据 / 有效性未确认 · {detail}")
    } else {
        detail
    };
    [summary, detail]
}

fn excitation_readout(ui: &mut Ui, labels: [String; 2]) {
    let rows = if ui.available_height() >= 110.0 { 2 } else { 1 };
    for text in labels.into_iter().take(rows) {
        let height = ui.available_height().clamp(0.0, 20.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width().max(0.0), height),
            Layout::top_down(Align::Min),
            |ui| {
                ui.add_sized(
                    [ui.available_width(), height],
                    egui::Label::new(RichText::new(&text).small().color(theme::TEXT_SECONDARY))
                        .truncate(),
                )
                .on_hover_text(text);
            },
        );
    }
}

fn excitation_unavailable(ui: &mut Ui) {
    if ui.available_height() < 50.0 {
        ui.add_sized(
            ui.available_size(),
            egui::Label::new("当前无有效激励波形 · 无可用逐点采样时间").truncate(),
        );
    } else {
        ui.centered_and_justified(|ui| {
            ui.label(
                "当前无有效激励波形\n无可用逐点采样时间\n已保留原始 100 个 wave 槽位及配置状态",
            );
        });
    }
}

fn mode_label(mode: u8) -> &'static str {
    match mode {
        0x55 => "三角波",
        0xAA => "正弦波",
        0xBB => "DC",
        _ => "未知模式",
    }
}

fn excitation_points(record: &ExcitationRecord) -> Vec<PlotPoint> {
    record
        .frame
        .wave_mv
        .iter()
        .enumerate()
        .filter_map(|(index, &millivolts)| {
            record.frame.sample_time_us(index).map(|time| PlotPoint {
                x: micros_as_seconds(time),
                y: f64::from(millivolts) / 1000.0,
            })
        })
        .collect()
}

#[expect(
    clippy::cast_precision_loss,
    reason = "protocol u32 seconds expressed as microseconds remain below 2^53"
)]
fn micros_as_seconds(time: u64) -> f64 {
    time as f64 / 1_000_000.0
}

fn receive_time(nanos: u64) -> String {
    let seconds = i64::try_from(nanos / 1_000_000_000).unwrap_or(i64::MAX);
    let subsec = u32::try_from(nanos % 1_000_000_000).expect("nanosecond remainder fits u32");
    chrono::DateTime::from_timestamp(seconds, subsec).map_or_else(
        || format!("Unix {nanos} ns"),
        |time| {
            time.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S%.6f %:z")
                .to_string()
        },
    )
}

fn output_path(path: &str, prefix: &str) -> String {
    if path.trim().is_empty() {
        format!(
            "data/{prefix}-{}.csv",
            Local::now().format("%Y%m%d-%H%M%S-%3f")
        )
    } else {
        path.trim().to_owned()
    }
}

fn excitation_storage_status(ui: &mut Ui, storage: &ExcitationStorageStatus) {
    ui.label(format!(
        "入队 {} 帧 · 实际写入 {} 帧 / {} 行 · 丢失 {}",
        storage.enqueued, storage.written, storage.rows_written, storage.dropped
    ));
    ui.label(if storage.error.is_some() {
        "AB CSV 写入错误"
    } else if storage.active {
        "AB CSV 写入中"
    } else if storage.closed {
        "AB CSV 已关闭"
    } else {
        "AB CSV 未开始"
    });
    if let Some(path) = &storage.path {
        ui.label(path);
    }
    if let Some(error) = &storage.error {
        ui.colored_label(theme::DANGER, error);
    }
}
