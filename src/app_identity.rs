pub const PRODUCT_NAME: &str = "CBZ Viewer";
pub const APP_ID: &str = "cbz-viewer";
pub const LOG_TARGET: &str = "cbz_viewer";

pub const IPC_LOG_PREFIX: &str = "cbz-viewer-ipc-";
pub const VIEWER_STANDALONE_LOG_PREFIX: &str = "cbz-viewer-standalone-";

pub fn app_data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(APP_ID)
}
