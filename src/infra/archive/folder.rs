use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{OnceLock, RwLock},
};

use anyhow::{Context, Result};
use bytes::Bytes;

use crate::util::{archive_path::is_supported_image_path, natural_sort};
use crate::util::path_eq::normalize_path_for_override;

use super::BookReader;

#[derive(Clone, Debug)]
struct FolderOrderOverride {
    ordered_images: Vec<PathBuf>,
}

static VIEWER_FOLDER_ORDER_OVERRIDES: OnceLock<RwLock<std::collections::HashMap<String, FolderOrderOverride>>> =
    OnceLock::new();

pub struct FolderImageReader {
    image_paths: Vec<PathBuf>,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug)]
pub(crate) struct FolderImageEntryInfo<'a> {
    pub page_index: u32,
    pub path: &'a Path,
}

impl FolderImageReader {
    /// DIR 本はファイル名の自然順で読書順を決める。
    pub fn open(path: &Path) -> Result<Self> {
        let mut image_paths: Vec<PathBuf> = fs::read_dir(path)
            .with_context(|| format!("read_dir: {}", path.display()))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|entry_path| entry_path.is_file() && is_supported_image_path(entry_path))
            .collect();

        image_paths.sort_by(|a, b| {
            let a_name = file_name_for_sort(a);
            let b_name = file_name_for_sort(b);
            natural_sort::compare(a_name, b_name)
        });

        tracing::debug!(
            path = %path.display(),
            image_count = image_paths.len(),
            "folder_reader: open complete"
        );

        Ok(Self { image_paths })
    }

    pub fn open_with_order(folder: &Path, ordered_images: Vec<PathBuf>) -> Result<Self> {
        validate_ordered_images(folder, &ordered_images)?;
        tracing::debug!(
            path = %folder.display(),
            image_count = ordered_images.len(),
            "folder_reader: open complete with snapshot order"
        );
        Ok(Self {
            image_paths: ordered_images,
        })
    }

    pub fn open_for_viewer(path: &Path) -> Result<Self> {
        let normalized = normalize_path_for_override(path);
        let override_map = VIEWER_FOLDER_ORDER_OVERRIDES.get_or_init(Default::default);
        if let Some(override_entry) = override_map.read().unwrap().get(&normalized).cloned() {
            return Self::open_with_order(path, override_entry.ordered_images);
        }
        Self::open(path)
    }

    pub fn install_viewer_order_override(folder: &Path, ordered_images: Vec<PathBuf>) -> Result<()> {
        validate_ordered_images(folder, &ordered_images)?;
        let normalized = normalize_path_for_override(folder);
        let override_map = VIEWER_FOLDER_ORDER_OVERRIDES.get_or_init(Default::default);
        override_map.write().unwrap().insert(
            normalized,
            FolderOrderOverride { ordered_images },
        );
        Ok(())
    }

    pub fn clear_viewer_order_override(folder: &Path) {
        let normalized = normalize_path_for_override(folder);
        if let Some(override_map) = VIEWER_FOLDER_ORDER_OVERRIDES.get() {
            override_map.write().unwrap().remove(&normalized);
        }
    }

    fn read_path(&self, idx: usize) -> Result<Bytes> {
        let path = self
            .image_paths
            .get(idx)
            .with_context(|| format!("index out of range: {idx}"))?;
        let bytes = fs::read(path).with_context(|| format!("read: {}", path.display()))?;
        Ok(Bytes::from(bytes))
    }

    pub fn page_index_for_path(&self, path: &Path) -> Option<u32> {
        self.image_paths
            .iter()
            .position(|candidate| candidate == path)
            .map(|idx| idx as u32)
    }

    pub(crate) fn page_display_labels(&self) -> Vec<String> {
        self.image_paths
            .iter()
            .map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| path.display().to_string())
            })
            .collect()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    /// Page Map / thumbnail 生成が使う画像エントリ順を返す。
    pub(crate) fn page_map_image_entry_infos(
        &self,
    ) -> impl Iterator<Item = FolderImageEntryInfo<'_>> + '_ {
        self.image_paths
            .iter()
            .enumerate()
            .map(|(page_index, path)| FolderImageEntryInfo {
                page_index: page_index as u32,
                path,
            })
    }
}

fn validate_ordered_images(folder: &Path, ordered_images: &[PathBuf]) -> Result<()> {
    if ordered_images.is_empty() {
        anyhow::bail!("ordered_images is empty");
    }
    let normalized_folder = normalize_path_for_override(folder);
    let mut seen = HashSet::with_capacity(ordered_images.len());
    for path in ordered_images {
        if !path.is_file() {
            anyhow::bail!("ordered image is not a file: {}", path.display());
        }
        if !is_supported_image_path(path) {
            anyhow::bail!("ordered image is not a supported image: {}", path.display());
        }
        let Some(parent) = path.parent() else {
            anyhow::bail!("ordered image has no parent: {}", path.display());
        };
        if normalize_path_for_override(parent) != normalized_folder {
            anyhow::bail!(
                "ordered image is outside folder: {} not in {}",
                path.display(),
                folder.display()
            );
        }
        let normalized = normalize_path_for_override(path);
        if !seen.insert(normalized) {
            anyhow::bail!("ordered image is duplicated: {}", path.display());
        }
    }
    Ok(())
}

impl BookReader for FolderImageReader {
    fn read_first_image(&self) -> Result<Bytes> {
        self.read_path(0)
    }

    fn page_count(&self) -> u32 {
        self.image_paths.len() as u32
    }

    fn read_page_n(&self, n: u32) -> Result<Bytes> {
        self.read_path(n as usize)
    }
}

fn file_name_for_sort(path: &Path) -> &str {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_reader_orders_images_naturally() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("10.jpg"), b"10").unwrap();
        fs::write(temp.path().join("2.jpg"), b"2").unwrap();
        fs::write(temp.path().join("001.png"), b"1").unwrap();
        fs::write(temp.path().join("note.txt"), b"x").unwrap();

        let reader = FolderImageReader::open(temp.path()).unwrap();
        assert_eq!(reader.page_count(), 3);
        assert_eq!(&reader.read_first_image().unwrap()[..], b"1");
        assert_eq!(&reader.read_page_n(1).unwrap()[..], b"2");
        assert_eq!(&reader.read_page_n(2).unwrap()[..], b"10");
    }

    #[test]
    fn folder_reader_rejects_missing_pages() {
        let temp = tempfile::tempdir().unwrap();
        let reader = FolderImageReader::open(temp.path()).unwrap();
        assert_eq!(reader.page_count(), 0);
        assert!(reader.read_first_image().is_err());
    }
}
