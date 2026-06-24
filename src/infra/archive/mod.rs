pub mod epub;
pub mod folder;
pub mod page_map;
pub mod rar;
pub mod zip;

use anyhow::Result;
use bytes::Bytes;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookSourceKind {
    Zip,
    Rar,
    Epub,
    Folder,
    Unsupported,
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
        BookSourceKind::Folder => Ok(folder::FolderImageReader::open_for_viewer(path)?.page_display_labels()),
        BookSourceKind::Zip | BookSourceKind::Unsupported => {
            Ok(zip::ZipReader::open(path)?.page_display_labels())
        }
        BookSourceKind::Rar => Ok(rar::RarReader::open(path)?.page_display_labels()),
        BookSourceKind::Epub => Ok(epub::EpubImageReader::open(path)?.page_display_labels()),
    }
}
