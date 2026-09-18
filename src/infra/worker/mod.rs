pub mod external_tool_worker;
pub mod prefetch;
mod storage_medium;
pub mod thumb_worker;
pub mod viewer_loader;

use std::path::Path;

fn spad_early_start_threshold_percent_for_medium(medium: storage_medium::StorageMedium) -> usize {
    match medium {
        storage_medium::StorageMedium::Hdd => 50,
        storage_medium::StorageMedium::Ssd | storage_medium::StorageMedium::Unknown => 25,
    }
}

pub(crate) fn spad_early_start_threshold_percent(path: &Path) -> usize {
    spad_early_start_threshold_percent_for_medium(storage_medium::detect_storage_medium_cached(
        path,
    ))
}
