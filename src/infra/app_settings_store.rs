use std::path::PathBuf;

use crate::domain::app_settings::AppSettings;
use crate::domain::performance::PerformanceResources;

#[path = "app_settings_resolution.rs"]
pub(crate) mod app_settings_resolution;

impl AppSettings {
    #[allow(dead_code)]
    pub fn load() -> Self {
        let resources = crate::infra::system_resources::detect_pc_resources();
        Self::load_with_resources(&resources)
    }

    pub fn load_with_resources(resources: &PerformanceResources) -> Self {
        let path = Self::settings_path();
        let mut settings = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let defaults =
                    app_settings_resolution::default_app_settings_for_resources(resources);
                match crate::domain::app_settings_codec::decode_settings_json(
                    &text,
                    defaults.clone(),
                ) {
                    Ok(Some(settings)) => settings,
                    Ok(None) => {
                        tracing::warn!(
                            path = %path.display(),
                            setting = "app_settings",
                            "invalid app settings schema or root shape; using default"
                        );
                        defaults
                    }
                    Err(err) => {
                        tracing::warn!(
                            ?err,
                            path = %path.display(),
                            setting = "app_settings",
                            "failed to parse json settings; using default"
                        );
                        defaults
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                app_settings_resolution::default_app_settings_for_resources(resources)
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    path = %path.display(),
                    setting = "app_settings",
                    "failed to read json settings; using default"
                );
                app_settings_resolution::default_app_settings_for_resources(resources)
            }
        };
        app_settings_resolution::resolve_app_settings_for_resources(&mut settings, resources);
        settings.normalize_persisted_values();
        settings.sanitize_external_tools();
        settings
    }

    #[allow(dead_code)]
    pub fn save(&self) {
        let resources = crate::infra::system_resources::detect_pc_resources();
        self.save_with_resources(&resources);
    }

    pub fn save_with_resources(&self, resources: &PerformanceResources) {
        let mut normalized = self.clone();
        app_settings_resolution::resolve_app_settings_for_resources(&mut normalized, resources);
        normalized.normalize_persisted_values();
        normalized.sanitize_external_tools();
        let path = Self::settings_path();
        if let Ok(json) = crate::domain::app_settings_codec::encode_settings_json(normalized) {
            if let Err(error) = crate::infra::config_io::atomic_write(&path, json.as_bytes()) {
                tracing::warn!(path = %path.display(), %error, "failed to save app settings");
            }
        }
    }

    pub fn settings_path() -> PathBuf {
        let local = std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        local
            .join(crate::app_identity::app_data_dir())
            .join("settings.json")
    }
}
