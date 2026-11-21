use super::*;
use crate::bridge::{ExcitationHistoryStatus, HistoryStatus};
use langmuir_core::{ExcitationFrame, ExcitationRecord, Timestamp};

fn app() -> LangmuirDesktopApp {
    let (bridge, _endpoint) = crate::bridge::channel_bridge(64);
    LangmuirDesktopApp::with_bridge(
        Box::new(bridge),
        None,
        #[cfg(feature = "mock")]
        false,
    )
}

pub(super) fn record(id: u64, seconds: u32) -> ExcitationRecord {
    let mut wave_mv = Box::new([1_800; 100]);
    wave_mv[0] = 0;
    wave_mv[1] = 3_300;
    ExcitationRecord {
        frame: ExcitationFrame {
            timestamp: Timestamp {
                utc_seconds: seconds,
                microseconds: 100_000,
            },
            f_act_mhz: 931,
            interval_us: 10_800,
            points: 100,
            work_mode: 0x55,
            scan_freq: 1,
            scan_voltage_mv: 0,
            ad9833_status: 0,
            dac8563_status: 1,
            sci_frame_seq: 23,
            pa0_seq: 255,
            wave_mv,
            raw_crc: [0x12, 0x34],
        },
        outer_sequence: 300,
        raw_record_id: id,
        source: "SYNTHETIC AB".to_owned(),
        session: "fixture-session".to_owned(),
        received_unix_nanos: 1_700_000_000_123_456_789,
    }
}

#[test]
fn excitation_uses_mv_and_latest_sample_time_without_joining_overlapping_frames() {
    // Catch AA ADC conversion, timestamp-at-wave[0], and append-on-ingress bugs.
    let mut app = app();
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(record(1, 2))));
    let points = app.excitation_display_points();
    assert_eq!(points.len(), 100);
    assert!((points[0][0] - 1.030_8).abs() < 1e-12);
    assert!((points[99][0] - 2.1).abs() < 1e-12);
    assert_eq!([points[0][1], points[1][1], points[99][1]], [0.0, 3.3, 1.8]);
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(record(2, 3))));
    let points = app.excitation_display_points();
    assert_eq!(points.len(), 100);
    assert!((points[0][0] - 2.030_8).abs() < 1e-12);
    assert_eq!(app.excitation_display_record().unwrap().raw_record_id, 2);
}

#[test]
fn excitation_manual_view_keeps_its_frame_and_does_not_change_aa_interaction() {
    let mut app = app();
    app.operator.plot_view = PlotViewMode::ManualView;
    app.operator.plot_x_range = Some([50.0, 51.0]);
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(record(1, 2))));
    app.excitation_enter_manual([1.3, 1.8]);
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(record(2, 3))));
    assert_eq!(app.excitation_display_record().unwrap().raw_record_id, 1);
    assert_eq!(app.excitation_plot_range(), [1.3, 1.8]);
    app.lvds_view.waveform = excitation_ui::WaveformView::Excitation;
    app.lvds_view.waveform = excitation_ui::WaveformView::Scientific;
    assert_eq!(app.operator.plot_view, PlotViewMode::ManualView);
    assert_eq!(app.plot_range(), [50.0, 51.0]);
    app.excitation_follow_latest();
    assert_eq!(app.excitation_display_record().unwrap().raw_record_id, 2);
    assert!((app.excitation_plot_range()[0] - 2.030_8).abs() < 1e-12);
    assert_eq!(app.plot_range(), [50.0, 51.0]);
}

#[test]
fn excitation_freeze_retains_snapshot_while_latest_status_recovers_visual_updates() {
    let mut app = app();
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(record(1, 2))));
    app.excitation_toggle_freeze();
    app.handle_backend_event(BackendEvent::LvdsStatus(Box::new(LvdsStatusSnapshot {
        source: "SYNTHETIC AB".to_owned(),
        excitation_latest: Some(record(8, 9)),
        ..LvdsStatusSnapshot::default()
    })));
    assert_eq!(app.excitation_display_record().unwrap().raw_record_id, 1);
    app.excitation_toggle_freeze();
    assert_eq!(app.excitation_display_record().unwrap().raw_record_id, 8);
    assert!(app.operator.frozen_plot.is_none());
}

#[test]
fn excitation_history_selection_resets_only_ab_and_rejects_stale_generations() {
    let mut app = app();
    app.operator.plot_x_range = Some([50.0, 51.0]);
    app.handle_backend_event(BackendEvent::HistoryStatus(Box::new(HistoryStatus {
        generation: 2,
        active: true,
        excitation: ExcitationHistoryStatus {
            frames: 10_000_000,
            selected_index: Some(4),
            selected: Some(record(5, 6)),
            ..ExcitationHistoryStatus::default()
        },
        ..HistoryStatus::default()
    })));
    app.excitation_enter_manual([5.5, 5.8]);
    app.excitation_toggle_freeze();
    let aa_range = app.operator.plot_x_range;
    app.prepare_excitation_navigation(5);
    let mut status = app.lvds_view.history.status.clone();
    status.excitation.selected_index = Some(5);
    status.excitation.selected = Some(record(6, 7));
    app.handle_backend_event(BackendEvent::HistoryStatus(Box::new(status)));
    assert_eq!(app.excitation_display_record().unwrap().raw_record_id, 6);
    assert!(app.lvds_view.excitation.frozen.is_none());
    assert_eq!(app.operator.plot_x_range, aa_range);
    app.handle_backend_event(BackendEvent::HistoryStatus(Box::new(HistoryStatus {
        generation: 1,
        active: true,
        excitation: ExcitationHistoryStatus {
            selected_index: Some(0),
            selected: Some(record(1, 2)),
            ..ExcitationHistoryStatus::default()
        },
        ..HistoryStatus::default()
    })));
    assert_eq!(app.excitation_display_record().unwrap().raw_record_id, 6);
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(record(99, 100))));
    assert_eq!(app.excitation_display_points().len(), 100);
    assert_eq!(app.excitation_display_record().unwrap().raw_record_id, 6);
}

#[test]
fn excitation_plot_paints_real_mode_frequency_raw_status_and_zero_semantics() {
    let mut app = app();
    let mut zero = record(1, 2);
    zero.frame.f_act_mhz = 0;
    zero.frame.wave_mv.fill(0);
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(zero)));
    app.lvds_view.waveform = excitation_ui::WaveformView::Excitation;
    let ctx = egui::Context::default();
    let _ = fonts::install_cjk_fallback(&ctx);
    let input = || egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(700.0, 500.0),
        )),
        ..egui::RawInput::default()
    };
    let draw = |app: &mut LangmuirDesktopApp| ctx.run_ui(input(), |ui| app.plot_panel(ui));
    let _ = draw(&mut app);
    let output = draw(&mut app);
    let labels: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some(t.galley.text()),
            _ => None,
        })
        .collect();
    let all = labels.join("\n");
    assert!(all.contains("三角波"));
    assert!(all.contains("0.000 Hz"));
    assert!(all.contains("AD9833 0x00"));
    assert!(all.contains("DAC8563 0x01"));
    assert!(all.contains("全零数据 / 有效性未确认"));
    assert!(!all.contains("故障"));
}

#[test]
fn excitation_unsolicited_history_updates_preserve_manual_and_frozen_frame() {
    let mut app = app();
    let mut status = HistoryStatus {
        generation: 1,
        active: true,
        excitation: ExcitationHistoryStatus {
            frames: 10,
            selected_index: Some(0),
            selected: Some(record(1, 2)),
            ..ExcitationHistoryStatus::default()
        },
        ..HistoryStatus::default()
    };
    app.handle_backend_event(BackendEvent::HistoryStatus(Box::new(status.clone())));
    app.excitation_enter_manual([1.4, 1.8]);
    app.excitation_toggle_freeze();
    status.excitation.selected_index = Some(1);
    status.excitation.selected = Some(record(2, 3));
    app.handle_backend_event(BackendEvent::HistoryStatus(Box::new(status)));
    assert_eq!(app.excitation_display_record().unwrap().raw_record_id, 1);
    assert_eq!(app.excitation_plot_range(), [1.4, 1.8]);
}

fn render_plot(
    app: &mut LangmuirDesktopApp,
    context: &egui::Context,
    events: Vec<egui::Event>,
) -> egui_plot::PlotBounds {
    let mut bounds = None;
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 500.0),
            )),
            events,
            ..egui::RawInput::default()
        },
        |ui| {
            app.plot_panel(ui);
            bounds = egui_plot::PlotMemory::load(
                context,
                ui.make_persistent_id(egui::Id::new("excitation_plot_ab")),
            )
            .map(|m| *m.bounds());
        },
    );
    bounds.expect("AB plot rendered")
}

#[test]
fn excitation_left_drag_preserves_frame_and_both_manual_axes_until_reset() {
    let mut app = app();
    app.lvds_view.waveform = excitation_ui::WaveformView::Excitation;
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(record(1, 2))));
    let ctx = egui::Context::default();
    render_plot(&mut app, &ctx, vec![]);
    let initial = render_plot(&mut app, &ctx, vec![]);
    let from = egui::pos2(450.0, 230.0);
    let to = egui::pos2(570.0, 260.0);
    render_plot(
        &mut app,
        &ctx,
        vec![
            egui::Event::PointerMoved(from),
            egui::Event::PointerButton {
                pos: from,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    render_plot(&mut app, &ctx, vec![egui::Event::PointerMoved(to)]);
    let manual = render_plot(
        &mut app,
        &ctx,
        vec![egui::Event::PointerButton {
            pos: to,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert!(manual.min()[0] < initial.min()[0]);
    assert_ne!(manual.range_y(), initial.range_y());
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(record(2, 3))));
    let after = render_plot(&mut app, &ctx, vec![]);
    assert_eq!(after, manual);
    assert_eq!(app.excitation_display_record().unwrap().raw_record_id, 1);
    app.excitation_follow_latest();
    let reset = render_plot(&mut app, &ctx, vec![]);
    assert!(reset.max()[0] > initial.max()[0]);
    assert_eq!(app.excitation_display_record().unwrap().raw_record_id, 2);
}

#[test]
fn excitation_plain_wheel_zooms_and_cursor_reports_mv_as_volts() {
    let mut app = app();
    app.lvds_view.waveform = excitation_ui::WaveformView::Excitation;
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(record(1, 2))));
    let ctx = egui::Context::default();
    render_plot(&mut app, &ctx, vec![]);
    let initial = render_plot(&mut app, &ctx, vec![]);
    render_plot(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(egui::pos2(450.0, 230.0))],
    );
    render_plot(
        &mut app,
        &ctx,
        vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 100.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        }],
    );
    assert!(app.excitation_plot_range()[1] - app.excitation_plot_range()[0] < initial.width());
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(record(2, 3))));
    assert_eq!(app.excitation_display_record().unwrap().raw_record_id, 1);
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 500.0),
            )),
            ..egui::RawInput::default()
        },
        |ui| app.plot_panel(ui),
    );
    assert!(output.shapes.iter().any(
        |s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text().contains("电压 1.800 V"))
    ));
}

#[test]
fn excitation_zero_interval_retains_raw_slots_without_fabricated_plot_samples() {
    let mut app = app();
    let bytes = include_bytes!(
        "../../../crates/langmuir-core/tests/fixtures/excitation/REAL-202611212141-ab-no-sample-time.bin"
    );
    let frame =
        langmuir_core::decode_excitation_frame(&langmuir_core::parse_common_frame(bytes).unwrap())
            .unwrap();
    let mut sample = record(1, 2);
    sample.frame = frame;
    sample.source = "REAL captured AB no-sample-time fixture".to_owned();
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(sample)));
    assert_eq!(app.excitation_display_points(), [] as [[f64; 2]; 0]);
    let actual = app.excitation_display_record().unwrap();
    assert_eq!(actual.frame.wave_mv.len(), 100);
    assert!(actual.frame.wave_mv.iter().all(|value| *value == 0));
    assert_eq!(actual.frame.pa0_seq, 183);
}

#[test]
fn excitation_details_show_all_modes_millihertz_and_actual_provenance() {
    for (mode, label, setting) in [
        (0x55, "三角波", "扫描频率设定值：100 Hz"),
        (0xAA, "正弦波", "扫描频率设定值：100 Hz"),
        (0xBB, "DC", "DC 设定电压：5.000 V（5000 mV）"),
    ] {
        let mut app = app();
        let mut sample = record(42, 2);
        sample.frame.work_mode = mode;
        sample.frame.scan_freq = 100;
        sample.frame.scan_voltage_mv = if mode == 0xBB { 5000 } else { 0 };
        app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(sample)));
        let ctx = egui::Context::default();
        let _ = fonts::install_cjk_fallback(&ctx);
        let draw = |app: &mut LangmuirDesktopApp| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 700.0),
                    )),
                    ..egui::RawInput::default()
                },
                |ui| app.excitation_details(ui, &mut None),
            )
        };
        let _ = draw(&mut app);
        let output = draw(&mut app);
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.galley.text()),
                _ => None,
            })
            .collect();
        let text = labels.join("\n");
        assert!(text.contains(&format!("{label} · 实际频率 0.931 Hz")));
        assert!(text.contains(setting));
        assert!(text.contains("原始记录 42 · UDP u32 300"));
        assert!(text.contains("来源：SYNTHETIC AB"));
        assert!(text.contains("采集会话：fixture-session"));
        assert!(text.contains("CRC_NOT_CHECKED"));
        if mode == 0xBB {
            assert!(!text.contains("扫描频率设定值"));
        }
    }
}

#[test]
fn excitation_late_old_display_event_cannot_replace_authoritative_new_session() {
    let mut app = app();
    let old = record(5, 6);
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(old.clone())));
    let mut current = record(1, 20);
    current.source = "new-source".to_owned();
    current.session = "new-session".to_owned();
    app.handle_backend_event(BackendEvent::LvdsStatus(Box::new(LvdsStatusSnapshot {
        source: "new-source".to_owned(),
        excitation_latest: Some(current.clone()),
        ..LvdsStatusSnapshot::default()
    })));
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(old)));
    assert_eq!(app.excitation_display_record(), Some(&current));
}

#[test]
fn excitation_late_display_event_cannot_resurrect_a_disconnected_source() {
    let mut app = app();
    let old = record(5, 6);
    app.handle_backend_event(BackendEvent::LvdsStatus(Box::new(LvdsStatusSnapshot {
        source: "old-source".to_owned(),
        excitation_latest: Some(old.clone()),
        ..LvdsStatusSnapshot::default()
    })));
    app.handle_backend_event(BackendEvent::LvdsStatus(Box::new(LvdsStatusSnapshot {
        source: "disconnected".to_owned(),
        excitation_latest: None,
        ..LvdsStatusSnapshot::default()
    })));
    assert!(app.excitation_display_record().is_none());
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(old)));
    assert!(app.excitation_display_record().is_none());
}

#[test]
fn s02_real_controls_expose_both_waveforms_history_csv_and_version_without_opening_menus() {
    let mut app = app();
    let ctx = egui::Context::default();
    let _ = fonts::install_cjk_fallback(&ctx);
    let draw = |app: &mut LangmuirDesktopApp, events| {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 200.0),
                )),
                events,
                ..egui::RawInput::default()
            },
            |ui| {
                LangmuirDesktopApp::header_title(ui);
                app.plot_toolbar(ui);
            },
        )
    };
    let _ = draw(&mut app, vec![]);
    let output = draw(&mut app, vec![]);
    let visible: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((
                text.galley.text().to_owned(),
                text.visual_bounding_rect().center(),
            )),
            _ => None,
        })
        .collect();
    for label in [
        "科学波形 AA",
        "激励波形 AB",
        "AB CSV",
        "S02-REAL-02 (20261121)",
    ] {
        assert!(
            visible.iter().any(|(text, _)| text == label),
            "missing visible {label}: {visible:?}"
        );
    }
    assert!(
        visible
            .iter()
            .any(|(text, _)| text == "历史" || text == "打开历史")
    );
    let ab = visible
        .iter()
        .find(|(text, _)| text == "激励波形 AB")
        .unwrap()
        .1;
    for pressed in [true, false] {
        let _ = draw(
            &mut app,
            vec![
                egui::Event::PointerMoved(ab),
                egui::Event::PointerButton {
                    pos: ab,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert_eq!(
        app.lvds_view.waveform,
        excitation_ui::WaveformView::Excitation
    );
    let output = draw(&mut app, vec![]);
    let history = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if ["历史", "打开历史"].contains(&text.galley.text()) => {
                Some(text.visual_bounding_rect().center())
            }
            _ => None,
        })
        .expect("history action visible on AB too");
    for pressed in [true, false] {
        let _ = draw(
            &mut app,
            vec![
                egui::Event::PointerMoved(history),
                egui::Event::PointerButton {
                    pos: history,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert!(app.lvds_view.history.dialog_open);
}
