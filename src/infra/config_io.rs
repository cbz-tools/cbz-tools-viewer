use std::path::Path;

use serde::de::DeserializeOwned;

pub fn load_json_or_default<T>(path: &Path, label: &str) -> T
where
    T: DeserializeOwned + Default,
{
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<T>(&text) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    ?err,
                    path = %path.display(),
                    setting = label,
                    "failed to parse json settings; using default"
                );
                T::default()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(err) => {
            tracing::warn!(
                ?err,
                path = %path.display(),
                setting = label,
                "failed to read json settings; using default"
            );
            T::default()
        }
    }
}

pub fn load_toml_or_default<T>(path: &Path, label: &str) -> T
where
    T: DeserializeOwned + Default,
{
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let normalized = raw
                .trim_start_matches('\u{FEFF}')
                .replace("\r\n", "\n")
                .replace('\r', "\n");
            match toml::from_str::<T>(&normalized) {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        path = %path.display(),
                        setting = label,
                        "failed to parse toml settings; using default"
                    );
                    T::default()
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(err) => {
            tracing::warn!(
                ?err,
                path = %path.display(),
                setting = label,
                "failed to read toml settings; using default"
            );
            T::default()
        }
    }
}
