use std::path::Path;

pub fn normalize_path_for_selection(path: &Path) -> String {
    let mut s = path.as_os_str().to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            s = format!(r"\\{rest}");
        } else if let Some(rest) = s.strip_prefix(r"\\?\") {
            s = rest.to_owned();
        }
        s = s.replace('/', r"\");
        s.make_ascii_lowercase();
    }
    #[cfg(not(windows))]
    {
        s = s.replace('\\', "/");
    }
    s
}

/// overrides のキー正規化
/// normalize_path_for_selection() に委譲・既存の選択復元・履歴と同方針
pub fn normalize_path_for_override(path: &Path) -> String {
    normalize_path_for_selection(path)
}

pub fn paths_equivalent_for_selection(a: &Path, b: &Path) -> bool {
    let na = normalize_path_for_selection(a);
    let nb = normalize_path_for_selection(b);
    #[cfg(windows)]
    {
        na == nb
    }
    #[cfg(not(windows))]
    {
        na == nb
    }
}
