use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let pointer_width = env::var("CARGO_CFG_TARGET_POINTER_WIDTH").unwrap_or_default();
    let source = source_dll_path(&pointer_width);
    let dll_name = expected_dll_name(&pointer_width);

    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed=third_party/unrar/LICENSE.txt");
    println!("cargo:rerun-if-changed=third_party/dav1d/dav1d.dll");
    println!("cargo:rerun-if-changed=third_party/dav1d/LICENSE");
    println!("cargo:rerun-if-changed=assets/viewer_icon.ico");
    println!("cargo:rerun-if-changed=assets/viewer_icon.png");

    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let target_profile_destination = Path::new("target").join(profile).join(dll_name);
    if let Err(err) = copy_dll(&source, &target_profile_destination) {
        panic!("failed to copy UnRAR DLL to target profile root: {err}");
    }

    embed_windows_icon();
    maybe_copy_dav1d_dll();
}

fn embed_windows_icon() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/viewer_icon.ico");
    if let Err(err) = res.compile() {
        panic!("failed to embed Windows icon resource: {err}");
    }
}

fn source_dll_path(pointer_width: &str) -> PathBuf {
    match pointer_width {
        "64" => PathBuf::from("third_party/unrar/x64/UnRAR64.dll"),
        "32" => PathBuf::from("third_party/unrar/x86/UnRAR.dll"),
        other => panic!("unsupported target pointer width: {other}"),
    }
}

fn expected_dll_name(pointer_width: &str) -> &'static str {
    match pointer_width {
        "64" => "UnRAR64.dll",
        "32" => "UnRAR.dll",
        _ => "UnRAR.dll",
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
