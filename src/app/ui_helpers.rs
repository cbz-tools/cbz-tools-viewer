use eframe::egui;

use crate::infra::cache::artifact_failure::ArtifactFailureDiskCache;
use crate::infra::cache::disk::DiskCache;
use crate::infra::cache::page_map::PageMapDiskCache;
use crate::ui::{icons, theme};

pub(super) fn calc_cache_size_mb() -> f32 {
    let roots = [
        DiskCache::default_root(),
        PageMapDiskCache::default_root(),
        ArtifactFailureDiskCache::default_root(),
    ];
    let total: u64 = roots
        .iter()
        .filter(|root| root.exists())
        .map(|root| walkdir_size(root))
        .sum();
    total as f32 / (1024.0 * 1024.0)
}

fn walkdir_size(dir: &std::path::Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.filter_map(|e| e.ok())
        .map(|e| {
            let meta = e.metadata();
            if let Ok(m) = meta {
                if m.is_dir() {
                    walkdir_size(&e.path())
                } else {
                    m.len()
                }
            } else {
                0
            }
        })
        .sum()
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DialogButtonSpec<'a> {
    pub id: egui::Id,
    pub label: &'a str,
    pub width: f32,
    pub is_default: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct DialogButtonResult {
    pub clicked: bool,
}

fn paint_dialog_button(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    label: &str,
    is_default: bool,
    emphasized: bool,
) {
    let painter = ui.painter_at(rect);
    let corner = egui::CornerRadius::same(7);

    if emphasized {
        painter.rect_filled(rect, corner, theme::ACCENT.linear_multiply(0.2));
    }
    if is_default || emphasized {
        painter.rect_stroke(
            rect,
            corner,
            egui::Stroke::new(1.0_f32, theme::ACCENT_ACTIVE),
            egui::StrokeKind::Inside,
        );
    }
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(theme::FONT_SIZE_BODY),
        theme::TEXT_MAIN,
    );
}

pub(super) fn dialog_button_row(
    ui: &mut egui::Ui,
    button_height: f32,
    specs: &[DialogButtonSpec<'_>],
) -> Vec<DialogButtonResult> {
    if specs.is_empty() {
        return Vec::new();
    }

    let mut rects = Vec::with_capacity(specs.len());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for spec in specs {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(spec.width, button_height), egui::Sense::hover());
            rects.push(rect);
        }
    });

    let mut responses = Vec::with_capacity(specs.len());
    let mut any_hovered = false;
    for (spec, rect) in specs.iter().zip(&rects) {
        let response = ui
            .interact(*rect, spec.id, egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        any_hovered |= response.hovered();
        responses.push(response);
    }

    let mut results = Vec::with_capacity(specs.len());
    for ((spec, rect), response) in specs.iter().zip(&rects).zip(&responses) {
        let emphasized = response.hovered() || (spec.is_default && !any_hovered);
        paint_dialog_button(ui, *rect, spec.label, spec.is_default, emphasized);
        results.push(DialogButtonResult {
            clicked: response.clicked(),
        });
    }

    results
}

pub(super) fn setup_style(ctx: &egui::Context) {
    let mut style = (*ctx.global_style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.window_fill = theme::WINDOW_BG;
    style.visuals.panel_fill = theme::SURFACE_BG;
    style.visuals.override_text_color = Some(theme::TEXT_MAIN);
    style.visuals.hyperlink_color = theme::ACCENT;
    style.visuals.selection.bg_fill = theme::ACCENT.linear_multiply(0.35);
    style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, theme::ACCENT_ACTIVE);
    style.visuals.window_stroke = egui::Stroke::new(1.0_f32, theme::BORDER);
    style.visuals.widgets.noninteractive.bg_fill = theme::SURFACE_BG;
    style.visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, theme::TEXT_SUBTLE);
    style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, theme::BORDER);
    style.visuals.widgets.inactive.bg_fill = theme::TOOLBAR_BG;
    style.visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, theme::TEXT_MAIN);
    style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, theme::BORDER);
    style.visuals.widgets.hovered.bg_fill = theme::BUTTON_HOVER;
    style.visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, theme::TEXT_MAIN);
    style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, theme::HOVER_BORDER);
    style.visuals.widgets.active.bg_fill = theme::BUTTON_ACTIVE;
    style.visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, theme::TEXT_MAIN);
    style.visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, theme::ACCENT_ACTIVE);
    style.visuals.widgets.open.bg_fill = theme::SURFACE_BG;
    style.visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0_f32, theme::TEXT_MAIN);
    style.visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, theme::BORDER);
    style.visuals.faint_bg_color = theme::SEPARATOR_WEAK;
    style.visuals.extreme_bg_color = theme::WINDOW_BG;
    style.visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(4);
    style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(4);
    style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(4);
    style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(4);
    ctx.set_global_style(style);

    let mut fonts = egui::FontDefinitions::default();
    let candidates: &[(&str, &str, u32)] = &[
        ("jp_meiryo", r"C:\Windows\Fonts\meiryo.ttc", 0),
        ("jp_yugothic", r"C:\Windows\Fonts\YuGothM.ttc", 0),
        ("jp_msgothic", r"C:\Windows\Fonts\msgothic.ttc", 2),
        ("jp_msmincho", r"C:\Windows\Fonts\msmincho.ttc", 0),
        ("zh_yahei", r"C:\Windows\Fonts\msyh.ttc", 0),
        ("zh_yahei_bold", r"C:\Windows\Fonts\msyhbd.ttc", 0),
        ("zh_simsun", r"C:\Windows\Fonts\simsun.ttc", 0),
        ("zh_simhei", r"C:\Windows\Fonts\simhei.ttf", 0),
    ];
    let mut added_any_font = false;
    for (font_name, path, index) in candidates {
        let Ok(data) = std::fs::read(path) else {
            tracing::debug!("フォント未読込: {path} (index={index})");
            continue;
        };
        tracing::debug!("フォント読込: {path} (index={index})");
        fonts.font_data.insert(
            (*font_name).to_owned(),
            std::sync::Arc::new(egui::FontData {
                font: std::borrow::Cow::Owned(data),
                index: *index,
                tweak: egui::FontTweak::default(),
            }),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push((*font_name).to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push((*font_name).to_owned());
        added_any_font = true;
    }
    if !added_any_font {
        tracing::warn!("Windows 標準フォントを追加できませんでした");
    }
    ctx.set_fonts(fonts);
    icons::initialize(ctx);
}
