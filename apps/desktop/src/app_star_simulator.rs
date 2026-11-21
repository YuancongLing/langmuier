use std::time::SystemTime;

use chrono::{DateTime, Utc};
use eframe::egui::{self, RichText, Ui};
use langmuir_core::{
    BroadcastKind, GNSS_PRESETS, GnssFix, GnssMode, StarTime, encode_broadcast, wgs84_ecef,
};

use super::{LangmuirDesktopApp, full_width_subtle_card, theme};
use crate::{
    bridge::UiRequest,
    star_simulator::{
        BROADCAST_INTERVAL_MAX_MILLIS, BROADCAST_INTERVAL_MIN_MILLIS, StarSimulatorConfig,
        StarSimulatorStatus,
    },
};

#[derive(Debug, Default)]
pub(super) struct StarSimulatorView {
    pub(super) selected: bool,
    pub(super) config: StarSimulatorConfig,
    pub(super) status: StarSimulatorStatus,
}

impl LangmuirDesktopApp {
    pub(super) fn rs422_control_panel(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.star_view.selected, false, "遥测 / 控制");
            ui.selectable_value(&mut self.star_view.selected, true, "星务模拟");
        });
        ui.add_space(6.0);
        if self.star_view.selected {
            self.star_simulator_panel(ui);
        } else {
            self.command_panel(ui);
        }
    }

    pub(super) fn apply_star_simulator_status(&mut self, mut status: StarSimulatorStatus) {
        // Repeated reliable snapshots must not erase edits made before Start.
        if status.config != self.star_view.status.config {
            self.star_view.config.clone_from(&status.config);
        }
        if !self.rs422.connected {
            status.active = false;
            status.queue_depth = 0;
        }
        #[cfg(feature = "mock")]
        if self.mock_mode {
            status.active = false;
        }
        self.star_view.status = status;
    }

    fn star_can_send(&self) -> bool {
        #[cfg(feature = "mock")]
        if self.mock_mode {
            return false;
        }
        self.rs422.connected
    }

    pub(super) fn star_simulator_panel(&mut self, ui: &mut Ui) {
        theme::section_title(ui, "RS422 星务计算机模拟");
        ui.label(RichText::new("SYNTHETIC 固定点位编码测试").color(theme::WARNING));
        ui.label("时间取系统 UTC；姿态、轨道与 GNSS 为合成测试值。");
        #[cfg(feature = "mock")]
        if self.mock_mode {
            ui.colored_label(
                theme::WARNING,
                "MOCK 科学波形模式没有真实串口；星务发送已禁用。",
            );
        }
        ui.label(if self.rs422.connected {
            "RS422 已连接"
        } else {
            "RS422 未连接"
        });
        ui.add(egui::Label::new(&self.rs422.detail).wrap());
        let before = self.star_view.config.clone();
        for kind in BroadcastKind::ALL {
            self.star_broadcast_controls(ui, kind);
        }
        self.star_simulator_actions(ui);
        self.star_point_preview(ui);
        if self.star_view.status.active && before != self.star_view.config {
            let request = if self.star_view.config.enabled.iter().any(|enabled| *enabled) {
                UiRequest::ConfigureStarSimulator(self.star_view.config.clone())
            } else {
                UiRequest::StopStarSimulator
            };
            self.submit(request);
        }
        self.star_send_status(ui);
        self.star_frame_previews(ui);
    }

    fn star_broadcast_controls(&mut self, ui: &mut Ui, kind: BroadcastKind) {
        let index = kind.index();
        full_width_subtle_card(ui, |ui| {
            ui.push_id(kind.label(), |ui| {
                let label = match kind {
                    BroadcastKind::Time => "整星时间广播",
                    BroadcastKind::Attitude => "姿态轨道广播",
                    BroadcastKind::Gnss => "GNSS 广播",
                };
                theme::aligned_checkbox(ui, &mut self.star_view.config.enabled[index], label);
                ui.horizontal_wrapped(|ui| {
                    ui.label("周期 ms");
                    ui.add(
                        egui::DragValue::new(&mut self.star_view.config.interval_millis[index])
                            .range(BROADCAST_INTERVAL_MIN_MILLIS..=BROADCAST_INTERVAL_MAX_MILLIS)
                            .speed(100),
                    );
                    let label = match kind {
                        BroadcastKind::Time => "时间单发",
                        BroadcastKind::Attitude => "姿态单发",
                        BroadcastKind::Gnss => "GNSS 单发",
                    };
                    if ui
                        .add_enabled(self.star_can_send(), egui::Button::new(label))
                        .clicked()
                    {
                        self.submit(UiRequest::SendStarBroadcast {
                            kind,
                            point: self.star_view.config.point,
                        });
                    }
                });
            });
        });
        ui.add_space(4.0);
    }

    fn star_simulator_actions(&mut self, ui: &mut Ui) {
        ui.label("周期范围 100–60000 ms；默认 1000 ms 为模拟策略。");
        let valid = self.star_view.config.validate();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.star_can_send() && !self.star_view.status.active && valid.is_ok(),
                    egui::Button::new("启动自动广播").fill(theme::PRIMARY),
                )
                .clicked()
            {
                self.submit(UiRequest::StartStarSimulator(self.star_view.config.clone()));
            }
            if ui
                .add_enabled(self.star_can_send(), egui::Button::new("停止全部广播"))
                .clicked()
            {
                self.submit(UiRequest::StopStarSimulator);
            }
        });
        if let Err(error) = valid {
            ui.colored_label(theme::TEXT_SECONDARY, error);
        }
        ui.label("默认关闭，点击启动后发送。断线与重连后须重新启动。");
        ui.label("遥测轮询使用现有发送通道，广播不能替代轮询。");
    }

    fn star_point_preview(&mut self, ui: &mut Ui) {
        full_width_subtle_card(ui, |ui| {
            theme::section_title(ui, "GNSS 合成点位");
            egui::ComboBox::from_id_salt("star_gnss_preset")
                .width((ui.available_width() - 12.0).max(40.0))
                .selected_text(self.star_view.config.point.label)
                .show_ui(ui, |ui| {
                    for point in GNSS_PRESETS {
                        ui.selectable_value(&mut self.star_view.config.point, point, point.label);
                    }
                });
            let point = self.star_view.config.point;
            ui.label(format!(
                "纬度 {:.6}°　经度 {:.6}°",
                point.latitude_deg, point.longitude_deg
            ));
            ui.label(format!("椭球高 {:.2} m", point.height_m));
            ui.label(format!(
                "ECEF 速度 {:.2} / {:.2} / {:.2} m/s",
                point.velocity_ecef_mps[0], point.velocity_ecef_mps[1], point.velocity_ecef_mps[2]
            ));
            let fix = match point.fix {
                GnssFix::Invalid => "无效",
                GnssFix::Position => "定位有效",
                GnssFix::Orbit => "定轨有效",
                GnssFix::Extrapolated => "定轨外推有效",
            };
            let mode = match point.mode {
                GnssMode::Gps => "GPS",
                GnssMode::Bds => "BDS",
                GnssMode::GpsBds => "GPS+BDS",
            };
            ui.label(format!("{fix} · {mode}"));
            ui.label(format!(
                "解算星数 GPS {} / BDS {}",
                point.gps_satellites, point.bds_satellites
            ));
            if let Ok(ecef) = wgs84_ecef(point) {
                for (axis, value) in ["X", "Y", "Z"].into_iter().zip(ecef) {
                    ui.label(format!("ECEF {axis} = {value:.2} m"));
                }
            }
            let now = SystemTime::now();
            ui.label(format!("模拟 UTC {}", utc_timestamp(now)));
            if let Ok(time) = StarTime::from_system_time(now) {
                ui.label(format!(
                    "2018 UTC + {} s，{} μs",
                    time.seconds, time.microseconds
                ));
            }
            ui.label("静态编码场景；切换仅影响后续帧，位置跳变不代表连续轨道。");
        });
    }

    fn star_send_status(&self, ui: &mut Ui) {
        full_width_subtle_card(ui, |ui| {
            theme::section_title(ui, "实际发送状态");
            ui.colored_label(
                if self.star_view.status.active {
                    theme::SUCCESS
                } else {
                    theme::TEXT_SECONDARY
                },
                if self.star_view.status.active {
                    "自动广播运行中"
                } else {
                    "自动广播已停止"
                },
            );
            for kind in BroadcastKind::ALL {
                let index = kind.index();
                ui.label(format!(
                    "{}　已发送 {}",
                    kind.label(),
                    self.star_view.status.sent[index]
                ));
                ui.label(format!(
                    "跳过 {}　失败 {}",
                    self.star_view.status.skipped[index], self.star_view.status.failed[index]
                ));
            }
            ui.label(format!(
                "待发广播队列 {} / 3",
                self.star_view.status.queue_depth
            ));
            if let Some(time) = self.star_view.status.last_sent_utc {
                ui.label(format!("最近实际写入 UTC {}", utc_timestamp(time)));
            } else {
                ui.label("最近实际写入：尚无");
            }
            if let Some(error) = &self.star_view.status.last_error {
                ui.add(egui::Label::new(RichText::new(error).color(theme::DANGER)).wrap());
            }
        });
    }

    fn star_frame_previews(&self, ui: &mut Ui) {
        theme::section_title(ui, "发送帧 HEX 预览");
        let Ok(time) = StarTime::from_system_time(SystemTime::now()) else {
            return;
        };
        for kind in BroadcastKind::ALL {
            egui::CollapsingHeader::new(format!("{} HEX", kind.label()))
                .id_salt(("star_hex", kind.index()))
                .show(ui, |ui| {
                    if let Ok(frame) = encode_broadcast(kind, time, self.star_view.config.point, 0)
                    {
                        ui.small(format!("当前合成预览 · {} B · 未发送", frame.len()));
                        hex_lines(ui, &frame);
                    }
                    let sent = &self.star_view.status.last_frames[kind.index()];
                    if !sent.is_empty() {
                        ui.small("最近完整实际写入帧");
                        hex_lines(ui, sent);
                    }
                });
        }
    }
}

fn utc_timestamp(time: SystemTime) -> String {
    DateTime::<Utc>::from(time)
        .format("%Y-%m-%d %H:%M:%S%.3f")
        .to_string()
}

fn hex_lines(ui: &mut Ui, bytes: &[u8]) {
    // Eight bytes per line also fit the existing 260px sidebar minimum.
    for chunk in bytes.chunks(8) {
        let text = chunk
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        ui.add(egui::Label::new(RichText::new(text).monospace()).wrap());
    }
}
