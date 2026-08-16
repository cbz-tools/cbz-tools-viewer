use eframe::egui;

use crate::domain::app_settings::{
    AppSettings, UiLanguage, WEB_SEARCHES_MAX, WebSearchBrowser, WebSearchOpenMode,
};

use super::super::i18n::{TextKey, tr};
use super::widgets::{section_header, setting_block, subtle_text};

pub(super) fn show_web_search_tab(
    ui: &mut egui::Ui,
    language: UiLanguage,
    settings: &mut AppSettings,
) {
    section_header(ui, tr(language, TextKey::WebSearch));

    setting_block(ui, tr(language, TextKey::Browser), |ui| {
        egui::ComboBox::from_id_salt("web_search_browser")
            .selected_text(browser_label(settings.web_search_browser))
            .width(220.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.web_search_browser,
                    WebSearchBrowser::Chrome,
                    "Chrome",
                );
                ui.selectable_value(
                    &mut settings.web_search_browser,
                    WebSearchBrowser::Edge,
                    "Edge",
                );
                ui.selectable_value(
                    &mut settings.web_search_browser,
                    WebSearchBrowser::Firefox,
                    "Firefox",
                );
            });
    });

    setting_block(ui, tr(language, TextKey::OpenMode), |ui| {
        egui::ComboBox::from_id_salt("web_search_open_mode")
            .selected_text(open_mode_label(language, settings.web_search_open_mode))
            .width(220.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.web_search_open_mode,
                    WebSearchOpenMode::Tab,
                    "Tab",
                );
                ui.selectable_value(
                    &mut settings.web_search_open_mode,
                    WebSearchOpenMode::NewWindow,
                    "New Window",
                );
            });
    });

    settings.sanitize_web_searches();
    settings
        .web_searches
        .resize_with(WEB_SEARCHES_MAX, Default::default);
    for (idx, search) in settings.web_searches.iter_mut().enumerate() {
        setting_block(
            ui,
            &format!("{} {}", tr(language, TextKey::WebSearchEntryLabel), idx + 1),
            |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr(language, TextKey::DisplayLabel));
                    ui.add_sized(
                        [280.0, 24.0],
                        egui::TextEdit::singleline(&mut search.display),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(tr(language, TextKey::LinkLabel));
                    ui.add_sized(
                        [500.0, 24.0],
                        egui::TextEdit::singleline(&mut search.link)
                            .hint_text(tr(language, TextKey::WebSearchLinkPlaceholder)),
                    );
                });
            },
        );
    }

    subtle_text(ui, tr(language, TextKey::WebSearchLinkNote));
}

fn browser_label(browser: WebSearchBrowser) -> &'static str {
    match browser {
        WebSearchBrowser::Chrome => "Chrome",
        WebSearchBrowser::Edge => "Edge",
        WebSearchBrowser::Firefox => "Firefox",
    }
}

fn open_mode_label(language: UiLanguage, mode: WebSearchOpenMode) -> &'static str {
    match mode {
        WebSearchOpenMode::Tab => "Tab",
        WebSearchOpenMode::NewWindow => {
            if language == UiLanguage::Japanese {
                "新しいウィンドウ"
            } else {
                "New Window"
            }
        }
    }
}
