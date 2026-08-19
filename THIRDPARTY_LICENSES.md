# Third-Party Licenses

This file primarily documents third-party binary components included in the distribution.
It is not intended to be a comprehensive license inventory of Rust crate dependencies. Rust crate dependencies are tracked through `Cargo.lock` and, when needed, generated license reports.

## libjpeg-turbo / TurboJPEG

- Purpose: The JPEG page decode backend for full images, thumbnails, and Viewer DCT-scaled images
- Source: `turbojpeg-sys 1.2.0` vendors libjpeg-turbo 3.1.0
- Linking: The bundled library is built from source and statically linked; no TurboJPEG or libjpeg DLL is distributed
- Build policy: `require-simd` is enabled and the Windows CI verifies that NASM is available
- Rust bindings: Pregenerated bindings are used; bindgen/libclang is not required for JPEG
- License: libjpeg-turbo uses compatible BSD-style licenses for the IJG/libjpeg and TurboJPEG components. The bundled source's `LICENSE.md` and `README.ijg` are the authoritative texts.
- Required attribution for binary distributions: “This software is based in part on the work of the Independent JPEG Group.”
- Scope note: The supported TIFF feature remains enabled. Its upstream `tiff` dependency has a separate, transitive `zune-jpeg` path for TIFF-internal ModernJPEG data; this is outside the standalone JPEG page path and is retained to avoid an unrelated TIFF codec refactor.

The TurboJPEG API component is also covered by the following Modified 3-clause BSD notice from libjpeg-turbo:

```text
Copyright (C)2009-2024 D. R. Commander. All Rights Reserved.
Copyright (C)2015 Viktor Szathmáry. All Rights Reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.
3. Neither the name of the libjpeg-turbo Project nor the names of its
   contributors may be used to endorse or promote products derived from this
   software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.
```

## UnRAR

- Purpose: RAR/CBR listing and extraction backend
- Role: The `unrar_sys` crate compiles the bundled UnRAR source into the application through static linking
- Note: Not used to recreate or implement the RAR compression algorithm
- Distribution: No external UnRAR DLL is distributed or loaded
- External runtime files `UnRAR64.dll` / `UnRAR.dll` are not distributed or loaded
- License: UnRAR is used under the following notice from `unrar_sys/vendor/unrar/license.txt`:

```text
 ******    *****   ******   UnRAR - free utility for RAR archives
 **   **  **   **  **   **  ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
 ******   *******  ******    License for use and distribution of
 **   **  **   **  **   **   ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
 **   **  **   **  **   **         FREE portable version
                                   ~~~~~~~~~~~~~~~~~~~~~

      The source code of UnRAR utility is freeware. This means:

   1. All copyrights to RAR and the utility UnRAR are exclusively
      owned by the author - Alexander Roshal.

   2. UnRAR source code may be used in any software to handle
      RAR archives without limitations free of charge, but cannot be
      used to develop RAR (WinRAR) compatible archiver and to
      re-create RAR compression algorithm, which is proprietary.
      Distribution of modified UnRAR source code in separate form
      or as a part of other software is permitted, provided that
      full text of this paragraph, starting from "UnRAR source code"
      words, is included in license, or in documentation if license
      is not available, and in source code comments of resulting package.

   3. The UnRAR utility may be freely distributed. It is allowed
      to distribute UnRAR inside of other software packages.

   4. THE RAR ARCHIVER AND THE UnRAR UTILITY ARE DISTRIBUTED "AS IS".
      NO WARRANTY OF ANY KIND IS EXPRESSED OR IMPLIED.  YOU USE AT
      YOUR OWN RISK. THE AUTHOR WILL NOT BE LIABLE FOR DATA LOSS,
      DAMAGES, LOSS OF PROFITS OR ANY OTHER KIND OF LOSS WHILE USING
      OR MISUSING THIS SOFTWARE.

   5. Installing and using the UnRAR utility signifies acceptance of
      these terms and conditions of the license.

   6. If you don't agree with terms of the license you must remove
      UnRAR files from your storage devices and cease to use the
      utility.

      Thank you for your interest in RAR and UnRAR.


                                            Alexander L. Roshal
```

## dav1d DLL

- Purpose: Runtime dependency for the AVIF decoding backend (`image/avif-native`)
- License: BSD-2-Clause
- Repository source: `third_party/dav1d/dav1d.dll`
- Copy policy: `build.rs` prepares it next to the core executable; the release launcher embeds it and extracts it into its versioned local runtime directory
- Load policy: The core loads it next to the extracted core executable through the standard Windows DLL search
- The distribution includes the original license text corresponding to `third_party/dav1d/LICENSE`

### License notes

- Redistribution and use of the dav1d DLL are subject to the dav1d license
- Managed separately from the project’s own license

## FFmpeg runtime DLLs

- Purpose: Representative-frame extraction from VideoFile using `ff-decode`
- Distribution method: `build.rs` prepares the **dynamically linked** FFmpeg runtime DLLs from vcpkg `x64-windows`; the release launcher embeds the allowlisted runtime closure and extracts it next to the core executable
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
