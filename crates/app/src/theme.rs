//! Look and feel: palette, egui style for both themes, small painting helpers.

use diskpulse_core::assess::Level;
use eframe::egui::{
    self, Color32, CornerRadius, FontId, Margin, Response, RichText, Sense, Stroke, Theme, Ui, Vec2, vec2,
};

pub const ACCENT: Color32 = Color32::from_rgb(76, 141, 255);
pub const GOOD: Color32 = Color32::from_rgb(63, 185, 80);
pub const WARN: Color32 = Color32::from_rgb(214, 158, 36);
pub const BAD: Color32 = Color32::from_rgb(226, 74, 74);
pub const NONE: Color32 = Color32::from_rgb(120, 126, 138);

pub fn level_color(l: Option<Level>) -> Color32 {
    match l {
        Some(Level::Good) => GOOD,
        Some(Level::Warning) => WARN,
        Some(Level::Bad) => BAD,
        None => NONE,
    }
}

pub fn install(ctx: &egui::Context) {
    for theme in [Theme::Dark, Theme::Light] {
        ctx.style_mut_of(theme, |s| {
            let dark = theme == Theme::Dark;
            s.spacing.item_spacing = vec2(8.0, 6.0);
            s.spacing.button_padding = vec2(10.0, 4.0);
            s.spacing.menu_margin = Margin::same(6);
            s.spacing.interact_size.y = 24.0;
            let v = &mut s.visuals;
            let r = CornerRadius::same(6);
            v.window_corner_radius = CornerRadius::same(10);
            v.menu_corner_radius = CornerRadius::same(8);
            for w in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                w.corner_radius = r;
            }
            v.selection.bg_fill = if dark {
                Color32::from_rgb(38, 79, 150)
            } else {
                Color32::from_rgb(190, 214, 255)
            };
            v.selection.stroke = Stroke::new(1.0, ACCENT);
            v.hyperlink_color = ACCENT;
            if dark {
                v.panel_fill = Color32::from_rgb(28, 30, 36);
                v.window_fill = Color32::from_rgb(35, 38, 46);
                v.extreme_bg_color = Color32::from_rgb(20, 22, 27);
                v.faint_bg_color = Color32::from_rgb(35, 38, 46);
                v.widgets.inactive.weak_bg_fill = Color32::from_rgb(48, 52, 62);
                v.widgets.inactive.bg_fill = Color32::from_rgb(48, 52, 62);
                v.widgets.hovered.weak_bg_fill = Color32::from_rgb(60, 65, 78);
                v.widgets.hovered.bg_fill = Color32::from_rgb(60, 65, 78);
            } else {
                v.panel_fill = Color32::from_rgb(246, 247, 249);
                v.window_fill = Color32::WHITE;
                v.extreme_bg_color = Color32::from_rgb(232, 234, 238);
                v.faint_bg_color = Color32::from_rgb(238, 240, 243);
            }
        });
    }
}

/// Small rounded label with a colored background.
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(11.5), Color32::WHITE);
    let size = galley.size() + vec2(12.0, 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, 9.0, color.gamma_multiply(0.85));
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    resp
}

/// Primary (accent-filled) button.
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
            .fill(ACCENT)
            .min_size(Vec2::new(0.0, 26.0)),
    )
}

/// Card-like frame for grouped content.
pub fn card(ui: &Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(ui.visuals().window_fill)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(10)
        .inner_margin(Margin::same(12))
}

/// A colored dot (status light).
pub fn dot(ui: &mut Ui, color: Color32, radius: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(radius * 2.0 + 2.0, radius * 2.0 + 2.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

/// Horizontal bar showing `frac` (0..=1) inside the allocated width.
pub fn bar(ui: &mut Ui, frac: f32, color: Color32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 8.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    if frac > 0.0 {
        let mut f = rect;
        f.set_width((rect.width() * frac.clamp(0.0, 1.0)).max(3.0));
        p.rect_filled(f, 4.0, color);
    }
}
