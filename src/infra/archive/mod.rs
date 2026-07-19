pub(crate) mod cbz_rebuild;
pub(crate) mod cbz_rebuild_transaction;
pub mod epub;
pub mod folder;
pub mod page_map;
pub mod rar;
pub mod zip;

use anyhow::Result;
use bytes::Bytes;
use std::{
    collections::HashSet,
    fs::OpenOptions,
    io::{Seek, Write},
    path::Path,
};
use zip_writer::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookSourceKind {
    Zip,
    Rar,
    Epub,
    Folder,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CbzRebuildArchiveEntryKind {
    Image,
    NonImage,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CbzRebuildArchiveEntry {
    pub name: String,
    pub kind: CbzRebuildArchiveEntryKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CbzRebuildArchiveSelection {
    pub keep_entries: Vec<CbzRebuildArchiveEntry>,
    pub delete_entries: Vec<CbzRebuildArchiveEntry>,
    pub remaining_image_entry_count: usize,
}

/// §2 BookReader trait：`&self` で並列呼出可
pub trait BookReader: Send + Sync {
    fn read_first_image(&self) -> Result<Bytes>;
    /// natural sort 順の画像ページ数
    fn page_count(&self) -> u32;
    /// natural sort 順で n 番目（0-indexed）の画像を返す
    fn read_page_n(&self, n: u32) -> Result<Bytes>;
}

/// パスから本リーダーを開く
pub fn open_book_reader(path: &Path) -> Result<Box<dyn BookReader>> {
    match book_source_kind(path) {
        BookSourceKind::Folder => Ok(Box::new(folder::FolderImageReader::open(path)?)),
        BookSourceKind::Rar => Ok(Box::new(rar::RarReader::open(path)?)),
        BookSourceKind::Epub => Ok(Box::new(epub::EpubImageReader::open(path)?)),
        BookSourceKind::Zip | BookSourceKind::Unsupported => {
            Ok(Box::new(zip::ZipReader::open(path)?))
        }
    }
}

pub fn book_source_kind(path: &Path) -> BookSourceKind {
    if path.is_dir() {
        return BookSourceKind::Folder;
    }

    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return BookSourceKind::Unsupported;
    };
    match ext.to_ascii_lowercase().as_str() {
        "zip" | "cbz" => BookSourceKind::Zip,
        "rar" | "cbr" => BookSourceKind::Rar,
        "epub" => BookSourceKind::Epub,
        _ => BookSourceKind::Unsupported,
    }
}

pub fn viewer_page_display_labels(path: &Path) -> Result<Vec<String>> {
    match book_source_kind(path) {
        BookSourceKind::Folder => {
            Ok(folder::FolderImageReader::open_for_viewer(path)?.page_display_labels())
        }
        BookSourceKind::Zip | BookSourceKind::Unsupported => {
            Ok(zip::ZipReader::open(path)?.page_display_labels())
        }
        BookSourceKind::Rar => Ok(rar::RarReader::open(path)?.page_display_labels()),
        BookSourceKind::Epub => Ok(epub::EpubImageReader::open(path)?.page_display_labels()),
    }
}

pub fn viewer_page_entry_names(path: &Path) -> Result<Vec<String>> {
    match book_source_kind(path) {
        BookSourceKind::Folder => {
            Ok(folder::FolderImageReader::open_for_viewer(path)?.page_entry_names())
        }
        BookSourceKind::Zip | BookSourceKind::Unsupported => {
            Ok(zip::ZipReader::open(path)?.page_entry_names())
        }
        BookSourceKind::Rar => Ok(rar::RarReader::open(path)?.page_entry_names()),
        BookSourceKind::Epub => Ok(epub::EpubImageReader::open(path)?.page_entry_names()),
    }
}

pub fn collect_cbz_rebuild_archive_selection(
    path: &Path,
    delete_entries: &[String],
) -> Result<CbzRebuildArchiveSelection> {
    let archive_entries = match book_source_kind(path) {
        BookSourceKind::Zip => zip::list_cbz_rebuild_entries(path)?,
        BookSourceKind::Rar => rar::list_cbz_rebuild_entries(path)?,
        BookSourceKind::Epub => anyhow::bail!("cbz rebuild does not support epub"),
        BookSourceKind::Folder => anyhow::bail!("cbz rebuild does not support folders"),
        BookSourceKind::Unsupported => anyhow::bail!("cbz rebuild only supports zip/cbz/rar/cbr"),
    };

    let requested_delete_names: HashSet<&str> = delete_entries.iter().map(String::as_str).collect();

    for requested_name in &requested_delete_names {
        if !archive_entries
            .iter()
            .any(|entry| entry.name.as_str() == *requested_name)
        {
            anyhow::bail!("cbz rebuild delete entry not found: {}", requested_name);
        }
    }

    let mut keep_entries = Vec::new();
    let mut deleted_entries = Vec::new();
    let mut remaining_image_entry_count = 0usize;

    for entry in archive_entries {
        let delete_requested = requested_delete_names.contains(entry.name.as_str());
        match entry.kind {
            CbzRebuildArchiveEntryKind::Image if delete_requested => deleted_entries.push(entry),
            CbzRebuildArchiveEntryKind::Image => {
                remaining_image_entry_count += 1;
                keep_entries.push(entry);
            }
            CbzRebuildArchiveEntryKind::NonImage | CbzRebuildArchiveEntryKind::Directory => {
                keep_entries.push(entry);
            }
        }
    }

    if remaining_image_entry_count == 0 {
        anyhow::bail!("cbz rebuild would leave no image entries");
    }

    Ok(CbzRebuildArchiveSelection {
        keep_entries,
        delete_entries: deleted_entries,
        remaining_image_entry_count,
    })
}

pub fn write_cbz_rebuild_tmp_archive(
    input_path: &Path,
    tmp_path: &Path,
    selection: &CbzRebuildArchiveSelection,
) -> Result<()> {
    let mut created_tmp = false;
    let result = (|| -> Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(tmp_path)?;
        created_tmp = true;

        let mut writer = ZipWriter::new(file);
        match book_source_kind(input_path) {
            BookSourceKind::Zip => zip::write_cbz_rebuild_keep_entries(
                input_path,
                &selection.keep_entries,
                &mut writer,
            )?,
            BookSourceKind::Rar => rar::write_cbz_rebuild_keep_entries(
                input_path,
                &selection.keep_entries,
                &mut writer,
            )?,
            BookSourceKind::Epub => anyhow::bail!("cbz rebuild does not support epub"),
            BookSourceKind::Folder => anyhow::bail!("cbz rebuild does not support folders"),
            BookSourceKind::Unsupported => {
                anyhow::bail!("cbz rebuild only supports zip/cbz/rar/cbr")
            }
        }
        writer.finish()?;
        Ok(())
    })();

    if result.is_err() && created_tmp {
        let _ = std::fs::remove_file(tmp_path);
    }
    result
}

pub(crate) fn cbz_rebuild_file_options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
}

pub(crate) fn cbz_rebuild_directory_options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
}

pub(crate) fn write_cbz_rebuild_directory_entry<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    entry_name: &str,
) -> Result<()> {
    writer.add_directory(entry_name, cbz_rebuild_directory_options())?;
    Ok(())
}

pub(crate) fn write_cbz_rebuild_file_entry<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    entry_name: &str,
    bytes: &[u8],
) -> Result<()> {
    writer.start_file(entry_name, cbz_rebuild_file_options())?;
    writer.write_all(bytes)?;
    Ok(())
}
