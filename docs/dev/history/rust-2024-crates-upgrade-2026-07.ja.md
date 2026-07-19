# Rust 2024・Rust 1.97・依存更新 履歴

## 1. 概要

2026年7月に、Rust 2024 compatibility対応、Rust / Cargo 1.97.0固定、edition 2024・resolver 3への移行、直接依存の段階更新、eframe / egui 0.35移行、証明済みcrate整理、package version 0.3.0への更新を実施した。本書は更新前調査、Phase 1・2・4～7の結果文書、最終監査文書とGit履歴を統合した履歴である。

Phase 3の結果文書として計画されていた `compatible-direct-dependencies-phase3-result.ja.md` は、作業ツリーにもGit履歴にも存在しなかった。そのためPhase 3はcommit `6a655fa1e59624a520629b4b114eae60fb63c21b` の実差分と最終監査結果から再構成した。

最終状態は次のとおりである。

- package: `cbz-viewer 0.3.0`
- Rust / Cargo: 1.97.0
- edition: 2024
- `rust-version`: 1.97
- resolver: 3
- eframe / egui: 0.35.0
- `egui_material_icons`: 0.7.0
- renderer: Glow active、WGPU inactive
- Rust 2024 compatibility warning: 0件
- unused direct dependency warning: 0件
- Clippy `-D warnings`: 成功
- release build: 成功
- 既知warning: 未使用method `spad_overlay_lines` 1件のみ
- Phase 6後のLibrary / Viewer GUI実機確認: ユーザー確認済み、問題なし

## 2. 更新前の状態

基準commitは `90c7c6d`（package 0.2.2）だった。調査時点の主な状態は次のとおりである。

| 項目 | 更新前 |
| --- | --- |
| package | `cbz-viewer 0.2.2` |
| repository toolchain固定 | なし |
| 調査時ローカルtoolchain | Rust / Cargo 1.94.1 |
| CI | `dtolnay/rust-toolchain@stable` |
| edition | 2021 |
| `rust-version` | 1.85 |
| resolver | edition 2021既定のresolver 2 |
| Cargo.lock | format 4 |
| eframe / egui | lock 0.34.1 |
| `egui_material_icons` | 0.6.0 |
| renderer | Glow |

`rust-version = "1.85"` は当時のlockと整合していなかった。`image 0.25.10` はRust 1.88、`eframe 0.34.1` はRust 1.92を要求しており、実質MSRVは少なくとも1.92だった。Cargo.lockは既にformat 4で、toolchainを1.97へ上げるだけならlock format変更は不要だった。

更新前調査では、全依存を一括更新せず、toolchain、互換範囲の更新、GUI baseline、非GUI major、GUI major、crate整理を独立gateにする段階更新を採用した。最初の調査ではedition 2021維持を安全側の案としたが、その後、Rust 2024の意味変更をedition 2021のまま先に解消し、次commitでedition 2024・resolver 3へ切り替える分割方針を採用した。これにより、edition移行と依存・GUI移行の原因を分離した。

## 3. 更新方針とcommit分割

更新は「各commitを単独で検証でき、問題時にPhase単位で切り戻せること」を原則とした。

1. edition 2021のままRust 2024 compatibility warningを意味レビュー付きで解消する。
2. Rust / Cargo 1.97.0、edition 2024、resolver 3、toolchain、CIを一括で整合させる。
3. 既存manifest要件内の互換直接依存だけをlock更新する。
4. eframe / egui 0.34.3で0.35移行前のclean baselineを作る。
5. 相互に独立した非GUI主要4クレートを限定更新する。
6. eframe / egui 0.35とicons 0.7を同時に更新し、lifecycle / root UIだけを最小移行する。
7. 利用されていないことを証明できた直接依存と不要allowだけを整理する。
8. 全Phaseを監査し、package versionを0.3.0へ更新する。

依存更新は引数なしの `cargo update` を避け、対象packageを限定した。native DLL、保存形式、renderer、UI外観、性能設計、worker / IPC / cache設計の意図的変更は対象外とした。

## 4. 最終到達状態

| 項目 | 最終状態 |
| --- | --- |
| package | `cbz-viewer 0.3.0` |
| Rust / Cargo | 1.97.0、MSVC host |
| manifest | `edition = "2024"`、`rust-version = "1.97"` |
| workspace | resolver 3 |
| toolchain | `rust-toolchain.toml`、channel 1.97.0、minimal、rustfmt / clippy |
| CI | `dtolnay/rust-toolchain@1.97.0` |
| GUI | eframe / egui 0.35.0、icons 0.7.0 |
| renderer | `Renderer::Glow`、Glow active、WGPU inactive |
| dependency graph | 465 package、MSRV fallbackなし、Git dependencyなし |
| warnings | compatibility 0、unused direct dependency 0、既知の `spad_overlay_lines` 1件 |

Phase 8はpackage version、Cargo.lockのroot package version、CHANGELOG、最終監査文書だけを変更し、`.rs`、dependency version、feature、target条件、UI / lifecycle、archive / cache / IPCには差分を加えなかった。

## 5. Phase別実施記録

### Phase 1: Rust 2024 compatibility semantics

edition 2021を維持したまま、`RUSTFLAGS=-Wrust-2024-compatibility cargo check --locked` の46 warningを0件にした。内訳は `missing_unsafe_on_extern` 1件、`if_let_rescope` 10件、`tail_expr_drop_order` 35件だった。

単なる構文置換ではなく、Rust 2021と2024で観測可能な意味を揃えるため、temporary、lock、artifact gate、resource ownership、drop順を明示した。

- User32 `GetSystemMetrics` のFFI境界を `unsafe extern` とし、ABIと呼出側のunsafe責務を維持した。
- `File::create`、disk cache、WebP decoder、DXGI COM object、warmup textureの結果を `match` または明示bindingにし、temporaryの生存範囲を固定した。
- artifact gateを保存・削除・renameの開始前に取得し、操作完了まで保持した。ErrorのDropだけをstatement化し、guard / cache handleの解放順を変えなかった。
- IPC receive結果を明示bindingにし、送受信と通知順を維持した。
- pending lockの結果をbindingし、lock保持範囲をscope内に固定した。
- `TextureHandle` はcloneを増やさずcacheへmoveし、GPU resource ownershipを維持した。
- log用Stringやpath表示をbindingし、macro内borrowのtemporary lifetimeを安定化した。

GUI実機、multi-monitor / DPI、COM/OLE drag、実archive / image corpusはこのPhaseでは未実施だった。

commit: `261aac142ed254d9aafd537558ba397ea1246f6a` `refactor: preserve semantics for Rust 2024 compatibility`

### Phase 2: Rust 1.97 / edition 2024 / resolver 3

Rust / Cargoを1.97.0に固定し、`edition = "2024"`、`rust-version = "1.97"`、root `[workspace]` の `resolver = "3"` を設定した。`rust-toolchain.toml` はchannel 1.97.0、minimal profile、rustfmt / clippy componentとし、CIのRust installも `dtolnay/rust-toolchain@1.97.0` に固定した。

Rust 1.97で `-D warnings` 対象になった `float_literal_f32_fallback` は、expected typeが`f32`のStroke幅58箇所を明示して対応した。`collapsible_if`、`manual_is_multiple_of`、`manual_checked_ops`、`let_and_return` は、Phase 1で明示した評価順、temporary、lock、drop範囲を自動的な畳み込みで変えないため、crate-level allowを採用した。

Cargo.lockには差分がなく、package version、checksum、source、dependency edge、lock format、MSRV fallbackの変更もなかった。依存更新とGUI更新はこのPhaseに含めなかった。

commit: `e2858eca6a11f5afbe0a59b5939e1a9b8ac4286b` `build: migrate to Rust 2024 and Rust 1.97`

### Phase 3: compatible direct dependencies

manifestの既存version要件を変更せず、Cargo.lock上の次の直接依存を更新した。source修正はなかった。

| crate | 更新前 | 更新後 |
| --- | ---: | ---: |
| `serde_json` | 1.0.149 | 1.0.150 |
| `toml` | 1.1.2 | 1.1.3 |
| `chrono` | 0.4.44 | 0.4.45 |
| `tokio` | 1.52.1 | 1.52.3 |
| `memmap2` | 0.9.10 | 0.9.11 |
| `blake3` | 1.8.4 | 1.8.5 |
| `bytes` | 1.11.1 | 1.12.1 |
| `anyhow` | 1.0.102 | 1.0.103 |
| `log` | 0.4.29 | 0.4.33 |

`regex 1.13.1` は調査時の更新候補だったが、Phase 3では `rust-version = "1.97"` とresolver 3による実際の解決結果を基準とし、オンライン上の最新版へ強制固定しなかった。実lockは `regex 1.12.3` を維持し、MSRV fallbackや解決失敗がない状態で検証済みだったため、その結果を採用した。

commit: `6a655fa1e59624a520629b4b114eae60fb63c21b` `build: update compatible direct dependencies`

### Phase 4: eframe / egui 0.34.3 baseline

0.35のlifecycle / root UI移行前に、API移行差分とpatch更新差分を分離できるclean baselineを作った。`eframe` を0.34系列から0.34.3へ限定更新し、egui familyもlock上で0.34.3に揃えた。`egui_material_icons` は0.6.0のまま維持し、source修正は不要だった。

`default-features = false` と `glow`, `x11`, `wayland`, `accesskit`, `default_fonts`, `web_screen_reader` を維持し、native rendererもGlowのままとした。WGPU rendererは採用していない。GUI実機確認はこのPhaseでは未実施だった。

commit: `673f461bd5533e8cff2a32f03fd6f78e5c89667c` `build: update eframe and egui to 0.34.3`

### Phase 5: 非GUI主要4クレート

相互に独立した4直接依存を限定更新した。

| crate | 更新前 | 更新後 |
| --- | ---: | ---: |
| `zip`（rename: `zip_writer`） | 2.4.2 | 8.6.0 |
| `fast_image_resize` | 4.2.3 | 6.0.0 |
| `lru` | 0.12.5 | 0.18.1 |
| direct `quick-xml` | 0.38.4 | 0.41.0 |

Cargo.tomlは4 version指定だけを変更し、既存feature、`default-features = false`、rename、target条件を維持した。source変更は `src/infra/archive/epub.rs` の局所deprecated allowだけだった。quick-xml 0.41の後継attribute APIはXML 1.0 whitespace normalizationを追加するため、既存のdecode-and-unescapeだけという意味を変える単純置換を避けた。

間接 `quick-xml 0.39.2` はWayland scannerとzbus XML系の上位依存由来で、直接0.41.0とは用途が異なるため残した。zip更新では `typed-path` が追加され、`arbitrary` / `derive_arbitrary` が削除された。MSRV fallback、Git source、対象外の直接依存更新、renderer変更はなかった。GUI実機確認はこのPhaseでは未実施だった。

commit: `8b57dfb103e1600f54c38dfe8bfd370988784ce6` `build: update core non-GUI dependencies`

### Phase 6: eframe / egui 0.35 lifecycle / root UI

`eframe` / egui familyを0.34.3から0.35.0へ、`egui_material_icons` を0.6.0から0.7.0へ更新した。新しいegui関連直接依存は追加せず、Glow featureと `Renderer::Glow` を維持した。`egui-wgpu 0.35.0` packageはlockに存在するがactive dependencyではなく、WGPU rendererは非採用である。

source変更は `src/app/mod.rs` と `src/app/viewer_app.rs` に限定し、Library / Viewerの `eframe::App` 実装を `logic` / `ui` に最小分離した。

- Libraryではframe前半の状態更新を `logic`、Settings・Library UI・UI後段処理を `ui` に置いた。ThumbWorker結果を次frameへ送らず、worker completion、状態反映、UI、後段worker applyの順序を維持した。
- ViewerではLibrary IPC receiveをUI前、boundary preview requestと設定反映をUI後に維持した。
- close frameではreading-session IPC、boundary preview close、viewport closeの順を維持し、旧早期returnと同じframeのUI抑止に `skip_ui_this_frame` だけを追加した。
- eframe 0.35が渡すroot `&mut egui::Ui` にLibrary / Viewerのpanelを接続した。panel、overlay、modal、dialogの描画順とframe styleは維持した。

icon / font初期化、keyboard / pointer / mouse wheel、IME / text input、clipboard、drag-and-drop、viewport、monitor / DPI / geometry、TextureHandle ownership、L1 / L2 / SPAD、Warmup / History / Future、Viewer frame cache、repaint条件は変更しなかった。Phase 1で明示したtemporary、lock scope、drop順にも変更を加えていない。

Phase結果文書作成時点ではGUI実機確認は未実施で、CLI検証とsource経路確認までだった。commit後にユーザーがLibrary / ViewerのGUI実機確認を行い、問題がないことを確認した。

commit: `8c83c55e023dc95ec6791632e5aad0e869f5e3b9` `build: migrate eframe and egui to 0.35`

### Phase 7: 証明済みcrate整理

Phase 6のGUI実機確認成功を基準に、不要と証明できたものだけを整理した。

- source、build script、CI、repository内の各targetを検索し、unused dependency lintでも唯一検出されたdirect dev-dependency `tempfile = "3"` を削除した。
- `tempfile` の直接edge削除で、既存の `CreateNamedPipeW` / `CreateFileW` が必要とするfeatureを偶発的に同依存から受けていたことが判明した。直接依存 `windows-sys 0.61.2` に `Win32_Security` を明示し、本来の依存要件を表現した。
- `tempfile 3.27.0` package自体は `uds_windows -> zbus -> accesskit_unix -> accesskit_winit -> egui-winit -> eframe` の全target推移依存として必要なためCargo.lockに残した。
- Viewerの不要な `#[allow(deprecated)]` 2件を削除した。UI scopeや処理順は変えなかった。
- crate-level Clippy allow 4件を外すと複数moduleに57件の既存指摘が発生し、広範な書換えと評価順・drop順の再監査が必要になるため維持した。
- quick-xmlの局所deprecated allowは、後継APIのwhitespace normalizationによる意味変更を避けるため維持した。

direct dev edgeが選んでいた `getrandom 0.4` とWASI / WIT系経路が不要になり、17 packageを削除した。概要は `wasip3`、`wit-bindgen-core/rust/rust-macro`、`wit-component`、`wit-parser`、`wasm-encoder/metadata/parser` とそれらに伴う重複packageの削除である。package追加、version更新、source / checksum変更はなかった。

commit: `200a2cf6beb9f078f4d1472232e0d92c5b26f087` `refactor: remove proven obsolete dependencies`

### Phase 8: 最終監査とversion 0.3.0

全PhaseのGit履歴、manifest、lockfile、metadata、duplicate tree、feature tree、renderer、warningを照合した。package versionを0.2.2から0.3.0へ更新し、Cargo.lockはroot package versionの1行だけを更新した。source差分なしで最終Phaseを完了した。

Phase 8 commit: `f1988e3b7249a2798057db14f3eb2929518a636b` `chore: finalize Rust 2024 dependency upgrade`

## 6. 重要な設計・実装判断

- Rust 2024 compatibilityはlint件数の消去ではなく、temporary、lock、artifact gate、IPC、GPU / COM resourceの観測可能な順序を維持する作業とした。
- Rust 2024 compatibility修正とedition切替を別commitにし、意味修正とtoolchain / resolver変更を分離した。
- Rust 1.97 lintは、型が明確なfloat literalを明示し、広範な評価順変更につながるClippy提案はcrate-level allowで保留した。
- Cargo resolver 3のRust 1.97解決を採用し、MSRVを満たさない最新版や不要な強制pinを導入しなかった。
- GUI major移行前に0.34.3 baselineを作り、dependency patchと0.35 API移行の差分を分離した。
- eframe 0.35移行ではlifecycleとroot UIの必須変更に限定し、worker / IPC / UI後段の相対順、close frameのUI抑止、save / shutdown、repaintを維持した。
- rendererは既存Glowを維持し、lockに存在するだけのWGPU packageをactive化しなかった。
- quick-xmlではdeprecated解消よりXML解釈の維持を優先し、局所allowを残した。
- crate整理は「未使用を証明できるもの」に限定し、feature縮小、推移依存の強制統合、crate置換を行わなかった。

## 7. 最終依存構成

括弧内は最終lock versionである。

| 分類 | 直接依存 |
| --- | --- |
| serialization | `serde 1` (1.0.228), `serde_json 1` (1.0.150), `toml 1` (1.1.3), `chrono 0.4` (0.4.45) |
| UI | `eframe 0.35` (0.35.0), `egui_material_icons 0.7` (0.7.0) |
| async / synchronization | `tokio 1` (1.52.3), `parking_lot 0.12` (0.12.5) |
| archive | `memmap2 0.9` (0.9.11), `flate2 1` (1.1.9), rename `zip_writer = zip 8.6` (8.6.0), optional `unrar 0.5` (0.5.8), optional `unrar_sys 0.5` (0.5.8) |
| image / decoder | `image 0.25` (0.25.10), `webp 0.3` (0.3.1), `libwebp-sys 0.9.6`, `zune-jpeg 0.5` (0.5.15), `zune-core 0.5` (0.5.1), `fast_image_resize 6` (6.0.0), optional `mozjpeg 0.10` (0.10.13) |
| cache | `lru 0.18` (0.18.1) |
| utility / parsing / log | `blake3 1` (1.8.5), `bytes 1` (1.12.1), `encoding_rs 0.8` (0.8.35), `natord 1` (1.0.9), `anyhow 1` (1.0.103), `regex 1` (1.12.3), `quick-xml 0.41` (0.41.0), `tracing 0.1` (0.1.44), `tracing-subscriber 0.3` (0.3.23), `log 0.4` (0.4.33) |
| Windows | `clipboard-win 5` (5.4.1), `raw-window-handle 0.6` (0.6.2), `windows 0.62.2`, `windows-core 0.62.2`, `windows-sys 0.61.2`, `wmi 0.18.4` |
| Windows build | target別build-dependency `winresource 0.1` (0.1.31) |

default featureは `mozjpeg`, `rar`, `avif`, `zlib-ng` である。eframeは `default-features = false` を維持し、`glow`, `x11`, `wayland`, `accesskit`, `default_fonts`, `web_screen_reader` を明示する。

## 8. Cargo.lock・duplicate・feature監査

最終lockは465 packageだった。基準commitの490 packageからnet 25減で、主な内訳はcompatible update、主要crateのversion置換、zip 8.6の `typed-path`、egui 0.35 text / paint系7 package追加、旧icons圧縮経路14 package削除、direct dev edgeに由来したWASI / WIT系17 package削除で説明できる。Git dependency、crates.io以外のsource、checksum異常、説明不能なpackage、MSRV fallbackはなかった。

duplicateは次の理由で維持した。

- `hashbrown 0.13.2 / 0.16.1 / 0.17.1`: mp4parse、accesskit、indexmap / lru / vello系の上位依存要件。
- direct `quick-xml 0.41.0` とtransitive `quick-xml 0.39.2`: EPUB parserとWayland / zbus XML系で用途が異なる。
- `windows-sys 0.52.0 / 0.59.0 / 0.60.2 / 0.61.2`: winit / glutin / rustix、nu-ansi-term、arboard / uds_windows、root / eframe / async系の要求差。
- 同versionの `log`、`serde_core`、`smallvec`、`toml`: host / targetまたはfeature unit表示で、別version混入ではない。

Windows feature treeではrootの `windows-sys/Win32_Security`、`eframe/glow`、`egui_glow 0.35.0`、`glow 0.17.0` がactiveだった。`egui-wgpu 0.35.0` はactive dependencyではない。egui 0.34以前の説明不能な残留と0.36以降の混入はなかった。

## 9. 検証結果

各Phaseでは対象変更後に同種のlocked検証を行い、最終的にPhase 8のpackage 0.3.0で次をすべて成功させた。検証コマンドは重複を避けてここに集約する。

| command / 確認 | 最終結果 |
| --- | --- |
| `rustc --version --verbose` | Rust 1.97.0、MSVC host |
| `cargo --version --verbose` | Cargo 1.97.0 |
| `rustup show active-toolchain` | project override 1.97.0 |
| `cargo check --locked` | `cbz-viewer 0.3.0`で成功 |
| `RUSTFLAGS=-Wrust-2024-compatibility cargo check --locked` | 成功、compatibility warning 0件 |
| `RUSTFLAGS=-Wunused-crate-dependencies cargo check --all-targets --locked` | 成功、unused direct dependency warning 0件 |
| `cargo clippy --all-targets --locked -- -D warnings` | 成功 |
| `cargo build --release --locked` | 成功、既知の `spad_overlay_lines` warning 1件のみ |
| `cargo metadata --locked --format-version 1` | root 0.3.0、465 package |
| `cargo tree --locked` | 成功 |
| `cargo tree -d --locked` | 成功、全duplicateを分類 |
| `cargo tree --target x86_64-pc-windows-msvc -e features --locked` | 成功、Glow / `Win32_Security` active |
| 対象crate / inverse tree抽出 | egui 0.35.0、icons 0.7.0、Glow active、WGPU inactive |
| `git diff --check` | 成功 |

repository方針に従いunit testは追加・実行していない。全面format、`cargo fix`、`cargo clippy --fix`、引数なしの `cargo update` も実行していない。

## 10. GUI実機確認

Phase 1から5の各結果時点ではGUI実機確認は未実施だった。Phase 6の結果文書作成時点でもCLI検証とsource経路確認までで、GUI実機確認は残件だった。

Phase 6 commit後、ユーザーがLibrary / ViewerのGUI実機確認を実施し、問題がないことを確認した。この成功をPhase 7開始のgateとした。Phase 7はUI挙動を変えるsource変更がなく、Phase 8はsource差分がないため、追加のGUI実機確認は実施していない。

## 11. commit一覧

| Phase | commit | subject |
| --- | --- | --- |
| 1 | `261aac142ed254d9aafd537558ba397ea1246f6a` | `refactor: preserve semantics for Rust 2024 compatibility` |
| 2 | `e2858eca6a11f5afbe0a59b5939e1a9b8ac4286b` | `build: migrate to Rust 2024 and Rust 1.97` |
| 3 | `6a655fa1e59624a520629b4b114eae60fb63c21b` | `build: update compatible direct dependencies` |
| 4 | `673f461bd5533e8cff2a32f03fd6f78e5c89667c` | `build: update eframe and egui to 0.34.3` |
| 5 | `8b57dfb103e1600f54c38dfe8bfd370988784ce6` | `build: update core non-GUI dependencies` |
| 6 | `8c83c55e023dc95ec6791632e5aad0e869f5e3b9` | `build: migrate eframe and egui to 0.35` |
| 7 | `200a2cf6beb9f078f4d1472232e0d92c5b26f087` | `refactor: remove proven obsolete dependencies` |
| 8 | `f1988e3b7249a2798057db14f3eb2929518a636b` | `chore: finalize Rust 2024 dependency upgrade` |

## 12. 維持した例外と既知事項

- release buildの未使用method `spad_overlay_lines` warning 1件は更新起因ではなく、今回の範囲外として維持した。
- crate-level Clippy allowの `collapsible_if`、`let_and_return`、`manual_checked_ops`、`manual_is_multiple_of` は、評価順・drop順を含む広範な再監査が必要なため維持した。
- quick-xmlの局所deprecated allowはXML whitespace normalizationによる意味変更を避けるため維持した。
- `tempfile 3.27.0` は直接依存ではないが、全targetの推移依存として残る。
- `libwebp-sys` の別系列への単独更新、`natord` 置換、native DLL更新、renderer変更は対象外とした。
- lockのduplicateは数の削減を品質目標にせず、上位依存・target・feature unitの理由を監査して維持した。

## 13. 残件

更新計画の実装、CLI検証、GUI実機確認、履歴統合に残件はない。既知の `spad_overlay_lines` warningは別作業の候補である。branchのmain統合、push、CI確認、tag / Release作成は本作業では実施していない。
