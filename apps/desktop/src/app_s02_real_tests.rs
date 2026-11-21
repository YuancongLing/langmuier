//! Actual user capture through the production history worker and egui plots.
//! This is automated egui verification, not a native Windows GUI certification.
use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
};

const CAPTURE: &str = "C:/Users/Administrator/Downloads/2026112101.pcapng";
const WORKSPACE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/qa/lvds-multitype-s02-real/independent-manifest.json"
);

fn output_directory() -> PathBuf {
    std::env::var_os("LANGMUIR_S02_REAL_UI_OUTPUT").map_or_else(
        || Path::new(WORKSPACE).join("target/qa/lvds-multitype-s02-real"),
        PathBuf::from,
    )
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
    reason = "u32 device seconds in microseconds remain below 2^53"
)]
fn seconds(value: &serde_json::Value) -> f64 {
    value.as_u64().expect("reference integer microseconds") as f64 / 1_000_000.0
}

fn paint_plot(
    app: &mut LangmuirDesktopApp,
    context: &egui::Context,
    id: &str,
) -> egui_plot::PlotBounds {
    let mut bounds = None;
    for _ in 0..2 {
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 500.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                app.plot_panel(ui);
                bounds =
                    egui_plot::PlotMemory::load(context, ui.make_persistent_id(egui::Id::new(id)))
                        .map(|memory| *memory.bounds());
            },
        );
    }
    bounds.expect("actual plot rendered")
}

#[test]
#[ignore = "requires the original 2026112101.pcapng and independent Python manifest"]
#[expect(
    clippy::too_many_lines,
    reason = "one actual-input history-to-plot acceptance with complete point comparisons"
)]
fn s02_real_production_history_to_egui_matches_all_aa_and_ab_points() {
    assert!(
        Path::new(CAPTURE).is_file(),
        "original user PCAP is required"
    );
    let reference: serde_json::Value =
        serde_json::from_slice(&fs::read(MANIFEST).expect("independent Python reference required"))
            .unwrap();
    let aa = reference["aa"].as_array().unwrap();
    let ab = reference["ab"].as_array().unwrap();
    let bridge = crate::backend::RealBackendBridge::spawn().unwrap();
    let mut app = LangmuirDesktopApp::with_bridge(
        Box::new(bridge),
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
        app.lvds_view.history.status.native,
        u64::try_from(aa.len()).unwrap()
    );
    assert_eq!(
        app.lvds_view.history.status.excitation.frames,
        u64::try_from(ab.len()).unwrap()
    );
    assert_eq!(app.lvds_view.history.status.error, None);
    let (one, two) = app.display_histories();
    assert_eq!(one.len(), aa.len() * 100);
    assert_eq!(two.len(), aa.len() * 100);
    for frame in aa {
        for sample in 0..100 {
            let time = seconds(&frame["sample_time_us"][sample]);
            for (buffer, channel) in [(one, "ch1_code10"), (two, "ch2_code10")] {
                let code = u16::try_from(frame[channel][sample].as_u64().unwrap()).unwrap();
                let expected = f64::from(code) * 0.01 - 5.12;
                assert!((buffer.nearest_value(time).unwrap() - expected).abs() < 1.0e-12);
            }
        }
    }
    let context = egui::Context::default();
    let _ = fonts::install_cjk_fallback(&context);
    let aa_bounds = paint_plot(&mut app, &context, "scientific_plot_overlay");
    assert!(aa_bounds.min()[1] < 0.0 && aa_bounds.max()[1] > 0.01);
    assert!(
        aa_bounds.height() < 0.02,
        "near-zero AA must fit actual volts: {aa_bounds:?}"
    );
    app.plot_layout = PlotLayout::Split;
    let _ = paint_plot(&mut app, &context, "scientific_plot_channel_1");
    let _ = paint_plot(&mut app, &context, "scientific_plot_channel_2");
    app.plot_layout = PlotLayout::Overlay;
    app.lvds_view.waveform = excitation_ui::WaveformView::Excitation;
    let mut plotted = 0;
    let mut first_bounds = None;
    let mut last_bounds = None;
    for (index, frame) in ab.iter().enumerate() {
        let index = u64::try_from(index).unwrap();
        app.prepare_excitation_navigation(index);
        app.submit(UiRequest::LoadHistoryExcitationFrame { index });
        wait_for(&mut app, |app| {
            app.lvds_view.history.status.excitation.selected_index == Some(index)
                && !app.lvds_view.history.status.excitation.loading
        });
        let record = app.excitation_display_record().unwrap();
        assert_eq!(
            u64::from(record.frame.pa0_seq),
            frame["pa0_seq"].as_u64().unwrap()
        );
        assert_eq!(
            u64::from(record.frame.sci_frame_seq),
            frame["sci_frame_seq"].as_u64().unwrap()
        );
        assert_eq!(record.frame.actual_frequency_hz(), 0.931);
        assert_eq!(record.frame.scan_freq, 1);
        assert_eq!(record.frame.work_mode, 0x55);
        let points = app.excitation_display_points();
        assert_eq!(points.len(), 100);
        for (sample, point) in points.iter().enumerate() {
            let expected_mv = u16::try_from(frame["wave_mv"][sample].as_u64().unwrap()).unwrap();
            assert!((point[0] - seconds(&frame["sample_time_us"][sample])).abs() < 1.0e-9);
            assert!((point[1] - f64::from(expected_mv) / 1000.0).abs() < 1.0e-12);
        }
        assert!((points[99][0] - points[0][0] - 1.0692).abs() < 1.0e-8);
        let bounds = paint_plot(&mut app, &context, "excitation_plot_ab");
        assert!(
            bounds.min()[1] > 1.6 && bounds.max()[1] < 1.8,
            "actual AB volts: {bounds:?}"
        );
        assert!((bounds.min()[0] - points[0][0]).abs() < 1.0e-8);
        assert!((bounds.max()[0] - points[99][0]).abs() < 1.0e-8);
        first_bounds.get_or_insert(bounds);
        last_bounds = Some(bounds);
        plotted += points.len();
    }
    // A background history update must not steal a frozen actual frame.
    let frozen = app.excitation_display_record().unwrap().clone();
    app.excitation_toggle_freeze();
    app.submit(UiRequest::LoadHistoryExcitationFrame { index: 0 });
    wait_for(&mut app, |app| {
        app.lvds_view.history.status.excitation.selected_index == Some(0)
            && !app.lvds_view.history.status.excitation.loading
    });
    assert_eq!(app.excitation_display_record(), Some(&frozen));
    app.excitation_toggle_freeze();
    assert_eq!(app.excitation_display_record().unwrap().frame.pa0_seq, 23);
    assert_eq!(app.excitation_display_points().len(), 100);
    let bounds =
        |bounds: egui_plot::PlotBounds| serde_json::json!({"min":bounds.min(), "max":bounds.max()});
    let evidence = serde_json::json!({
        "verification":"AUTOMATED_EGUI_NOT_NATIVE_WINDOWS_GUI", "capture":CAPTURE, "independent_manifest":MANIFEST,
        "aa_frames":aa.len(), "aa_points_per_channel":aa.len()*100, "aa_overlay_bounds":bounds(aa_bounds),
        "aa_all_plot_values_compared":true, "aa_split_rendered":true,
        "ab_frames_selected_and_rendered":ab.len(), "ab_points_compared":plotted,
        "ab_first_bounds":bounds(first_bounds.unwrap()), "ab_last_bounds":bounds(last_bounds.unwrap()),
        "freeze_and_resume_actual_history_frames":true, "native_windows_gui":"NOT_RUN"
    });
    let output = output_directory().join(format!(
        "ui-automation-{}.json",
        Local::now().format("%Y%m%d-%H%M%S-%3f")
    ));
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(&output, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    println!("Actual capture egui evidence: {}", output.display());
    app.submit(UiRequest::CloseHistory);
}
