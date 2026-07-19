use crate::domain::app_settings::AppSettings;
use crate::domain::performance::{PerformanceResources, PerformanceSettingsResolved};

pub(crate) fn default_app_settings_for_resources(resources: &PerformanceResources) -> AppSettings {
    let mut settings = AppSettings::default();
    let defaults = resources.default_performance_settings();
    settings.viewer_l1_vram_cache_max_mb = defaults.l1_vram_cache_max_mib;
    settings.viewer_rgba_cache_max_mb = defaults.l2_ram_cache_max_mib;
    settings.viewer_background_worker_count = defaults.background_worker_count as u16;
    settings
}

pub(crate) fn resolve_app_settings_for_resources(
    settings: &mut AppSettings,
    resources: &PerformanceResources,
) {
    settings.viewer_l1_vram_cache_max_mb = resources.normalize_l1_mib(
        settings.viewer_l1_vram_cache_max_mb,
        settings.viewer_danger_zone_enabled,
    );
    settings.viewer_rgba_cache_max_mb = resources.normalize_l2_mib(
        settings.viewer_rgba_cache_max_mb,
        settings.viewer_danger_zone_enabled,
    );
    settings.viewer_background_worker_count = resources.normalize_bg_workers(
        settings.viewer_background_worker_count,
        settings.viewer_danger_zone_enabled,
    );
}

pub(crate) fn resolve_performance_settings(
    settings: &AppSettings,
    resources: &PerformanceResources,
) -> PerformanceSettingsResolved {
    resources.resolved_performance_settings(
        settings.viewer_l1_vram_cache_max_mb,
        settings.viewer_rgba_cache_max_mb,
        settings.viewer_background_worker_count,
        settings.viewer_danger_zone_enabled,
        settings.viewer_spad_ram_ratio_percent,
    )
}
