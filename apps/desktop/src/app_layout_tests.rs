//! Widget geometry at the requested physical viewport and UI scale combinations.
//! These run actual themed controls; they do not certify native window appearance.
use eframe::egui::{
    self, Context, FontFamily, FullOutput, Pos2, RawInput, Rect, Shape, Vec2, pos2, vec2,
};

use super::{
    COMMAND_SIDEBAR_WIDTH, COMPACT_COMMAND_SIDEBAR_WIDTH, COMPACT_LAYOUT_BREAKPOINT,
    COMPACT_TELEMETRY_SIDEBAR_WIDTH, HEADER_COMPACT_BREAKPOINT, HEADER_COMPACT_HEIGHT,
    HEADER_HEIGHT, IntegerParameterEdit, LangmuirDesktopApp, PlotLayout, TELEMETRY_SIDEBAR_WIDTH,
    central_layout_rects, fonts, frequency_parameter, full_width_card, full_width_subtle_card,
    integer_parameter_u16, theme,
};
use crate::bridge::{BackendEndpoint, LvdsStatusSnapshot, channel_bridge};

const VIEWPORTS: [(Vec2, f32); 3] = [
    (vec2(1_360.0, 860.0), 1.0),
    (vec2(1_600.0, 900.0), 1.0),
    (vec2(1_280.0, 720.0), 1.25),
];

fn test_app() -> (LangmuirDesktopApp, BackendEndpoint) {
    let (bridge, endpoint) = channel_bridge(32);
    let app = LangmuirDesktopApp::with_bridge(
        Box::new(bridge),
        None,
        #[cfg(feature = "mock")]
        false,
    );
    (app, endpoint)
}

fn context_at_scale(scale: f32) -> Context {
    let context = Context::default();
    let _ = fonts::install_cjk_fallback(&context);
    theme::configure(&context);
    context.set_zoom_factor(scale);
    // Apply the zoom transition before the measured pass. egui rescales the
    // previous viewport rectangle on that one transition frame.
    let _ = context.run_ui(RawInput::default(), |_| {});
    context
}

fn input_for(logical_size: Vec2) -> RawInput {
    let mut input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, logical_size)),
        ..RawInput::default()
    };
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .expect("root viewport")
        .native_pixels_per_point = Some(1.0);
    input
}

fn central_root(logical_size: Vec2) -> Rect {
    let compact = logical_size.x < COMPACT_LAYOUT_BREAKPOINT;
    let sidebars = if compact {
        COMPACT_COMMAND_SIDEBAR_WIDTH + COMPACT_TELEMETRY_SIDEBAR_WIDTH
    } else {
        COMMAND_SIDEBAR_WIDTH + TELEMETRY_SIDEBAR_WIDTH
    };
    let header = if logical_size.x < HEADER_COMPACT_BREAKPOINT {
        HEADER_COMPACT_HEIGHT
    } else {
        HEADER_HEIGHT
    };
    let left = if compact {
        COMPACT_COMMAND_SIDEBAR_WIDTH
    } else {
        COMMAND_SIDEBAR_WIDTH
    };
    Rect::from_min_size(
        pos2(left + 8.0, header + 8.0),
        vec2(
            logical_size.x - sidebars - 16.0,
            logical_size.y - header - 16.0,
        ),
    )
}

fn assert_inside(name: &str, actual: Rect, assigned: Rect, physical: Vec2, scale: f32) {
    assert!(
        rect_fits(actual, assigned),
        "{name} overflows at {physical:?}, scale {scale}: assigned {assigned:?}, widgets {actual:?}"
    );
}

fn rect_fits(actual: Rect, assigned: Rect) -> bool {
    let tolerance = 1.0;
    actual.min.x >= assigned.min.x - tolerance
        && actual.min.y >= assigned.min.y - tolerance
        && actual.max.x <= assigned.max.x + tolerance
        && actual.max.y <= assigned.max.y + tolerance
}

/// Measurements come from the actual painted glyph mesh and glyph baselines,
/// rather than assuming that equally sized widget rectangles align their text.
#[derive(Debug, Clone)]
struct PaintedText {
    text: String,
    visible: Rect,
    baseline: f32,
    monospace: bool,
    clip: Rect,
    elided: bool,
}

fn painted_text(output: &FullOutput) -> Vec<PaintedText> {
    fn collect(shape: &Shape, clip: Rect, texts: &mut Vec<PaintedText>) {
        match shape {
            Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, clip, texts);
                }
            }
            Shape::Text(text) => {
                if let Some((row, glyph)) = text.galley.rows.iter().find_map(|row| {
                    row.glyphs
                        .iter()
                        .find(|glyph| !glyph.chr.is_whitespace())
                        .map(|glyph| (row, glyph))
                }) {
                    texts.push(PaintedText {
                        text: text.galley.text().to_owned(),
                        visible: text.visual_bounding_rect(),
                        baseline: text.pos.y + row.pos.y + glyph.pos.y,
                        monospace: text
                            .galley
                            .job
                            .sections
                            .iter()
                            .all(|section| section.format.font_id.family == FontFamily::Monospace),
                        clip,
                        elided: text.galley.elided,
                    });
                }
            }
            _ => {}
        }
    }

    let mut texts = Vec::new();
    for shape in &output.shapes {
        collect(&shape.shape, shape.clip_rect, &mut texts);
    }
    texts
}

fn text_named<'a>(texts: &'a [PaintedText], name: &str) -> &'a PaintedText {
    texts
        .iter()
        .find(|text| text.text == name)
        .unwrap_or_else(|| panic!("painted text {name:?} missing; got {texts:?}"))
}

fn painted_control_rect(output: &FullOutput, text: &PaintedText) -> Rect {
    fn collect(shape: &Shape, text: &PaintedText, rects: &mut Vec<Rect>) {
        match shape {
            Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, text, rects);
                }
            }
            Shape::Rect(rect)
                if rect.rect.contains(text.visible.center())
                    && rect.rect.height() >= 20.0
                    && rect.rect.height() <= 40.0 =>
            {
                rects.push(rect.rect);
            }
            _ => {}
        }
    }
    let mut rects = Vec::new();
    for shape in &output.shapes {
        collect(&shape.shape, text, &mut rects);
    }
    rects
        .into_iter()
        .min_by(|one, two| one.area().total_cmp(&two.area()))
        .unwrap_or_else(|| panic!("painted control background missing for {text:?}"))
}

fn render_central(
    app: &mut LangmuirDesktopApp,
    context: &Context,
    logical: Vec2,
) -> (FullOutput, [Rect; 4]) {
    render_central_with_input(app, context, logical, input_for(logical))
}

fn render_central_with_input(
    app: &mut LangmuirDesktopApp,
    context: &Context,
    logical: Vec2,
    input: RawInput,
) -> (FullOutput, [Rect; 4]) {
    let assigned = central_layout_rects(central_root(logical));
    let mut measured = [Rect::NOTHING; 4];
    let output = context.run_ui(input, |ui| {
        ui.scope_builder(egui::UiBuilder::new().max_rect(assigned.operations), |ui| {
            app.operations_panel(ui);
            measured[0] = ui.min_rect();
        });
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(assigned.plot_toolbar),
            |ui| {
                full_width_subtle_card(ui, |ui| app.plot_toolbar(ui));
                measured[1] = ui.min_rect();
            },
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(assigned.plot_body), |ui| {
            full_width_card(ui, |ui| app.plot_panel(ui));
            measured[2] = ui.min_rect();
        });
        ui.scope_builder(egui::UiBuilder::new().max_rect(assigned.logs), |ui| {
            full_width_card(ui, |ui| app.log_panel(ui));
            measured[3] = ui.min_rect();
        });
    });
    (output, measured)
}

fn controls_named<'a>(texts: &'a [PaintedText], names: &[&str]) -> &'a PaintedText {
    texts
        .iter()
        .find(|text| names.contains(&text.text.as_str()))
        .unwrap_or_else(|| panic!("none of the controls {names:?} were painted: {texts:?}"))
}

fn assert_same_baseline(texts: &[&PaintedText], tolerance: f32) {
    let baseline = texts[0].baseline;
    for text in texts {
        assert!(
            (text.baseline - baseline).abs() <= tolerance,
            "painted glyph baselines differ by more than {tolerance}pt: {texts:?}"
        );
    }
}

fn assert_central_bounds(measured: [Rect; 4], logical: Vec2, physical: Vec2, scale: f32) {
    let assigned = central_layout_rects(central_root(logical));
    for ((name, actual), region) in ["operations", "toolbar", "plot", "logs"]
        .into_iter()
        .zip(measured)
        .zip([
            assigned.operations,
            assigned.plot_toolbar,
            assigned.plot_body,
            assigned.logs,
        ])
    {
        assert_inside(name, actual, region, physical, scale);
    }
    assert_eq!(assigned.logs.bottom(), assigned.root.bottom());
    assert!(assigned.plot_body.bottom() <= assigned.logs.top());
}

#[test]
fn central_controls_use_uniform_heights_and_painted_text_alignment() {
    for (physical, scale) in VIEWPORTS {
        let logical = physical / scale;
        let context = context_at_scale(scale);
        let (mut app, _endpoint) = test_app();
        let address = app.lvds.bind_address.clone();
        let port = app.lvds.port.to_string();
        let _ = render_central(&mut app, &context, logical);
        let (output, measured) = render_central(&mut app, &context, logical);
        assert_central_bounds(measured, logical, physical, scale);
        let texts = painted_text(&output);
        let address = text_named(&texts, &address);
        let port = text_named(&texts, &port);
        let listen = controls_named(&texts, &["开始监听", "监听"]);
        let stop = controls_named(&texts, &["停止监听", "停止"]);
        let replay = controls_named(&texts, &["离线回放 PCAP", "PCAP 回放", "回放"]);
        let save = controls_named(&texts, &["开始保存", "保存"]);
        for text in [address, port, listen, stop, replay, save] {
            let rect = painted_control_rect(&output, text);
            assert!(
                (rect.height() - theme::CONTROL_HEIGHT).abs() <= 0.1,
                "{} has a different painted control height at {physical:?}/{scale}: {rect:?}",
                text.text
            );
            assert!(
                (text.visible.center().y - rect.center().y).abs() <= 4.5,
                "painted text {} is not visibly centered in {rect:?}: {:?}",
                text.text,
                text.visible
            );
        }
        assert!(!address.elided, "the full configured IPv4 must be visible");
        assert!(rect_fits(address.visible, address.clip));
        assert_same_baseline(&[listen, stop], 0.5);
        // The TextEdit and DragValue share their font. At fractional UI scale,
        // the two galleys may independently round their origins to pixels.
        assert_same_baseline(&[address, port], 1.0);
        assert!(
            (address.visible.center().y - port.visible.center().y).abs() <= 2.0,
            "different numeric glyphs must remain visually aligned: {address:?}, {port:?}"
        );
        if (listen.visible.center().y - address.visible.center().y).abs() < 16.0 {
            assert!(
                (listen.visible.center().y - address.visible.center().y).abs() <= 3.5,
                "same-row IPv4 and listen text have different visible centers"
            );
        }
        let toolbar_actions =
            ["跟随最新", "重置视图", "冻结画面", "清空"].map(|label| text_named(&texts, label));
        assert_same_baseline(&toolbar_actions, 0.5);
        for text in toolbar_actions {
            assert!(
                (painted_control_rect(&output, text).height() - theme::CONTROL_HEIGHT).abs() <= 0.1
            );
        }
        let channel_one = controls_named(&texts, &["通道 1", "通1"]);
        let channel_two = controls_named(&texts, &["通道 2", "通2"]);
        let display_mode = controls_named(&texts, &["叠加", "显示模式"]);
        assert_same_baseline(&[channel_one, channel_two, display_mode], 0.5);
        assert!(toolbar_actions[0].visible.top() > channel_one.visible.bottom());
        eprintln!(
            "central glyph geometry {physical:?}/{scale}: IP baseline {:.2}, listen baseline {:.2}, save baseline {:.2}; control height {}",
            address.baseline,
            listen.baseline,
            save.baseline,
            painted_control_rect(&output, save).height()
        );
    }
}

fn statistic_texts(texts: &[PaintedText], value: u64) -> [Vec<&PaintedText>; 2] {
    let labels: Vec<_> = ["接收", "结构合法", "CRC正确", "CRC错误", "绘图"]
        .into_iter()
        .map(|label| text_named(texts, label))
        .collect();
    let baseline = labels[0].baseline;
    let value = value.to_string();
    let mut numbers: Vec<_> = texts
        .iter()
        .filter(|text| {
            text.text == value && text.baseline > baseline && text.baseline < baseline + 30.0
        })
        .collect();
    numbers.sort_by(|one, two| one.visible.center().x.total_cmp(&two.visible.center().x));
    [labels, numbers]
}

#[test]
fn quality_grid_is_fixed_across_digit_counts_long_errors_and_saving_states() {
    for (physical, scale) in VIEWPORTS {
        let logical = physical / scale;
        let context = context_at_scale(scale);
        let (mut app, _endpoint) = test_app();
        let mut previous_geometry = None;
        for (index, count) in [0, 9, 1_057, 9_999, 10_000, u64::MAX]
            .into_iter()
            .enumerate()
        {
            app.acquisition_running = index % 2 == 1;
            app.diagnostic.plotted_frames = count;
            let mut status = LvdsStatusSnapshot {
                source: "stable layout source".to_owned(),
                receive_state: "UDP 有效科学帧 · 很长的接收状态不应挤出控件".to_owned(),
                ..LvdsStatusSnapshot::default()
            };
            status.stats.udp_packets = count;
            status.stats.structure_valid_frames = count;
            status.stats.crc_passed = count;
            status.stats.crc_failed = count;
            status.storage.enqueued = count;
            status.storage.sink_written = count;
            status.storage.closed = !app.acquisition_running;
            if index >= 2 {
                app.lvds.detail = "UDP bind 192.168.1.100:1534: Can't assign requested address (os error 49); long original socket error must remain available as detail".to_owned();
                status.receive_state.clone_from(&app.lvds.detail);
                status.storage.error = Some(
                    "一个非常长的科学文件关闭错误，入队成功不等于sink写入或成功关闭".to_owned(),
                );
            }
            app.diagnostic.status = Some(status);
            let _ = render_central(&mut app, &context, logical);
            let (output, measured) = render_central(&mut app, &context, logical);
            assert_central_bounds(measured, logical, physical, scale);
            let texts = painted_text(&output);
            if index >= 2 {
                let error = text_named(&texts, "监听失败：本机未配置该 IP 地址");
                let listen = controls_named(&texts, &["开始监听", "监听"]);
                assert!(error.visible.top() > listen.visible.bottom());
                assert!(!app.lvds.connected);
            }
            let [labels, numbers] = statistic_texts(&texts, count);
            assert_eq!(
                numbers.len(),
                5,
                "five independent numeric cells: {texts:?}"
            );
            assert_same_baseline(&labels, 0.5);
            assert_same_baseline(&numbers, 0.5);
            let spacing = labels[1].visible.center().x - labels[0].visible.center().x;
            for pair in labels.windows(2) {
                assert!(
                    ((pair[1].visible.center().x - pair[0].visible.center().x) - spacing).abs()
                        <= 1.1,
                    "statistic columns must have equal widths: {labels:?}"
                );
            }
            for (label, number) in labels.iter().zip(&numbers) {
                assert!(
                    number.monospace,
                    "statistic digits must be tabular/monospace"
                );
                assert!(
                    (label.visible.center().x - number.visible.center().x).abs() <= 1.1,
                    "label and numeric cell centers differ: {label:?}, {number:?}"
                );
            }
            let geometry = (
                measured,
                labels
                    .iter()
                    .map(|text| text.visible.center())
                    .collect::<Vec<_>>(),
                numbers.iter().map(|text| text.baseline).collect::<Vec<_>>(),
            );
            if let Some(previous) = &previous_geometry {
                assert_eq!(
                    &geometry, previous,
                    "digit count/error/save state moved layout"
                );
            }
            previous_geometry = Some(geometry);
        }
    }
}

fn click_central_text(app: &mut LangmuirDesktopApp, context: &Context, logical: Vec2, label: &str) {
    let (output, _) = render_central(app, context, logical);
    let texts = painted_text(&output);
    let position = text_named(&texts, label).visible.center();
    for pressed in [true, false] {
        let mut input = input_for(logical);
        input.events = vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let _ = render_central_with_input(app, context, logical, input);
    }
}

#[test]
fn save_buttons_wait_for_backend_state_and_bind_error_keeps_original_hover_detail() {
    use crate::bridge::{BackendEvent, UiRequest};

    for (physical, scale) in VIEWPORTS {
        let logical = physical / scale;
        let context = context_at_scale(scale);
        let (mut app, endpoint) = test_app();
        assert_eq!(
            endpoint.request_rx.try_recv().unwrap(),
            UiRequest::RefreshPorts
        );
        let original_address = app.lvds.bind_address.clone();
        let _ = render_central(&mut app, &context, logical);
        click_central_text(&mut app, &context, logical, "开始保存");
        assert_eq!(
            endpoint.request_rx.try_recv().unwrap(),
            UiRequest::StartAcquisition
        );
        assert!(
            !app.acquisition_running,
            "a click is not backend save confirmation"
        );
        app.handle_backend_event(BackendEvent::AcquisitionChanged { running: true });
        click_central_text(&mut app, &context, logical, "停止保存");
        assert_eq!(
            endpoint.request_rx.try_recv().unwrap(),
            UiRequest::StopAcquisition
        );
        assert!(
            app.acquisition_running,
            "wait for the actual shutdown result"
        );
        app.handle_backend_event(BackendEvent::AcquisitionChanged { running: false });

        let raw = "UDP bind 192.168.1.100:1534: Can't assign requested address (os error 49); original socket bind failure";
        app.lvds.detail = raw.to_owned();
        app.lvds.connected = false;
        app.diagnostic.status = Some(LvdsStatusSnapshot {
            receive_state: raw.to_owned(),
            ..LvdsStatusSnapshot::default()
        });
        let (output, measured) = render_central(&mut app, &context, logical);
        assert_central_bounds(measured, logical, physical, scale);
        let texts = painted_text(&output);
        let summary = text_named(&texts, "监听失败：本机未配置该 IP 地址");
        let position = summary.visible.center();
        assert!(!texts.iter().any(|text| text.text.contains(raw)));
        context.all_styles_mut(|style| {
            style.interaction.tooltip_delay = 0.0;
            style.interaction.show_tooltips_only_when_still = false;
        });
        let mut full_detail_visible = false;
        for frame in 0..4 {
            let mut input = input_for(logical);
            input.time = Some(1_000.0 + f64::from(frame) * 0.1);
            input.events = vec![egui::Event::PointerMoved(position)];
            let (output, _) = render_central_with_input(&mut app, &context, logical, input);
            full_detail_visible |= painted_text(&output)
                .iter()
                .any(|text| text.text.contains(raw));
        }
        assert!(
            full_detail_visible,
            "original bind error was lost from hover detail"
        );
        assert_eq!(app.lvds.bind_address, original_address);
        assert!(!app.lvds.connected);
        assert!(!app.acquisition_running);
        assert!(endpoint.request_rx.try_recv().is_err());
    }
}

#[test]
fn empty_state_is_one_centered_pair_and_does_not_move_quality_information() {
    for (physical, scale) in VIEWPORTS {
        let logical = physical / scale;
        let context = context_at_scale(scale);
        let (mut app, _endpoint) = test_app();
        let _ = render_central(&mut app, &context, logical);
        let (output, measured) = render_central(&mut app, &context, logical);
        assert_central_bounds(measured, logical, physical, scale);
        let texts = painted_text(&output);
        let title = text_named(&texts, "暂无 LVDS 数据");
        let description = text_named(&texts, "请开始 UDP 监听或离线 PCAP 回放");
        let info = controls_named(&texts, &["尚无可校验数据", "当前视图不含 CRC 未通过的样本"]);
        let assigned = central_layout_rects(central_root(logical));
        let plot_inner = assigned.plot_body.shrink(9.0);
        let group = title.visible.union(description.visible);
        assert!((title.visible.center().x - description.visible.center().x).abs() <= 1.0);
        assert!((group.center().x - plot_inner.center().x).abs() <= 1.0);
        // The quality/cursor rows stay above the empty group. The actual ink
        // can differ slightly from font line boxes, hence a 4pt visual tolerance.
        let empty_body_center = info.visible.bottom().midpoint(plot_inner.bottom());
        assert!((group.center().y - empty_body_center).abs() <= 14.0);
        assert!(info.visible.bottom() < group.top());
        assert!(rect_fits(group, plot_inner));
        assert!(!texts.iter().any(|text| text.text.contains("左键拖动平移")));
    }
}

#[test]
fn sixty_seconds_of_mixed_quality_updates_keep_central_docks_and_grid_stable() {
    use crate::bridge::BackendEvent;

    for (physical, scale) in VIEWPORTS {
        let logical = physical / scale;
        let context = context_at_scale(scale);
        let (mut app, _endpoint) = test_app();
        app.diagnostic.status = Some(LvdsStatusSnapshot::default());
        let mut baseline_geometry = None;
        // Sixty seconds of device timestamps, with 20 frames/s and 125 ADC
        // points/channel. This is a deterministic UI simulation, not wall time
        // or a native visual-performance claim.
        for frame in 0..=1_200_u32 {
            let start = 1_800_000_000.0 + f64::from(frame) * 0.05;
            if frame % 2 == 0 {
                app.handle_scientific_samples(
                    start,
                    start,
                    0.0004,
                    vec![32_768.0; 125],
                    vec![32_832.0; 125],
                );
            } else {
                app.handle_backend_event(BackendEvent::DiagnosticSamples {
                    sequence: Some(frame),
                    channel_1_start_seconds: start,
                    channel_2_start_seconds: start,
                    sample_interval_seconds: 0.0004,
                    channel_1: vec![32_768.0; 125],
                    channel_2: vec![32_832.0; 125],
                });
            }
            let status = app.diagnostic.status.as_mut().unwrap();
            let count = u64::from(frame) + 1;
            status.stats.udp_packets = count;
            status.stats.structure_valid_frames = count;
            status.stats.crc_passed = count.div_ceil(2);
            status.stats.crc_failed = count / 2;
            if frame % 20 != 0 {
                continue;
            }
            let _ = render_central(&mut app, &context, logical);
            let (output, measured) = render_central(&mut app, &context, logical);
            assert_central_bounds(measured, logical, physical, scale);
            let texts = painted_text(&output);
            let labels = ["接收", "结构合法", "CRC正确", "CRC错误", "绘图"]
                .map(|label| text_named(&texts, label).visible.center());
            let geometry = (measured, labels);
            if let Some(baseline) = &baseline_geometry {
                assert_eq!(&geometry, baseline, "layout jumped at device frame {frame}");
            }
            baseline_geometry = Some(geometry);
            assert!(
                app.measurements
                    .rendered
                    .into_iter()
                    .all(|count| count <= 2_000)
            );
        }
        assert_eq!(app.diagnostic.plotted_frames, 1_201);
        assert!(app.channel_1.len() <= super::PLOT_CAPACITY);
        assert!(app.channel_2.len() <= super::PLOT_CAPACITY);
        assert!(app.current_plot_contains_unverified());
    }
}

#[test]
fn themed_central_widgets_fit_requested_viewports_with_live_and_frozen_data() {
    let mut overflows = Vec::new();
    for (physical, scale) in VIEWPORTS {
        let logical = physical / scale;
        let context = context_at_scale(scale);
        let (mut app, _endpoint) = test_app();
        let mut status = LvdsStatusSnapshot {
            source: "layout test source".to_owned(),
            receive_state: "UDP 到达 · 等待完整科学帧 · 很长的状态说明用于验证截断".to_owned(),
            ..LvdsStatusSnapshot::default()
        };
        status.stats.udp_packets = u64::MAX;
        status.stats.structure_valid_frames = u64::MAX;
        status.stats.crc_failed = u64::MAX;
        status.storage.enqueued = u64::MAX;
        status.storage.sink_written = u64::MAX;
        status.storage.error = Some("存储错误，不能以绘图成功替代关闭结果".to_owned());
        app.diagnostic.status = Some(status);
        let assigned = central_layout_rects(central_root(logical));

        for data_state in 0..3 {
            if data_state == 1 {
                app.handle_scientific_samples(
                    1_800_000_000.0,
                    1_800_000_000.000_2,
                    0.0004,
                    vec![32_800.0; 125],
                    vec![32_900.0; 125],
                );
                app.acquisition_running = true;
            } else if data_state == 2 {
                app.toggle_display_freeze();
            }
            for layout in [PlotLayout::Overlay, PlotLayout::Split] {
                app.plot_layout = layout;
                // Two passes allow font/plot measurements to settle just as
                // they do before a native window's second frame.
                for _ in 0..2 {
                    let mut measured = [Rect::NOTHING; 4];
                    let output = context.run_ui(input_for(logical), |ui| {
                        ui.scope_builder(
                            egui::UiBuilder::new().max_rect(assigned.operations),
                            |ui| {
                                app.operations_panel(ui);
                                measured[0] = ui.min_rect();
                            },
                        );
                        ui.scope_builder(
                            egui::UiBuilder::new().max_rect(assigned.plot_toolbar),
                            |ui| {
                                full_width_subtle_card(ui, |ui| app.plot_toolbar(ui));
                                measured[1] = ui.min_rect();
                            },
                        );
                        ui.scope_builder(
                            egui::UiBuilder::new().max_rect(assigned.plot_body),
                            |ui| {
                                full_width_card(ui, |ui| app.plot_panel(ui));
                                measured[2] = ui.min_rect();
                            },
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(assigned.logs), |ui| {
                            full_width_card(ui, |ui| app.log_panel(ui));
                            measured[3] = ui.min_rect();
                        });
                    });
                    assert!(!output.shapes.is_empty());
                    assert!((context.pixels_per_point() - scale).abs() < 0.001);
                    for ((name, actual), region) in ["operations", "toolbar", "plot", "logs"]
                        .into_iter()
                        .zip(measured)
                        .zip([
                            assigned.operations,
                            assigned.plot_toolbar,
                            assigned.plot_body,
                            assigned.logs,
                        ])
                    {
                        if !rect_fits(actual, region) {
                            let error = format!(
                                "{name} at {physical:?}, scale {scale}, data_state {data_state}, {layout:?}: assigned {region:?}, widgets {actual:?}"
                            );
                            if !overflows.contains(&error) {
                                overflows.push(error);
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        overflows.is_empty(),
        "widget overflow:\n{}",
        overflows.join("\n")
    );
}

#[test]
fn themed_parameter_cards_and_horizontal_rails_fit_requested_sidebar_widths() {
    for (physical, scale) in VIEWPORTS {
        let logical = physical / scale;
        let context = context_at_scale(scale);
        let sidebar_width = if logical.x < COMPACT_LAYOUT_BREAKPOINT {
            COMPACT_COMMAND_SIDEBAR_WIDTH
        } else {
            COMMAND_SIDEBAR_WIDTH
        };
        // The native sidebar frame reserves 8 logical points on each side.
        let assigned = Rect::from_min_size(pos2(8.0, 8.0), vec2(sidebar_width - 16.0, 260.0));
        for dc in [false, true] {
            let mut frequency = 37;
            let mut voltage = u16::MAX;
            let mut edit =
                IntegerParameterEdit::new(if dc { voltage } else { u16::from(frequency) });
            for invalid in [false, true] {
                if invalid {
                    edit.text = "37.5".to_owned();
                    edit.dirty = true;
                    let _ = edit.confirm(if dc { u16::MAX } else { 100 });
                }
                let mut actual = Rect::NOTHING;
                let output = context.run_ui(input_for(logical), |ui| {
                    ui.scope_builder(egui::UiBuilder::new().max_rect(assigned), |ui| {
                        full_width_subtle_card(ui, |ui| {
                            if dc {
                                integer_parameter_u16(ui, &mut voltage, &mut edit, &[1, 10, 100]);
                            } else {
                                frequency_parameter(ui, &mut frequency, &mut edit);
                            }
                        });
                        actual = ui.min_rect();
                    });
                });
                assert_inside("parameter card", actual, assigned, physical, scale);
                let rail = output.shapes.iter().find_map(|shape| match &shape.shape {
                    Shape::Rect(rect)
                        if (rect.rect.height() - 6.0).abs() < 0.1
                            && rect.rect.width() > assigned.width() * 0.7 =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                });
                assert!(
                    rail.is_some(),
                    "full horizontal rail missing at {physical:?}, scale {scale}, DC={dc}"
                );
                assert_inside("slider rail", rail.unwrap(), assigned, physical, scale);
            }
        }
    }
}
