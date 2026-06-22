//! 設定ウィンドウ。
//!
//! `show()` を毎フレーム呼び、`open` が false のときだけ閉じる。
use eframe::egui;

use crate::domain::app_settings::AppSettings;
use crate::domain::app_settings::UiLanguage;
use crate::domain::performance::PerformanceResources;

use super::i18n::{tr, TextKey};
use super::theme;

mod cache;
mod display;
mod external_tools;
mod performance;
mod widgets;

const SETTINGS_BUTTON_HEIGHT: f32 = theme::CONTROL_HEIGHT;
const SETTINGS_TAB_BASE_WIDTH: f32 = 88.0;
const SETTINGS_TAB_SIZE: egui::Vec2 = egui::vec2(
    SETTINGS_TAB_BASE_WIDTH + theme::ICON_BUTTON_HOVER_GUARD_X,
    SETTINGS_BUTTON_HEIGHT,
);
const SETTINGS_WINDOW_DEFAULT_SIZE: egui::Vec2 = egui::vec2(720.0, 560.0);

// ── イベント ──────────────────────────────────────────────────────────────────

/// 設定操作の結果イベント
#[derive(Debug, PartialEq)]
pub enum SettingsEvent {
    None,
    /// カードサイズが変更された（表示グリッド再描画が必要）
    ThumbSizeChanged,
    /// キャッシュクリアが要求された
    ClearCache,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsTab {
    General,
    Library,
    Viewer,
    Performance,
    ExternalTools,
}

impl SettingsTab {
    fn all(language: UiLanguage) -> [(Self, &'static str); 5] {
        [
            (Self::General, tr(language, TextKey::General)),
            (Self::Library, tr(language, TextKey::Library)),
            (Self::Viewer, tr(language, TextKey::ViewerTab)),
            (Self::Performance, tr(language, TextKey::Performance)),
            (Self::ExternalTools, tr(language, TextKey::ExternalTools)),
        ]
    }
}

pub fn show(
    ctx: &egui::Context,
    open: &mut bool,
    language: UiLanguage,
    settings: &mut AppSettings,
    resources: &PerformanceResources,
    cache_size_mb: f32,
) -> SettingsEvent {
    let mut event = SettingsEvent::None;

    if *open && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        *open = false;
        return event;
    }

    let available = ctx.content_rect();
    let default_pos = egui::pos2(
        available.center().x - SETTINGS_WINDOW_DEFAULT_SIZE.x / 2.0,
        available.center().y - SETTINGS_WINDOW_DEFAULT_SIZE.y / 2.0,
    );

    egui::Window::new(tr(language, TextKey::Settings))
        .open(open)
        .resizable(false)
        .collapsible(false)
        .movable(true)
        .default_pos(default_pos)
        .default_size(SETTINGS_WINDOW_DEFAULT_SIZE)
        .min_width(720.0)
        .show(ctx, |ui| {
            ui.set_min_width(680.0);
            ui.visuals_mut().widgets.inactive.bg_fill = theme::SEPARATOR_WEAK;
            ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::new(1.0, theme::BORDER);
            ui.visuals_mut().widgets.hovered.bg_fill = theme::BUTTON_HOVER;
            ui.visuals_mut().widgets.hovered.bg_stroke =
                egui::Stroke::new(1.0, theme::HOVER_BORDER);
            ui.visuals_mut().widgets.active.bg_fill = theme::BUTTON_ACTIVE;
            ui.visuals_mut().widgets.active.bg_stroke =
                egui::Stroke::new(1.0, theme::ACCENT_ACTIVE);
            let mut selected_tab = ctx.memory_mut(|mem| {
                mem.data
                    .get_temp::<SettingsTab>(egui::Id::new("settings_selected_tab"))
                    .unwrap_or(SettingsTab::General)
            });
            tab_selector(ui, language, &mut selected_tab);
            ctx.memory_mut(|mem| {
                mem.data
                    .insert_temp(egui::Id::new("settings_selected_tab"), selected_tab);
            });

            ui.scope(|ui| {
                ui.style_mut().spacing.scroll = egui::style::ScrollStyle {
                    floating: false,
                    bar_width: 10.0,
                    handle_min_length: 40.0,
                    bar_inner_margin: 2.0,
                    bar_outer_margin: 2.0,
                    foreground_color: true,
                    ..egui::style::ScrollStyle::solid()
                };

                egui::ScrollArea::vertical()
                    .scroll_bar_visibility(
                        egui::containers::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                    )
                    .auto_shrink([false, false])
                    .show(ui, |ui| match selected_tab {
                        SettingsTab::General => {
                            display::show_general_tab(ui, language, settings);
                        }
                        SettingsTab::Library => {
                            cache::show_library_tab(ui, language, settings, &mut event);
                        }
                        SettingsTab::Viewer => {
                            display::show_viewer_tab(ui, language, settings);
                        }
                        SettingsTab::Performance => {
                            performance::show_performance_tab(
                                ui,
                                language,
                                settings,
                                resources,
                                cache_size_mb,
                                &mut event,
                            );
                        }
                        SettingsTab::ExternalTools => {
                            external_tools::show_external_tools_tab(ui, language, settings);
                        }
                    });
            });
        });

    event
}

fn tab_selector(ui: &mut egui::Ui, language: UiLanguage, selected: &mut SettingsTab) {
    ui.horizontal(|ui| {
        for (tab, label) in SettingsTab::all(language) {
            let resp = ui.add_sized(
                SETTINGS_TAB_SIZE,
                egui::Button::selectable(*selected == tab, label),
            );
            if resp.clicked() {
                *selected = tab;
            }
        }
    });
    ui.add_space(6.0);
    ui.separator();
}
