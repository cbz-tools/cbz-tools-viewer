[日本語](README.ja.md)

# cbz-tools-viewer

CBZ Viewer is a Windows comic book viewer. It opens CBZ, ZIP, RAR, CBR, and EPUB image books, as well as folders that contain images directly under them. The Library also lists supported video files.

Selecting a video file opens it with the associated Windows application. CBZ Viewer does not play video.

The package contains the single executable `cbz-viewer.exe`. It is a small
launcher; on first run it validates and extracts the viewer core and its
runtime files into a versioned directory under `%LOCALAPPDATA%\cbz-viewer`.

---

# Download

Download the latest release from [Latest Release](https://github.com/cbz-tools/cbz-tools-viewer/releases/latest).

Extract the ZIP and run `cbz-viewer.exe` directly. No installation is required;
the launcher keeps the packaged runtime in the current user's local app data.

---

# Screenshots

| Library | Viewer | Fullscreen |
|---|---|---|
| [![Library](docs/assets/screenshots/Library.png)](docs/assets/screenshots/Library.png) | [![Viewer](docs/assets/screenshots/Viewer_Windowed.png)](docs/assets/screenshots/Viewer_Windowed.png) | [![Fullscreen](docs/assets/screenshots/Viewer_Fullscreen.png)](docs/assets/screenshots/Viewer_Fullscreen.png) |

### Demo content

**Sovereign Stars** is a fictional comic created with GPT for CBZ Viewer demos and screenshots.

It is not related to any real work, person, or organization.

The demo manga assets are also licensed under the MIT License.

---

# Highlights

* Predictive loading and caching help reduce page-turn latency, even in large books. Pages from the next and previous books are also prepared in the background to reduce delays when moving between books.
* In the Viewer, you can move between adjacent books while deleting unwanted books, and remove unwanted page ranges by rebuilding the archive.
* Animated WebP streaming playback is supported, with seamless page navigation just like regular pages. Spread view is also supported.
* Filename tokens can be used from both the Library and Viewer for filtering, copying, and web searches.
* The Library provides collection management features such as search, favorites, groups, rename, and delete.
* Registered external tools can be launched from both the Library and Viewer. With the companion project [**CBZ Tools Optimizer**](https://github.com/cbz-tools/cbz-tools-optimizer), archives can be optimized, converted, and reduced in size.
* The Library supports automatic previews and scrubbing. Availability is shown in the table below.

| Library item | Thumbnail | Auto preview | Scrub |
| --- | --- | --- | --- |
| Video | Yes | Yes | Yes |
| Image book | Yes | — | Yes |
| Animated WebP | Yes | Yes | Yes |

---

# Why

I used ZipPla for many years.

Its excellent reading experience was one of the main reasons this project started.

CBZ Viewer is built as the Windows comic book viewer I personally wanted to use.

---

# Design Philosophy

CBZ Viewer focuses on reducing page-turn latency.

It uses CPU, RAM, and VRAM-aware settings for background predictive loading, caching, and thumbnail generation, so that even large books remain comfortable to read.

It is also an offline application that does not require an internet connection.

---

# Features

CBZ Viewer provides three main workflows:

* Reading: page navigation, spread view, slideshow, progress display, and predictive cache.
* Managing: library, search, history, favorites, groups, and book navigation.
* Organizing: rename, copy, delete, open in Explorer, and archive rebuild from selected page ranges.

See the [Operation Guide](docs/operation.md) for details.

---

# External tools

CBZ Viewer can launch external tools while you read.

With the companion project [**CBZ Tools Optimizer**](https://github.com/cbz-tools/cbz-tools-optimizer), you can run CBZ / ZIP archive optimization, format conversion, and size reduction workflows.

---

# Requirements

* Windows 10
* Windows 11

---

# Supported formats

## Archive

* CBZ / ZIP
* RAR / CBR
* EPUB image books

Image-based EPUB books are supported. Text EPUB, reflow and CSS-based layouts, DRM-protected EPUB, audio, video, JavaScript, and SVG rendering are not supported.

## Folder

* Folders with images directly under them can be opened as books.

## Image

* JPEG
* PNG
* WebP (static / animated)
* AVIF (.avif / .avifs)
* BMP
* TIFF
* GIF (static / animated)

When you open a single supported image file, CBZ Viewer opens the parent folder as a book and starts from that image.

---

# Documentation

See the following for detailed usage:

* [Operation Guide](docs/operation.md)
* [Library display settings](docs/operation.md#library-display-settings)
* [Danger Zone Recovery](docs/DANGER_ZONE_RECOVERY.md)
* [L1 / L2 Streaming Cache](docs/dev/SimpleStreaming.md)
* [SPAD: Adjacent Book Scratchpad](docs/dev/Spad.md)

See `docs` for implementation and architecture details.

---

# Acknowledgements

I have learned from and been influenced by the strengths and user experience of many viewers, not just ZipPla.

This project is implemented from scratch in Rust, but it stands on the work of many predecessors.

My thanks go to everyone who has released excellent software.

---

# Changelog

See [CHANGELOG.md](CHANGELOG.md).

---

## Third-Party Licenses

See [THIRDPARTY_LICENSES.md](THIRDPARTY_LICENSES.md).

---

## License

MIT — see [LICENSE](LICENSE).
