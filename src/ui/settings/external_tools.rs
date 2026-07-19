use eframe::egui;

use crate::domain::app_settings::UiLanguage;
use crate::domain::app_settings::{
    AppSettings, EXTERNAL_TOOLS_MAX, ExternalTool, ExternalToolShortcut,
};

use super::super::i18n::{TextKey, tr};
use super::super::theme;
use super::widgets::{
    SETTINGS_SMALL_BUTTON_SIZE, SETTINGS_TOOL_DELETE_BUTTON_SIZE, section_header, setting_block,
    subtle_text,
};

pub(super) fn show_external_tools_tab(
    ui: &mut egui::Ui,
    language: UiLanguage,
    settings: &mut AppSettings,
) {
    section_header(ui, tr(language, TextKey::ExternalTools));

    if settings.external_tools.len() > EXTERNAL_TOOLS_MAX {
        settings.sanitize_external_tools();
    }

    let mut remove_idx: Option<usize> = None;
    for idx in 0..settings.external_tools.len() {
        let used_shortcuts = settings
            .external_tools
            .iter()
            .enumerate()
            .map(|(tool_idx, tool)| (tool_idx, tool.shortcut))
            .collect::<Vec<_>>();
        setting_block(
            ui,
            &format!("{} {}", tr(language, TextKey::ExternalToolLabel), idx + 1),
            |ui| {
                let tool = &mut settings.external_tools[idx];
                tool_editor(ui, language, idx, tool, &used_shortcuts);
                ui.add_space(4.0);
                if ui
                    .add_sized(
                        SETTINGS_TOOL_DELETE_BUTTON_SIZE,
                        egui::Button::new(
                            egui::RichText::new(tr(language, TextKey::ExternalToolDelete))
                                .color(theme::DELETE_RED)
                                .size(theme::FONT_SIZE_BODY),
                        ),
                    )
                    .clicked()
                {
                    remove_idx = Some(idx);
                }
            },
        );
    }

    if let Some(idx) = remove_idx {
        settings.external_tools.remove(idx);
    }

    ui.horizontal(|ui| {
        let can_add = settings.external_tools.len() < EXTERNAL_TOOLS_MAX
            && AppSettings::next_available_external_tool_shortcut(&settings.external_tools)
                .is_some();
        let add_resp = ui.add_enabled(
            can_add,
            egui::Button::new(tr(language, TextKey::ExternalToolAdd))
                .min_size(SETTINGS_SMALL_BUTTON_SIZE),
        );
        if add_resp.clicked() {
            if let Some(shortcut) =
                AppSettings::next_available_external_tool_shortcut(&settings.external_tools)
            {
                settings.external_tools.push(ExternalTool {
                    name: String::new(),
                    executable: String::new(),
                    args: "\"{path}\"".to_owned(),
                    shortcut,
                    background: true,
                });
            }
        }
        subtle_text(
            ui,
            &format!(
                "{}: {} / {}",
                tr(language, TextKey::CurrentCount),
                settings.external_tools.len(),
                EXTERNAL_TOOLS_MAX
            ),
        );
    });

    subtle_text(ui, tr(language, TextKey::ExternalToolPathNote));
    subtle_text(ui, tr(language, TextKey::SampleExternalTool));
}

fn tool_editor(
    ui: &mut egui::Ui,
    language: UiLanguage,
    idx: usize,
    tool: &mut ExternalTool,
    used_shortcuts: &[(usize, ExternalToolShortcut)],
) {
    ui.horizontal(|ui| {
        ui.label(tr(language, TextKey::NameLabel));
        ui.add_sized(
            [220.0, 24.0],
            egui::TextEdit::singleline(&mut tool.name).hint_text(format!(
                "{}: {}",
                tr(language, TextKey::Example),
                tr(language, TextKey::Optimizer)
            )),
        );
    });

    ui.horizontal(|ui| {
        ui.label(tr(language, TextKey::ShortcutLabel));
        let mut selected = tool.shortcut;
        egui::ComboBox::from_id_salt(format!("external_tool_shortcut_{}", idx))
            .selected_text(selected.as_char().to_string())
            .width(110.0)
            .show_ui(ui, |ui| {
                for candidate in AppSettings::external_tool_shortcut_candidates() {
                    let used_by_other = used_shortcuts.iter().any(|(other_idx, other_shortcut)| {
                        *other_idx != idx && *other_shortcut == *candidate
                    });
                    if used_by_other {
                        ui.add_enabled(false, egui::Button::new(candidate.as_char().to_string()));
                    } else {
                        ui.selectable_value(
                            &mut selected,
                            *candidate,
                            candidate.as_char().to_string(),
                        );
                    }
                }
            });
        tool.shortcut = selected;
    });

    ui.horizontal(|ui| {
        ui.label(tr(language, TextKey::StartModeLabel));
        egui::ComboBox::from_id_salt(format!("external_tool_background_{}", idx))
            .selected_text(if tool.background {
                tr(language, TextKey::BackgroundMode)
            } else {
                tr(language, TextKey::NormalMode)
            })
            .width(140.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut tool.background,
                    true,
                    tr(language, TextKey::BackgroundMode),
                );
                ui.selectable_value(
                    &mut tool.background,
                    false,
                    tr(language, TextKey::NormalMode),
                );
            });
    });

    ui.horizontal(|ui| {
        ui.label(tr(language, TextKey::ExecutableLabel));
        ui.add_sized(
            [420.0, 24.0],
            egui::TextEdit::singleline(&mut tool.executable)
                .hint_text(format!("{}: cbz-opt.exe", tr(language, TextKey::Example))),
        );
    });

    ui.horizontal(|ui| {
        ui.label(tr(language, TextKey::ArgumentsLabel));
        ui.add_sized(
            [420.0, 24.0],
            egui::TextEdit::singleline(&mut tool.args).hint_text(format!(
                "{}: --json \"{{path}}\"",
                tr(language, TextKey::Example)
            )),
        );
    });
}
