use std::collections::BTreeMap;

use eframe::egui::{
    self, Align, Checkbox, Color32, CornerRadius, FontFamily, FontId, Frame, Label, Layout, Margin,
    Response, RichText, Sense, Stroke, TextStyle, Ui, Vec2,
};

pub const CONTROL_HEIGHT: f32 = 32.0;
pub const CRITICAL_CONTROL_HEIGHT: f32 = 44.0;

pub const WHITE: Color32 = Color32::from_rgb(224, 224, 224);
pub const PAGE: Color32 = Color32::from_rgb(30, 30, 30);
pub const SURFACE: Color32 = Color32::from_rgb(37, 41, 48);
pub const SURFACE_ALT: Color32 = Color32::from_rgb(47, 53, 62);
pub const BORDER: Color32 = Color32::from_rgb(70, 78, 91);
pub const PRIMARY: Color32 = Color32::from_rgb(59, 130, 246);
pub const PRIMARY_DARK: Color32 = Color32::from_rgb(147, 197, 253);
pub const PRIMARY_SOFT: Color32 = Color32::from_rgb(30, 58, 95);
pub const SLIDER_TRACK: Color32 = Color32::from_rgb(82, 97, 114);
pub const SUCCESS: Color32 = Color32::from_rgb(46, 179, 112);
pub const SUCCESS_SOFT: Color32 = Color32::from_rgb(27, 54, 42);
pub const WARNING: Color32 = Color32::from_rgb(240, 180, 41);
pub const DANGER: Color32 = Color32::from_rgb(224, 82, 82);
pub const DANGER_SOFT: Color32 = Color32::from_rgb(67, 34, 39);
pub const OFFLINE: Color32 = Color32::from_rgb(120, 130, 145);
pub const TEXT: Color32 = WHITE;
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(174, 183, 196);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(125, 136, 152);

pub fn configure(context: &egui::Context) {
    context.all_styles_mut(|style| {
        style.text_styles = BTreeMap::from([
            (
                TextStyle::Heading,
                FontId::new(16.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(12.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(12.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(10.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(11.0, FontFamily::Monospace),
            ),
        ]);
        style.spacing.item_spacing = Vec2::new(6.0, 5.0);
        style.spacing.button_padding = Vec2::new(8.0, 5.0);
        style.spacing.interact_size.y = CONTROL_HEIGHT;
        style.spacing.combo_width = 150.0;
        style.override_text_valign = Some(Align::Center);
        style.visuals = visuals();
    });
}

fn visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = PAGE;
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = SURFACE;
    visuals.text_edit_bg_color = Some(SURFACE_ALT);
    visuals.faint_bg_color = SURFACE_ALT;
    visuals.code_bg_color = SURFACE_ALT;
    visuals.selection.bg_fill = PRIMARY;
    visuals.selection.stroke = Stroke::new(1.0, WHITE);
    visuals.hyperlink_color = PRIMARY;
    visuals.warn_fg_color = WARNING;
    visuals.error_fg_color = DANGER;
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.widgets.noninteractive.bg_fill = SURFACE;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);
    visuals.widgets.inactive.bg_fill = SURFACE_ALT;
    visuals.widgets.inactive.weak_bg_fill = SURFACE_ALT;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    visuals.widgets.hovered.bg_fill = PRIMARY_SOFT;
    visuals.widgets.hovered.weak_bg_fill = PRIMARY_SOFT;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, PRIMARY);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    visuals.widgets.active.bg_fill = PRIMARY;
    visuals.widgets.active.weak_bg_fill = PRIMARY;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, PRIMARY_DARK);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, WHITE);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);
    visuals.widgets.open.bg_fill = PRIMARY_SOFT;
    visuals.widgets.open.weak_bg_fill = PRIMARY_SOFT;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, PRIMARY);
    visuals.widgets.open.corner_radius = CornerRadius::same(6);
    visuals
}

pub fn card() -> Frame {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(6.0)
        .inner_margin(Margin::same(8))
}

pub fn subtle_card() -> Frame {
    Frame::new()
        .fill(SURFACE_ALT)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(6.0)
        .inner_margin(Margin::same(6))
}

pub fn section_title(ui: &mut Ui, title: &str) {
    ui.label(RichText::new(title).strong().color(PRIMARY_DARK).size(12.0));
}

pub fn status_badge(ui: &mut Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color)
        .corner_radius(6.0)
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).strong().color(WHITE).size(10.0));
        });
}

pub fn aligned_checkbox(ui: &mut Ui, checked: &mut bool, text: &str) -> Response {
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), CONTROL_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            let checkbox = ui.add(Checkbox::without_text(checked));
            let label = ui.add(Label::new(text).sense(Sense::click()));
            let label_clicked = label.clicked();
            let mut response = checkbox.labelled_by(label.id).union(label);
            if label_clicked {
                *checked = !*checked;
                response.mark_changed();
            }
            response
        },
    )
    .inner
}

#[cfg(test)]
mod tests {
    use super::{BORDER, CONTROL_HEIGHT, PAGE, PRIMARY, TEXT, visuals};

    #[test]
    fn dark_theme_uses_the_industrial_palette_contract() {
        let visuals = visuals();
        assert!(visuals.dark_mode);
        assert_eq!(visuals.panel_fill, PAGE);
        assert_eq!(visuals.override_text_color, Some(TEXT));
        assert_eq!(visuals.selection.bg_fill, PRIMARY);
        assert_eq!(visuals.widgets.inactive.bg_stroke.color, BORDER);
    }

    #[test]
    fn functional_controls_share_one_row_height() {
        assert!((CONTROL_HEIGHT - 32.0).abs() < f32::EPSILON);
    }
}
