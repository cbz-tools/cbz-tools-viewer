[English](README.md)

# cbz-tools-viewer

CBZ Viewer は、Windows 向けの漫画ビューアです。CBZ / ZIP / RAR / CBR / EPUB画像本と、直下に画像を持つフォルダを画像本として扱えます。

実行ファイルは `cbz-viewer.exe` です。

---

# ダウンロード

最新版は [Latest Release](https://github.com/cbz-tools/cbz-tools-viewer/releases/latest) からダウンロードできます。

ZIP を展開し、`cbz-viewer.exe` を直接実行してください。追加インストールは不要です。

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

# 主な特徴

* 現在の本を削除して、次の本へ進めます。
* ビューア上で不要なページ範囲を選択し、そのページを除外してアーカイブを再構築できます。
* 先読みとキャッシュにより、大量ページの本でもページ移動の待ち時間を抑えます。
* 隣接本の先読み: 次の本・前の本の近いページをバックグラウンドで準備し、本を移動した直後の待ち時間を軽減します。
* アニメーション WebP のストリーミング再生に対応し、見開き表示でも扱えます。

---

# 背景

私は長年 ZipPla を利用していました。

その優れた閲覧体験は、本プロジェクトを開発する大きなきっかけとなりました。

CBZ Viewer は、自分自身が本当に使いたい Windows 向け漫画ビューアを目指して開発しています。

---

# 設計方針

CBZ Viewer は、ページ送りの待ち時間を抑えることを重視しています。

PC の CPU / RAM / VRAM に基づいて、先読み、キャッシュ、サムネイル生成をバックグラウンドで処理し、大量ページの本でも快適に閲覧できるよう設計しています。

また、インターネット接続を必要としないオフライン完結のアプリケーションです。

---

# 主な機能

CBZ Viewer は、次の3つの作業をまとめて扱えます。

* 読む: ページ移動、見開き、スライドショー、進捗表示、先読みキャッシュ
* 管理する: ライブラリ、検索、履歴、お気に入り、グループ、本移動
* 整理する: 名前変更、コピー、削除、Explorer で開く、ページ範囲を除外したアーカイブ再構築

詳細は [操作説明](docs/operation.ja.md) を参照してください。

---

# 外部ツール連携

CBZ Viewer は、読書中に外部ツールを呼び出せます。

兄弟プロジェクト **CBZ Tools Optimizer** と連携することで、CBZ / ZIP の圧縮最適化、フォーマット変換、サイズ削減などを行えます。

---

# 動作環境

* Windows 10
* Windows 11

---

# 対応形式

## アーカイブ

* CBZ
* ZIP
* RAR
* CBR
* EPUB画像本

EPUB対応は、画像主体のEPUBを対象としています。CBZ Viewer は EPUB の読書順を使い、XHTMLページ内の画像参照を漫画ページとして扱います。

テキストEPUB、reflow layout、CSS layout再現、DRM保護されたEPUB、音声、動画、JavaScript、SVGそのものの描画には対応していません。

## フォルダ

* 直下に対応画像を持つフォルダを、画像本として開けます。

## 画像

* JPEG
* PNG
* WebP（静止画 / アニメーション）
* AVIF（.avif / .avifs）
* BMP
* TIFF
* GIF

単体の対応画像ファイルから起動した場合は、親フォルダを画像本として開き、指定画像から表示を開始します。

---

# ドキュメント

詳細な操作方法については以下を参照してください。

* [操作説明](docs/operation.ja.md)
* [Library表示設定](docs/operation.ja.md#library-display-settings)
* [Danger Zone 設定からの復旧](docs/DANGER_ZONE_RECOVERY.md)
* [L1 / L2 Streaming Cache](docs/dev/SimpleStreaming.md)
* [SPAD: Adjacent Book Scratchpad](docs/dev/Spad.md)

実装やアーキテクチャの詳細については docs を参照してください。

---

# 謝辞

ZipPla に限らず、多くの既存ビューアの優れた機能やユーザー体験から学び、影響を受けています。

本プロジェクトはゼロから Rust で実装していますが、その背景には先人たちの積み重ねがあります。

素晴らしいソフトウェアを公開してくださった作者の皆様に感謝いたします。

---

# ライセンス

This project is licensed under the MIT License.

See the LICENSE file for details.

Third-party components are documented in THIRDPARTY_LICENSES.md.

Demo manga assets are also licensed under the MIT License.
