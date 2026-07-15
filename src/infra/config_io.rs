use std::{
    io::Write as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::de::DeserializeOwned;

/// 設定ファイルを同一ディレクトリの一時ファイル経由で置換する。
///
/// 呼出し側の設定所有者や更新タイミングは変えず、保存途中の終了で本体が
/// 部分書込み状態になることだけを防ぐ。
pub fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let temp_path = unique_temp_path(path);
    let result = (|| {
        {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp_path)?;
            file.write_all(data)?;
            file.flush()?;
            file.sync_all()?;
        }
        replace_file(&temp_path, path)
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn unique_temp_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut temp = path.to_path_buf();
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!("{extension}.tmp"))
        .unwrap_or_else(|| "tmp".to_owned());
    temp.set_extension(format!("{extension}.{pid}.{nanos}.{unique}"));
    temp
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temp_wide: Vec<u16> = temp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let path_wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: 両パスはこの呼出し中に生存する NUL 終端 UTF-16 文字列であり、同一ボリューム上にある。
    let moved = unsafe {
        MoveFileExW(
            temp_wide.as_ptr(),
            path_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, path: &Path) -> std::io::Result<()> {
    std::fs::rename(temp_path, path)
}

pub fn load_json_or_default<T>(path: &Path, label: &str) -> T
where
    T: DeserializeOwned + Default,
{
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<T>(&text) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    ?err,
                    path = %path.display(),
                    setting = label,
                    "failed to parse json settings; using default"
                );
                T::default()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(err) => {
            tracing::warn!(
                ?err,
                path = %path.display(),
                setting = label,
                "failed to read json settings; using default"
            );
            T::default()
        }
    }
}

pub fn load_toml_or_default<T>(path: &Path, label: &str) -> T
where
    T: DeserializeOwned + Default,
{
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let normalized = raw
                .trim_start_matches('\u{FEFF}')
                .replace("\r\n", "\n")
                .replace('\r', "\n");
            match toml::from_str::<T>(&normalized) {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        path = %path.display(),
                        setting = label,
                        "failed to parse toml settings; using default"
                    );
                    T::default()
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(err) => {
            tracing::warn!(
                ?err,
                path = %path.display(),
                setting = label,
                "failed to read toml settings; using default"
            );
            T::default()
        }
    }
}
