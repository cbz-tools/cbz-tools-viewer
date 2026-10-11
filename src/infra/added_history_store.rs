use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    session::{HISTORY_LIMIT, HistoryEntry, app_base_dir},
    util::path_eq::normalize_path_for_selection,
};

const ADDED_HISTORY_SCHEMA_VERSION: u16 = 1;

pub(crate) struct AddedHistoryStore {
    path: PathBuf,
    entries: VecDeque<HistoryEntry>,
}

impl AddedHistoryStore {
    pub(crate) fn load() -> Self {
        let path = app_base_dir().join("added_history.json");
        let mut store = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(value) => {
                    let schema_version = value
                        .as_object()
                        .and_then(|object| object.get("schema_version"))
                        .and_then(serde_json::Value::as_u64);
                    if schema_version != Some(ADDED_HISTORY_SCHEMA_VERSION as u64) {
                        tracing::warn!(
                            path = %path.display(),
                            ?schema_version,
                            "unsupported added history schema version; using default"
                        );
                        Self::default_at(path.clone())
                    } else {
                        #[derive(serde::Deserialize)]
                        struct StoredAddedHistory {
                            #[serde(default)]
                            entries: VecDeque<HistoryEntry>,
                        }

                        match serde_json::from_value::<StoredAddedHistory>(value) {
                            Ok(stored) => Self {
                                path: path.clone(),
                                entries: stored.entries,
                            },
                            Err(error) => {
                                tracing::warn!(
                                    path = %path.display(),
                                    %error,
                                    "failed to parse added history; using default"
                                );
                                Self::default_at(path.clone())
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "failed to parse added history; using default"
                    );
                    Self::default_at(path.clone())
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Self::default_at(path.clone())
            }
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "failed to read added history; using default"
                );
                Self::default_at(path.clone())
            }
        };
        store.sanitize_entries();
        store
    }

    fn default_at(path: PathBuf) -> Self {
        Self {
            path,
            entries: VecDeque::new(),
        }
    }

    pub(crate) fn entries(&self) -> &VecDeque<HistoryEntry> {
        &self.entries
    }

    pub(crate) fn entries_mut(&mut self) -> &mut VecDeque<HistoryEntry> {
        &mut self.entries
    }

    pub(crate) fn record_generated_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut changed = false;
        for path in paths {
            let normalized_path = normalize_path_for_selection(&path);
            let (file_size, modified_unix_ns) = std::fs::metadata(&path)
                .map(|metadata| {
                    (
                        Some(metadata.len()),
                        metadata.modified().ok().and_then(|modified| {
                            modified
                                .duration_since(UNIX_EPOCH)
                                .ok()
                                .map(|duration| duration.as_nanos())
                        }),
                    )
                })
                .unwrap_or((None, None));
            self.entries
                .retain(|entry| entry.normalized_path != normalized_path);
            self.entries.push_front(HistoryEntry {
                path,
                normalized_path,
                opened_at_ms: now_unix_ms(),
                file_size,
                modified_unix_ns,
            });
            self.entries.truncate(HISTORY_LIMIT);
            changed = true;
        }
        if changed {
            self.save();
        }
    }

    pub(crate) fn update_path(
        &mut self,
        old_normalized_path: &str,
        new_path: Option<&Path>,
        remove_descendants: bool,
    ) {
        let mut changed = false;
        if let Some(new_path) = new_path {
            let new_normalized_path = normalize_path_for_selection(new_path);
            for entry in &mut self.entries {
                if entry.normalized_path == old_normalized_path {
                    entry.path = new_path.to_path_buf();
                    entry.normalized_path = new_normalized_path.clone();
                    changed = true;
                }
            }
            if changed {
                let mut seen = HashSet::new();
                self.entries
                    .retain(|entry| seen.insert(entry.normalized_path.clone()));
                self.entries.truncate(HISTORY_LIMIT);
            }
        } else {
            self.entries.retain(|entry| {
                let matches = if remove_descendants {
                    crate::util::path_eq::normalized_path_is_same_or_descendant(
                        &entry.normalized_path,
                        old_normalized_path,
                    )
                } else {
                    entry.normalized_path == old_normalized_path
                };
                if matches {
                    changed = true;
                }
                !matches
            });
        }
        if changed {
            self.save();
        }
    }

    fn sanitize_entries(&mut self) {
        let mut seen = HashSet::new();
        for entry in &mut self.entries {
            if entry.normalized_path.is_empty() {
                entry.normalized_path = normalize_path_for_selection(&entry.path);
            }
        }
        self.entries
            .retain(|entry| seen.insert(entry.normalized_path.clone()));
        self.entries.truncate(HISTORY_LIMIT);
    }

    fn save(&self) {
        #[derive(serde::Serialize)]
        struct StoredAddedHistory<'a> {
            schema_version: u16,
            entries: &'a VecDeque<HistoryEntry>,
        }

        let Ok(json) = serde_json::to_vec_pretty(&StoredAddedHistory {
            schema_version: ADDED_HISTORY_SCHEMA_VERSION,
            entries: &self.entries,
        }) else {
            return;
        };
        if let Err(error) = crate::infra::config_io::atomic_write(&self.path, &json) {
            tracing::warn!(
                path = %self.path.display(),
                %error,
                "failed to save added history"
            );
        }
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
