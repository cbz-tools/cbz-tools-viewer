use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::archive::BookId;
use crate::domain::archive_settings::ReadingState;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ViewerToLibrary {
    RequestViewerState {
        request_id: u64,
        current_path: PathBuf,
    },
    FavoriteToggle {
        request_id: u64,
        current_path: PathBuf,
    },
    RequestAdjacentBooks {
        request_id: u64,
        kind: AdjacentBooksKind,
    },
    RequestNextBook {
        request_id: u64,
    },
    RequestPrevBook {
        request_id: u64,
    },
    DeleteAndNext {
        request_id: u64,
        book_id: BookId,
    },
    RebuildSelectedImagesAsCbzAndNext {
        request_id: u64,
        book_id: BookId,
        delete_entries: Vec<String>,
    },
    ReadingSessionFinished {
        request_id: u64,
        book_path: PathBuf,
        displayed_any_page: bool,
        reached_end: bool,
        resume_page: Option<usize>,
        page_count: usize,
    },
    Delete {
        request_id: u64,
        book_id: BookId,
    },
    // Viewer 側の明示的 close 通知のために予約してある IPC メッセージ。
    // 現在の本運用では pipe 切断を close として扱う。
    #[allow(dead_code)]
    Closed,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ViewerBookState {
    pub favorite_state: ViewerFavoriteState,
    pub reading_state: ReadingState,
    pub start_page: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdjacentBook {
    pub path: PathBuf,
    pub book_state: ViewerBookState,
    pub page_count: Option<u32>,
}

/// DeleteAndNext の遷移先に対して同期的に返す SPAD 候補。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeleteAndNextSpadTargets {
    pub prev: Option<AdjacentBook>,
    pub next: Option<AdjacentBook>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageOrderSnapshot {
    pub folder: PathBuf,
    pub start_image: PathBuf,
    pub ordered_images: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LibraryToViewer {
    ResponseViewerState {
        request_id: u64,
        book_state: ViewerBookState,
        #[serde(default)]
        image_order_snapshot: Option<ImageOrderSnapshot>,
    },
    FavoriteToggleResponse {
        request_id: u64,
        favorite_state: ViewerFavoriteState,
    },
    Deleted {
        request_id: u64,
        deleted_path: PathBuf,
        next_path: Option<PathBuf>,
        next_book_state: Option<ViewerBookState>,
        /// DeleteAndNext の遷移先に対する SPAD 候補。None は旧Library応答との互換用。
        #[serde(default)]
        spad_targets: Option<Box<DeleteAndNextSpadTargets>>,
    },
    NavigateTo {
        request_id: u64,
        path: PathBuf,
        book_state: ViewerBookState,
    },
    ReadingSessionFinishedAck {
        request_id: u64,
    },
    AdjacentBooks {
        request_id: u64,
        kind: AdjacentBooksKind,
        prev: Option<AdjacentBook>,
        next: Option<AdjacentBook>,
    },
    NoMoreBooks {
        request_id: u64,
    },
    Error {
        request_id: u64,
        code: IpcErrorCode,
        retryable: bool,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AdjacentBooksKind {
    DeleteDialog,
    BoundaryPreview,
    Spad,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum IpcErrorCode {
    DeleteFailed,
    // IPC エラー契約の互換性と将来拡張のために予約してある。
    #[allow(dead_code)]
    AccessDenied,
    FileNotFound,
    // 一時的な snapshot 状態のために予約してある。retryable 契約に残す。
    #[allow(dead_code)]
    SnapshotUnavailable,
    SnapshotPathMismatch,
    // 将来の IPC コマンドの request 検証失敗に備えて予約してある。
    #[allow(dead_code)]
    InvalidRequest,
    // 将来互換のための IPC エラー mapping の受け皿として予約してある。
    #[allow(dead_code)]
    Unknown,
}

impl IpcErrorCode {
    pub fn retryable(&self) -> bool {
        matches!(self, Self::SnapshotUnavailable | Self::SnapshotPathMismatch)
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ViewerFavoriteState {
    #[default]
    Unknown,
    Off,
    On,
}
