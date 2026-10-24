use super::*;
use eframe::egui::{Context, Event, Modifiers, MouseWheelUnit, Pos2, RawInput, Rect, Vec2};
use langmuir_core::SAMPLE_COUNT;

#[test]
fn migration_keeps_reconstructed_signed_adc_and_first_sample_device_time() {
    // A second offset/mV conversion would corrupt all four literal edge values.
    let (bridge, _endpoint) = crate::bridge::channel_bridge(64);
    let mut app = LangmuirDesktopApp::with_bridge(
        Box::new(bridge),
        None,
        #[cfg(feature = "mock")]
        false,
    );
    let channel_1: Vec<_> = [-32_768.0, -64.0, 0.0, 32_704.0]
        .into_iter()
        .cycle()
        .take(100)
        .collect();
    let channel_2: Vec<_> = [32_704.0, 0.0, -64.0, -32_768.0]
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
    assert_eq!(first[0], [9.999_800, -32_768.0]);
    assert_eq!(second[0], [9.999_800, 32_704.0]);
    assert!((first[1][0] - 10.000_300).abs() < 1.0e-10);
    assert_eq!(first[1][1], -64.0);
    assert_eq!(first[2][1], 0.0);
    assert_eq!(first[3][1], 32_704.0);
    assert!((first[99][0] - 10.049_300).abs() < 1.0e-10);
    assert_eq!(second[99][1], -32_768.0);
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
fn plot_labels_and_cursor_use_adc_units() {
    let mut app = test_app();
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
    assert!(labels.iter().any(|text| text.contains("ADC 码值")));
    assert!(labels.iter().any(|text| text.contains("设备时间 (s)")));
    assert!(
        labels
            .iter()
            .any(|text| text.contains("通道1") && text.contains("ADC"))
    );
    assert!(
        !labels
            .iter()
            .any(|text| text.contains("mV") || text.contains("电压估算"))
    );
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
    assert!(
        normalized.height() < manual_bounds.height() / 10.0,
        "normalization must not retain the manual ADC scale"
    );

    click_normalization(&mut app, &controls);
    assert!(!app.overlay_normalized);
    let raw = render(&mut app, &context, vec![])[0];
    assert_eq!(app.operator.plot_view, PlotViewMode::ManualView);
    assert_eq!(app.plot_range(), manual_range);
    assert!(
        raw.min()[1] <= 0.0 && raw.max()[1] >= 100.0,
        "turning normalization off must fit the original signed ADC values"
    );
}
