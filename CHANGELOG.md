# Changelog

## Unreleased

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
