# Changelog

## Unreleased

### Added

- Added `Alt+Enter` in the Library to open Properties for the selected item.
- Added adjacent-book scratchpad preloading for smoother next/previous book navigation.
- Improved adjacent-book preloading by decoding next and previous books in parallel.
- Improved adjacent-book layout matching by using the existing Page Map cache when available to reduce spread/single-page promotion misses.
- Added a Danger Zone setting for adjacent-book preload RAM, with a 5% default and a 5–30% per-book range.
- Added a localized central Library card HUD for Page Map failures, updating when generation completes and restoring cached failure status when cards are first shown.

### Changed

- Added revision-aware failure caching for Page Map and thumbnail generation, avoiding repeated work for unchanged sources after a terminal failure, and pruning obsolete thumbnail, Page Map, and failure-cache revisions for displayed books.
- Migrated the project from Rust 2021 to Rust 2024, pinned the toolchain and CI to Rust 1.97.0, and adopted Cargo resolver 3.
- Updated eframe and egui to 0.35 and `egui_material_icons` to 0.7, including the required eframe lifecycle and root UI API migration while retaining the Glow renderer.
- Updated `zip` to 8.6, `fast_image_resize` to 6, `lru` to 0.18, and `quick-xml` to 0.41.
- Refreshed compatible direct dependencies within their existing version requirements, including serde_json, toml, chrono, tokio, memmap2, blake3, bytes, anyhow, and log.
- Removed the unused direct development dependency on `tempfile` and made the required `windows-sys` `Win32_Security` feature explicit.

### Fixed

- Fixed the viewer opening-page cover-blank layout so Cover Blank now consistently shows a blank page paired with the cover in spread and auto modes.
- Fixed viewer toolbar page titles for cover-blank spreads, including left-to-right and right-to-left reading directions.
- Localized the viewer cover-blank toolbar label so English shows `Blank` and Japanese shows `ブランク`.
- Added Viewer page-range delete and archive rebuild for ZIP/CBZ/RAR/CBR, with ZIP/CBZ rebuilt in place, RAR/CBR rebuilt as CBZ, all-image-delete prevention, and an option to open the rebuilt archive in a new Viewer.
- Bound the Library thumbnail GPU texture cache to a 256 MiB budget, prioritizing visible thumbnails while evicting off-screen textures.
- Changed the Library thumbnail CPU memory cache from a fixed 500-entry limit to a 256 MiB byte budget.
- Reduced CPU/GPU memory high-water usage after bulk thumbnail generation in large libraries without changing Viewer L1/L2 caches or thumbnail request policy.
- Fixed a Page Map issue where some JPEG files could fail lightweight metadata probing when a JPEG marker was split across an internal read chunk boundary.

- Reorganized the Settings window into General, Library, Viewer, Performance, and External Tools tabs.
- Added favorite indicators to Library card HUDs.
- Unified favorite star drawing across the Library and card HUDs.
- Added Library Card HUD Style and Library Card Selection Style settings.
- Improved the Settings dialog layout so common tabs are easier to review.
- Refined the Library card selection presentation.
- Fixed ImageFile viewer navigation so pages opened from the Library follow the current Library order.
- Added current image file names to the Viewer toolbar.
- Added a Library entry Properties dialog for archives, folder books, and image files, showing the file name, full path, kind, size, modified time, and archive page count when available.
- Stabilized the Library entry Properties dialog layout with fixed value/copy columns, three-line name/path display, full-text copy buttons, and a centered close button.
