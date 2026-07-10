use eframe::egui;

use crate::domain::app_settings::AppSettings;
use crate::domain::app_settings::UiLanguage;
use crate::domain::performance::{
    PerformanceResources, PERFORMANCE_CACHE_MIN_MIB, SPAD_RAM_RATIO_MAX_PERCENT,
    SPAD_RAM_RATIO_MIN_PERCENT,
};

use super::super::i18n::{tr, TextKey};
use super::super::{icons, theme};
use super::widgets::{
    format_bytes_label, format_mib_label, inline_reset_button, section_header, setting_block,
    subsection_header, subtle_text, PERFORMANCE_SELECT_WIDTH,
};
use super::SettingsEvent;

struct PerformanceChoiceRowConfig<'a> {
    label: &'a str,
    value: &'a mut u16,
    enable_normal_ui: bool,
    candidates: Vec<u16>,
    default_value: u16,
    reset_hover_text: String,
    summary_text: String,
    id_suffix: &'a str,
    selection_width: f32,
    description: &'a str,
    value_formatter: fn(u16) -> String,
}

pub(super) fn show_performance_tab(
    ui: &mut egui::Ui,
    language: UiLanguage,
    settings: &mut AppSettings,
    resources: &PerformanceResources,
    cache_size_mb: f32,
    event: &mut SettingsEvent,
) {
    section_header(ui, tr(language, TextKey::Performance));
    performance_resources_block(ui, language, resources);

    subsection_header(ui, tr(language, TextKey::Processing));
    performance_choice_row(
        ui,
        language,
        PerformanceChoiceRowConfig {
            label: tr(language, TextKey::BackgroundWorkers),
            value: &mut settings.viewer_background_worker_count,
            enable_normal_ui: !settings.viewer_danger_zone_enabled,
            candidates: resources.bg_normal_candidates(),
            default_value: resources.bg_default_workers(),
            reset_hover_text: tr(language, TextKey::BackgroundWorkersReset).to_owned(),
            summary_text: format!(
                "{}: {} / {}: {}",
                tr(language, TextKey::CacheUpperLimit),
                resources.bg_normal_upper_workers(),
                tr(language, TextKey::SizeDefault),
                resources.bg_default_workers()
            ),
            id_suffix: "viewer_background_worker_count",
            selection_width: PERFORMANCE_SELECT_WIDTH,
            description: tr(language, TextKey::BackgroundWorkersDescription),
            value_formatter: |value| value.to_string(),
        },
    );

    subsection_header(ui, tr(language, TextKey::MemoryCache));
    performance_choice_row(
        ui,
        language,
        PerformanceChoiceRowConfig {
            label: tr(language, TextKey::L1VramCache),
            value: &mut settings.viewer_l1_vram_cache_max_mb,
            enable_normal_ui: !settings.viewer_danger_zone_enabled,
            candidates: resources.l1_normal_candidates(),
            default_value: resources.l1_default_mib(),
            reset_hover_text: format!(
                "{} {}",
                tr(language, TextKey::CacheResetDefault),
                format_mib_label(resources.l1_default_mib())
            ),
            summary_text: format!(
                "{} / {}",
                tr(language, TextKey::CacheUpperLimit),
                format_mib_label(resources.l1_normal_upper_mib())
            ),
            id_suffix: "viewer_l1_vram_cache_max_mb",
            selection_width: PERFORMANCE_SELECT_WIDTH,
            description: tr(language, TextKey::GPUKeepNote),
            value_formatter: format_mib_label,
        },
    );
    performance_choice_row(
        ui,
        language,
        PerformanceChoiceRowConfig {
            label: tr(language, TextKey::L2RamCache),
            value: &mut settings.viewer_rgba_cache_max_mb,
            enable_normal_ui: !settings.viewer_danger_zone_enabled,
            candidates: resources.l2_normal_candidates(),
            default_value: resources.l2_default_mib(),
            reset_hover_text: format!(
                "{} {}",
                tr(language, TextKey::CacheResetDefault),
                format_mib_label(resources.l2_default_mib())
            ),
            summary_text: format!(
                "{} / {}",
                tr(language, TextKey::CacheUpperLimit),
                format_mib_label(resources.l2_normal_upper_mib())
            ),
            id_suffix: "viewer_rgba_cache_max_mb",
            selection_width: PERFORMANCE_SELECT_WIDTH,
            description: tr(language, TextKey::L2RamCacheDescription),
            value_formatter: format_mib_label,
        },
    );

    subsection_header(ui, tr(language, TextKey::DiskCache));
    setting_block(ui, tr(language, TextKey::CacheUsage), |ui| {
        let size_text = if cache_size_mb < 0.0 {
            tr(language, TextKey::Unavailable).to_owned()
        } else {
            format!("{:.1} MiB", cache_size_mb)
        };
        ui.label(
            egui::RichText::new(size_text)
                .color(theme::TEXT_MAIN)
                .size(theme::FONT_SIZE_BODY),
        );
    });

    ui.add_space(4.0);
    if ui
        .add(
            egui::Button::new(
                icons::icon_label(
                    ui,
                    icons::ICON_DELETE,
                    16.0,
                    tr(language, TextKey::CacheClear),
                )
                .color(theme::DELETE_RED),
            )
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::new(1.0, egui::Color32::TRANSPARENT)),
        )
        .on_hover_text(tr(language, TextKey::CacheClearTooltip))
        .clicked()
    {
        *event = SettingsEvent::ClearCache;
    }

    subsection_header(ui, tr(language, TextKey::DangerZone));
    danger_zone_block(ui, language, settings, resources);
}

fn performance_resources_block(
    ui: &mut egui::Ui,
    language: UiLanguage,
    resources: &PerformanceResources,
) {
    setting_block(ui, tr(language, TextKey::PcResourcesLabel), |ui| {
        ui.label(
            egui::RichText::new(format!(
                "RAM: {} / VRAM: {} / CPU: {} / GPU: {}",
                format_bytes_label(resources.physical_ram_bytes),
                resources
                    .dedicated_vram_bytes
                    .map(format_bytes_label)
                    .unwrap_or_else(|| tr(language, TextKey::GettingUnavailable).to_owned()),
                resources.logical_cpu_count,
                resources
                    .gpu_adapter_name
                    .as_deref()
                    .unwrap_or(tr(language, TextKey::GettingUnavailable)),
            ))
            .color(theme::TEXT_MAIN)
            .size(theme::FONT_SIZE_SMALL),
        );
    });
}

fn performance_choice_row(
    ui: &mut egui::Ui,
    language: UiLanguage,
    config: PerformanceChoiceRowConfig<'_>,
) {
    setting_block(ui, config.label, |ui| {
        ui.add_enabled_ui(config.enable_normal_ui, |ui| {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(config.id_suffix)
                    .selected_text((config.value_formatter)(*config.value))
                    .width(config.selection_width)
                    .show_ui(ui, |ui| {
                        for candidate in config.candidates.iter().copied() {
                            ui.selectable_value(
                                config.value,
                                candidate,
                                (config.value_formatter)(candidate),
                            );
                        }
                    });
                ui.add_space(8.0);
                inline_reset_button(
                    ui,
                    language,
                    *config.value != config.default_value,
                    Some(config.reset_hover_text),
                    || {
                        *config.value = config.default_value;
                    },
                );
            });
        });
        subtle_text(ui, config.description);
        subtle_text(ui, &config.summary_text);
    });
}

fn danger_zone_block(
    ui: &mut egui::Ui,
    language: UiLanguage,
    settings: &mut AppSettings,
    resources: &PerformanceResources,
) {
    let frame = egui::Frame::new()
        .fill(theme::SURFACE_BG)
        .stroke(egui::Stroke::new(1.0, theme::DELETE_RED))
        .inner_margin(egui::Margin::symmetric(10, 8));
    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(icons::icon(icons::ICON_WARNING, 18.0).color(theme::DELETE_RED));
            ui.label(
                egui::RichText::new(tr(language, TextKey::DangerZoneTitle))
                    .size(theme::FONT_SIZE_BODY)
                    .color(theme::TEXT_MAIN)
                    .strong(),
            );
        });
        ui.add_space(4.0);
        ui.checkbox(
            &mut settings.viewer_danger_zone_enabled,
            tr(language, TextKey::DangerZoneEnableLabel),
        );
        subtle_text(ui, tr(language, TextKey::DangerZoneEnableDescription));
        if settings.viewer_danger_zone_enabled {
            subtle_text(ui, tr(language, TextKey::DangerZoneBodyText));
            setting_block(ui, tr(language, TextKey::L1VramCache), |ui| {
                let mut l1 = settings.viewer_l1_vram_cache_max_mb as i32;
                let max = resources.l1_danger_upper_mib() as i32;
                ui.add_enabled_ui(settings.viewer_danger_zone_enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr(language, TextKey::MiB));
                        if ui
                            .add(
                                egui::DragValue::new(&mut l1)
                                    .range(PERFORMANCE_CACHE_MIN_MIB as i32..=max)
                                    .speed(1.0),
                            )
                            .changed()
                        {
                            settings.viewer_l1_vram_cache_max_mb =
                                l1.clamp(PERFORMANCE_CACHE_MIN_MIB as i32, max) as u16;
                        }
                    });
                });
                subtle_text(
                    ui,
                    &format!(
                        "{}: 256～{} {}",
                        tr(language, TextKey::InputRangeLabel),
                        format_mib_label(max as u16),
                        tr(language, TextKey::MiB)
                    ),
                );
            });

            setting_block(ui, tr(language, TextKey::L2RamCache), |ui| {
                let mut l2 = settings.viewer_rgba_cache_max_mb as i32;
                let max = resources.l2_danger_upper_mib() as i32;
                ui.add_enabled_ui(settings.viewer_danger_zone_enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr(language, TextKey::MiB));
                        if ui
                            .add(
                                egui::DragValue::new(&mut l2)
                                    .range(PERFORMANCE_CACHE_MIN_MIB as i32..=max)
                                    .speed(1.0),
                            )
                            .changed()
                        {
                            settings.viewer_rgba_cache_max_mb =
                                l2.clamp(PERFORMANCE_CACHE_MIN_MIB as i32, max) as u16;
                        }
                    });
                });
                subtle_text(
                    ui,
                    &format!(
                        "{}: 256～{} {}",
                        tr(language, TextKey::InputRangeLabel),
                        format_mib_label(max as u16),
                        tr(language, TextKey::MiB)
                    ),
                );
            });

            setting_block(ui, tr(language, TextKey::AdjacentBookPreloadRam), |ui| {
                let mut ratio = settings.viewer_spad_ram_ratio_percent as i32;
                ui.horizontal(|ui| {
                    ui.label("%");
                    if ui
                        .add(
                            egui::DragValue::new(&mut ratio)
                                .range(
                                    SPAD_RAM_RATIO_MIN_PERCENT as i32
                                        ..=SPAD_RAM_RATIO_MAX_PERCENT as i32,
                                )
                                .speed(1.0),
                        )
                        .changed()
                    {
                        settings.viewer_spad_ram_ratio_percent = ratio.clamp(
                            SPAD_RAM_RATIO_MIN_PERCENT as i32,
                            SPAD_RAM_RATIO_MAX_PERCENT as i32,
                        ) as u8;
                    }
                });
                subtle_text(ui, tr(language, TextKey::AdjacentBookPreloadRamDescription));
            });

            setting_block(ui, tr(language, TextKey::BackgroundWorkers), |ui| {
                let mut workers = settings.viewer_background_worker_count as i32;
                let max = resources.bg_danger_upper_workers() as i32;
                ui.add_enabled_ui(settings.viewer_danger_zone_enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr(language, TextKey::Count));
                        if ui
                            .add(egui::DragValue::new(&mut workers).range(1..=max).speed(1.0))
                            .changed()
                        {
                            settings.viewer_background_worker_count = workers.clamp(1, max) as u16;
                        }
                    });
                });
                subtle_text(
                    ui,
                    &format!("{}: 1～{}", tr(language, TextKey::InputRangeLabel), max),
                );
            });
        }
    });
}
