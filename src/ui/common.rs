use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke};

use crate::domain::app_settings::{ReadingDirection, UiLanguage};

use super::{
    i18n::{tr, TextKey},
    theme,
};

pub fn paint_quiet_hover_border(ui: &egui::Ui, resp: &egui::Response) {
    if resp.hovered() {
        ui.painter().rect_stroke(
            resp.rect,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.0, theme::HOVER_BORDER),
            egui::StrokeKind::Inside,
        );
    }
}

pub fn reading_direction_label(language: UiLanguage, direction: ReadingDirection) -> &'static str {
    match direction {
        ReadingDirection::RightToLeft => tr(language, TextKey::RightOpen),
        ReadingDirection::LeftToRight => tr(language, TextKey::LeftOpen),
    }
}

pub fn paint_favorite_star(
    painter: &egui::Painter,
    center: Pos2,
    outer_radius: f32,
    color: Color32,
) {
    if outer_radius <= 0.0 {
        return;
    }

    let inner_radius = outer_radius * 0.45;
    let start_angle = -std::f32::consts::FRAC_PI_2;
    let step = std::f32::consts::PI / 5.0;
    let mut points = Vec::with_capacity(10);
    for idx in 0..10 {
        let radius = if idx % 2 == 0 {
            outer_radius
        } else {
            inner_radius
        };
        let angle = start_angle + step * idx as f32;
        points.push(Pos2::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        ));
    }

    painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
}

pub fn paint_favorite_star_in_rect(painter: &egui::Painter, rect: Rect, color: Color32) {
    let radius = rect.width().min(rect.height()) * 0.5;
    paint_favorite_star(painter, rect.center(), radius, color);
}
