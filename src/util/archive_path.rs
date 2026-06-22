use std::path::Path;

const SUPPORTED_ARCHIVE_EXTENSIONS: &[&str] = &["cbz", "zip", "rar", "cbr", "epub"];
const SUPPORTED_IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "gif", "avif", "avifs", "bmp", "tif", "tiff",
];

pub fn is_supported_archive_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|x| x.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    SUPPORTED_ARCHIVE_EXTENSIONS.contains(&ext.as_str())
}

pub fn is_supported_image_name(name: &str) -> bool {
    let ext = name.to_ascii_lowercase();
    SUPPORTED_IMAGE_EXTENSIONS.contains(&ext.as_str())
}

pub fn is_supported_image_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|x| x.to_str()) else {
        return false;
    };
    is_supported_image_name(ext)
}
