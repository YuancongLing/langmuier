use super::*;
use eframe::egui::{Context, Event, Modifiers, MouseWheelUnit, Pos2, RawInput, Rect, Vec2};

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
            0.0004,
            vec![32_768.0; 125],
            vec![33_408.0; 125],
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
        0.0004,
        vec![32_768.0; 125],
        vec![32_768.0; 125],
    );
    render(&mut app, &context, vec![]);
    assert_eq!(app.plot_range(), manual);
    app.follow_latest();
    render(&mut app, &context, vec![]);
    assert_eq!(app.operator.plot_view, PlotViewMode::FollowLatest);
    assert!(app.plot_range()[1] > initial[1]);
}

#[test]
fn plot_plain_wheel_zooms_and_hover_updates_with_integrity_notice() {
    let mut app = test_app();
    app.handle_backend_event(BackendEvent::ScientificSamples {
        channel_1_start_seconds: 140.0,
        channel_2_start_seconds: 140.0,
        sample_interval_seconds: 0.0004,
        channel_1: vec![32_768.0; 125],
        channel_2: vec![32_832.0; 125],
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
        snapshot_count, 100_000,
        "freeze retains raw history for later zooming"
    );
    app.handle_scientific_samples(
        150.0,
        150.0,
        0.0004,
        vec![32_768.0; 125],
        vec![32_768.0; 125],
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
        0.0004,
        vec![32_768.0; 125],
        vec![32_768.0; 125],
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
        "normalization must not retain the manual mV scale"
    );

    click_normalization(&mut app, &controls);
    assert!(!app.overlay_normalized);
    let raw = render(&mut app, &context, vec![])[0];
    assert_eq!(app.operator.plot_view, PlotViewMode::ManualView);
    assert_eq!(app.plot_range(), manual_range);
    assert!(
        raw.min()[1] <= 0.0 && raw.max()[1] >= 100.0,
        "turning normalization off must fit the original mV values"
    );
}
