# Changelog

## Unreleased

### Added

- Added `Alt+Enter` in the Library to open Properties for the selected item.
- Added adjacent-book scratchpad preloading for smoother next/previous book navigation.
- Improved adjacent-book preloading by decoding next and previous books in parallel.
- Improved adjacent-book layout matching by using the existing Page Map cache when available to reduce spread/single-page promotion misses.
- Added a Danger Zone setting for adjacent-book preload RAM, with a 5% default and a 5–30% per-book range.
- Added a localized central Library card HUD for Page Map failures, updating when generation completes and restoring cached failure status when cards are first shown.
- Added Library support for video files, including generated thumbnails, automatic thumbnail preview, filename-HUD scrubbing, and opening videos with their associated Windows app.
- Added full-page filename-HUD scrubbing for image books with an available Page Map, with page-axis priority for multi-page books, plus in-place playback for animated WebP thumbnails in archives (first image) and standalone image files.
- Added filename-HUD time scrubbing for standalone animated WebP files and one-page image books whose page is animated WebP; animated WebP pages in multi-page books remain on the page axis.
- Added Library sidebar filtering by file extension, with item counts and independently collapsible Extensions and Groups sections.
- Added shared Library and Viewer filename-token actions for filtering, copying, Web Search, and Clear Filter.
- Added configurable Web Search destinations with Chrome, Edge, or Firefox, Tab or New Window opening, and up to five entries.

### Changed

- Switched JPEG decoding to the TurboJPEG backend with direct RGBA output, unified thumbnail and Viewer JPEG decoding, and a Windows WIC fallback for CMYK/YCCK JPEGs.
- Cleaned up the obsolete JPEG/native dependency paths and related native build configuration.
- Expanded the Library thumbnail display range to 120–660 px in 20 px steps, added persistent Ctrl+Wheel resizing, and distributed extra row width evenly across both side margins and inter-column gaps while preserving the existing minimum gap and virtual-grid column positions.
- Increased normal Library thumbnail generation and storage width from 320 px to 500 px, while keeping the display size independent from the cached thumbnail size.
- Changed Library runtime previews for video, animated WebP, and static page scrubbing to decode at the current thumbnail display width instead of the fixed thumbnail storage width; preview results remain runtime-only and are not written to the thumbnail disk cache.
- Added revision-aware failure caching for Page Map and thumbnail generation, avoiding repeated work for unchanged sources after a terminal failure, and pruning obsolete thumbnail, Page Map, and failure-cache revisions for displayed books.
- Added revision-aware video-thumbnail cache and artifact lifecycle handling, including source-change refresh, terminal-failure suppression, and cleanup alignment for deletion and cache clear.
- Migrated the project from Rust 2021 to Rust 2024, pinned the toolchain and CI to Rust 1.97.0, and adopted Cargo resolver 3.
- Updated eframe and egui to 0.35 and `egui_material_icons` to 0.7, including the required eframe lifecycle and root UI API migration while retaining the Glow renderer.
- Updated `zip` to 8.6, `fast_image_resize` to 6, `lru` to 0.18, and `quick-xml` to 0.41.
- Moved animated WebP inspection and frame decoding into the published crates.io `webp-anim` `0.1.1` crate, replacing the viewer's direct `libwebp-sys` integration.
- Refreshed compatible direct dependencies within their existing version requirements, including serde_json, toml, chrono, tokio, memmap2, blake3, bytes, anyhow, and log.
- Removed the unused direct development dependency on `tempfile` and made the required `windows-sys` `Win32_Security` feature explicit.
- Changed External Tools settings to show all three slots in place, with empty slots omitted from launch surfaces.
- Expanded the operation guides with filename-token, Web Search, and fixed-slot External Tools setup details.

### Fixed

- Fixed the Library top-bar path input field extending into the `Viewer:` controls.
- Fixed the viewer opening-page cover-blank layout so Cover Blank now consistently shows a blank page paired with the cover in spread and auto modes.
- Fixed viewer toolbar page titles for cover-blank spreads, including left-to-right and right-to-left reading directions.
- Localized the viewer cover-blank toolbar label so English shows `Blank` and Japanese shows `ブランク`.
- Added Viewer page-range delete and archive rebuild for ZIP/CBZ/RAR/CBR, with ZIP/CBZ rebuilt in place, RAR/CBR rebuilt as CBZ, all-image-delete prevention, and an option to open the rebuilt archive in a new Viewer.
- Bound the Library thumbnail GPU texture cache to a 256 MiB budget, prioritizing visible thumbnails while evicting off-screen textures.
- Changed the Library thumbnail CPU memory cache from a fixed 500-entry limit to a 256 MiB byte budget.
- Reduced CPU/GPU memory high-water usage after bulk thumbnail generation in large libraries without changing Viewer L1/L2 caches or thumbnail request policy.
- Fixed a Page Map issue where some JPEG files could fail lightweight metadata probing when a JPEG marker was split across an internal read chunk boundary.

- Reorganized the Settings window into General, Library, Viewer, Performance, External Tools, and Web Search tabs.
- Added favorite indicators to Library card HUDs.
- Unified favorite star drawing across the Library and card HUDs.
- Added Library Card HUD Style and Library Card Selection Style settings.
- Improved the Settings dialog layout so common tabs are easier to review.
- Refined the Library card selection presentation.
- Fixed ImageFile viewer navigation so pages opened from the Library follow the current Library order.
- Added current image file names to the Viewer toolbar.
- Added a Library entry Properties dialog for archives, folder books, and image files, showing the file name, full path, kind, size, modified time, and archive page count when available.
- Stabilized the Library entry Properties dialog layout with fixed value/copy columns, three-line name/path display, full-text copy buttons, and a centered close button.
