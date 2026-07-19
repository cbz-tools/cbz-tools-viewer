use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

use crate::domain::archive::BookId;
use crate::domain::page_map::SourceRevision;

const ARTIFACT_FAILURE_SCHEMA_VERSION: u16 = 1;
const ARTIFACT_FAILURE_MAGIC: &[u8; 8] = b"CBZFAIL\0";
const ARTIFACT_FAILURE_HEADER_LEN: usize = 52;
const ARTIFACT_FAILURE_SOURCE_REVISION_OFFSET: usize = 20;
const ARTIFACT_FAILURE_SOURCE_REVISION_END: usize =
    ARTIFACT_FAILURE_SOURCE_REVISION_OFFSET + SourceRevision::ENCODED_LEN;

/// 同じ source revision で再生成を抑止する成果物種別。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactKind {
    PageMap,
    Thumbnail,
}

impl ArtifactKind {
    const fn flag(self) -> u8 {
        match self {
            Self::PageMap => 0b0000_0001,
            Self::Thumbnail => 0b0000_0010,
        }
    }
}

/// Page Map / サムネイルの終端失敗を source revision ごとに保持するディスクキャッシュ。
///
/// `.pmap` と同じ BookId shard / revision filename を使い、固定長ヘッダ内のフラグだけを保持する。
#[derive(Debug)]
pub struct ArtifactFailureDiskCache {
    root: PathBuf,
}

impl ArtifactFailureDiskCache {
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
            .join("artifact_failures")
    }

    pub fn has_failure_for_revision(
        &self,
        id: &BookId,
        revision: &SourceRevision,
        artifact: ArtifactKind,
    ) -> bool {
        self.read_flags_for_revision(id, revision, true)
            .is_some_and(|flags| flags & artifact.flag() != 0)
    }

    pub fn mark_failure_for_revision(
        &self,
        id: &BookId,
        revision: &SourceRevision,
        artifact: ArtifactKind,
    ) -> Result<bool> {
        if !revision.is_persistable() {
            return Ok(false);
        }
        let current_flags = self
            .read_flags_for_revision(id, revision, true)
            .unwrap_or(0);
        let flags = current_flags | artifact.flag();
        if flags == current_flags {
            return Ok(false);
        }
        self.write_flags_for_revision(id, revision, flags)?;
        Ok(true)
    }

    pub fn clear_failure_for_revision(
        &self,
        id: &BookId,
        revision: &SourceRevision,
        artifact: ArtifactKind,
    ) -> Result<bool> {
        let Some(flags) = self.read_flags_for_revision(id, revision, true) else {
            return Ok(false);
        };
        if flags & artifact.flag() == 0 {
            return Ok(false);
        }
        let flags = flags & !artifact.flag();
        if flags == 0 {
            self.remove_for_revision(id, revision)?;
        } else {
            self.write_flags_for_revision(id, revision, flags)?;
        }
        Ok(true)
    }

    pub fn clear_all(&self) -> Result<()> {
        if self.root.exists() {
            std::fs::remove_dir_all(&self.root)?;
        }
        std::fs::create_dir_all(&self.root)?;
        Ok(())
    }

    pub fn remove_for_revision(&self, id: &BookId, revision: &SourceRevision) -> Result<()> {
        if !revision.is_persistable() {
            return Ok(());
        }
        let path = self.failure_path(id, revision);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn remove_by_id(&self, id: &BookId) -> Result<usize> {
        let hex = id.0.to_hex();
        let dir = self.root.join(&hex[..2]);
        if !dir.exists() {
            return Ok(0);
        }

        let mut removed = 0usize;
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(hex.as_str()) && name.ends_with(".fail"))
                && std::fs::remove_file(&path).is_ok()
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// current revision 以外の同一 BookId の失敗キャッシュを削除する。
    pub fn prune_failures_except_revision(
        &self,
        id: &BookId,
        revision: &SourceRevision,
    ) -> Result<usize> {
        if !revision.is_persistable() {
            return Ok(0);
        }
        let hex = id.0.to_hex();
        let dir = self.root.join(&hex[..2]);
        if !dir.exists() {
            return Ok(0);
        }
        let current_name = self
            .failure_path(id, revision)
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
                    .is_some_and(|name| name.starts_with(hex.as_str()) && name.ends_with(".fail"))
                && std::fs::remove_file(&path).is_ok()
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn rename_artifact_for_revision(
        &self,
        old_id: &BookId,
        new_id: &BookId,
        revision: &SourceRevision,
    ) -> Result<bool> {
        if !revision.is_persistable() {
            return Ok(false);
        }
        let old_path = self.failure_path(old_id, revision);
        if !old_path.exists() {
            return Ok(false);
        }
        let new_path = self.failure_path(new_id, revision);
        if let Some(dir) = new_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if new_path.exists() {
            let _ = std::fs::remove_file(&new_path);
        }
        std::fs::rename(old_path, new_path)?;
        Ok(true)
    }

    fn read_flags_for_revision(
        &self,
        id: &BookId,
        revision: &SourceRevision,
        remove_invalid_entry: bool,
    ) -> Option<u8> {
        if !revision.is_persistable() {
            return None;
        }
        let path = self.failure_path(id, revision);
        let Ok(bytes) = std::fs::read(&path) else {
            return None;
        };
        let flags = decode_flags(&bytes, revision);
        if flags.is_none() && remove_invalid_entry {
            let _ = std::fs::remove_file(path);
        }
        flags
    }

    fn write_flags_for_revision(
        &self,
        id: &BookId,
        revision: &SourceRevision,
        flags: u8,
    ) -> Result<()> {
        let path = self.failure_path(id, revision);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        atomic_write(&path, &encode_flags(revision, flags))
    }

    fn failure_path(&self, id: &BookId, revision: &SourceRevision) -> PathBuf {
        let hex = id.0.to_hex();
        let prefix = &hex[..2];
        let Some((file_size, modified_ns)) = revision.persistable_key() else {
            return self
                .root
                .join(prefix)
                .join(format!("{}_invalid.fail", &*hex));
        };
        self.root
            .join(prefix)
            .join(format!("{}_{}_{}.fail", &*hex, file_size, modified_ns))
    }
}

fn encode_flags(revision: &SourceRevision, flags: u8) -> Vec<u8> {
    // magic(8) + schema(2) + flags(1) + reserved(9) + revision(24) + reserved(8)
    let mut out = Vec::with_capacity(ARTIFACT_FAILURE_HEADER_LEN);
    out.extend_from_slice(ARTIFACT_FAILURE_MAGIC);
    out.extend_from_slice(&ARTIFACT_FAILURE_SCHEMA_VERSION.to_le_bytes());
    out.push(flags);
    out.push(0);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    revision.encode_into(&mut out);
    out.extend_from_slice(&[0u8; 8]);
    out
}

fn decode_flags(data: &[u8], expected_revision: &SourceRevision) -> Option<u8> {
    if data.len() != ARTIFACT_FAILURE_HEADER_LEN
        || &data[0..8] != ARTIFACT_FAILURE_MAGIC
        || !expected_revision.is_persistable()
    {
        return None;
    }
    let version = u16::from_le_bytes(data[8..10].try_into().ok()?);
    let flags = data[10];
    if version != ARTIFACT_FAILURE_SCHEMA_VERSION
        || flags == 0
        || data[11] != 0
        || data[12..20].iter().any(|&byte| byte != 0)
        || data[ARTIFACT_FAILURE_SOURCE_REVISION_END..]
            .iter()
            .any(|&byte| byte != 0)
    {
        return None;
    }
    let revision = SourceRevision::decode(
        &data[ARTIFACT_FAILURE_SOURCE_REVISION_OFFSET..ARTIFACT_FAILURE_SOURCE_REVISION_END],
    )?;
    (revision == *expected_revision).then_some(flags)
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
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Err(error) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error.into());
    }
    Ok(())
}

fn unique_temp_path(path: &std::path::Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut tmp = path.to_path_buf();
    tmp.set_extension(format!("fail.tmp.{pid}.{nanos}.{unique}"));
    tmp
}
