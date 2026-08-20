use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

// These are the runtime families already allowlisted by the core build and
// release packaging. The concrete DLL names are discovered from target output.
const FFMPEG_RUNTIME_PREFIXES: [&str; 5] = [
    "avcodec-",
    "avformat-",
    "avutil-",
    "swresample-",
    "swscale-",
];

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is required"));
    let generated = out_dir.join("embedded_assets.rs");

    println!("cargo:rerun-if-env-changed=CARGO_TARGET_DIR");
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        write_generated(&generated, &[]).expect("write empty embedded asset module");
        return;
    }

    let workspace_root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is required"))
            .parent()
            .expect("launcher must be a workspace member")
            .to_path_buf();
    let target_dir = match env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        Some(path) => workspace_root.join(path),
        None => workspace_root.join("target"),
    };
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let release_dir = target_dir.join(profile);

    let core = release_dir.join("cbz-viewer-core.exe");
    let dav1d = release_dir.join("dav1d.dll");
    let mut assets = vec![core, dav1d];
    assets.extend(discover_ffmpeg_runtime_dlls(&release_dir));

    for path in &assets {
        println!("cargo:rerun-if-changed={}", path.display());
        if !path.is_file() {
            panic!(
                "launcher embedding input is missing: {}. Build cbz-viewer-core first",
                path.display()
            );
        }
    }

    let mut res = winresource::WindowsResource::new();
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is required");
    res.set("ProductName", "CBZ Viewer")
        .set("FileDescription", "CBZ Viewer")
        .set("OriginalFilename", "cbz-viewer.exe")
        .set("FileVersion", &version)
        .set("ProductVersion", &version);
    res.compile()
        .expect("failed to embed Windows version resource");

    write_generated(&generated, &assets).expect("write embedded asset module");
}

fn discover_ffmpeg_runtime_dlls(release_dir: &Path) -> Vec<PathBuf> {
    let entries = fs::read_dir(release_dir).unwrap_or_else(|error| {
        panic!(
            "could not read core release directory {}: {error}",
            release_dir.display()
        )
    });
    let mut discovered: Vec<Vec<PathBuf>> = (0..FFMPEG_RUNTIME_PREFIXES.len())
        .map(|_| Vec::new())
        .collect();

    for entry in entries {
        let entry =
            entry.unwrap_or_else(|error| panic!("could not enumerate release inputs: {error}"));
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("dll") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(index) = FFMPEG_RUNTIME_PREFIXES
            .iter()
            .position(|prefix| name.starts_with(prefix) && is_numeric_dll_suffix(name, prefix))
        else {
            continue;
        };
        discovered[index].push(path);
    }

    let mut result = Vec::with_capacity(FFMPEG_RUNTIME_PREFIXES.len());
    for (index, prefix) in FFMPEG_RUNTIME_PREFIXES.iter().enumerate() {
        if discovered[index].len() != 1 {
            let paths = discovered[index]
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            panic!(
                "expected exactly one staged FFmpeg runtime DLL for {prefix}, found {}: {paths}",
                discovered[index].len()
            );
        }
        result.push(discovered[index][0].clone());
    }
    result
}

fn is_numeric_dll_suffix(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(".dll"))
        .is_some_and(|version| {
            !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn write_generated(destination: &Path, assets: &[PathBuf]) -> io::Result<()> {
    let mut source = String::from(
        "#[derive(Clone, Copy)]\n\
pub struct EmbeddedAsset {\n\
    pub name: &'static str,\n\
    pub bytes: &'static [u8],\n\
    pub size: u64,\n\
    pub sha256: [u8; 32],\n\
}\n\
pub static ASSETS: &[EmbeddedAsset] = &[\n",
    );

    for path in assets {
        let bytes = fs::read(path)?;
        let hash = Sha256::digest(&bytes);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "asset has no UTF-8 name"))?;
        let escaped_path = path.to_string_lossy().replace('"', "\\\"");
        let hash_bytes = hash
            .iter()
            .map(|byte| format!("0x{byte:02x}"))
            .collect::<Vec<_>>()
            .join(", ");
        source.push_str(&format!(
            "    EmbeddedAsset {{ name: {name:?}, bytes: include_bytes!(r#\"{escaped_path}\"#), size: {}, sha256: [{hash_bytes}] }},\n",
            bytes.len()
        ));
    }
    source.push_str("];\n");
    fs::write(destination, source)
}
