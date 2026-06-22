use std::{path::Path, sync::Arc, time::SystemTime};

use blake3::Hash;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BookId(pub Hash);

impl BookId {
    pub fn from_path(path: &Path) -> Self {
        let bytes = path.to_string_lossy();
        Self(blake3::hash(bytes.as_bytes()))
    }
}

impl Serialize for BookId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.0.to_hex().as_str())
    }
}

impl<'de> Deserialize<'de> for BookId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let hash = Hash::from_hex(s).map_err(serde::de::Error::custom)?;
        Ok(Self(hash))
    }
}

#[derive(Clone, Debug)]
pub struct BookMeta {
    pub id: BookId,
    pub path: Arc<Path>,
    pub title: Arc<str>,
    pub size: u64,
    pub modified: SystemTime,
    pub page_count: Option<u32>, // 未スキャン時は None
}

#[derive(Clone, Debug)]
pub struct FolderMeta {
    pub path: Arc<Path>,
    pub title: Arc<str>,
    pub modified: SystemTime,
}

#[derive(Clone, Debug)]
pub struct ImageFileMeta {
    pub path: Arc<Path>,
    pub title: Arc<str>,
    pub size: u64,
    pub modified: SystemTime,
}

#[derive(Clone, Debug)]
/// Library の1件分の実体。
///
/// `Archive` は書庫、`FolderBook` は直下に画像を持つフォルダ、`ImageFile` は
/// 画像本への入口、`Folder` はナビゲーション対象外の通常フォルダ。
pub enum LibraryEntry {
    Folder(FolderMeta),
    FolderBook(FolderMeta),
    ImageFile(ImageFileMeta),
    Archive(BookMeta),
}

impl LibraryEntry {
    /// お気に入りやグループ設定のような「本単位」の操作に乗るのは、
    /// 現状は Archive と FolderBook だけ。
    pub fn is_favorite_target(&self) -> bool {
        matches!(self, LibraryEntry::Archive(_) | LibraryEntry::FolderBook(_))
    }

    /// サムネイル / navigation で本を同一視するための安定キー。
    /// Folder は本ではないので None、ImageFile は親フォルダの画像本と分けるため
    /// 自身の path をキーにする。
    pub fn thumb_id(&self) -> Option<BookId> {
        match self {
            LibraryEntry::Archive(entry) => Some(entry.id.clone()),
            LibraryEntry::FolderBook(entry) => Some(BookId::from_path(entry.path.as_ref())),
            LibraryEntry::ImageFile(entry) => Some(BookId::from_path(entry.path.as_ref())),
            LibraryEntry::Folder(_) => None,
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            LibraryEntry::Folder(entry) => entry.path.as_ref(),
            LibraryEntry::FolderBook(entry) => entry.path.as_ref(),
            LibraryEntry::ImageFile(entry) => entry.path.as_ref(),
            LibraryEntry::Archive(entry) => entry.path.as_ref(),
        }
    }

    pub fn title(&self) -> &str {
        match self {
            LibraryEntry::Folder(entry) => entry.title.as_ref(),
            LibraryEntry::FolderBook(entry) => entry.title.as_ref(),
            LibraryEntry::ImageFile(entry) => entry.title.as_ref(),
            LibraryEntry::Archive(entry) => entry.title.as_ref(),
        }
    }

    pub fn modified(&self) -> SystemTime {
        match self {
            LibraryEntry::Folder(entry) => entry.modified,
            LibraryEntry::FolderBook(entry) => entry.modified,
            LibraryEntry::ImageFile(entry) => entry.modified,
            LibraryEntry::Archive(entry) => entry.modified,
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            LibraryEntry::ImageFile(entry) => entry.size,
            LibraryEntry::Archive(entry) => entry.size,
            LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_) => 0,
        }
    }

    pub fn page_count(&self) -> Option<u32> {
        match self {
            LibraryEntry::Archive(entry) => entry.page_count,
            LibraryEntry::Folder(_) | LibraryEntry::FolderBook(_) | LibraryEntry::ImageFile(_) => {
                None
            }
        }
    }
}
