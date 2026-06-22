[English](README.md)

# cbz-tools-viewer

CBZ Viewer は、Windows 向けの漫画ビューアです。CBZ / ZIP / RAR / CBR / EPUB画像本と、直下に画像を持つフォルダを画像本として扱えます。

実行ファイルは `cbz-viewer.exe` です。

---

# ダウンロード

最新版は [Latest Release](https://github.com/cbz-tools/cbz-tools-viewer/releases/latest) からダウンロードできます。

ZIP を展開し、`cbz-viewer.exe` を直接実行してください。追加インストールは不要です。

単に本を読むだけではなく、次の機能を備えています。

* ライブラリ管理
* お気に入り管理
* グループ管理
* 本移動
* 外部ツール連携
* 既定言語は英語
* 設定から日本語へ切替可能
* 言語変更は即時反映
* 再起動不要

---

# Why

私は長年 ZipPla を利用していました。

その優れた閲覧体験は、本プロジェクトを開発する大きなきっかけとなりました。

CBZ Viewer は、自分自身が本当に使いたい Windows 向け漫画ビューアを目指して開発しています。

---

# Design Philosophy

CBZ Viewer は、ページ送りの待ち時間を最小化することを重視しています。

必要なページを優先的に処理し、大量ページの書籍でも快適に閲覧できるよう設計しています。

また、インターネット接続を必要としないオフライン完結のアプリケーションです。

---

# 主な機能

## 読む

* 右開き / 左開き
* AUTO / 単ページ / 見開き
* 表紙ブランク
* スライドショー
* 画質 4 モード
  * 速度優先
  * 標準
  * 画質優先
  * 原寸優先
* Page Map を使ったページ進捗表示
* L1 / L2 Streaming Cache
* アニメーションWebPのストリーミング再生（見開き表示対応）

アニメーション画像には一部の画質処理が適用されません。

## 管理する

* ライブラリ管理
* 検索
* 履歴
* お気に入り
* グループ

## 整理する

* 名前変更
* コピー
* 削除
* Explorer で開く

### 外部ツール連携

読書をしながら、

* 圧縮最適化
* フォーマット変換
* サイズ削減

を実行できます。

兄弟プロジェクトである **CBZ Tools Optimizer** と連携することで、閲覧と最適化をシームレスに行えます。

---

# 動作環境

* Windows 10
* Windows 11

---

# 対応形式

## Archive

* CBZ
* ZIP
* RAR
* CBR
* EPUB画像本

EPUB対応は、画像主体のEPUBを対象としています。CBZ Viewer は EPUB の読書順を使い、XHTMLページ内の画像参照を漫画ページとして扱います。

テキストEPUB、reflow layout、CSS layout再現、DRM保護されたEPUB、音声、動画、JavaScript、SVGそのものの描画には対応していません。

## Folder

* 直下に対応画像を持つフォルダを、画像本として開けます。

## Image

* JPEG
* PNG
* WebP（静止画 / アニメーション）
* AVIF（.avif / .avifs）
* BMP
* TIFF
* GIF

単体の対応画像ファイルから起動した場合は、親フォルダを画像本として開き、指定画像から表示を開始します。

---

# スクリーンショット

| Library | Viewer | Fullscreen |
|---|---|---|
| [![Library](docs/assets/screenshots/Library.png)](docs/assets/screenshots/Library.png) | [![Viewer](docs/assets/screenshots/Viewer_Windowed.png)](docs/assets/screenshots/Viewer_Windowed.png) | [![Fullscreen](docs/assets/screenshots/Viewer_Fullscreen.png)](docs/assets/screenshots/Viewer_Fullscreen.png) |

### デモコンテンツについて

スクリーンショットに使用している **Sovereign Stars** は、CBZ Viewer のデモおよびスクリーンショット撮影用として GPT で生成した架空の漫画作品です。

実在の作品、人物、団体とは関係ありません。

デモ漫画素材も MIT License です。

---

# インストール

Releases から ZIP をダウンロードし、任意のフォルダへ展開してください。

追加インストールは不要です。

---

# ドキュメント

詳細な操作方法については以下を参照してください。

* [操作説明](docs/operation.ja.md)
* [Library表示設定](docs/operation.ja.md#library-display-settings)
* [Danger Zone 設定からの復旧](docs/DANGER_ZONE_RECOVERY.md)
* [L1 / L2 Streaming Cache](docs/dev/SimpleStreaming.md)

実装やアーキテクチャの詳細については docs を参照してください。

---

# Acknowledgements

ZipPla に限らず、多くの既存ビューアの優れた機能やユーザー体験から学び、影響を受けています。

本プロジェクトはゼロから Rust で実装していますが、その背景には先人たちの積み重ねがあります。

素晴らしいソフトウェアを公開してくださった作者の皆様に感謝いたします。

---

# License

This project is licensed under the MIT License.

See the LICENSE file for details.

Third-party components are documented in THIRDPARTY_LICENSES.md.

Demo manga assets are also licensed under the MIT License.
