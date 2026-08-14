use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    println!("cargo:rerun-if-changed=third_party/dav1d/dav1d.dll");
    println!("cargo:rerun-if-changed=third_party/dav1d/LICENSE");
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-env-changed=CARGO_TARGET_DIR");
    println!("cargo:rerun-if-changed=assets/viewer_icon.ico");
    println!("cargo:rerun-if-changed=assets/viewer_icon.png");

    embed_windows_icon();
    maybe_copy_ffmpeg_runtime_dlls();
    maybe_copy_dav1d_dll();
}

fn embed_windows_icon() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/viewer_icon.ico");
    if let Err(err) = res.compile() {
        panic!("failed to embed Windows icon resource: {err}");
    }
}

fn copy_dll(source: &Path, destination: &Path) -> io::Result<()> {
    if !source.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("missing source DLL: {}", source.display()),
        ));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)?;
    Ok(())
}

fn maybe_copy_dav1d_dll() {
    if env::var_os("CARGO_FEATURE_AVIF").is_none() {
        return;
    }

    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let mut copied = false;
    for source in candidate_dav1d_dll_paths() {
        if !source.exists() {
            continue;
        }

        let exe_dir_destination = Path::new("target").join(&profile).join("dav1d.dll");

        match copy_dll(&source, &exe_dir_destination) {
            Ok(_) => {
                eprintln!(
                    "copied dav1d.dll from '{}' to '{}'",
                    source.display(),
                    exe_dir_destination.display()
                );
                copied = true;
                break;
            }
            Err(err) => {
                eprintln!(
                    "failed to copy dav1d.dll from '{}': {}",
                    source.display(),
                    err
                );
            }
        }
    }

    if !copied {
        eprintln!(
            "dav1d.dll was not found. AVIF decode may fail at runtime unless dav1d.dll is on PATH."
        );
    }
}

/// ff-sys links FFmpeg dynamically on Windows. Copy only the runtime closure
/// proven by the release executable's PE imports next to the executable so
/// release packages do not depend on a developer machine's PATH.
fn maybe_copy_ffmpeg_runtime_dlls() {
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let vcpkg_root = env::var("VCPKG_ROOT").unwrap_or_else(|_| "C:\\vcpkg".to_string());
    let bin_dir = Path::new(&vcpkg_root)
        .join("installed")
        .join("x64-windows")
        .join("bin");
    if !bin_dir.is_dir() {
        panic!(
            "FFmpeg runtime DLL directory not found: {}. Install ffmpeg:x64-windows via vcpkg",
            bin_dir.display()
        );
    }

    // OUT_DIR is authoritative even when Cargo is invoked with
    // `--target-dir`; walking from `.../<profile>/build/<pkg>/out` keeps the
    // runtime beside the executable in both the default and custom target
    // directories.
    let destination_dir = env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .and_then(|out_dir| {
            out_dir
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| {
            let target_dir = env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("target"));
            target_dir.join(profile)
        });
    // ff-sys still requires avfilter.lib at link time, but the release
    // executable does not import avfilter-*.dll. The runtime closure is:
    // avformat -> avcodec -> swresample, with avutil and swscale also
    // imported directly by the executable or by that closure.
    const RUNTIME_PREFIXES: [&str; 5] = [
        "avcodec-",
        "avformat-",
        "avutil-",
        "swresample-",
        "swscale-",
    ];

    // Remove stale FFmpeg DLLs from a reused target directory, including an
    // old avfilter DLL from the former six-DLL packaging rule. Do not remove
    // arbitrary target files.
    const KNOWN_FFMPEG_PREFIXES: [&str; 8] = [
        "avcodec-",
        "avdevice-",
        "avfilter-",
        "avformat-",
        "avresample-",
        "avutil-",
        "swresample-",
        "swscale-",
    ];
    fs::create_dir_all(&destination_dir).unwrap_or_else(|err| {
        panic!(
            "failed to create FFmpeg runtime destination directory {}: {err}",
            destination_dir.display()
        )
    });
    let destination_entries = fs::read_dir(&destination_dir).unwrap_or_else(|err| {
        panic!(
            "failed to read FFmpeg runtime destination directory {}: {err}",
            destination_dir.display()
        )
    });
    for entry in destination_entries {
        let entry = entry.unwrap_or_else(|err| {
            panic!(
                "failed to enumerate FFmpeg runtime destination directory {}: {err}",
                destination_dir.display()
            )
        });
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("dll") {
            continue;
        }
        if KNOWN_FFMPEG_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            fs::remove_file(entry.path()).unwrap_or_else(|err| {
                panic!(
                    "failed to remove stale FFmpeg runtime DLL {}: {err}",
                    entry.path().display()
                )
            });
        }
    }

    let entries = fs::read_dir(&bin_dir).unwrap_or_else(|err| {
        panic!(
            "failed to read FFmpeg runtime DLL directory {}: {err}",
            bin_dir.display()
        )
    });
    let mut candidates: Vec<Vec<PathBuf>> =
        (0..RUNTIME_PREFIXES.len()).map(|_| Vec::new()).collect();
    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("failed to enumerate FFmpeg DLLs: {err}"));
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("dll") {
            continue;
        }
        let Some(prefix_index) = RUNTIME_PREFIXES
            .iter()
            .position(|prefix| name.starts_with(prefix))
        else {
            continue;
        };
        candidates[prefix_index].push(entry.path());
    }

    for (index, prefix) in RUNTIME_PREFIXES.iter().enumerate() {
        if candidates[index].len() != 1 {
            let files = candidates[index]
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            panic!(
                "expected exactly one FFmpeg runtime DLL for {prefix}, found {}: {files}",
                candidates[index].len()
            );
        }
        let source = &candidates[index][0];
        let file_name = source.file_name().unwrap_or_else(|| {
            panic!("FFmpeg runtime path has no file name: {}", source.display())
        });
        let destination = destination_dir.join(file_name);
        copy_dll(source, &destination).unwrap_or_else(|err| {
            panic!(
                "failed to copy FFmpeg runtime DLL {}: {err}",
                source.display()
            )
        });
    }
}

fn candidate_dav1d_dll_paths() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("third_party/dav1d/dav1d.dll")];

    if let Some(explicit) = env::var_os("DAV1D_DLL_PATH") {
        candidates.push(PathBuf::from(explicit));
    }

    if let Some(vcpkg_root) = env::var_os("VCPKG_ROOT") {
        let root = PathBuf::from(vcpkg_root);
        candidates.push(root.join("installed/x64-windows/bin/dav1d.dll"));
        candidates.push(root.join("installed/x64-windows/debug/bin/dav1d.dll"));
        candidates.push(root.join("installed/x86-windows/bin/dav1d.dll"));
        candidates.push(root.join("installed/x86-windows/debug/bin/dav1d.dll"));
    }

    candidates
}
