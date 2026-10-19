use eframe::egui::{
    self, Context, Event, FullOutput, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, Shape,
    pos2, vec2,
};

use super::{
    CommandFamily, ConfigurationMode, IntegerParameterEdit, LangmuirDesktopApp, command_checksum,
    frequency_parameter, full_width_integer_slider, integer_parameter_input, integer_parameter_u16,
};
use crate::{
    bridge::{BackendEndpoint, UiRequest, channel_bridge},
    ui::theme,
};

fn app() -> (LangmuirDesktopApp, BackendEndpoint) {
    let (bridge, endpoint) = channel_bridge(32);
    let mut app = LangmuirDesktopApp::with_bridge(
        Box::new(bridge),
        None,
        #[cfg(feature = "mock")]
        false,
    );
    assert_eq!(
        endpoint.request_rx.try_recv().unwrap(),
        UiRequest::RefreshPorts
    );
    app.command_family = CommandFamily::Configuration;
    (app, endpoint)
}

fn input(events: Vec<Event>) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(420.0, 900.0))),
        events,
        ..RawInput::default()
    }
}

fn pointer(position: Pos2, pressed: bool) -> Vec<Event> {
    vec![
        Event::PointerMoved(position),
        Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        },
    ]
}

fn key(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn render_command(
    app: &mut LangmuirDesktopApp,
    context: &Context,
    events: Vec<Event>,
) -> FullOutput {
    context.run_ui(input(events), |ui| app.command_panel(ui))
}

fn text_position(output: &FullOutput, text: &str) -> Pos2 {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            Shape::Text(shape) if shape.galley.text() == text => {
                Some(shape.pos + shape.galley.rect.center().to_vec2())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("control {text} was not rendered"))
}

fn click_command(app: &mut LangmuirDesktopApp, context: &Context, text: &str) {
    let _ = render_command(app, context, vec![]);
    let output = render_command(app, context, vec![]);
    let position = text_position(&output, text);
    for pressed in [true, false] {
        let _ = render_command(app, context, pointer(position, pressed));
    }
}

fn render_input(
    context: &Context,
    value: &mut u16,
    edit: &mut IntegerParameterEdit,
    events: Vec<Event>,
) -> egui::Response {
    let mut response = None;
    let _ = context.run_ui(input(events), |ui| {
        ui.horizontal(|ui| {
            response = Some(integer_parameter_input(
                ui,
                value,
                edit,
                100,
                "test.frequency",
            ));
        });
    });
    response.unwrap()
}

#[test]
fn parameter_exact_text_keeps_draft_until_enter_and_commits_without_rounding() {
    let context = Context::default();
    let mut value = 10;
    let mut edit = IntegerParameterEdit::new(value);
    let response = render_input(&context, &mut value, &mut edit, vec![]);
    for pressed in [true, false] {
        let _ = render_input(
            &context,
            &mut value,
            &mut edit,
            pointer(response.rect.center(), pressed),
        );
    }
    let command = Modifiers {
        command: true,
        ctrl: true,
        ..Modifiers::NONE
    };
    let _ = render_input(
        &context,
        &mut value,
        &mut edit,
        vec![key(Key::A, command), Event::Text("37".into())],
    );
    assert_eq!(value, 10);
    assert_eq!(edit.text, "37");
    assert!(edit.ensure_confirmed(value, 100).is_err());
    for _ in 0..5 {
        let _ = render_input(&context, &mut value, &mut edit, vec![]);
        assert_eq!(edit.text, "37");
        assert_eq!(value, 10);
    }
    let _ = render_input(
        &context,
        &mut value,
        &mut edit,
        vec![key(Key::Enter, Modifiers::NONE)],
    );
    assert_eq!(value, 37);
    assert_eq!(edit.ensure_confirmed(value, 100), Ok(()));

    let response = render_input(&context, &mut value, &mut edit, vec![]);
    for pressed in [true, false] {
        let _ = render_input(
            &context,
            &mut value,
            &mut edit,
            pointer(response.rect.center(), pressed),
        );
    }
    let _ = render_input(
        &context,
        &mut value,
        &mut edit,
        vec![key(Key::A, command), Event::Text("99".into())],
    );
    assert_eq!(value, 37);
    for pressed in [true, false] {
        let _ = render_input(
            &context,
            &mut value,
            &mut edit,
            pointer(pos2(350.0, 500.0), pressed),
        );
    }
    assert_eq!(value, 99);
    assert_eq!(edit.ensure_confirmed(value, 100), Ok(()));
}

#[test]
fn parameter_invalid_or_pending_frequency_cannot_generate_or_send_a_command() {
    let (mut app, endpoint) = app();
    let context = Context::default();
    theme::configure(&context);
    app.configuration_mode = ConfigurationMode::Sine;
    for invalid in ["", "37.5", "-1", "+37", "101", "999999999", "abc", "37 "] {
        app.frequency_edit.text = invalid.into();
        app.frequency_edit.dirty = true;
        assert!(app.frequency_edit.confirm(100).is_none(), "{invalid}");
        assert!(app.frequency_edit.error.is_some(), "{invalid}");
        assert_eq!(app.frequency, 10);
        assert!(app.typed_command_bytes().is_err(), "{invalid}");
        click_command(&mut app, &context, "发送指令");
        assert!(endpoint.request_rx.try_recv().is_err(), "{invalid}");
    }
    app.frequency_edit.text = "73".into();
    app.frequency_edit.dirty = true;
    assert!(app.typed_command_bytes().is_err());
    click_command(&mut app, &context, "发送指令");
    assert!(endpoint.request_rx.try_recv().is_err());
}

#[test]
fn parameter_triangle_and_sine_exact_values_survive_idle_slider_frames_and_match_bytes() {
    let (mut app, _endpoint) = app();
    let context = Context::default();
    theme::configure(&context);
    for mode in [ConfigurationMode::Triangle, ConfigurationMode::Sine] {
        app.configuration_mode = mode;
        for frequency in [1, 20, 37, 50, 73, 99, 100] {
            app.frequency_edit.text = frequency.to_string();
            app.frequency_edit.dirty = true;
            app.frequency = u8::try_from(app.frequency_edit.confirm(100).unwrap()).unwrap();
            for _ in 0..4 {
                let _ = render_command(&mut app, &context, vec![]);
                assert_eq!(app.frequency, frequency, "{mode:?}");
                assert_eq!(app.frequency_edit.text, frequency.to_string());
            }
            let frame = app.typed_command_bytes().unwrap();
            assert_eq!(frame[6], 0x21);
            assert_eq!(
                frame[7],
                if mode == ConfigurationMode::Triangle {
                    0x55
                } else {
                    0xAA
                }
            );
            assert_eq!(frame[8], frequency);
            assert_eq!(frame[11], command_checksum(&frame));
        }
    }
}

#[test]
fn parameter_sine_37_preview_checksum_and_clicked_send_match_golden_bytes() {
    let (mut app, endpoint) = app();
    let context = Context::default();
    theme::configure(&context);
    app.configuration_mode = ConfigurationMode::Sine;
    app.frequency = 37;
    app.frequency_edit.set_confirmed(37);
    let expected = [
        0xEB, 0x90, 0xF0, 0x55, 0x00, 0x06, 0x21, 0xAA, 0x25, 0x9B, 0x9B, 0x26,
    ];
    let output = render_command(&mut app, &context, vec![]);
    let _ = text_position(&output, "EB 90 F0 55 00 06 21 AA 25 9B 9B 26");
    assert_eq!(app.typed_command_bytes().unwrap(), expected);
    click_command(&mut app, &context, "发送指令");
    assert_eq!(
        endpoint.request_rx.try_recv().unwrap(),
        UiRequest::SendCommand(expected)
    );
    assert!(endpoint.request_rx.try_recv().is_err());
}

#[test]
fn parameter_frequency_presets_change_the_value_only_when_clicked() {
    let (mut app, _endpoint) = app();
    let context = Context::default();
    theme::configure(&context);
    app.frequency = 37;
    app.frequency_edit.set_confirmed(37);
    for preset in [1, 50, 100] {
        let _ = render_command(&mut app, &context, vec![]);
        assert_eq!(app.frequency_edit.text, app.frequency.to_string());
        click_command(&mut app, &context, &preset.to_string());
        assert_eq!(app.frequency, preset);
        assert_eq!(app.frequency_edit.text, preset.to_string());
        assert_eq!(app.typed_command_bytes().unwrap()[8], preset);
    }
}

#[test]
fn parameter_frequency_drag_selects_37_and_does_not_snap_to_a_tens_preset() {
    let context = Context::default();
    theme::configure(&context);
    let mut value = 10_u8;
    let mut rect = Rect::NOTHING;
    let mut render = |events| {
        let _ = context.run_ui(input(events), |ui| {
            ui.set_width(284.0);
            rect = full_width_integer_slider(ui, &mut value, 0..=100, "test.frequency_slider").rect;
        });
        (rect, value)
    };
    let (rect, _) = render(vec![]);
    let radius = rect.height() / 2.5;
    let at = |fraction: f32| {
        pos2(
            rect.left() + radius + (rect.width() - 2.0 * radius) * fraction,
            rect.center().y,
        )
    };
    let _ = render(pointer(at(0.10), true));
    let _ = render(vec![Event::PointerMoved(at(0.37))]);
    let (_, value) = render(pointer(at(0.37), false));
    assert_eq!(value, 37);
    for _ in 0..5 {
        assert_eq!(render(vec![]).1, 37);
    }
}

#[test]
fn parameter_dc_rail_has_parent_width_and_visible_track_at_all_control_widths() {
    for width in [180.0, 224.0, 284.0, 344.0] {
        let context = Context::default();
        theme::configure(&context);
        let mut value = 5_001_u16;
        let mut rect = Rect::NOTHING;
        let output = context.run_ui(input(vec![]), |ui| {
            ui.set_width(width);
            rect = full_width_integer_slider(ui, &mut value, 0..=u16::MAX, "test.dc_slider").rect;
        });
        assert!((rect.width() - width).abs() < 1.0, "{width}: {rect:?}");
        assert_eq!(value, 5_001);
        let rail = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                Shape::Rect(shape)
                    if shape.fill == theme::SLIDER_TRACK
                        && (shape.rect.height() - 6.0).abs() < 0.1 =>
                {
                    Some(shape.rect)
                }
                _ => None,
            })
            .expect("DC slider must paint a visible horizontal rail");
        assert!((rail.width() - width).abs() < 1.0, "{width}: {rail:?}");
        assert!(rail.width() > rail.height() * 20.0);
    }
}

#[test]
fn parameter_dc_exact_value_is_not_rounded_and_keeps_big_endian_encoding() {
    let (mut app, _endpoint) = app();
    let context = Context::default();
    theme::configure(&context);
    app.configuration_mode = ConfigurationMode::Dc;
    for value in [0, 1, 5_001, 32_769, u16::MAX] {
        app.voltage_edit.text = value.to_string();
        app.voltage_edit.dirty = true;
        app.voltage = app.voltage_edit.confirm(u16::MAX).unwrap();
        for _ in 0..4 {
            let _ = render_command(&mut app, &context, vec![]);
            assert_eq!(app.voltage, value);
        }
        let frame = app.typed_command_bytes().unwrap();
        assert_eq!(frame[7], 0xBB);
        assert_eq!(&frame[8..10], &value.to_be_bytes());
        assert_eq!(frame[11], command_checksum(&frame));
    }
}

#[test]
fn parameter_dc_and_frequency_rows_stay_inside_the_parent_card() {
    for width in [224.0, 284.0, 344.0] {
        for dc in [false, true] {
            let context = Context::default();
            theme::configure(&context);
            let mut edit = IntegerParameterEdit::new(if dc { 5_000 } else { 37 });
            let _ = context.run_ui(input(vec![]), |ui| {
                ui.allocate_ui_with_layout(
                    vec2(width, 280.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let right = ui.available_rect_before_wrap().right();
                        if dc {
                            integer_parameter_u16(ui, &mut 5_000, &mut edit, &[1, 10, 100]);
                        } else {
                            frequency_parameter(ui, &mut 37, &mut edit);
                        }
                        assert!(
                            ui.min_rect().right() <= right + 1.0,
                            "width {width}, dc {dc}: {:?}",
                            ui.min_rect()
                        );
                    },
                );
            });
        }
    }
}
