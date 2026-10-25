use super::*;
use eframe::egui::{Context, Event, Modifiers, MouseWheelUnit, Pos2, RawInput, Rect, Vec2};
use langmuir_core::SAMPLE_COUNT;

#[test]
fn voltage_ingress_converts_adc_once_and_keeps_first_sample_device_time() {
    // Missing scaling, the old mV scaling or a second conversion breaks these
    // independent literal hardware range cases.
    let (bridge, _endpoint) = crate::bridge::channel_bridge(64);
    let mut app = LangmuirDesktopApp::with_bridge(
        Box::new(bridge),
        None,
        #[cfg(feature = "mock")]
        false,
    );
    let channel_1: Vec<_> = [
        -32_768.0, -16_384.0, 0.0, 12_096.0, 12_160.0, 20_032.0, 32_704.0,
    ]
    .into_iter()
    .cycle()
    .take(100)
    .collect();
    let channel_2: Vec<_> = [
        32_704.0, 20_032.0, 12_160.0, 12_096.0, 0.0, -16_384.0, -32_768.0,
    ]
    .into_iter()
    .cycle()
    .take(100)
    .collect();
    app.handle_backend_event(BackendEvent::ScientificSamples {
        channel_1_start_seconds: 9.999_800,
        channel_2_start_seconds: 9.999_800,
        sample_interval_seconds: 0.000_500,
        channel_1,
        channel_2,
    });
    let (first, second) = app.display_plot_points();
    assert_eq!((first.len(), second.len()), (100, 100));
    assert_eq!(first[0], [9.999_800, -5.12]);
    assert_eq!(second[0], [9.999_800, 5.11]);
    assert!((first[1][0] - 10.000_300).abs() < 1.0e-10);
    for (index, expected) in [-5.12, -2.56, 0.0, 1.89, 1.90, 3.13, 5.11]
        .into_iter()
        .enumerate()
    {
        assert!((first[index][1] - expected).abs() < 1.0e-12);
        assert!((second[6 - index][1] - expected).abs() < 1.0e-12);
        assert_eq!(format!("{:.2}", first[index][1]), format!("{expected:.2}"));
    }
    assert!((first[99][0] - 10.049_300).abs() < 1.0e-10);
    assert_eq!(second[99][1], 3.13);
}

#[test]
fn migration_device_axis_is_numeric_seconds_and_resolves_500_microseconds() {
    // Calendar conversion or millisecond-only formatting would hide the new
    // sample spacing and incorrectly assume the device seconds' epoch.
    assert_eq!(format_plot_timestamp(148.899_965, 0.05), "148.899965");
    assert_eq!(format_plot_timestamp(148.900_465, 0.05), "148.900465");
    assert_eq!(
        format_plot_timestamp(4_294_967_295.999_5, 0.0),
        "4294967295.999500"
    );
    assert_eq!(format_plot_timestamp(148.0, 60.0), "148.000");
    assert_eq!(format_plot_timestamp(f64::NAN, 1.0), "");
}

#[test]
fn voltage_plot_labels_and_cursor_use_volts_with_two_decimals() {
    let mut app = voltage_cursor_app();
    app.operator.plot_cursor_seconds = Some(100.000_500);
    let context = Context::default();
    let draw = |app: &mut LangmuirDesktopApp| {
        context.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0))),
                ..RawInput::default()
            },
            |ui| app.plot_panel(ui),
        )
    };
    let _ = draw(&mut app);
    let output = draw(&mut app);
    let labels: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.text()),
            _ => None,
        })
        .collect();
    assert!(labels.iter().any(|text| text.contains("电压 (V)")));
    assert!(labels.iter().any(|text| text.contains("设备时间 (s)")));
    assert!(
        labels
            .iter()
            .any(|text| text.contains("通道1 1.90 V  通道2 1.89 V"))
    );
    assert!(
        !labels
            .iter()
            .any(|text| text.contains("mV") || text.contains("ADC") || text.contains("电压估算"))
    );
}

fn voltage_cursor_app() -> LangmuirDesktopApp {
    let mut app = test_app();
    app.clear_current_plot();
    app.handle_scientific_samples(
        100.0,
        100.0,
        0.000_500,
        vec![12_160.0; SAMPLE_COUNT],
        vec![12_096.0; SAMPLE_COUNT],
    );
    app
}

#[test]
fn voltage_legal_55aa_frame_keeps_all_100_codes_and_both_sample_times() {
    // These five-byte wire groups are literal packed 10-bit samples from the
    // manufacturer protocol vector, independent of the display conversion.
    let mut bytes = [0_u8; langmuir_core::LVDS_FRAME_LEN];
    bytes[..15].copy_from_slice(&[
        0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA, 0, 0, 0, 100, 0, 0, 0, 0,
    ]);
    for group in bytes[15..140].as_chunks_mut::<5>().0 {
        group.copy_from_slice(&[0xFF, 0xC0, 0x08, 0, 1]);
    }
    for group in bytes[140..265].as_chunks_mut::<5>().0 {
        group.copy_from_slice(&[0, 0x3F, 0xF0, 3, 0xFF]);
    }
    bytes[265] = 23;
    let frame = langmuir_core::decode_scientific_frame(&bytes).unwrap();
    let original = frame.clone();
    let mut app = voltage_cursor_app();
    app.clear_current_plot();
    app.handle_backend_event(BackendEvent::ScientificSamples {
        channel_1_start_seconds: frame.channel_1_base_seconds(),
        channel_2_start_seconds: frame.channel_2_base_seconds(),
        sample_interval_seconds: 0.000_500,
        channel_1: frame
            .channel_1
            .iter()
            .map(|&code| f64::from(langmuir_core::reconstruct_adc16(code)))
            .collect(),
        channel_2: frame
            .channel_2
            .iter()
            .map(|&code| f64::from(langmuir_core::reconstruct_adc16(code)))
            .collect(),
    });
    let (one, two) = app.display_plot_points();
    assert_eq!((one.len(), two.len()), (100, 100));
    for index in 0..100 {
        let expected_one = [5.11, -5.12, 0.0, -5.11][index % 4];
        let expected_two = [-5.12, 5.11, -5.12, 5.11][index % 4];
        let expected_time = 100.0 + f64::from(u32::try_from(index).unwrap()) * 0.000_500;
        assert!((one[index][1] - expected_one).abs() < 1.0e-12);
        assert!((two[index][1] - expected_two).abs() < 1.0e-12);
        assert!((one[index][0] - expected_time).abs() < 1.0e-10);
        assert_eq!(one[index][0], two[index][0]);
    }
    assert_eq!(
        frame, original,
        "displaying volts cannot mutate source codes"
    );
}

#[test]
fn voltage_all_live_qualities_and_history_rebuilds_use_one_conversion() {
    use crate::bridge::{HistoryPoint, HistoryStatus, HistoryWindow, QualifiedSamples};
    use history_ui::QualitySelection;
    use langmuir_core::FrameQuality;

    let cases = [
        (FrameQuality::Native, -32_768.0, 32_704.0, -5.12, 5.11),
        (FrameQuality::Recovered1Bit, -16_384.0, 0.0, -2.56, 0.0),
        (FrameQuality::Recovered4Bit, 12_160.0, 12_096.0, 1.90, 1.89),
        (
            FrameQuality::RecoveryCandidate,
            20_032.0,
            -64.0,
            3.13,
            -0.01,
        ),
    ];
    let mut app = voltage_cursor_app();
    app.clear_current_plot();
    let mut channel_1 = Vec::new();
    let mut channel_2 = Vec::new();
    for (index, &(quality, adc_one, adc_two, _, _)) in cases.iter().enumerate() {
        let start = 100.0 + f64::from(u32::try_from(index).unwrap());
        app.handle_backend_event(BackendEvent::QualifiedScientificSamples(Box::new(
            QualifiedSamples {
                quality,
                raw_record_id: u64::try_from(index).unwrap() + 1,
                outer_sequence: Some(u32::try_from(index).unwrap()),
                frame_seq_u8: u8::try_from(index).unwrap(),
                start_seconds: start,
                sample_interval_seconds: 0.000_500,
                channel_1: vec![adc_one; SAMPLE_COUNT],
                channel_2: vec![adc_two; SAMPLE_COUNT],
                source: "SYNTHETIC voltage display regression".to_owned(),
                reason: String::new(),
            },
        )));
        for sample in 0..100 {
            let time_seconds = start + f64::from(sample) * 0.000_500;
            channel_1.push(HistoryPoint {
                time_seconds,
                value: adc_one,
                quality,
                record_id: u64::try_from(index).unwrap() + 1,
            });
            channel_2.push(HistoryPoint {
                time_seconds,
                value: adc_two,
                quality,
                record_id: u64::try_from(index).unwrap() + 1,
            });
        }
    }
    let check = |app: &mut LangmuirDesktopApp, count: usize| {
        let (one, two) = app.display_plot_points();
        assert_eq!((one.len(), two.len()), (count * 100, count * 100));
        for (index, &(_, _, _, volts_one, volts_two)) in cases.iter().take(count).enumerate() {
            assert!((one[index * 100][1] - volts_one).abs() < 1.0e-12);
            assert!((two[index * 100][1] - volts_two).abs() < 1.0e-12);
        }
    };
    for (selection, count) in [
        (QualitySelection::Native, 1),
        (QualitySelection::Confirmed, 3),
        (QualitySelection::Diagnostic, 4),
        (QualitySelection::Confirmed, 3),
    ] {
        app.set_quality_selection(selection);
        check(&mut app, count);
    }
    app.handle_history_event(BackendEvent::HistoryStatus(Box::new(HistoryStatus {
        active: true,
        generation: 1,
        ..HistoryStatus::default()
    })));
    app.handle_history_event(BackendEvent::HistoryWindow(Box::new(HistoryWindow {
        generation: 1,
        revision: 1,
        range: [100.0, 104.0],
        channel_1,
        channel_2,
        frames: Vec::new(),
    })));
    for (selection, count) in [
        (QualitySelection::Native, 1),
        (QualitySelection::Confirmed, 3),
        (QualitySelection::Diagnostic, 4),
        (QualitySelection::Confirmed, 3),
    ] {
        app.set_quality_selection(selection);
        check(&mut app, count);
    }
}

#[test]
fn voltage_normalization_labels_unitless_axes_but_preserves_actual_cursor() {
    let mut app = voltage_cursor_app();
    app.overlay_normalized = true;
    app.operator.plot_cursor_seconds = Some(100.000_500);
    let context = Context::default();
    let draw = |app: &mut LangmuirDesktopApp| {
        context.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0))),
                ..RawInput::default()
            },
            |ui| app.plot_panel(ui),
        )
    };
    let _ = draw(&mut app);
    let output = draw(&mut app);
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
            .any(|text| text.contains("独立归一化幅值（无量纲"))
    );
    assert!(!labels.iter().any(|text| text.contains("电压 (V)")));
    assert!(
        labels
            .iter()
            .any(|text| text.contains("通道 1 [1.90, 1.90] V"))
    );
    assert!(
        labels
            .iter()
            .any(|text| text.contains("通道 2 [1.89, 1.89] V"))
    );
    assert!(
        labels
            .iter()
            .any(|text| text.contains("通道1 1.90 V  通道2 1.89 V"))
    );
    let (one, two) = app.display_histories();
    assert!(
        one.visible_points()
            .iter()
            .all(|point| (point.y - 0.5).abs() < 1.0e-12)
    );
    assert!(
        two.visible_points()
            .iter()
            .all(|point| (point.y - 0.5).abs() < 1.0e-12)
    );
    assert!((one.nearest_value(100.0).unwrap() - 1.90).abs() < 1.0e-12);
    assert!((two.nearest_value(100.0).unwrap() - 1.89).abs() < 1.0e-12);
}

#[test]
fn voltage_mouse_hover_formats_volts_and_marks_normalized_amplitude_unitless() {
    for (normalized, y, expected) in [
        (false, 1.90, "电压 1.90 V"),
        (true, 0.50, "归一化幅值 0.50（无量纲）"),
    ] {
        let context = Context::default();
        let draw = |events| {
            let mut hover = Pos2::ZERO;
            let output = context.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0))),
                    events,
                    ..RawInput::default()
                },
                |ui| {
                    let config = ScientificPlotConfig {
                        id: "voltage_hover_test",
                        height: 500.0,
                        range: [100.0, 101.0],
                        reset: false,
                        normalized,
                        y_label: if normalized {
                            "归一化幅值（无量纲）"
                        } else {
                            "电压 (V)"
                        },
                        channels: [true, false],
                        names: ["通道 1", "通道 2"],
                    };
                    let response = configured_scientific_plot(&config).show(ui, |plot_ui| {
                        plot_ui.set_plot_bounds_x(100.0..=101.0);
                        plot_ui.line(Line::new("通道 1", vec![[100.0, y], [101.0, y]]));
                    });
                    hover = response
                        .transform
                        .position_from_point(&PlotPoint::new(100.5, y));
                },
            );
            (output, hover)
        };
        let _ = draw(vec![]);
        let (_, pointer) = draw(vec![]);
        let _ = draw(vec![Event::PointerMoved(pointer)]);
        let (output, _) = draw(vec![Event::PointerMoved(pointer)]);
        assert!(
            output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.text().contains(expected)
            )),
            "normalized={normalized}, pointer={pointer:?}, labels={:?}",
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.text()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn voltage_auto_y_fits_visible_channels_and_manual_y_survives_until_reset() {
    let mut app = voltage_cursor_app();
    app.handle_scientific_samples(
        100.05,
        100.05,
        0.000_500,
        vec![-64.0, 0.0, 64.0],
        vec![12_160.0, 12_096.0, 0.0],
    );
    let context = Context::default();
    render(&mut app, &context, vec![]);
    let auto = render(&mut app, &context, vec![])[0];
    assert!(auto.min()[1] < -0.01 && auto.max()[1] > 1.90);
    assert!(
        auto.height() < 3.0,
        "a local signal must not fit the full 10.23 V range"
    );
    assert!(
        auto.min()[1] > -0.3 && auto.max()[1] < 2.2,
        "keep the existing small auto margin"
    );
    drag(
        &mut app,
        &context,
        Pos2::new(450.0, 220.0),
        Pos2::new(570.0, 250.0),
    );
    let manual = render(&mut app, &context, vec![])[0];
    assert_eq!(app.operator.plot_view, PlotViewMode::ManualView);
    assert_ne!(manual.range_y(), auto.range_y());
    app.handle_scientific_samples(
        101.0,
        101.0,
        0.000_500,
        vec![-32_768.0; 100],
        vec![32_704.0; 100],
    );
    for _ in 0..3 {
        let repainted = render(&mut app, &context, vec![])[0];
        assert_eq!(repainted.range_y(), manual.range_y());
        assert_eq!(repainted.range_x(), manual.range_x());
    }
    app.follow_latest();
    let reset = render(&mut app, &context, vec![])[0];
    assert_eq!(app.operator.plot_view, PlotViewMode::FollowLatest);
    assert!(reset.min()[1] < -5.12 && reset.max()[1] > 5.11);
    assert!(reset.height() < 12.0);
}

fn test_app() -> LangmuirDesktopApp {
    let (bridge, _endpoint) = crate::bridge::channel_bridge(64);
    let mut app = LangmuirDesktopApp::with_bridge(
        Box::new(bridge),
        None,
        #[cfg(feature = "mock")]
        false,
    );
    for index in 0..800 {
        let start = 100.0 + f64::from(index) * 0.05;
        app.handle_scientific_samples(
            start,
            start,
            0.000_500,
            vec![0.0; SAMPLE_COUNT],
            vec![640.0; SAMPLE_COUNT],
        );
    }
    app
}

fn render(
    app: &mut LangmuirDesktopApp,
    context: &Context,
    events: Vec<Event>,
) -> Vec<egui_plot::PlotBounds> {
    let mut bounds = Vec::new();
    let _ = context.run_ui(
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            events,
            ..RawInput::default()
        },
        |ui| {
            app.plot_panel(ui);
            for id in [
                "scientific_plot_overlay",
                "scientific_plot_channel_1",
                "scientific_plot_channel_2",
            ] {
                if let Some(memory) =
                    egui_plot::PlotMemory::load(context, ui.make_persistent_id(egui::Id::new(id)))
                {
                    bounds.push(*memory.bounds());
                }
            }
        },
    );
    bounds
}

fn drag(app: &mut LangmuirDesktopApp, context: &Context, from: Pos2, to: Pos2) {
    render(
        app,
        context,
        vec![
            Event::PointerMoved(from),
            Event::PointerButton {
                pos: from,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    render(app, context, vec![Event::PointerMoved(to)]);
    render(
        app,
        context,
        vec![Event::PointerButton {
            pos: to,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
    );
}

#[test]
fn plot_left_drag_enters_manual_and_incoming_samples_do_not_steal_view() {
    let mut app = test_app();
    let context = Context::default();
    render(&mut app, &context, vec![]);
    render(&mut app, &context, vec![]);
    let initial = app.plot_range();
    drag(
        &mut app,
        &context,
        Pos2::new(450.0, 220.0),
        Pos2::new(570.0, 250.0),
    );
    assert_eq!(app.operator.plot_view, PlotViewMode::ManualView);
    let manual = app.plot_range();
    assert!(
        manual[0] < initial[0],
        "dragging right exposes older history"
    );
    app.handle_scientific_samples(
        141.0,
        141.0,
        0.000_500,
        vec![0.0; SAMPLE_COUNT],
        vec![0.0; SAMPLE_COUNT],
    );
    render(&mut app, &context, vec![]);
    assert_eq!(app.plot_range(), manual);
    app.follow_latest();
    render(&mut app, &context, vec![]);
    assert_eq!(app.operator.plot_view, PlotViewMode::FollowLatest);
    assert!(app.plot_range()[1] > initial[1]);
}

#[test]
fn plot_plain_wheel_zooms_and_hover_updates() {
    let mut app = test_app();
    app.handle_backend_event(BackendEvent::ScientificSamples {
        channel_1_start_seconds: 140.0,
        channel_2_start_seconds: 140.0,
        sample_interval_seconds: 0.000_500,
        channel_1: vec![0.0; SAMPLE_COUNT],
        channel_2: vec![64.0; SAMPLE_COUNT],
    });
    let context = Context::default();
    render(&mut app, &context, vec![]);
    render(&mut app, &context, vec![]);
    let initial = app.plot_range();
    render(
        &mut app,
        &context,
        vec![Event::PointerMoved(Pos2::new(450.0, 220.0))],
    );
    assert!(app.operator.plot_cursor_seconds.is_some());
    render(
        &mut app,
        &context,
        vec![Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: Vec2::new(0.0, 100.0),
            modifiers: Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        }],
    );
    assert_eq!(app.operator.plot_view, PlotViewMode::ManualView);
    let zoomed = app.plot_range();
    assert!(zoomed[1] - zoomed[0] < initial[1] - initial[0]);
}

#[test]
fn plot_split_synchronizes_time_and_freeze_keeps_entire_snapshot() {
    let mut app = test_app();
    let context = Context::default();
    app.plot_layout = PlotLayout::Split;
    render(&mut app, &context, vec![]);
    render(&mut app, &context, vec![]);
    drag(
        &mut app,
        &context,
        Pos2::new(450.0, 150.0),
        Pos2::new(570.0, 170.0),
    );
    let bounds = render(&mut app, &context, vec![]);
    assert_eq!(bounds.len(), 2);
    assert_eq!(bounds[0].range_x(), bounds[1].range_x());
    let range = app.plot_range();
    app.toggle_display_freeze();
    let frozen = app.operator.frozen_plot.as_ref().unwrap();
    let snapshot_count = frozen.channel_1.len();
    let snapshot_end = frozen.channel_1.latest_time();
    assert_eq!(
        snapshot_count, 80_000,
        "freeze retains raw history for later zooming"
    );
    app.handle_scientific_samples(
        150.0,
        150.0,
        0.000_500,
        vec![0.0; SAMPLE_COUNT],
        vec![0.0; SAMPLE_COUNT],
    );
    render(&mut app, &context, vec![]);
    assert_eq!(app.plot_range(), range);
    assert_eq!(
        app.operator
            .frozen_plot
            .as_ref()
            .unwrap()
            .channel_1
            .latest_time(),
        snapshot_end
    );
    app.follow_latest();
    render(&mut app, &context, vec![]);
    assert_eq!(
        app.operator.frozen_plot.as_ref().unwrap().channel_1.len(),
        snapshot_count
    );
    app.toggle_display_freeze();
    render(&mut app, &context, vec![]);
    assert!(app.plot_range()[1] > snapshot_end.unwrap());
}

fn click_normalization(app: &mut LangmuirDesktopApp, context: &Context) {
    let controls = |app: &mut LangmuirDesktopApp, events| {
        context.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 100.0))),
                events,
                ..RawInput::default()
            },
            |ui| {
                ui.horizontal(|ui| app.plot_toolbar_modes(ui, false));
            },
        )
    };
    let _ = controls(app, vec![]);
    let output = controls(app, vec![]);
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == "独立归一化" => {
                Some(text.pos + text.galley.rect.center().to_vec2())
            }
            _ => None,
        })
        .expect("normalization checkbox is rendered");
    for pressed in [true, false] {
        let _ = controls(
            app,
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
fn plot_normalization_refits_y_without_losing_manual_time_range() {
    let mut app = test_app();
    let context = Context::default();
    render(&mut app, &context, vec![]);
    render(&mut app, &context, vec![]);
    drag(
        &mut app,
        &context,
        Pos2::new(450.0, 220.0),
        Pos2::new(570.0, 250.0),
    );
    let manual_range = app.plot_range();
    let manual_bounds = render(&mut app, &context, vec![])[0];
    app.handle_scientific_samples(
        141.0,
        141.0,
        0.000_500,
        vec![0.0; SAMPLE_COUNT],
        vec![0.0; SAMPLE_COUNT],
    );
    let after_new_data = render(&mut app, &context, vec![])[0];
    assert_eq!(
        after_new_data.range_y(),
        manual_bounds.range_y(),
        "new data must preserve manually panned Y bounds"
    );

    let controls = Context::default();
    click_normalization(&mut app, &controls);
    assert!(app.overlay_normalized);
    assert!(app.operator.plot_reset_pending);
    let normalized = render(&mut app, &context, vec![])[0];
    assert_eq!(app.operator.plot_view, PlotViewMode::ManualView);
    assert_eq!(app.plot_range(), manual_range);
    assert!(normalized.min()[1] <= 0.5 && normalized.max()[1] >= 0.5);
    assert_ne!(normalized.range_y(), manual_bounds.range_y());
    assert!(
        (normalized.center().y - 0.5).abs() < 1.0e-12,
        "normalization must refit the constant normalized samples around 0.5"
    );

    click_normalization(&mut app, &controls);
    assert!(!app.overlay_normalized);
    let raw = render(&mut app, &context, vec![])[0];
    assert_eq!(app.operator.plot_view, PlotViewMode::ManualView);
    assert_eq!(app.plot_range(), manual_range);
    assert!(
        raw.min()[1] <= 0.0 && raw.max()[1] >= 0.1,
        "turning normalization off must fit the actual signed voltage values"
    );
}
