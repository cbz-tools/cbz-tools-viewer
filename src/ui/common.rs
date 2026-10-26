use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke};
use egui_material_icons::MaterialIcon;

use crate::domain::app_settings::{ReadingDirection, UiLanguage};

use super::{
    i18n::{TextKey, tr},
    theme,
};

pub fn paint_quiet_hover_border(ui: &egui::Ui, resp: &egui::Response) {
    if resp.hovered() {
        ui.painter().rect_stroke(
            resp.rect,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.0_f32, theme::HOVER_BORDER),
            egui::StrokeKind::Inside,
        );
    }
}

pub const DELETE_ICON_DRAW_SIZE: f32 = 16.0;

pub fn paint_delete_icon(painter: &egui::Painter, rect: Rect, color: Color32) {
    let size = rect.width().min(rect.height());
    if size <= 0.0 {
        return;
    }

    let scale = size / 16.0;
    let origin = rect.center() - egui::vec2(size * 0.5, size * 0.5);
    let point = |x: f32, y: f32| Pos2::new(origin.x + x * scale, origin.y + y * scale);
    let paint_segment = |points: [(f32, f32); 4]| {
        painter.add(Shape::convex_polygon(
            points.into_iter().map(|(x, y)| point(x, y)).collect(),
            color,
            Stroke::NONE,
        ));
    };

    // Three body segments leave two narrow, vertical slots like the Material delete glyph.
    paint_segment([(4.0, 5.2), (6.35, 5.2), (6.35, 14.2), (4.7, 14.2)]);
    paint_segment([(6.95, 5.2), (9.05, 5.2), (9.05, 14.2), (6.95, 14.2)]);
    paint_segment([(9.65, 5.2), (12.0, 5.2), (11.3, 14.2), (9.65, 14.2)]);

    let lid = Rect::from_min_max(point(2.6, 3.45), point(13.4, 4.75));
    painter.rect_filled(lid, egui::CornerRadius::same(1), color);

    let handle = Rect::from_min_max(point(6.25, 1.5), point(9.75, 3.5));
    painter.rect_filled(handle, egui::CornerRadius::same(1), color);
}

pub fn icon_button_text(label: &str, font_size: f32) -> egui::RichText {
    egui::RichText::new(format!("\u{2003} {label}")).size(font_size)
}

pub fn paint_icon_button_label(
    ui: &egui::Ui,
    response: &egui::Response,
    icon: MaterialIcon,
    label: &str,
    font_size: f32,
    icon_size: f32,
    color: Color32,
) {
    let font_id = egui::FontId::proportional(font_size);
    let painter = ui.painter();
    let icon_space = painter.layout_no_wrap("\u{2003}".to_owned(), font_id.clone(), color);
    let content = painter.layout_no_wrap(format!("\u{2003} {label}"), font_id, color);
    let icon_center = Pos2::new(
        response.rect.center().x - content.size().x * 0.5 + icon_space.size().x * 0.5,
        response.rect.center().y,
    );
    let icon_rect = Rect::from_center_size(icon_center, egui::vec2(icon_size, icon_size));
    painter.text(
        icon_rect.center(),
        egui::Align2::CENTER_CENTER,
        icon.codepoint,
        egui::FontId::new(icon_size, icon.font_family()),
        color,
    );
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
