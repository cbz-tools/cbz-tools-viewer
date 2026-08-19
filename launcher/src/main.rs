#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(windows)]
use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(windows)]
use sha2::{Digest, Sha256};

#[cfg(windows)]
include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));

#[cfg(windows)]
const APP_ID: &str = "cbz-viewer";
#[cfg(windows)]
const RUNTIME_SUBDIRECTORY: &str = "runtime";
#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            show_error(&format!("CBZ Viewer could not start.\n\n{error}"));
            std::process::exit(1);
        }
    }
}

#[cfg(windows)]
fn run() -> anyhow_free::Result<i32> {
    let runtime_dir = ensure_runtime()?;
    let core_path = runtime_dir.join("cbz-viewer-core.exe");
    Command::new(core_path)
        .args(std::env::args_os().skip(1))
        .spawn()
        .map_err(|error| {
            anyhow_free::Error::new(format!("failed to start core executable: {error}"))
        })?;
    Ok(0)
}

#[cfg(not(windows))]
fn run() -> anyhow_free::Result<i32> {
    Err(anyhow_free::Error::new(
        "the bundled launcher is only supported on Windows",
    ))
}

#[cfg(windows)]
fn ensure_runtime() -> anyhow_free::Result<PathBuf> {
    let runtime_dir = runtime_dir()?;
    fs::create_dir_all(&runtime_dir).map_err(|error| {
        anyhow_free::Error::new(format!("could not create runtime directory: {error}"))
    })?;

    let mut missing = Vec::new();
    for asset in ASSETS {
        let destination = runtime_dir.join(asset.name);
        if !is_valid_asset(&destination, asset) {
            missing.push(*asset);
        }
    }
    if missing.is_empty() {
        return Ok(runtime_dir);
    }

    let temporary_dir = make_temporary_directory(&runtime_dir)?;
    let result = (|| {
        for asset in &missing {
            let temporary_path = temporary_dir.join(asset.name);
            let mut file = File::options()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
                .map_err(|error| {
                    anyhow_free::Error::new(format!("could not stage {}: {error}", asset.name))
                })?;
            file.write_all(asset.bytes).map_err(|error| {
                anyhow_free::Error::new(format!("could not write {}: {error}", asset.name))
            })?;
            file.sync_all().map_err(|error| {
                anyhow_free::Error::new(format!("could not flush {}: {error}", asset.name))
            })?;
        }

        for asset in &missing {
            let temporary_path = temporary_dir.join(asset.name);
            let destination = runtime_dir.join(asset.name);
            if let Err(error) = replace_file_atomically(&temporary_path, &destination) {
                if is_valid_asset(&destination, asset) {
                    continue;
                }
                return Err(anyhow_free::Error::new(format!(
                    "could not install {} at {}: {error}; destination revalidation failed (missing or invalid)",
                    asset.name,
                    destination.display()
                )));
            }
        }

        for asset in ASSETS {
            if !is_valid_asset(&runtime_dir.join(asset.name), asset) {
                return Err(anyhow_free::Error::new(format!(
                    "installed asset failed validation: {}",
                    asset.name
                )));
            }
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&temporary_dir);
    result.map(|()| runtime_dir)
}

#[cfg(windows)]
fn runtime_dir() -> anyhow_free::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    Ok(base
        .join(APP_ID)
        .join(RUNTIME_SUBDIRECTORY)
        .join(env!("CARGO_PKG_VERSION")))
}

#[cfg(windows)]
fn is_valid_asset(path: &Path, asset: &EmbeddedAsset) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() != asset.size {
        return false;
    }
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    Sha256::digest(bytes).as_slice() == asset.sha256.as_slice()
}

#[cfg(windows)]
fn make_temporary_directory(runtime_dir: &Path) -> anyhow_free::Result<PathBuf> {
    let pid = std::process::id();
    for attempt in 0..32u32 {
        let candidate = runtime_dir.join(format!(".extract-{pid}-{attempt}"));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(anyhow_free::Error::new(format!(
                    "could not create temporary runtime directory: {error}"
                )));
            }
        }
    }
    Err(anyhow_free::Error::new(
        "could not allocate a unique temporary runtime directory",
    ))
}

#[cfg(windows)]
fn replace_file_atomically(source: &Path, destination: &Path) -> io::Result<()> {
    let source = wide_null(source.as_os_str());
    let destination = wide_null(destination.as_os_str());
    // SAFETY: both paths are valid, NUL-terminated UTF-16 strings owned for
    // the duration of this call; the API only replaces the named file.
    let ok = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn wide_null(value: &OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn show_error(message: &str) {
    #[cfg(windows)]
    {
        use std::ptr::null_mut;
        let text = wide_null(OsStr::new(message));
        let title = wide_null(OsStr::new("CBZ Viewer"));
        // SAFETY: both UTF-16 strings remain alive for the duration of the call.
        unsafe {
            MessageBoxW(
                null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    eprintln!("{message}");
}

#[cfg(windows)]
const MB_OK: u32 = 0x0000_0000;
#[cfg(windows)]
const MB_ICONERROR: u32 = 0x0000_0010;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(
        window: *mut core::ffi::c_void,
        text: *const u16,
        caption: *const u16,
        type_: u32,
    ) -> i32;
}

mod anyhow_free {
    pub struct Error(String);

    impl Error {
        pub fn new(message: impl Into<String>) -> Self {
            Self(message.into())
        }
    }

    impl std::fmt::Display for Error {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    pub type Result<T> = std::result::Result<T, Error>;
}
