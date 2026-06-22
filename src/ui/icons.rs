use eframe::egui::{self, FontFamily, FontId, RichText, TextFormat, WidgetText};
use egui_material_icons::MaterialIcon;

pub use egui_material_icons::icons::*;

pub fn initialize(ctx: &egui::Context) {
    egui_material_icons::initialize(ctx);
}

pub fn icon(icon: MaterialIcon, size: f32) -> RichText {
    egui_material_icons::icon_text(icon).size(size)
}

pub fn icon_label(
    ui: &egui::Ui,
    icon: MaterialIcon,
    size: f32,
    text: impl AsRef<str>,
) -> WidgetText {
    let mut job = egui::text::LayoutJob::default();
    let color = ui.visuals().text_color();

    job.append(
        icon.codepoint,
        0.0,
        TextFormat {
            font_id: FontId::new(size, icon.font_family()),
            color,
            ..Default::default()
        },
    );
    job.append(
        " ",
        0.0,
        TextFormat {
            font_id: FontId::new(size, FontFamily::Proportional),
            color,
            ..Default::default()
        },
    );
    job.append(
        text.as_ref(),
        0.0,
        TextFormat {
            font_id: FontId::new(size, FontFamily::Proportional),
            color,
            ..Default::default()
        },
    );

    job.into()
}
