//! サムネイル用ディスクキャッシュ。
//!
//! `%LOCALAPPDATA%/cbz-viewer/thumbs` 配下へ保存し、
//! file size / modified を含めて中身違いを別キャッシュとして扱う。

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::domain::archive::BookId;

pub struct DiskCache {
    root: PathBuf,
}

impl DiskCache {
    pub fn open(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn default_root() -> PathBuf {
        let local = std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        local
            .join(crate::app_identity::app_data_dir())
            .join("thumbs")
    }

    // ── サムネイル専用 API ──────────────────────────────────────────────────

    pub fn get_thumb(
        &self,
        id: &BookId,
        file_size: u64,
        modified: Option<SystemTime>,
    ) -> Option<Vec<u8>> {
        std::fs::read(self.thumb_path(id, file_size, modified)).ok()
    }

    /// サムネキャッシュをすべて削除する（設定変更・手動クリア用）
    pub fn clear_all(&self) -> Result<()> {
        if self.root.exists() {
            std::fs::remove_dir_all(&self.root)?;
        }
        std::fs::create_dir_all(&self.root)?;
        Ok(())
    }

    pub fn put_thumb(
        &self,
        id: &BookId,
        file_size: u64,
        modified: Option<SystemTime>,
        webp: &[u8],
    ) -> Result<()> {
        let path = self.thumb_path(id, file_size, modified);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, webp)?;
        Ok(())
    }

    pub fn remove_thumb(
        &self,
        id: &BookId,
        file_size: u64,
        modified: Option<SystemTime>,
    ) -> Result<()> {
        let path = self.thumb_path(id, file_size, modified);
        if path.exists() {
            tracing::debug!(
                id = %id.0.to_hex(),
                file_size,
                modified_ns = modified_to_nanos(modified),
                cache_path = %path.display(),
                "disk-cache: remove thumb hit"
            );
            std::fs::remove_file(path)?;
        } else {
            tracing::debug!(
                id = %id.0.to_hex(),
                file_size,
                modified_ns = modified_to_nanos(modified),
                cache_path = %path.display(),
                "disk-cache: remove thumb miss"
            );
        }
        Ok(())
    }

    /// 指定 id に紐づくサムネイルを file_size / modified に依らず全削除する。
    pub fn remove_thumbs_by_id(&self, id: &BookId) -> Result<usize> {
        let hex = id.0.to_hex();
        let prefix = &hex[..2];
        let dir = self.root.join(prefix);
        if !dir.exists() {
            return Ok(0);
        }

        let mut removed = 0usize;
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if name.starts_with(hex.as_str())
                && name.ends_with(".webp")
                && std::fs::remove_file(&path).is_ok()
            {
                removed += 1;
            }
        }

        Ok(removed)
    }

    /// current revision 以外の同一 BookId のサムネイルを削除する。
    pub fn prune_thumbs_except(
        &self,
        id: &BookId,
        file_size: u64,
        modified: Option<SystemTime>,
    ) -> Result<usize> {
        let hex = id.0.to_hex();
        let dir = self.root.join(&hex[..2]);
        if !dir.exists() {
            return Ok(0);
        }
        let current_name = self
            .thumb_path(id, file_size, modified)
            .file_name()
            .map(|name| name.to_owned());

        let mut removed = 0usize;
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name() else {
                continue;
            };
            if current_name.as_deref() != Some(name)
                && name
                    .to_str()
                    .is_some_and(|name| name.starts_with(hex.as_str()) && name.ends_with(".webp"))
                && std::fs::remove_file(&path).is_ok()
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn rename_thumb_artifact(
        &self,
        old_id: &BookId,
        new_id: &BookId,
        file_size: u64,
        modified: Option<SystemTime>,
    ) -> Result<bool> {
        let old_path = self.thumb_path(old_id, file_size, modified);
        if !old_path.exists() {
            return Ok(false);
        }
        let new_path = self.thumb_path(new_id, file_size, modified);
        if let Some(dir) = new_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if new_path.exists() {
            let _ = std::fs::remove_file(&new_path);
        }
        std::fs::rename(&old_path, &new_path)?;
        Ok(true)
    }

    fn thumb_path(&self, id: &BookId, file_size: u64, modified: Option<SystemTime>) -> PathBuf {
        let hex = id.0.to_hex();
        let prefix = &hex[..2];
        let modified_ns = modified_to_nanos(modified);
        self.root
            .join(prefix)
            .join(format!("{}_{}_{}.webp", &*hex, file_size, modified_ns))
    }
}

fn modified_to_nanos(modified: Option<SystemTime>) -> u128 {
    modified
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
