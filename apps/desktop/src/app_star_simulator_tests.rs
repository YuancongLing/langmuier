use eframe::egui::{
    Context, Event, FullOutput, Modifiers, PointerButton, Pos2, RawInput, Rect, Shape, vec2,
};
use langmuir_core::GNSS_PRESETS;

use super::LangmuirDesktopApp;
use crate::{
    bridge::{
        BackendEndpoint, BackendEvent, BackendStateSnapshot, LinkKind, UiRequest, channel_bridge,
    },
    star_simulator::StarSimulatorStatus,
    ui::{fonts, theme},
};

fn app() -> (LangmuirDesktopApp, BackendEndpoint) {
    let (bridge, endpoint) = channel_bridge(32);
    let app = LangmuirDesktopApp::with_bridge(
        Box::new(bridge),
        None,
        #[cfg(feature = "mock")]
        false,
    );
    assert_eq!(
        endpoint.request_rx.try_recv().unwrap(),
        UiRequest::RefreshPorts
    );
    (app, endpoint)
}

fn render(
    app: &mut LangmuirDesktopApp,
    context: &Context,
    width: f32,
    events: Vec<Event>,
) -> FullOutput {
    context.run_ui(
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1400.0))),
            events,
            ..Default::default()
        },
        |ui| app.star_simulator_panel(ui),
    )
}

fn text_position(output: &FullOutput, label: &str) -> Pos2 {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.rect.center().to_vec2())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("{label} was not rendered"))
}

fn click(app: &mut LangmuirDesktopApp, context: &Context, label: &str) {
    let _ = render(app, context, 344.0, vec![]);
    let output = render(app, context, 344.0, vec![]);
    let pos = text_position(&output, label);
    for pressed in [true, false] {
        let _ = render(
            app,
            context,
            344.0,
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
    }
}

#[test]
fn star_simulator_requires_connection_and_an_explicit_start_click() {
    let (mut app, endpoint) = app();
    let context = Context::default();
    theme::configure(&context);
    click(&mut app, &context, "GNSS 广播");
    click(&mut app, &context, "启动自动广播");
    assert!(endpoint.request_rx.is_empty());
    app.handle_backend_event(BackendEvent::LinkChanged {
        link: LinkKind::Rs422,
        connected: true,
        detail: "controlled test adapter".to_owned(),
    });
    let _ = render(&mut app, &context, 344.0, vec![]);
    assert!(endpoint.request_rx.is_empty());
    click(&mut app, &context, "启动自动广播");
    let UiRequest::StartStarSimulator(config) = endpoint.request_rx.try_recv().unwrap() else {
        panic!("explicit start must request a simulator start")
    };
    assert_eq!(config.enabled, [false, false, true]);
    assert_eq!(config.interval_millis, [1000; 3]);
    assert!(endpoint.request_rx.is_empty());
}

#[test]
fn reliable_simulator_snapshot_repairs_status_and_disconnect_never_resumes_broadcasts() {
    let (mut app, endpoint) = app();
    let mut status = StarSimulatorStatus {
        active: true,
        sent: [7, 8, 9],
        ..Default::default()
    };
    status.config.enabled = [true; 3];
    status.config.point = GNSS_PRESETS[2];
    app.handle_backend_event(BackendEvent::StateSnapshot(Box::new(
        BackendStateSnapshot {
            rs422_connected: true,
            star_simulator: status,
            ..Default::default()
        },
    )));
    assert!(app.star_view.status.active);
    assert_eq!(app.star_view.status.sent, [7, 8, 9]);
    assert_eq!(app.star_view.config.point.label, "SYNTHETIC C · 南半球");
    app.handle_backend_event(BackendEvent::LinkChanged {
        link: LinkKind::Rs422,
        connected: false,
        detail: "test unplug".to_owned(),
    });
    assert!(!app.star_view.status.active);
    app.handle_backend_event(BackendEvent::LinkChanged {
        link: LinkKind::Rs422,
        connected: true,
        detail: "test reconnect".to_owned(),
    });
    assert!(!app.star_view.status.active);
    assert!(endpoint.request_rx.is_empty());
}

#[test]
fn periodic_backend_snapshot_preserves_unstarted_local_edits() {
    let (mut app, _) = app();
    app.star_view.config.enabled[0] = true;
    app.star_view.config.interval_millis[0] = 2500;
    app.star_view.config.point = GNSS_PRESETS[1];
    app.handle_backend_event(BackendEvent::StateSnapshot(Box::default()));
    assert!(app.star_view.config.enabled[0]);
    assert_eq!(app.star_view.config.interval_millis[0], 2500);
    assert_eq!(
        app.star_view.config.point.latitude_deg,
        GNSS_PRESETS[1].latitude_deg
    );
}

#[test]
fn disabling_the_last_running_broadcast_requests_stop_instead_of_leaving_old_frames_enabled() {
    let (mut app, endpoint) = app();
    app.rs422.connected = true;
    let mut status = StarSimulatorStatus {
        active: true,
        ..Default::default()
    };
    status.config.enabled = [false, false, true];
    app.handle_backend_event(BackendEvent::StarSimulatorStatus(Box::new(status)));
    let context = Context::default();
    theme::configure(&context);
    click(&mut app, &context, "GNSS 广播");
    assert_eq!(
        endpoint.request_rx.try_recv().unwrap(),
        UiRequest::StopStarSimulator
    );
    assert!(endpoint.request_rx.is_empty());
}

#[cfg(feature = "mock")]
#[test]
fn mock_connection_cannot_enable_real_broadcast_buttons() {
    let (bridge, endpoint) = channel_bridge(32);
    let mut app = LangmuirDesktopApp::with_bridge(Box::new(bridge), None, true);
    assert_eq!(
        endpoint.request_rx.try_recv().unwrap(),
        UiRequest::RefreshPorts
    );
    app.rs422.connected = true;
    app.star_view.config.enabled = [true; 3];
    let context = Context::default();
    theme::configure(&context);
    for label in ["时间单发", "姿态单发", "GNSS 单发", "启动自动广播"] {
        click(&mut app, &context, label);
    }
    assert!(endpoint.request_rx.is_empty());
}

#[test]
fn star_simulator_cards_fit_existing_sidebar_widths_with_large_counters_and_errors() {
    for width in [244.0, 284.0, 344.0, 392.0] {
        let (mut app, _) = app();
        app.star_view.status.sent = [u64::MAX; 3];
        app.star_view.status.last_error=Some("一个非常长的受控串口错误，串口断开之后不能继续广播。a-long-error-that-must-wrap-within-the-sidebar".to_owned());
        let context = Context::default();
        let _ = fonts::install_cjk_fallback(&context);
        theme::configure(&context);
        for _ in 0..2 {
            let mut actual = None;
            let _ = context.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1800.0))),
                    ..Default::default()
                },
                |ui| {
                    let assigned = ui.available_rect_before_wrap();
                    app.star_simulator_panel(ui);
                    actual = Some((assigned, ui.min_rect()));
                },
            );
            let (assigned, actual) = actual.unwrap();
            assert!(
                actual.right() <= assigned.right() + 1.0,
                "width {width}: {actual:?} exceeds {assigned:?}"
            );
        }
    }
}
