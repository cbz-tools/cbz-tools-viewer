# Third-Party Licenses

このファイルは、配布物に同梱するサードパーティ製バイナリコンポーネントを中心に記載する。
Rust crate 依存の網羅的なライセンス一覧ではない。Rust crate 依存は `Cargo.lock` および必要に応じて生成するライセンス一覧で管理する。

## UnRAR DLL

- 用途: RAR/CBR 読み込みバックエンドで利用
- 役割: RAR/CBR の展開と一覧取得にのみ利用する
- 注意: RAR 圧縮アルゴリズムの再作成や実装目的では利用しない
- 配置方針: DLL rename 前提は取らない
- 管理元 (Git): `third_party/unrar/x64/UnRAR64.dll`
- コピー方針: `build.rs` が `target_pointer_width` で判定し `target/<profile>/` (exe 横) へ自動コピー
- x64 build の実ロード名: `UnRAR64.dll` (exe 横)
- ロード方針: 起動時チェックは行わず、RAR open 時に lazy load
- 障害時挙動: DLL が無い/ロードできない場合でもアプリ全体は起動可能。RAR open のみ失敗扱い
- 配布物には `third_party/unrar/LICENSE.txt` 相当のライセンス原文を同梱する

### ライセンスメモ

- UnRAR DLL の再配布・利用条件は UnRAR 側ライセンスに従う
- 本プロジェクト本体ライセンスとは別管理とする

## dav1d DLL

- 用途: AVIF デコードバックエンド (`image/avif-native`) のランタイム依存
- ライセンス: BSD-2-Clause
- 管理元 (Git): `third_party/dav1d/dav1d.dll`
- コピー方針: `build.rs` が `target/<profile>/dav1d.dll` (exe 横) へ自動コピー
- ロード方針: Windows 標準 DLL 探索で exe 横からロード
- 配布物には `third_party/dav1d/LICENSE` 相当のライセンス原文を同梱する

### ライセンスメモ

- dav1d DLL の再配布・利用条件は dav1d 側ライセンスに従う
- 本プロジェクト本体ライセンスとは別管理とする
