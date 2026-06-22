# Page Map 形式別実験記録と現行採用状況

## 1. 目的

この文書は、CBZ Viewer の Page Map について、形式別の実験結果と、現在の採用状況を整理するための記録である。

Page Map は、各ページについて本格デコード前に次の軽量情報を取得するための仕組みである。

```text
page_index
entry_index
image format
width
height
```

Page Map は、読書の必須条件ではない。
Viewer の読書体験を安定させるための補助情報である。

主な利用目的は次の通りである。

```text
ページ進捗表示
AUTO見開き判定
Streaming / cache 計画
ページ数の一貫性確認
```

Page Map が生成できない場合でも、Viewer・Library・通常 decode 経路は動作可能とする。

---

## 2. 現在の結論

現在、Page Map は次の経路で採用済みである。

```text
ZIP / CBZ
  FAST Page Map 採用済み

FolderBook
  FAST Page Map 採用済み

EPUB画像本
  EPUB読書順に基づく FAST Page Map 採用済み

RAR / CBR
  SLOW Page Map 採用済み
```

重要な点として、RAR / CBR は FAST Page Map ではない。
採用しているのは、低速スロットで動作する SLOW Page Map 経路である。

RAR / CBR の現在採用している方式は次である。

```text
callback全量受信
+
JPEG / PNG lightweight metadata probe
```

Viewer は読書中に SLOW Page Map を起動しない。
SLOW Page Map は Library / PageMapCoordinator 側のバックグラウンド処理として扱う。

---

## 3. Page Map の生成経路

Page Map 生成には、大きく分けて FAST と SLOW がある。

```text
FAST Page Map
  軽量metadataだけで全ページ情報を取得できる場合に使う

SLOW Page Map
  FASTで作れない場合のフォールバック
  より重い読み取りを許容する
  バックグラウンドで実行する
```

Page Map はサムネイル生成と関係するが、同じ責務ではない。

```text
優先サムネイル生成
  = サムネイル + FAST Page Map

リトライサムネイル生成
  = サムネイルのみ

SLOW Page Map
  = FAST Page Map失敗時の別系統フォールバック
```

リトライサムネイル生成に Page Map 生成を混ぜない。
SLOW Page Map を thumbnail retry queue と混ぜない。

---

## 4. コンテナ形式

現在扱う主なコンテナ形式は次である。

```text
ZIP
CBZ
RAR
CBR
FolderBook
EPUB画像本
```

`CBZ` は ZIP 系として扱う。

`CBR` は RAR 系として扱う。

EPUB画像本は ZIP 系コンテナだが、ページ順は ZIP / CBZ の natural sort ではなく、EPUB 自身の読書順を使う。

---

## 5. 画像形式

CBZ Viewer が画像として扱う主な形式は次である。

```text
JPEG
PNG
WebP
AVIF
BMP
GIF
TIFF
```

Page Map における扱いは、画像表示対応とは別に判断する。

```text
画像として表示できる
```

ことと、

```text
Page Mapを高速・安定して事前生成できる
```

ことは同じではない。

---

# 6. 形式別の現在状況

## 6.1 ZIP / CBZ + JPEG

### 状態

```text
FAST Page Map:
採用済み

軽量解析:
成立

実データ計測:
あり

単体テスト:
あり
```

### 軽量取得方式

```text
少量ずつ Deflate 展開
↓
JPEG marker を走査
↓
SOF marker から width / height を取得
↓
画像全体を展開せず終了
```

baseline JPEG と progressive JPEG を対象とする。

Stored entry は mmap 上の slice を直接参照し、entry 全量コピーを避ける。

### 判定

```text
現行FAST採用
```

ZIP / CBZ + JPEG は、現在の FAST Page Map の主対象である。

---

## 6.2 ZIP / CBZ + PNG

### 状態

```text
FAST Page Map:
採用済み

軽量解析:
成立

実データ計測:
あり

単体テスト:
あり
```

### 軽量取得方式

```text
PNG signature確認
↓
IHDR確認
↓
width / height取得
↓
画像全体を展開せず終了
```

Page Map では画像全体の CRC 検証は行わない。

Stored entry は mmap 上の slice を直接参照する。

### 判定

```text
現行FAST採用
```

ZIP / CBZ + PNG は、現在の FAST Page Map の主対象である。

---

## 6.3 ZIP / CBZ + WebP

### 状態

```text
FAST Page Map:
未採用

軽量解析:
未実装

fallback:
SLOW / metadata経路で取得可能な場合あり

現行判定:
FAST対象外
```

WebP は画像表示対象ではあるが、ZIP / CBZ の FAST Page Map 対象ではない。

軽量metadataで全ページを高速に確定する経路は現時点では採用していない。

### 判定

```text
FAST未採用
必要に応じてSLOW側で扱う
```

---

## 6.4 ZIP / CBZ + AVIF

### 状態

```text
FAST Page Map:
未採用

軽量解析:
未実装

実データ計測:
あり

現行判定:
FAST対象外
```

過去実験では、AVIF は metadata 解析が重いことが確認されている。

圧縮entryの展開よりも、AVIF metadata 解析が支配的になる。

### 判定

```text
FAST未採用
通常のViewer decode経路には影響させない
```

---

## 6.5 ZIP / CBZ + BMP / GIF / TIFF

### 状態

```text
FAST Page Map:
未採用

Page Map専用軽量解析:
未採用

現行判定:
FAST対象外
```

これらは画像表示対象になり得るが、FAST Page Map の主対象ではない。

Page Map として扱う場合は、FASTではなくSLOWまたは通常metadata取得側の問題として扱う。

### 判定

```text
FAST未採用
```

---

# 7. RAR / CBR の現在状況

## 7.1 現在の採用状況

RAR / CBR は、現在 SLOW Page Map として採用済みである。

```text
RAR / CBR
  => SLOW Page Map経路で正式接続済み
```

採用しているのは FAST Page Map ではない。

RAR は archive 内の読み取り特性上、ZIP / CBZ のように安価な FAST 経路として扱わない。

---

## 7.2 採用方式

現在採用しているRAR系の基本方針は次である。

```text
RAROpenArchiveEx
RARSetCallback
RARReadHeaderEx 順次走査
RARProcessFileW(..., RAR_TEST, ...)
UCM_PROCESSDATA を受信
callbackから中断しない
entry完了後にmetadata取得
JPEG / PNG は lightweight metadata probe を優先
```

重要なのは、callback中断方式ではないことである。

```text
採用:
  callback全量受信 + lightweight metadata probe

不採用:
  callback中断方式
```

---

## 7.3 callback中断方式を採用しない理由

過去実験では、小規模 RAR5 では callback 中断後に同一 handle で次 entry へ進める可能性が観測された。

しかし、巨大実データでは同じ位置で破綻した。

```text
RARProcessFileW:
  ERAR_UNKNOWN

次のRARReadHeaderEx:
  ERAR_BAD_DATA
```

また、UnRAR が callback に渡す時点で、実質的にかなりの量を展開していることも分かった。

そのため、callback 中断方式は正式経路へ接続しない。

### 判定

```text
callback中断方式:
不採用
```

---

## 7.4 callback全量受信方式

callback全量受信方式では、entryごとに展開済みデータを受信し、entry完了後にmetadataを取得する。

この方式では、temp file write/read/remove を避けられる。

過去計測では、temp file方式より大幅に改善した。

```text
tempfile方式:
  約25秒

callback全量受信:
  約11.5秒
```

### 判定

```text
成立
SLOW Page Map採用の基礎
```

---

## 7.5 callback全量受信 + lightweight metadata probe

現在採用しているRAR系SLOW Page Mapでは、JPEG / PNG に対して lightweight metadata probe を優先する。

```text
callback全量受信
↓
JPEG / PNG lightweight metadata probe
↓
成功時は通常metadata解析を呼ばない
```

巨大JPEG RARの過去計測では、metadata時間を大きく削減できた。

```text
metadata:
  約1344ms
  ↓
  約3ms
```

ただし、read/decompress 自体は必要である。

そのため、RAR / CBR は FAST ではなく SLOW として扱う。

### 判定

```text
RAR / CBR SLOW Page Mapとして採用済み
```

---

## 7.6 non-solid RAR parallel PoC

non-solid RAR では、複数 handle を使った parallel PoC が有望だった。

過去実験では、parallel-4 が速度とPeak Working Setのバランスで良好だった。

ただし、このPoCは正式経路には接続しない。

```text
parallel PoC:
  ignored test / 参考実験

正式経路:
  未接続
```

### 判定

```text
参考記録として保持
正式採用しない
```

---

# 8. FolderBook の現在状況

FolderBook は FAST Page Map に対応済みである。

FolderBook では archive 展開が不要であり、フォルダ内画像を reader のページ順で扱う。

```text
FolderBook
  => FAST Page Map採用済み
```

ただし、大量ファイルを含む FolderBook では、ファイルI/Oの都合でZIPより重くなる場合がある。

これはSLOW落ちではなく、FAST生成自体が重いケースとして扱う。

### 判定

```text
現行FAST採用
```

---

# 9. EPUB画像本の現在状況

EPUB画像本は、現在 Page Map に対応済みである。

```text
EPUB画像本
  => EPUB読書順に基づく Page Map採用済み
```

EPUB は ZIP コンテナだが、ZIP / CBZ の natural sort とは異なる。

EPUB には自身の読書順があるため、Page Map もそれに従う。

---

## 9.1 EPUB のページ順

EPUB画像本のページ順は、次の情報から決定する。

```text
META-INF/container.xml
↓
OPF package document
↓
manifest
↓
spine
↓
XHTMLページ内の画像参照順
```

XHTML内の画像参照として、次を扱う。

```text
<img src="">
<image href="">
<image xlink:href="">
```

`svg:image` 形式も、XHTML内の raster image 参照として扱う。

ただし、SVGファイルそのもののレンダリングや、SVGファイルを開いて内部を再帰解析する処理は対象外である。

---

## 9.2 EPUB FAST Page Map

EPUB画像本では、読書順に確定した全ページ画像に対して FAST Page Map を試みる。

FAST Ready になる条件は次である。

```text
全ページ画像が JPEG / PNG
かつ
lightweight metadata取得に成功
```

1ページでも WebP / GIF / その他のFAST非対象形式が含まれる場合、FAST Ready にはしない。

その場合は、Library側のSLOW Page Map候補になる。

ViewerからSLOW Page Mapは起動しない。

### 判定

```text
EPUB画像本 FAST Page Map採用済み
```

---

## 9.3 EPUB SLOW Page Map

EPUB画像本で FAST が成立しない場合、Library側のSLOW Page Mapで補完可能である。

SLOWでは、EPUBの読書順を維持したまま、通常metadata取得でPage Mapを作る。

Viewerは読書中にEPUB SLOW Page Mapを起動しない。

```text
Library:
  FAST失敗時にSLOW候補

Viewer:
  起動時にcache hitまたはFASTのみ
  SLOWは起動しない
```

---

## 9.4 DRM / encryption.xml

DRM保護されたEPUBは対象外である。

`META-INF/encryption.xml` が存在する場合、DRMまたは暗号化EPUBとして扱い、恒久失敗に分類する。

```text
DRM EPUB:
  retryしない
  SLOWへ流さない
  natural sort fallbackしない
```

### 判定

```text
DRM EPUB:
非対応
恒久失敗
```

---

# 10. Viewer での Page Map 方針

Viewer は、起動時点でその読書セッションにおける Page Map 利用可否を確定する。

```text
Viewer起動
  |
  +-- 既存Page Mapあり
  |     => Mapped
  |
  +-- cache miss かつ FAST生成可能
  |     => FAST生成してMapped
  |
  +-- FAST生成不可
        => Unavailable
```

Viewer は読書中に SLOW Page Map を起動しない。

理由は、読書中に Page Map の有無が変わると、次のような表示・計画が変化する可能性があるためである。

```text
Progress Bar
AUTO見開き
Streaming Plan
cache計画
```

読書セッション中の前提を固定することで、読書体験を安定させる。

---

# 11. 現在の信頼度一覧

| コンテナ       | 画像形式        | 現在の経路                    | 現在の判定      |
| ---------- | ----------- | ------------------------ | ---------- |
| ZIP / CBZ  | JPEG        | FAST                     | 採用済み       |
| ZIP / CBZ  | PNG         | FAST                     | 採用済み       |
| ZIP / CBZ  | WebP        | FAST対象外 / 必要時SLOW        | FAST未採用    |
| ZIP / CBZ  | AVIF        | FAST対象外 / 必要時SLOW        | FAST未採用    |
| ZIP / CBZ  | BMP         | FAST対象外                  | 未採用        |
| ZIP / CBZ  | GIF         | FAST対象外 / 必要時SLOW        | FAST未採用    |
| ZIP / CBZ  | TIFF        | FAST対象外                  | 未採用        |
| RAR / CBR  | JPEG        | SLOW + lightweight probe | 採用済み       |
| RAR / CBR  | PNG         | SLOW + lightweight probe | 採用済み       |
| RAR / CBR  | その他         | SLOW / metadata依存        | 限定対応       |
| FolderBook | JPEG / PNG  | FAST                     | 採用済み       |
| FolderBook | その他         | FAST不可時SLOW候補            | 形式依存       |
| EPUB画像本    | JPEG / PNG  | EPUB読書順FAST              | 採用済み       |
| EPUB画像本    | WebP / GIF等 | EPUB読書順SLOW候補            | FAST未採用    |
| DRM EPUB   | すべて         | なし                       | 非対応 / 恒久失敗 |

---

# 12. 現在の採用方針

## 12.1 FAST Page Map

FAST Page Map の主対象は次である。

```text
ZIP / CBZ + JPEG / PNG
FolderBook + JPEG / PNG
EPUB画像本 + JPEG / PNG
```

FAST Page Map は、軽量metadata取得で全ページを確定できる場合だけ Ready とする。

混在形式でFAST対象外画像を含む場合は、FAST Ready にしない。

---

## 12.2 SLOW Page Map

SLOW Page Map の主対象は次である。

```text
RAR / CBR
FAST Page Map が RequiresComplete になった本
軽量metadataだけでは確定できない本
```

SLOW Page Map はバックグラウンド処理として扱う。

サムネイル表示、Viewer起動、通常読書を妨げない。

---

## 12.3 対象外

次は、Page Mapまたは本そのものの対象外として扱う。

```text
DRM / encrypted EPUB
破損archive
読書順を確定できないEPUB
画像ページを持たないEPUB
```

対象外は、画像表示対応とは別の判断である。

Page Map対象外でも、通常のViewer decodeで読める本は読めるようにする。

---

# 13. GPUとの関係

Page Map はGPU処理の必須条件ではない。

```text
Page Mapあり
  => 形式・元寸法を事前利用できる
  => Streaming / cache / GPU計画を改善可能

Page Mapなし
  => 従来どおりページ処理時に判定する
  => CPU decode / fallbackで表示可能
```

Page Map がないことを理由に、GPU L1、通常描画、既存decode経路を止めてはいけない。

---

# 14. 設計上の注意点

Page Map は読書体験を改善するための補助データである。

そのため、次の原則を守る。

```text
Page Mapがなくても読める
Page Map生成失敗でLibrary UXを壊さない
SLOW Page MapをViewer読書中に起動しない
thumbnail retryとSLOW Page Mapを混ぜない
形式ごとの読書順を守る
```

特に EPUB画像本では、ZIP / CBZ の natural sort を流用しない。

EPUB は必ず EPUB 自身の読書順に従う。

---

# 15. 現在地点

Page Map は、ZIP / CBZ のJPEG / PNGだけを対象とする初期構想から、現在はより広い正式経路へ進んでいる。

現在の到達点は次である。

```text
ZIP / CBZ:
  FAST Page Map 採用済み

FolderBook:
  FAST Page Map 採用済み

RAR / CBR:
  SLOW Page Map 採用済み

EPUB画像本:
  EPUB読書順に基づく FAST / SLOW Page Map 対応済み
```

ただし、Page Map はあくまで補助情報である。

最終原則は次である。

```text
Page Map は読書体験を改善するが、読書の必須条件にしてはいけない。
```
