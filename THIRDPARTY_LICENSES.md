# Third-Party Licenses

This file primarily documents third-party binary components included in the distribution.
It is not intended to be a comprehensive license inventory of Rust crate dependencies. Rust crate dependencies are tracked through `Cargo.lock` and, when needed, generated license reports.

## UnRAR DLL

- Purpose: Used as the RAR/CBR reading backend
- Role: Used only for RAR/CBR extraction and listing
- Note: Not used to recreate or implement the RAR compression algorithm
- Placement policy: Does not assume a DLL rename
- Repository source: `third_party/unrar/x64/UnRAR64.dll`
- Copy policy: `build.rs` detects `target_pointer_width` and automatically copies it to `target/<profile>/` (next to the executable)
- Actual runtime name for x64 builds: `UnRAR64.dll` (next to the executable)
- Load policy: No startup check; lazy-loaded when opening a RAR
- Failure behavior: The application can still start if the DLL is missing or cannot be loaded. Only opening a RAR fails.
- The distribution includes the original license text corresponding to `third_party/unrar/LICENSE.txt`

### License notes

- Redistribution and use of the UnRAR DLL are subject to the UnRAR license
- Managed separately from the project’s own license

## dav1d DLL

- Purpose: Runtime dependency for the AVIF decoding backend (`image/avif-native`)
- License: BSD-2-Clause
- Repository source: `third_party/dav1d/dav1d.dll`
- Copy policy: `build.rs` automatically copies it to `target/<profile>/dav1d.dll` (next to the executable)
- Load policy: Loaded next to the executable through the standard Windows DLL search
- The distribution includes the original license text corresponding to `third_party/dav1d/LICENSE`

### License notes

- Redistribution and use of the dav1d DLL are subject to the dav1d license
- Managed separately from the project’s own license

## FFmpeg runtime DLLs

- Purpose: Representative-frame extraction from VideoFile using `ff-decode`
- Distribution method: `build.rs` copies the **dynamically linked** FFmpeg runtime DLLs from vcpkg `x64-windows` next to the executable
- Build dependency: `ffmpeg:x64-windows` (FFmpeg 7/8-compatible `ff-sys` development libraries) and LLVM/libclang for bindgen
- Build policy: GPL and nonfree FFmpeg features are not enabled
- License: FFmpeg is used under an LGPL 2.1-or-later configuration. At distribution time, the applicable LGPL/source notices for the bundled DLLs and the notices for vcpkg dependency components are retained
- Source: https://ffmpeg.org/ and https://github.com/microsoft/vcpkg/tree/master/ports/ffmpeg
- Placement policy: Only the five families `avcodec-*`, `avformat-*`, `avutil-*`, `swresample-*`, and `swscale-*` are copied to `target/<profile>/` as an allowlist based on the actual binary's PE import closure and included in the release package
- `avfilter-*` is retained in vcpkg as a link-time development dependency of the current `ff-sys`, but it is not part of the runtime dependency closure of `cbz-viewer.exe` and is therefore not distributed
- Linking: vcpkg `x64-windows` dynamic/shared build. Static linking is not used
- Build requirements: MSVC x64, vcpkg FFmpeg development files, and LLVM/libclang for bindgen
- Release package: The `share/ffmpeg/copyright` file from the vcpkg installation used and `vcpkg.spdx.json` are included under `third_party/ffmpeg/`
- FFmpeg source bundle: The FFmpeg source after applying the vcpkg patches corresponding to the distributed runtime DLLs, together with the FFmpeg port/patch information, is provided as a separate GitHub Release asset (`ffmpeg-source-<tag>.zip`)

### License notes

- The configuration and redistribution conditions for FFmpeg follow the license notices for the actual vcpkg port and FFmpeg source used
- DLLs from vcpkg overlays or custom triplets containing GPL/nonfree codecs or libraries must not be mixed into the distribution
