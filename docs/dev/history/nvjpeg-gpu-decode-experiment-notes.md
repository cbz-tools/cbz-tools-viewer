# nvJPEG / CUDA GPU JPEG Decode 実験記録

## 現在の結論

現時点では nvJPEG / CUDA GPU JPEG decode は採用しない。

この判断は、今回の測定条件・今回のJPEG workload・今回のnvJPEG経路に限った実験結果に基づくものである。
GPU decode 一般を否定するものではない。

結論だけを先に書くと、現行CPU経路のほうが今回の条件では明確に速かった。
したがって、nvJPEG を通常JPEG decode 経路へ接続する根拠はない。

---

## 1. 背景

JPEG decode は Viewer / Thumbnail / Streaming の主要なCPU負荷である。
そのため、CUDA / nvJPEG により decode をGPUへ移せるかを検証した。

ただし、現行のCPU経路はすでに用途別に最適化されている。
full decode は zune-jpeg、Viewer / Thumbnail は mozjpeg DCT縮小 decode を使う。
この実験は、nvJPEG がその現行CPU経路を置き換える根拠になるかを確認するために行った。

---

## 2. 測定条件

今回の比較は、次の条件で行った。

```text
bench:
  nvjpeg_large_parallel_cpu_vs_gpu_benchmark

image_count:
  512

CPU:
  available logical threads

GPU batch_size:
  64 / 128 / 256 / 512

stage:
  device_only
  copy_back_rgb
  copy_back_rgba

追加確認:
  default stream / explicit stream
  pinned memory / async copy
  host submit parallel

sample:
  local JPEG test dataset

input:
  JPEG folder
  JPEG ZIP archive

collected_jpeg_count:
  64

collected_bytes_total:
  195831597
```

入力JPEGは64枚のみ保持し、`image_count` 分は循環参照した。

decode結果、RGB、RGBA は全件保持しない。
checksum だけを集計した。

RAM の異常増加は観測されなかった。

---

## 3. 測定結果

最終測定結果は次のとおりである。

```text
CPU best:
  138.10 images/sec

nvJPEG device_only best:
  33.35 images/sec

nvJPEG copy_back_rgb best:
  29.85 images/sec

nvJPEG copy_back_rgba best:
  16.82 images/sec

best batch_size:
  device_only: 64
  copy_back_rgb: 64
  copy_back_rgba: 64
```

batched initialize を分離した後の代表値は次のとおりである。

```text
batched_initialize_ms:
  device_only best row: 1.497 ms
  copy_back_rgb best row: 0.393 ms
  copy_back_rgba best row: 0.405 ms

decode_submit_ms:
  device_only best row: 15235.206 ms
  copy_back_rgb best row: 15492.937 ms
  copy_back_rgba best row: 15585.833 ms

cuda_sync_ms:
  device_only best row: 119.133 ms
  copy_back_rgb best row: 167.865 ms
  copy_back_rgba best row: 195.372 ms

total_ms:
  device_only best row: 15775.444 ms
  copy_back_rgb best row: 17537.841 ms
  copy_back_rgba best row: 30443.877 ms
```

比較すると、nvJPEG device_only は CPU best の約0.25x だった。
成功した GPU variant の中では pageable 経由の device_only variant が最良だった。
copy_back_rgb は device_only より低下し、copy_back_rgba はさらに大きく低下した。
pinned / async 系は今回の条件では nvJPEG status error 等で成功値を得られなかった。
ZIP 入力では GPU variant の成功値を得られず、性能比較値は出ていない。ZIP 入力そのものが原因と断定せず、今回の条件では安定性を確認できなかった、という扱いに留める。

---

## 4. 観測結果

今回の観測で重要だった点は次である。

```text
nvjpegDecodeBatchedInitialize を batch_size ごと 1 回に修正しても大きな改善はなかった
batched_initialize_ms は小さく、支配要因ではなかった
支配的だったのは decode_submit_ms 側だった
batch_size を 128 / 256 / 512 に増やしても改善せず、best は 64 だった
この bench で制御しているのは GPU の CUDA thread 数ではなく nvJPEG の batch_size である
default stream だけでなく explicit stream も確認したが、採用ラインには届かなかった
pinned / async 系は試したが、今回の条件では nvJPEG status error 等により成功値を得られなかった
host submit parallel も確認したが、成功した範囲では CPU full decode を超えなかった
decode_submit_ms は nvjpegDecodeBatched 呼び出しの壁時計時間であり、純粋な GPU kernel 時間ではない
```

ここでの結論は、GPU 一般が遅いという意味ではない。
今回の経路では、nvJPEG の device_only 段階の時点で CPU に負けていた、という事実だけを記録する。

GPU→CPU copy back や RGBA 変換の前に、device_only の段階で既に差がついていた。

---

## 5. CPU 経路の整理

現行CPU経路は、用途別に分けて最適化されている。

```text
full decode / 汎用 decode:
  zune-jpeg

Viewer / Thumbnail:
  mozjpeg DCT縮小 decode
```

JPEG decoder が複数あるのはバグではない。
用途別最適化として扱っている。

Viewer の実用経路は full decode ではなく、必要サイズに合わせて decode 量を減らしている。
そのため、nvJPEG の full decode 系は今回よりさらに不利になる可能性がある。

---

## 6. 判断理由

判断は次の 1 点に集約できる。

```text
nvJPEG device_only が CPU maximum parallel より遅い
```

したがって、今回の用途では nvJPEG 自体が採用ラインに届いていない。
stream / pinned / async / host submit parallel を追加確認しても、成功した範囲では CPU full decode を超えなかった。

ここでの比較対象は、現行CPU経路の最大並列 decode である。
その条件で CPU が大きく速かったため、nvJPEG を通常JPEG decode の基盤に置き換える理由がない。

---

## 7. 採用しないもの

今回の判断により、次は採用しない。

```text
nvJPEG を通常JPEG decode 経路へ接続しない
Thumbnail 経路へ接続しない
Viewer interactive 経路へ接続しない
L2 RAM cache へ接続しない
L1 GPU texture cache へ接続しない
README へ CUDA 対応として記載しない
配布物に CUDA DLL を追加しない
```

これは、今回の測定結果に対する実装方針の記録である。

---

## 8. 将来の再評価条件

再評価するなら、少なくとも次のいずれかが必要である。

```text
JPEG decode専用支援があるGPUで再測定する場合
nvJPEG の API / 実装方式を大きく変える場合
GPU 上で decode → resize → texture 化まで戻さず完結できる実装を作る場合
現行CPU経路を超える実測値が出た場合
```

条件が変わらない限り、今回の判断を維持する。

---

## 9. まとめ

この実験により、現行のCPU JPEG decode設計は十分に強く、nvJPEGを単純接続して高速化する方針は採用しない。
今後の最適化は GPU decode ではなく、decode回数削減、cache命中率、Page Map、Streaming制御を優先する。
