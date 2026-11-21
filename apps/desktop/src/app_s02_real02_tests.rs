//! Actual multi-mode user capture; automated egui verification, not native GUI.
use super::*;
use langmuir_core::{
    ExcitationRecord, LvdsPacketType, decode_excitation_frame, parse_common_frame,
    parse_udp_lvds_datagram,
};
use langmuir_transport::PcapNgReader;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::BufReader,
    path::Path,
    thread,
};

const CAPTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../202611212141.pcapng");
const WORKSPACE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/qa/lvds-multitype-s02-real-02/independent-manifest.json"
);

fn app() -> LangmuirDesktopApp {
    let (bridge, _endpoint) = crate::bridge::channel_bridge(64);
    LangmuirDesktopApp::with_bridge(
        Box::new(bridge),
        None,
        #[cfg(feature = "mock")]
        false,
    )
}

fn first_actual_zero_frame() -> ExcitationRecord {
    let mut reader = PcapNgReader::new(BufReader::new(
        fs::File::open(CAPTURE).expect("original capture is required"),
    ));
    let mut raw_record_id = 0;
    while let Some(packet) = reader.next_packet().unwrap() {
        let Some(udp) = packet.udp else { continue };
        raw_record_id += 1;
        if udp.source.to_string() != "192.168.1.128:1534" {
            continue;
        }
        let datagram = parse_udp_lvds_datagram(&udp.payload).unwrap();
        let common = parse_common_frame(datagram.payload).unwrap();
        if common.packet_type() != LvdsPacketType::Excitation {
            continue;
        }
        let frame = decode_excitation_frame(&common).unwrap();
        if frame.interval_us == 0 {
            return ExcitationRecord {
                frame,
                outer_sequence: datagram.header.frame_sequence,
                raw_record_id,
                source: udp.source.to_string(),
                session: CAPTURE.to_owned(),
                received_unix_nanos: u64::try_from(packet.timestamp.as_nanos()).unwrap(),
            };
        }
    }
    panic!("actual capture has no zero-interval frame");
}

#[test]
#[ignore = "requires original real multi-mode 202611212141.pcapng"]
fn real02_zero_interval_actual_frame_has_no_fabricated_plot_or_cursor() {
    let record = first_actual_zero_frame();
    assert_eq!(record.frame.points, 100);
    assert!(record.frame.wave_mv.iter().all(|value| *value == 0));
    let mut app = app();
    app.lvds_view.waveform = excitation_ui::WaveformView::Excitation;
    app.handle_backend_event(BackendEvent::ExcitationFrame(Box::new(record)));
    assert!(
        app.excitation_display_points().is_empty(),
        "zero interval must not fabricate100 coincident plot points"
    );
    let ctx = egui::Context::default();
    let _ = fonts::install_cjk_fallback(&ctx);
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 500.0),
            )),
            events: vec![egui::Event::PointerMoved(egui::pos2(400.0, 250.0))],
            ..egui::RawInput::default()
        },
        |ui| app.plot_panel(ui),
    );
    let labels: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.text()),
            _ => None,
        })
        .collect();
    assert!(
        labels
            .iter()
            .any(|text| text.contains("当前无有效激励波形"))
    );
    assert!(
        labels
            .iter()
            .any(|text| text.contains("无可用逐点采样时间"))
    );
    assert!(!labels.iter().any(|text| text.contains("电压 0.000 V")));
    assert_eq!(app.measurements.rendered, [0, 0]);
    assert_eq!(
        app.excitation_display_record().unwrap().frame.wave_mv.len(),
        100
    );
}

fn wait_for(app: &mut LangmuirDesktopApp, predicate: impl Fn(&LangmuirDesktopApp) -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        app.drain_backend();
        if predicate(app) {
            return;
        }
        assert!(
            Instant::now() < until,
            "production history timeout: {:?}",
            app.lvds_view.history.status
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "u32 protocol seconds in microseconds remain below 2^53"
)]
fn reference_seconds(value: &serde_json::Value) -> f64 {
    value.as_u64().unwrap() as f64 / 1_000_000.0
}

fn select(app: &mut LangmuirDesktopApp, index: u64, explicit: bool) {
    if explicit {
        app.prepare_excitation_navigation(index);
    }
    app.submit(UiRequest::LoadHistoryExcitationFrame { index });
    wait_for(app, |app| {
        app.lvds_view.history.status.excitation.selected_index == Some(index)
            && !app.lvds_view.history.status.excitation.loading
    });
}

fn paint(
    app: &mut LangmuirDesktopApp,
    ctx: &egui::Context,
) -> (Vec<String>, Option<egui_plot::PlotBounds>) {
    let mut labels = Vec::new();
    let mut bounds = None;
    for _ in 0..2 {
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 500.0),
                )),
                events: vec![egui::Event::PointerMoved(egui::pos2(400.0, 250.0))],
                ..egui::RawInput::default()
            },
            |ui| {
                app.plot_panel(ui);
                let id = if app.lvds_view.waveform == excitation_ui::WaveformView::Excitation {
                    "excitation_plot_ab"
                } else {
                    "scientific_plot_overlay"
                };
                bounds = egui_plot::PlotMemory::load(ctx, ui.make_persistent_id(egui::Id::new(id)))
                    .map(|memory| *memory.bounds());
            },
        );
        labels = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect();
    }
    (labels, bounds)
}

fn click_history_button(app: &mut LangmuirDesktopApp, ctx: &egui::Context, label: &str) {
    let draw = |app: &mut LangmuirDesktopApp, events| {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 100.0),
                )),
                events,
                ..egui::RawInput::default()
            },
            |ui| app.plot_toolbar(ui),
        )
    };
    let _ = draw(app, vec![]);
    let output = draw(app, vec![]);
    let pos = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(text.visual_bounding_rect().center())
            }
            _ => None,
        })
        .expect("history frame button visible");
    for pressed in [true, false] {
        let _ = draw(
            app,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}

fn verify_aa_windows(
    app: &mut LangmuirDesktopApp,
    reference: &serde_json::Value,
    ctx: &egui::Context,
) -> usize {
    let aa = reference["aa"].as_array().unwrap();
    let expected: HashMap<_, _> = aa
        .iter()
        .map(|frame| (frame["raw_record_id"].as_u64().unwrap(), frame))
        .collect();
    let mut checked = HashSet::new();
    for first in (0..aa.len()).step_by(1200) {
        let start = reference_seconds(&aa[first]["sample_time_us"][0]);
        let range = [start, start + 60.0];
        let revision = app
            .lvds_view
            .history
            .window
            .as_ref()
            .map_or(0, |window| window.revision);
        app.operator.plot_view = PlotViewMode::ManualView;
        app.operator.plot_x_range = Some(range);
        app.operator.plot_reset_pending = true;
        app.submit(UiRequest::LoadHistoryWindow {
            start_seconds: range[0],
            end_seconds: range[1],
        });
        wait_for(app, |app| {
            app.lvds_view.history.window.as_ref().is_some_and(|window| {
                window.revision > revision && (window.range[0] - range[0]).abs() < 1.0e-8
            })
        });
        let window = app.lvds_view.history.window.as_ref().unwrap();
        let (one, two) = app.display_histories();
        assert!(one.len() <= PLOT_CAPACITY && two.len() <= PLOT_CAPACITY);
        let mut ordinal = 0;
        for info in &window.frames {
            let frame = expected[&info.record_id];
            for sample in 0..100 {
                let time = reference_seconds(&frame["sample_time_us"][sample]);
                if time < range[0] || time > range[1] {
                    continue;
                }
                checked.insert((info.record_id, sample));
                for (buffer, points, channel) in [
                    (one, &window.channel_1, "ch1_code10"),
                    (two, &window.channel_2, "ch2_code10"),
                ] {
                    let point = &points[ordinal];
                    assert_eq!(point.record_id, info.record_id);
                    assert!((point.time_seconds - time).abs() < 1.0e-8);
                    let code = u16::try_from(frame[channel][sample].as_u64().unwrap()).unwrap();
                    let volts = f64::from(code) * 0.01 - 5.12;
                    assert_eq!(point.value, f64::from((i32::from(code) << 6) - 32_768));
                    assert!((buffer.nearest_value(time).unwrap() - volts).abs() < 1.0e-12);
                }
                ordinal += 1;
            }
        }
        assert_eq!(window.channel_1.len(), ordinal);
        assert_eq!(window.channel_2.len(), ordinal);
        let (_, bounds) = paint(app, ctx);
        assert!(
            bounds.unwrap().height() < 0.1,
            "near-zero AA stays in actual volts"
        );
    }
    assert_eq!(
        checked.len(),
        aa.len() * 100,
        "all AA samples covered through bounded windows"
    );
    aa.len()
}

#[test]
#[ignore = "requires actual 202611212141.pcapng and independent multi-mode manifest"]
#[expect(
    clippy::too_many_lines,
    reason = "one multi-mode actual history-to-egui acceptance with complete reference comparisons"
)]
fn real02_all_modes_production_history_to_egui_and_zero_state_transitions() {
    let reference: serde_json::Value =
        serde_json::from_slice(&fs::read(MANIFEST).unwrap()).unwrap();
    let ab = reference["ab"].as_array().unwrap();
    let mut app = LangmuirDesktopApp::with_bridge(
        Box::new(crate::backend::RealBackendBridge::spawn().unwrap()),
        None,
        #[cfg(feature = "mock")]
        false,
    );
    app.submit(UiRequest::OpenHistory {
        path: CAPTURE.to_owned(),
    });
    wait_for(&mut app, |app| {
        app.lvds_view.history.status.active
            && !app.lvds_view.history.status.loading
            && app.lvds_view.history.window.is_some()
    });
    assert_eq!(
        app.lvds_view.history.status.excitation.frames,
        u64::try_from(ab.len()).unwrap()
    );
    assert_eq!(app.lvds_view.history.status.error, None);
    let ctx = egui::Context::default();
    let _ = fonts::install_cjk_fallback(&ctx);
    let aa_checked = verify_aa_windows(&mut app, &reference, &ctx);
    app.lvds_view.waveform = excitation_ui::WaveformView::Excitation;
    let mut timed_frames = 0;
    let mut state_frames = 0;
    let mut stage_counts = BTreeMap::<u64, usize>::new();
    let mut stage_evidence = BTreeMap::new();
    let mut first_timed = None;
    for (index, expected) in ab.iter().enumerate() {
        let index = u64::try_from(index).unwrap();
        select(&mut app, index, true);
        let frame = &app.excitation_display_record().unwrap().frame;
        for (name, actual) in [
            ("work_mode", u64::from(frame.work_mode)),
            ("scan_freq", u64::from(frame.scan_freq)),
            ("scan_voltage_mv", u64::from(frame.scan_voltage_mv)),
            ("f_act_mhz", u64::from(frame.f_act_mhz)),
            ("interval_us", u64::from(frame.interval_us)),
            ("points", u64::from(frame.points)),
            ("ad9833_status", u64::from(frame.ad9833_status)),
            ("dac8563_status", u64::from(frame.dac8563_status)),
            ("pa0_seq", u64::from(frame.pa0_seq)),
            ("sci_frame_seq", u64::from(frame.sci_frame_seq)),
        ] {
            assert_eq!(
                actual,
                expected[name].as_u64().unwrap(),
                "frame {index} {name}"
            );
        }
        assert_eq!(frame.wave_mv.len(), 100);
        for (sample, actual) in frame.wave_mv.iter().enumerate() {
            assert_eq!(
                u64::from(*actual),
                expected["wave_mv"][sample].as_u64().unwrap(),
                "raw slot {sample} in AB {index}"
            );
        }
        let points = app.excitation_display_points();
        let timed = frame.interval_us > 0;
        assert_eq!(
            frame.has_sample_timing(),
            expected["state"] == "TIMED_SAMPLES"
        );
        let stage = expected["stage_index"].as_u64().unwrap();
        let voltage_setting = format!("设定 {:.3} V", f64::from(frame.scan_voltage_mv) / 1000.0);
        let is_dc = frame.work_mode == 0xBB;
        if timed {
            assert_eq!(points.len(), 100);
            for (sample, point) in points.iter().enumerate() {
                assert!(
                    (point[0] - reference_seconds(&expected["sample_time_us"][sample])).abs()
                        < 1.0e-8
                );
                let mv = u16::try_from(expected["wave_mv"][sample].as_u64().unwrap()).unwrap();
                assert!((point[1] - f64::from(mv) / 1000.0).abs() < 1.0e-12);
            }
            first_timed.get_or_insert(index);
            timed_frames += 1;
        } else {
            assert_eq!(points, [] as [[f64; 2]; 0]);
            assert!((0..100).all(|sample| frame.sample_time_us(sample).is_none()));
            assert!(
                expected["sample_time_us"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(serde_json::Value::is_null)
            );
            state_frames += 1;
        }
        let (labels, bounds) = paint(&mut app, &ctx);
        if timed {
            let bounds = bounds.unwrap();
            assert!((bounds.min()[0] - points[0][0]).abs() < 1.0e-8);
            assert!((bounds.max()[0] - points[99][0]).abs() < 1.0e-8);
            assert_eq!(app.measurements.rendered, [100, 0]);
        } else {
            assert!(
                labels
                    .iter()
                    .any(|text| text.contains("当前无有效激励波形"))
            );
            assert!(
                labels
                    .iter()
                    .any(|text| text.contains("无可用逐点采样时间"))
            );
            assert!(!labels.iter().any(|text| text.contains("电压 0.000 V")));
            assert_eq!(app.measurements.rendered, [0, 0]);
        }
        if is_dc {
            assert!(labels.iter().any(|text| text.contains(&voltage_setting)));
        }
        *stage_counts.entry(stage).or_default() += 1;
        stage_evidence.entry(stage).or_insert_with(|| serde_json::json!({
            "stage_index":stage, "selected_ab_index":index, "work_mode":expected["work_mode"],
            "scan_freq":expected["scan_freq"], "scan_voltage_mv":expected["scan_voltage_mv"],
            "f_act_mhz":expected["f_act_mhz"], "interval_us":expected["interval_us"], "state":expected["state"],
            "rendered_points":points.len(), "labels":labels, "native_gui_screenshot":"NOT_RUN"
        }));
    }
    for stage in reference["stages"].as_array().unwrap() {
        assert_eq!(
            u64::try_from(stage_counts[&stage["stage_index"].as_u64().unwrap()]).unwrap(),
            stage["frames"].as_u64().unwrap()
        );
    }
    let toolbar = egui::Context::default();
    let _ = fonts::install_cjk_fallback(&toolbar);
    click_history_button(&mut app, &toolbar, "上一帧");
    wait_for(&mut app, |app| {
        app.lvds_view.history.status.excitation.selected_index
            == Some(u64::try_from(ab.len() - 2).unwrap())
            && !app.lvds_view.history.status.excitation.loading
    });
    click_history_button(&mut app, &toolbar, "下一帧");
    wait_for(&mut app, |app| {
        app.lvds_view.history.status.excitation.selected_index
            == Some(u64::try_from(ab.len() - 1).unwrap())
            && !app.lvds_view.history.status.excitation.loading
    });
    select(&mut app, 0, true);
    app.excitation_toggle_freeze();
    select(&mut app, first_timed.unwrap(), false);
    assert!(
        app.excitation_display_points().is_empty(),
        "frozen real state remains empty"
    );
    app.excitation_toggle_freeze();
    assert_eq!(app.excitation_display_points().len(), 100);
    app.excitation_toggle_freeze();
    select(&mut app, 0, false);
    assert_eq!(
        app.excitation_display_points().len(),
        100,
        "frozen measured frame retained"
    );
    app.excitation_toggle_freeze();
    assert_eq!(app.excitation_display_points(), [] as [[f64; 2]; 0]);
    let evidence = serde_json::json!({
        "verification":"AUTOMATED_EGUI_NOT_NATIVE_WINDOWS_GUI", "capture":CAPTURE, "manifest":MANIFEST,
        "aa_frames_checked_in_bounded_windows":aa_checked, "aa_voltage_values_compared":aa_checked*200,
        "ab_frames_checked":ab.len(), "timed_frames":timed_frames, "timed_points":timed_frames*100,
        "no_sample_time_frames":state_frames, "raw_slots_retained":ab.len()*100, "stages":stage_evidence.values().collect::<Vec<_>>(),
        "previous_next_actual_pointer_input":true, "specified_frame_selection":true,
        "freeze_resume_empty_to_timed_and_timed_to_empty":true, "native_windows_gui":"NOT_RUN"
    });
    let output = Path::new(WORKSPACE)
        .join("target/qa/lvds-multitype-s02-real-02")
        .join(format!(
            "ui-multimode-{}.json",
            Local::now().format("%Y%m%d-%H%M%S-%3f")
        ));
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(&output, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    println!("Actual multi-mode egui evidence: {}", output.display());
    app.submit(UiRequest::CloseHistory);
}
