//! About ウィンドウ。

use eframe::egui;

use super::theme;

const GITHUB_URL: &str = "https://github.com/cbz-tools/cbz-tools-viewer";
const LATEST_RELEASE_URL: &str = "https://github.com/cbz-tools/cbz-tools-viewer/releases/latest";
const ABOUT_WINDOW_DEFAULT_SIZE: egui::Vec2 = egui::vec2(320.0, 180.0);

pub fn show(ctx: &egui::Context, open: &mut bool) {
    if *open && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        *open = false;
        return;
    }

    let mut close_requested = false;
    let available = ctx.content_rect();
    let default_pos = egui::pos2(
        available.center().x - ABOUT_WINDOW_DEFAULT_SIZE.x / 2.0,
        available.center().y - ABOUT_WINDOW_DEFAULT_SIZE.y / 2.0,
    );
    egui::Window::new(crate::app_identity::PRODUCT_NAME)
        .open(open)
        .resizable(false)
        .collapsible(false)
        .default_pos(default_pos)
        .default_size(ABOUT_WINDOW_DEFAULT_SIZE)
        .show(ctx, |ui| {
            ui.set_min_width(280.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(crate::app_identity::PRODUCT_NAME)
                        .size(theme::FONT_SIZE_LARGE)
                        .strong(),
                );
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            });

            ui.add_space(12.0);
            if ui.link("GitHub").clicked() {
                ctx.open_url(egui::OpenUrl::new_tab(GITHUB_URL));
            }
            if ui.link("Check for latest version").clicked() {
                ctx.open_url(egui::OpenUrl::new_tab(LATEST_RELEASE_URL));
            }

            ui.separator();
            if ui.button("Close").clicked() {
                close_requested = true;
            }
        });

    if close_requested {
        *open = false;
    }
}
