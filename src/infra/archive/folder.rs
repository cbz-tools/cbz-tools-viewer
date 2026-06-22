use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use bytes::Bytes;

use crate::util::{archive_path::is_supported_image_path, natural_sort};

use super::BookReader;

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
