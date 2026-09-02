# Changelog

All notable changes to **CBZ Viewer** are documented in this file.

## [0.5.6] - 2026-09-02

### Changed

- Changed the Library left-pane toggle to visually indicate its active/open state.

## [0.5.5] - 2026-08-31

### Added

- Added file-name placeholders for External Tool arguments and clarified the placeholder guidance in Settings.

## [0.5.4] - 2026-08-29

### Fixed

- Aligned the Viewer filename-token context menu with Library's compact labels and separator before Clear Filter.

### Changed

- Changed standard L1/L2 cache calculation from 1/8 to 1/16 of available memory.

## [0.5.3] - 2026-08-27

### Changed

- Changed the Windows Viewer launcher to best-effort remove versioned runtime directories older than the current version at startup.
- Changed startup cleanup failures to be ignored so the Viewer continues to launch and retries cleanup on the next start.

## [0.5.2] - 2026-08-26

### Changed

- Integrated the Library, Filter, and History views into the shared sidebar tab organization and Library scroll structure.
- Changed Tree synchronization to reveal the active directory without auto-scrolling.
- Simplified folder context menus to one normalized **Add to Library** or **Remove from Library** action.

## [0.5.1] - 2026-08-25

### Changed

- Extended per-book settings clear to invalidate that book's thumbnails, Page Map state, and cached artifact failures for regeneration.

## [0.5.0] - 2026-08-25

### Added

- Added Animated GIF support for Viewer streaming playback and Library AUTO/Scrub previews.
- Added filesystem Tree navigation to the Library left pane.

### Changed

- Migrated GIF decoding to `zengif` and routed animated GIFs through the shared animation source and streaming paths.
- Changed visible spread Animated WebP stream requests to split left and right sides and handle each side independently on the existing `interactive-even` and `interactive-odd` workers.

## [0.4.13] - 2026-08-23

### Added

- Added Library search with space-separated AND terms, uppercase `OR` alternatives, and quoted phrases.

## [0.4.12] - 2026-08-22

### Fixed

- Reduced unnecessary Library/Viewer wakeups by making Viewer process monitoring, Library scan completion, Viewer IPC updates, and External Tool completion event-driven.
- Changed Toast expiry to deadline-based scheduling while preserving required thumbnail and preview updates.

## [0.4.11] - 2026-08-22

### Fixed

- Embedded the application icon into the Windows launcher executable.

## [0.4.10] - 2026-08-21

### Fixed

- Reduced unnecessary Library idle CPU/GPU activity by moving rate-limited background artifact scheduling off the UI repaint loop.
- Improved Animated WebP playback by updating existing textures in place for continuing frames instead of recreating texture handles.

## [0.4.9] - 2026-08-20

### Changed

- Changed the adjacent-book/SPAD preload gate to require a stable display and use 30% L2 usage or Page Map retention thresholds; once L2 is settled, the full dispatch scope is allowed.
- Changed visible spread Animated WebP stream requests to split left and right sides and handle each side independently.
- Changed the Library top-bar Settings button to open a menu with `Preferences...`, `Language: EN ⇔ JP`, and `About CBZ Viewer...`.
- Moved language switching out of the Settings window and removed the now-empty General tab.

### Fixed

- Fixed spread playback so that when both sides are Animated WebP, playback starts from a common time captured at display commit.

## [0.4.8] - 2026-08-19

### Changed

- Changed Viewer External Tools to queue requests for other books while a tool is running, process them in order, and show the request count on tool buttons.
- Changed guaranteed adjacent-book/SPAD preload to begin 500 ms after the current book's first display commit, while additional preloading still waits for L2 to settle.

### Fixed

- Improved Settings window sizing so its width remains bounded.

## [0.4.7] - 2026-08-19

### Changed

- Changed the Windows release to use a single `cbz-viewer.exe` launcher.
- Embedded the viewer core, dav1d, and FFmpeg runtime DLLs into the launcher.
- Added validated extraction of runtime files to a versioned user-local directory when needed.

## [0.4.6] - 2026-08-18

### Changed

- Reorganized and updated the README Highlights to consolidate the current feature descriptions.
- Changed the HDD global thumbnail generation goal to use `base_goal.min(2)`.

## [0.4.5] - 2026-08-17

### Changed

- Switched JPEG decoding to the TurboJPEG backend with direct RGBA output.
- Unified thumbnail and Viewer JPEG decoding.
- Added a Windows WIC fallback for CMYK/YCCK JPEGs.
- Cleaned up obsolete JPEG/native dependency paths and related native build configuration.

## [0.4.4] - 2026-08-16

### Added

- Added shared Library and Viewer filename-token actions for filtering, copying, Web Search, and Clear Filter.
- Added configurable Web Search destinations with Chrome, Edge, or Firefox, Tab or New Window opening, and up to five entries.

### Changed

- Changed External Tools settings to show all three slots in place, with empty slots omitted from launch surfaces.
- Expanded the operation guides with filename-token, Web Search, and fixed-slot External Tools setup details.
- Added the Web Search section to Settings.

### Fixed

- Fixed Web Search and External Tools submenu rows so their left edges align with the surrounding context-menu rows.

## [0.4.3] - 2026-08-15

### Changed

- Expanded the Library thumbnail display range to 120–660 px in 20 px steps.
- Added persistent Ctrl+Wheel thumbnail resizing.
- Distributed extra row width evenly across side margins and inter-column gaps while preserving the existing minimum gap and virtual-grid column positions.
- Increased normal Library thumbnail generation and storage width from 320 px to 500 px while keeping display size independent from cached thumbnail size.
- Changed Library runtime previews for video, Animated WebP, and static page scrubbing to decode at the current thumbnail display width.
- Kept runtime preview results out of the thumbnail disk cache.

## [0.4.2] - 2026-08-14

### Changed

- Removed the external UnRAR runtime DLL requirement from the Windows release.

### Fixed

- Changed CI to force a local FFmpeg build when the provenance cache cannot be used.

## [0.4.1] - 2026-08-14

### Fixed

- Improved Library thumbnail scheduling and cache restoration.

## [0.4.0] - 2026-08-13

### Added

- Added Library support for video files, including generated thumbnails, automatic thumbnail preview, filename-HUD scrubbing, and opening videos with their associated Windows app.
- Added full-page filename-HUD scrubbing for image books with an available Page Map.
- Added in-place playback for Animated WebP thumbnails in archives and standalone image files.
- Added filename-HUD time scrubbing for standalone Animated WebP files and one-page image books whose page is Animated WebP.
- Added Library sidebar filtering by file extension, with item counts and independently collapsible Extensions and Groups sections.

### Changed

- Added revision-aware video-thumbnail cache and artifact lifecycle handling, including source-change refresh, terminal-failure suppression, and cleanup alignment for deletion and cache clear.
- Updated `webp-anim` integration to `0.1.1`.

## [0.3.3] - 2026-08-02

### Fixed

- Fixed the Library top-bar path input field extending into the `Viewer:` controls.

## [0.3.2] - 2026-08-01

### Changed

- Moved Animated WebP inspection and frame decoding into the published crates.io `webp-anim` `0.1.0` crate, replacing the viewer's direct `libwebp-sys` integration.

## [0.3.1] - 2026-07-22

### Added

- Added a localized central Library card HUD for Page Map failures.
- Updated the Page Map failure HUD when generation completes and restored cached failure status when cards are first shown.

## [0.3.0] - 2026-07-19

### Changed

- Added revision-aware failure caching for Page Map and thumbnail generation, avoiding repeated work for unchanged sources after a terminal failure and pruning obsolete revisions for displayed books.
- Migrated the project from Rust 2021 to Rust 2024.
- Pinned the toolchain and CI to Rust 1.97.0 and adopted Cargo resolver 3.
- Updated eframe and egui to 0.35 and `egui_material_icons` to 0.7 while retaining the Glow renderer.
- Updated `zip` to 8.6, `fast_image_resize` to 6, `lru` to 0.18, and `quick-xml` to 0.41.
- Refreshed compatible direct dependencies including serde_json, toml, chrono, tokio, memmap2, blake3, bytes, anyhow, and log.
- Removed the unused direct development dependency on `tempfile`.
- Made the required `windows-sys` `Win32_Security` feature explicit.

## [0.2.2] - 2026-07-16

### Fixed

- Hardened resource handling.
- Changed settings-file updates to use atomic writes.

## [0.2.1] - 2026-07-15

### Added

- Added `Alt+Enter` in the Library to open Properties for the selected item.

## [0.2.0] - 2026-07-10

### Added

- Added adjacent-book scratchpad preloading for smoother next/previous book navigation.
- Improved adjacent-book preloading by decoding next and previous books in parallel.
- Improved adjacent-book layout matching by using the existing Page Map cache when available to reduce spread/single-page promotion misses.
- Added a Danger Zone setting for adjacent-book preload RAM, with a 5% default and a 5–30% per-book range.

## [0.1.9] - 2026-07-08

### Added

- Added a Library entry Properties dialog for archives, folder books, and image files.
- Added file name, full path, kind, size, modified time, and archive page count where available.
- Stabilized the Properties dialog layout with fixed value/copy columns, three-line name/path display, full-text copy buttons, and a centered close button.

## [0.1.8] - 2026-07-06

### Fixed

- Fixed a Page Map issue where some JPEG files could fail lightweight metadata probing when a JPEG marker was split across an internal read chunk boundary.

## [0.1.7] - 2026-07-04

### Changed

- Bound the Library thumbnail GPU texture cache to a 256 MiB budget, prioritizing visible thumbnails while evicting off-screen textures.
- Changed the Library thumbnail CPU memory cache from a fixed 500-entry limit to a 256 MiB byte budget.
- Reduced CPU/GPU memory high-water usage after bulk thumbnail generation without changing Viewer L1/L2 caches or thumbnail request policy.

## [0.1.6] - 2026-07-01

### Added

- Added a page-local delete-range overlay to Viewer pages.

## [0.1.5] - 2026-06-30

### Changed

- Improved Viewer boundary help and delete-range presentation.
- Refreshed README structure and Highlights.

## [0.1.4] - 2026-06-29

### Added

- Added Viewer page-range delete and archive rebuild for ZIP/CBZ/RAR/CBR.
- ZIP/CBZ archives are rebuilt in place; RAR/CBR archives are rebuilt as CBZ.
- Added all-image-delete prevention.
- Added an option to open the rebuilt archive in a new Viewer.

## [0.1.3] - 2026-06-27

### Fixed

- Changed broken Viewer pages to render as synthetic failed pages rather than failing the entire Viewer load.

## [0.1.2] - 2026-06-27

### Fixed

- Fixed the Viewer opening-page Cover Blank layout so a blank page is consistently paired with the cover in spread and auto modes.
- Fixed Viewer toolbar page titles for Cover Blank spreads in both left-to-right and right-to-left reading directions.
- Localized the Cover Blank toolbar label as `Blank` in English and `ブランク` in Japanese.

## [0.1.1] - 2026-06-24

### Added

- Added current image file names to the Viewer toolbar.

### Fixed

- Fixed ImageFile Viewer navigation so pages opened from the Library follow the current Library order.

## [0.1.0] - 2026-06-22

### Added

- Added favorite indicators to Library card HUDs.
- Added Library Card HUD Style and Library Card Selection Style settings.

### Changed

- Reorganized the Settings window into General, Library, Viewer, Performance, and External Tools tabs.
- Unified favorite star drawing across the Library and card HUDs.
- Improved the Settings dialog layout so common tabs are easier to review.
- Refined the Library card selection presentation.
