# nvJPEG GPUバッチ画像処理 実験方針

## 1. 目的

本実験の目的は、nvJPEGを利用して多数のJPEGをGPUで一括処理した場合の性能特性を確認することである。

単一画像の最速デコードを主目的とはしない。

主に確認したいのは、次の2点である。

```text
1. 多数のJPEGを一括デコードし、VRAMを高速に充填できるか
2. GPU中心のパイプラインで大量サムネイルを高速生成できるか
```

特に、CPUデコードと比較して、

```text
大量処理時の総スループット
CPU資源の消費
初動コスト
適切なバッチサイズ
VRAM利用効率
```

を実測する。

---

## 2. 背景

現在のCBZ Image Viewerでは、CPUのBG workerが画像をデコードし、CPU側RGBA cacheを経由してGPU Textureへ転送している。

概念上は次の経路である。

```text
JPEG bytes
→ CPU decode
→ CPU RGBA
→ RAM cache
→ GPU Texture
```

nvJPEGを利用した場合、JPEGをGPUメモリへ直接デコードできる。

```text
JPEG bytes
→ nvJPEG batch decode
→ VRAM上のRGB
→ GPU上で必要な後処理
→ VRAMへ保持
```

この方式の価値は、単一画像のデコード時間よりも、

```text
多数ページを並列に処理できること
CPU側に巨大なRGBAを作らないこと
BG処理によるCPU競合を減らせること
```

にある。

---

## 3. 実験プロジェクトの位置づけ

本実験は、既存のCBZ Image Viewerへ直接実装しない。

独立したRustプロジェクトとして作成する。

理由は次の通り。

```text
nvJPEG自体の価値を先に確認する
既存Viewerのgeneration、cache、queue、UI同期を持ち込まない
失敗時に容易に破棄できる
測定条件を単純に保つ
成果を独立したRustライブラリへ発展させられる
```

既存Viewerへの統合は、実験結果が十分に有望だった場合にのみ検討する。

---

## 4. 実験対象

### 4.1 GPU VRAM充填

JPEG群をnvJPEGで一括デコードし、結果をCPUへ戻さずVRAMへ保持する。

```text
JPEG bytes（RAM）
→ nvJPEG batch decode
→ RGB（VRAM）
→ 必要に応じてGPU上でRGBA化
→ VRAM上に保持
```

画面表示は行わない。

D3D12、wgpu、eguiとの連携も対象外とする。

完了条件は、GPU処理の完了とVRAM上の出力保持である。

---

### 4.2 大量サムネイル生成

多数のJPEGから、GPU中心のパイプラインでJPEGサムネイルを生成し、ディスクへ保存する。

```text
JPEG bytes
→ nvJPEG decode
→ GPU resize
→ nvJPEG encode
→ 圧縮JPEG bytesのみCPUへ取得
→ disk write
```

サムネイルの保存形式はJPEGとする。

現在のViewerが使用しているWebP形式には合わせない。

実験段階では既存サムネイルキャッシュとの互換性を求めない。

---

## 5. 非目標

本実験では、次の項目を扱わない。

```text
CBZ Image Viewer本体への統合
画面表示
wgpu Textureとの共有
CUDAとD3D12の外部メモリ共有
GPU L1への直接登録
AMD / Intel向けGPU実装
非JPEG形式のGPUデコード
既存WebPサムネイルキャッシュとの互換性
DirectStorage
```

AMDおよびIntel環境は将来のViewer統合時にCPUフォールバックとする想定だが、本実験ではNVIDIA環境のみを対象とする。

---

## 6. 対象環境

基準環境は次の通り。

```text
OS      : Windows 11
CPU     : Ryzen 7 5700X
RAM     : 32 GiB
GPU     : GeForce RTX 3060 Ti 8 GiB
言語    : Rust
GPU API : CUDA / nvJPEG
```

RTX 3060 Tiには、A100等で利用される専用JPEGハードウェアデコーダーがない可能性がある。

そのため、劇的な単枚性能向上ではなく、

```text
画像間並列
大量バッチ処理
CPU負荷の移動
VRAM直接出力
```

を主な評価対象とする。

---

## 7. 入力データ

実際の漫画画像に近いJPEG群を使用する。

入力データには、可能な範囲で次を含める。

```text
Baseline JPEG
Progressive JPEG
異なる画像サイズ
異なる圧縮品質
異なるクロマサブサンプリング
縦長漫画ページ
小サイズ画像
破損または特殊JPEG
```

同一データセットをCPU経路とGPU経路の双方で使用する。

入力JPEGは事前にファイルまたはメモリへ準備し、ZIP展開時間は初期比較から除外する。

必要に応じて、後からディスク読込込みの測定を追加する。

---

## 8. 比較対象

### 8.1 CPU経路

CPU側は、現行Viewerに近いJPEGデコーダーを使用する。

```text
JPEG bytes
→ CPU decode
→ RGBAまたはRGB
```

並列数は次を比較する。

```text
1 worker
8 workers
```

CPU側の出力先はRAMとする。

---

### 8.2 GPU経路

```text
JPEG bytes
→ nvJPEG batch decode
→ VRAM上のRGB
```

必要に応じて、GPU上でRGBからRGBAへ変換する。

性能測定ではCPU readbackを行わない。

readbackは結果確認用の任意デバッグ機能としてのみ実装する。

---

## 9. バッチサイズ

最低限、次のバッチサイズを比較する。

```text
1
8
16
32
64
```

中心となる候補は32件である。

ただし、最適なバッチサイズを事前に固定しない。

以下の条件により性能が変化するため、実測で判断する。

```text
画像サイズ
JPEG形式
CUDA初期化状態
VRAM使用量
nvJPEGバックエンド
GPUメモリ帯域
CPU側に残るエントロピー処理
```

将来的には、枚数だけでなく出力容量による上限も検討する。

```text
最大画像数
最大入力JPEG bytes
最大出力VRAM bytes
```

---

## 10. 計測区間

GPU測定は、少なくとも次の3種類に分離する。

### 10.1 Cold

CUDAおよびnvJPEGの初期化を含む。

```text
process start
→ CUDA初期化
→ nvJPEG初期化
→ buffer確保
→ 最初のbatch完了
```

初回表示や初回BG実行に相当するコストを確認する。

---

### 10.2 Warm

初期化済み状態で、既存バッファを再利用する。

```text
decoder initialized
buffer pool ready
→ batch decode
→ completion
```

通常運用時のバッチ性能を確認する。

---

### 10.3 Steady

複数バッチを連続投入し、所定容量までVRAMを充填する。

```text
batch
→ batch
→ batch
→ ...
→ 1 GiB相当の出力をVRAMへ保持
```

大量処理時の持続性能を確認する。

---

## 11. VRAM充填性能の計測項目

最低限、次を記録する。

```text
バッチサイズ
画像枚数
入力JPEG合計bytes
出力RGB/RGBA合計bytes
初回完了時間
全件完了時間
images/sec
decoded MiB/sec
1 GiB到達時間
CPU使用率
GPU使用率
VRAM使用量
エラー件数
JPEG形式別の結果
```

時間計測は可能な限りCUDA Eventを使用する。

CPU側の準備時間とGPU実行時間を分離する。

```text
入力準備
nvJPEG API処理
GPU decode
RGB→RGBA
同期
```

を個別に測定できる構造が望ましい。

---

## 12. サムネイル生成条件

初期条件は次を想定する。

```text
出力形式 : JPEG
長辺または幅 : 320 px
アスペクト比 : 維持
JPEG quality : 80
alpha : 不要
```

必要に応じて、次も比較する。

```text
160 px
320 px
640 px

quality 70
quality 80
quality 90
```

縮小方式は初期実験では単純なGPU resizeでよい。

性能確認後に、画質と速度のバランスを評価する。

---

## 13. サムネイル生成の計測項目

次を記録する。

```text
最初の1枚が保存されるまでの時間
32枚完了時間
64枚完了時間
256枚完了時間
1000枚完了時間
images/sec
decode時間
resize時間
encode時間
host transfer時間
disk write時間
CPU使用率
GPU使用率
VRAM使用量
平均ファイルサイズ
生成失敗数
```

圧縮JPEGをディスクへ保存するため、最終的なJPEG bitstreamのCPU取得は許容する。

ただし、巨大なRGB/RGBA画像をCPUへreadbackしてはならない。

---

## 14. 正当性確認

性能測定時はCPUへ画像全体を戻さない。

ただし、実装確認用として限定的な検証経路を用意する。

例：

```text
最初の1枚だけreadback
特定batchから1枚だけreadback
生成JPEGを通常デコーダーで再読込
幅、高さ、形式を確認
画像hashまたは画素差を確認
```

検証処理は性能測定区間から除外する。

---

## 15. メモリ管理

毎回の`cudaMalloc`／`cudaFree`を避け、バッファを再利用する。

初期実装から、最低限次の所有権を分離する。

```text
nvJPEG handle
nvJPEG state
CUDA stream
入力メタデータ
RGB出力buffer
RGBA出力buffer
resize出力buffer
encode用buffer
```

バッファプールは、最初は固定容量でもよい。

32枚バッチを扱えることを初期目標とする。

VRAM不足時は明示的にエラーとし、自動的な複雑なevictionは実装しない。

---

## 16. Rust実装方針

nvJPEGのC APIは、`nvjpeg-sys`等のFFIを利用するか、必要なAPIだけの薄い自前FFIを作成する。

上位層にはunsafeを漏らさない。

想定構成：

```text
src/
├─ main.rs
├─ cpu/
│  └─ decode.rs
├─ gpu/
│  ├─ mod.rs
│  ├─ nvjpeg.rs
│  ├─ cuda.rs
│  ├─ buffer_pool.rs
│  ├─ batch.rs
│  ├─ resize.rs
│  └─ encode.rs
├─ benchmark/
│  ├─ mod.rs
│  ├─ metrics.rs
│  └─ report.rs
└─ verify/
   └─ image_check.rs
```

nvJPEGおよびCUDAリソースはRAIIで管理する。

```text
create
→ use
→ Dropでdestroy
```

エラーはRustの`Result`へ変換する。

---

## 17. 実装段階

### Phase 1: 環境確認

```text
CUDA Toolkit検出
nvJPEG DLL / import library確認
RustからnvJPEG初期化
GPU情報表示
1枚のJPEG情報取得
```

この段階では性能を評価しない。

---

### Phase 2: 単枚VRAMデコード

```text
JPEG bytes
→ nvJPEG
→ VRAM RGB
```

1枚の正常完了を確認する。

デバッグ用に1枚だけreadbackし、出力画像を確認してよい。

---

### Phase 3: バッチデコード

```text
1 / 8 / 16 / 32 / 64枚
→ nvJPEG batch
→ VRAM保持
```

Cold、Warmを測定する。

---

### Phase 4: 1 GiB VRAM充填

複数バッチを連続投入し、出力合計が1 GiB相当になるまで保持する。

```text
time to fill 1 GiB
decoded MiB/sec
CPU負荷
GPU負荷
```

を記録する。

---

### Phase 5: CPU比較

同じJPEG群をCPUでデコードする。

```text
CPU 1 worker
CPU 8 workers
GPU batch 1 / 8 / 16 / 32 / 64
```

を同一レポートへ出力する。

---

### Phase 6: GPU resize

nvJPEG出力をGPU上で320px幅へ縮小する。

CPUへ戻さず、VRAM内で完了させる。

---

### Phase 7: nvJPEG encode

縮小画像をnvJPEGでJPEGエンコードする。

圧縮済みJPEG bytesのみCPUへ取得する。

---

### Phase 8: ディスク保存

生成JPEGを大量にディスクへ保存する。

GPU処理とdisk writeを分離して計測する。

---

### Phase 9: ライブラリ化判断

実験コードから、次の部分を切り出せるか判断する。

```text
JPEG bytes
→ batch decode
→ device image

device image
→ resize
→ JPEG encode
```

Viewer固有のキャッシュ制御やページ順序はライブラリへ含めない。

---

## 18. 成功条件

次のいずれかを満たせば、実験には価値がある。

### VRAM充填

```text
32枚以上のbatchでCPUより高い総スループットを得る
```

または、

```text
CPUと同等の完了時間で、CPU使用率を大きく削減できる
```

または、

```text
1 GiB相当のVRAM充填で明確な優位性を確認できる
```

---

### サムネイル生成

```text
大量生成時にCPU実装より高いimages/secを得る
```

または、

```text
同等速度でCPU使用率を大幅に削減できる
```

または、

```text
decode → resize → encodeをGPU中心で安定して連続処理できる
```

---

## 19. 撤退条件

次の場合は、Viewer統合を行わない。

```text
Warm状態でもCPU経路より大幅に遅い
32～64枚でもスループットが伸びない
CPU使用率がほとんど減らない
Progressive JPEG等で実用性が低い
VRAM消費または一時bufferが過大
nvJPEGのRust連携が不安定
CUDA Toolkit依存の配布負担に見合わない
```

ただし、撤退しても以下の知見は成果とする。

```text
初動コスト
適切なバッチサイズ
RTX 3060 Ti上のnvJPEG特性
CPUに残る処理量
VRAMバッファ設計
GPU resize / encodeの実測
Rust FFIの実装知見
```

---

## 20. Viewer統合を検討する場合

実験が成功した場合のみ、次の構成を検討する。

```text
interactive decode
  → 現行CPU経路

BG JPEG
  → nvJPEG batch

BG 非JPEG
  → 現行CPU経路

nvJPEG失敗
  → CPU fallback
```

Viewer側では、既存のDesired SequenceやGPU L1制御を維持し、実行バックエンドだけを切り替える。

ただし、CUDA出力をwgpu/D3D12 Textureへ直接接続する作業は、別フェーズとする。

---

## 21. 最終的な評価軸

本実験の評価は、

```text
単一画像が何ms速くなったか
```

だけでは行わない。

最重要評価軸は次である。

```text
多数の画像を一括処理した時の総スループット
1 GiBのVRAMを満たす速度
CPU資源をどれだけ解放できるか
初動コストと定常性能の差
GPU batch sizeの最適点
大量サムネイル生成への実用性
```

本実験の本質は、

```text
JPEGを1枚ずつ速く読む
```

ことではなく、

```text
多数のJPEGをGPU上で一括処理する画像パイプラインを構築し、
その性能特性を理解する
```

ことである。
