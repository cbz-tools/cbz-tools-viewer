use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use anyhow::Result;

use crate::domain::archive::BookId;
use crate::domain::page_map::{BookPageMap, SourceRevision};

#[derive(Debug)]
pub struct PageMapDiskCache {
    root: PathBuf,
}

impl PageMapDiskCache {
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
            .join("page_maps")
    }

    pub fn get_page_map_for_revision(
        &self,
        id: &BookId,
        revision: &SourceRevision,
    ) -> Option<BookPageMap> {
        if !revision.is_persistable() {
            return None;
        }
        let path = self.page_map_path(id, revision);
        let Ok(bytes) = std::fs::read(&path) else {
            return None;
        };
        match BookPageMap::decode_cache_bytes(&bytes, revision) {
            Ok(Some(map)) => Some(map),
            Ok(None) => {
                tracing::debug!(
                    id = %id.0.to_hex(),
                    source_revision = ?revision,
                    cache_path = %path.display(),
                    "page-map cache miss"
                );
                let _ = std::fs::remove_file(&path);
                None
            }
            Err(e) => {
                tracing::debug!(
                    id = %id.0.to_hex(),
                    source_revision = ?revision,
                    cache_path = %path.display(),
                    error = %e,
                    "page-map cache read failed"
                );
                None
            }
        }
    }

    pub fn put_page_map_bytes_for_revision(
        &self,
        id: &BookId,
        revision: &SourceRevision,
        data: &[u8],
    ) -> Result<()> {
        if !revision.is_persistable() {
            return Err(anyhow::anyhow!(
                "unknown source revision cannot be persisted"
            ));
        }
        let path = self.page_map_path(id, revision);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        atomic_write(&path, data)?;
        Ok(())
    }

    pub fn clear_all(&self) -> Result<()> {
        if self.root.exists() {
            std::fs::remove_dir_all(&self.root)?;
        }
        std::fs::create_dir_all(&self.root)?;
        Ok(())
    }

    pub fn remove_page_map(
        &self,
        id: &BookId,
        file_size: u64,
        modified: Option<SystemTime>,
    ) -> Result<()> {
        let revision = SourceRevision::from_file_state(file_size, modified);
        self.remove_page_map_for_revision(id, &revision)
    }

    pub fn remove_page_map_for_revision(
        &self,
        id: &BookId,
        revision: &SourceRevision,
    ) -> Result<()> {
        if !revision.is_persistable() {
            return Ok(());
        }
        let path = self.page_map_path(id, revision);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn remove_page_maps_by_id(&self, id: &BookId) -> Result<usize> {
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
                && name.ends_with(".pmap")
                && std::fs::remove_file(&path).is_ok()
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn rename_page_map_artifact(
        &self,
        old_id: &BookId,
        new_id: &BookId,
        file_size: u64,
        modified: Option<SystemTime>,
    ) -> Result<bool> {
        let revision = SourceRevision::from_file_state(file_size, modified);
        self.rename_page_map_artifact_for_revision(old_id, new_id, &revision)
    }

    pub fn rename_page_map_artifact_for_revision(
        &self,
        old_id: &BookId,
        new_id: &BookId,
        revision: &SourceRevision,
    ) -> Result<bool> {
        if !revision.is_persistable() {
            return Ok(false);
        }
        let old_path = self.page_map_path(old_id, revision);
        if !old_path.exists() {
            return Ok(false);
        }
        let new_path = self.page_map_path(new_id, revision);
        if let Some(dir) = new_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if new_path.exists() {
            let _ = std::fs::remove_file(&new_path);
        }
        std::fs::rename(&old_path, &new_path)?;
        Ok(true)
    }

    fn page_map_path(&self, id: &BookId, revision: &SourceRevision) -> PathBuf {
        let hex = id.0.to_hex();
        let prefix = &hex[..2];
        let Some((file_size, modified_ns)) = revision.persistable_key() else {
            return self
                .root
                .join(prefix)
                .join(format!("{}_invalid.pmap", &*hex));
        };
        self.root
            .join(prefix)
            .join(format!("{}_{}_{}.pmap", &*hex, file_size, modified_ns))
    }
}

fn atomic_write(path: &std::path::Path, data: &[u8]) -> Result<()> {
    let tmp_path = unique_temp_path(path);
    {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        use std::io::Write as _;
        file.write_all(data)?;
        file.flush()?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

fn unique_temp_path(path: &std::path::Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut tmp = path.to_path_buf();
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|ext| format!("{ext}.tmp"))
        .unwrap_or_else(|| "tmp".to_owned());
    tmp.set_extension(format!("{ext}.{pid}.{nanos}.{unique}"));
    tmp
}
