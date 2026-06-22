use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session::app_base_dir;
use crate::util::path_eq::normalize_path_for_selection;

/// favorites.json のエントリ状態。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FavoriteState {
    #[default]
    NotFavorite,
    Favorite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteEntry {
    pub normalized_path: String,
    pub file_size: u64,
    pub modified: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct FavoriteFile {
    #[serde(default)]
    entries: Vec<FavoriteEntry>,
}

/// Library 側の favorites 永続化ストア。
///
/// 保存先: `%LOCALAPPDATA%\cbz-viewer\favorites.json`
pub struct FavoriteStore {
    file_path: PathBuf,
    entries: Vec<FavoriteEntry>,
}

impl FavoriteStore {
    /// favorites.json を読み込む。ファイルがなければ空として扱う。
    pub fn load() -> Self {
        let file_path = favorites_json_path();
        let file =
            crate::infra::config_io::load_json_or_default::<FavoriteFile>(&file_path, "favorites");
        let mut store = Self {
            file_path,
            entries: file.entries,
        };
        let _ = store.compact();
        store
    }

    /// 現在の entries を favorites.json に保存する。
    pub fn save(&mut self) -> bool {
        let _ = self.compact();

        let file = FavoriteFile {
            entries: self.entries.clone(),
        };

        if let Some(parent) = self.file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            if std::fs::write(&self.file_path, json).is_ok() {
                return true;
            }
            tracing::warn!(path = %self.file_path.display(), "failed to write favorites json");
            return false;
        }
        tracing::warn!(path = %self.file_path.display(), "failed to serialize favorites json");
        false
    }

    /// normalized_path が favorites に含まれるか判定する。
    pub fn contains(&self, normalized_path: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.normalized_path == normalized_path)
    }

    /// 対象が存在すれば削除、存在しなければ追加する。
    pub fn toggle(&mut self, path: &Path) -> FavoriteState {
        let normalized_path = normalize_path_for_selection(path);
        let (file_size, modified) = read_entry_metadata(path);
        self.toggle_with_metadata_internal(normalized_path, file_size, modified)
    }

    /// metadata を明示して、お気に入り状態を切り替える。
    pub fn toggle_with_metadata(
        &mut self,
        path: &Path,
        file_size: u64,
        modified: u64,
    ) -> FavoriteState {
        let normalized_path = normalize_path_for_selection(path);
        self.toggle_with_metadata_internal(normalized_path, file_size, modified)
    }

    fn toggle_with_metadata_internal(
        &mut self,
        normalized_path: String,
        file_size: u64,
        modified: u64,
    ) -> FavoriteState {
        if self.remove_by_normalized_path(&normalized_path) {
            return FavoriteState::NotFavorite;
        }

        self.entries.push(FavoriteEntry {
            normalized_path,
            file_size,
            modified,
        });
        FavoriteState::Favorite
    }

    /// normalized_path に対応する favorite を削除する。
    pub fn remove(&mut self, path: &Path) -> bool {
        let normalized_path = normalize_path_for_selection(path);
        self.remove_by_normalized_path(&normalized_path)
    }

    pub fn rename_path(&mut self, old_path: &Path, new_path: &Path) -> bool {
        let old_normalized = normalize_path_for_selection(old_path);
        let new_normalized = normalize_path_for_selection(new_path);
        if old_normalized == new_normalized {
            return false;
        }
        let mut renamed = false;
        for entry in &mut self.entries {
            if entry.normalized_path == old_normalized {
                entry.normalized_path = new_normalized.clone();
                renamed = true;
            }
        }
        renamed
    }

    /// 存在しない normalized_path を削除する。
    ///
    /// 重複した normalized_path も同時に整理する。
    pub fn compact(&mut self) -> usize {
        let before = self.entries.len();
        let mut seen = HashSet::with_capacity(self.entries.len());
        self.entries.retain(|entry| {
            !entry.normalized_path.is_empty()
                && path_exists(&entry.normalized_path)
                && seen.insert(entry.normalized_path.clone())
        });
        before - self.entries.len()
    }

    fn remove_by_normalized_path(&mut self, normalized_path: &str) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|entry| entry.normalized_path != normalized_path);
        before != self.entries.len()
    }
}

#[cfg(test)]
impl FavoriteStore {
    pub(crate) fn from_entries(entries: Vec<FavoriteEntry>) -> Self {
        Self {
            file_path: PathBuf::new(),
            entries,
        }
    }
}

fn favorites_json_path() -> PathBuf {
    app_base_dir().join("favorites.json")
}

fn read_entry_metadata(path: &Path) -> (u64, u64) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return (0, 0);
    };

    let file_size = metadata.len();
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    (file_size, modified)
}

fn path_exists(normalized_path: &str) -> bool {
    !normalized_path.is_empty() && Path::new(normalized_path).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_for_tempfile(tempdir: &tempfile::TempDir) -> FavoriteStore {
        FavoriteStore {
            file_path: tempdir.path().join("favorites.json"),
            entries: Vec::new(),
        }
    }

    #[test]
    fn toggle_adds_and_removes_by_normalized_path() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let file_path = tempdir.path().join("book.cbz");
        std::fs::write(&file_path, b"abc").expect("write file");

        let mut store = store_for_tempfile(&tempdir);
        let normalized = normalize_path_for_selection(&file_path);

        assert_eq!(store.toggle(&file_path), FavoriteState::Favorite);
        assert!(store.contains(&normalized));

        assert_eq!(store.toggle(&file_path), FavoriteState::NotFavorite);
        assert!(!store.contains(&normalized));
    }

    #[test]
    fn remove_deletes_by_normalized_path() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let file_path = tempdir.path().join("book.cbz");
        std::fs::write(&file_path, b"abc").expect("write file");

        let mut store = store_for_tempfile(&tempdir);
        let normalized = normalize_path_for_selection(&file_path);
        store.entries.push(FavoriteEntry {
            normalized_path: normalized.clone(),
            file_size: 3,
            modified: 1,
        });

        assert!(store.remove(&file_path));
        assert!(!store.contains(&normalized));
        assert!(!store.remove(&file_path));
    }

    #[test]
    fn compact_removes_missing_and_duplicate_entries() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let existing = tempdir.path().join("book.cbz");
        std::fs::write(&existing, b"abc").expect("write file");
        let missing = tempdir.path().join("missing.cbz");

        let normalized_existing = normalize_path_for_selection(&existing);
        let normalized_missing = normalize_path_for_selection(&missing);

        let mut store = store_for_tempfile(&tempdir);
        store.entries = vec![
            FavoriteEntry {
                normalized_path: normalized_existing.clone(),
                file_size: 3,
                modified: 1,
            },
            FavoriteEntry {
                normalized_path: normalized_existing,
                file_size: 4,
                modified: 2,
            },
            FavoriteEntry {
                normalized_path: normalized_missing,
                file_size: 0,
                modified: 0,
            },
            FavoriteEntry {
                normalized_path: String::new(),
                file_size: 0,
                modified: 0,
            },
        ];

        let removed = store.compact();

        assert_eq!(removed, 3);
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].file_size, 3);
    }
}
