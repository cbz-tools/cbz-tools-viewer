# nvJPEG / CUDA GPU JPEG Decode 実験履歴

## 1. 現在の結論

nvJPEG / CUDA を利用した JPEG サムネイル生成は、技術的には成立したが、公開版には採用しない。

初期の batched API 実験では GPU 側が CPU 側を大きく下回った。その後、nvImageCodec、nvJPEG phased decode、C ABI DLL、Rust 側の補助レーン統合まで方式を変えて検証し、単体処理では CPU 比で十分に速い結果を得た。しかし、最終的なアプリ全体の測定では短縮率が約 5% に留まり、CUDA ランタイムと DLL の配布、CPU fallback、GPU リソース管理、保守対象の増加に見合う効果ではなかった。

この結論は GPU decode 一般を否定するものではない。また、初期の batched API の結果だけで nvJPEG 全体を評価するものでもない。今回の用途、実装、測定環境、配布条件を合わせた製品判断であり、CUDA 実装は実験的な検討として扱う。

---

## 2. 実験全体の時系列

検証は次の順に進めた。

1. nvJPEG batched API で full decode の基礎性能を測定した。
2. nvImageCodec と NPP を組み合わせ、縮小済みサムネイルだけを CPU へ戻す経路を検証した。
3. nvJPEG の phased decode を worker ごとに並列実行し、NPP resize まで含む単体 PoC を作成した。
4. phased decode 経路を C ABI DLL として分離し、Rust から利用できる契約を検証した。
5. Rust のサムネイル生成へ GPU 補助レーンとして統合し、fallback、完了処理、idle shutdown を含むアプリ全体を測定した。

この順序により、次の三つは分けて評価する必要があることが分かった。

```text
初期 batched full decode:
  今回の構成では遅かった

phased decode + GPU resize の単体経路:
  十分に高速化できた

アプリ全体:
  GPU 以外の処理が残るため、製品価値としての改善幅は小さかった
```

---

## 3. 初期 nvJPEG batched API 実験

### 3.1 条件

初期実験では固定の JPEG ベンチマークセットを使い、複数の batch size を比較した。GPU 側は次の段階を個別に測定した。

```text
device_only
copy_back_rgb
copy_back_rgba
```

default stream と explicit stream、pinned memory と async copy、host submit parallel も追加確認した。入力は保持数を限定して循環参照し、decode 結果全体ではなく checksum のみを集計した。RAM の異常増加は観測されなかった。

### 3.2 結果

```text
CPU best:
  約 140 images/s

nvJPEG device_only best:
  約 33 images/s

nvJPEG copy_back_rgb best:
  約 30 images/s

nvJPEG copy_back_rgba best:
  約 17 images/s
```

device_only は CPU best の約 4 分の 1 だった。初期化時間は小さく、支配的だったのは `nvjpegDecodeBatched` 呼び出しの壁時計時間である decode submission 側だった。batch size を段階的に増やしても改善せず、比較した範囲では小さい batch が最良だった。ここで制御した値は CUDA thread 数ではなく nvJPEG の batch size である。

pinned / async 系では成功値を得られず、explicit stream と host submit parallel も成功した範囲で CPU full decode を超えなかった。アーカイブ入力についても、この構成では GPU variant の安定性を確認できなかった。

当時使用した nvJPEG batched API 中心の実装経路には採用根拠がなく、その時点の実装方式と測定結果に対する不採用判断は妥当だった。この結果は、後に再評価した worker-driven phased decode 方式の性能を否定するものではない。

---

## 4. nvImageCodec C API 実験

次に NVIDIA nvImageCodec の C API を調査し、`HYBRID_CPU_GPU` backend、`I_RGB` format、`STRIDED_DEVICE` buffer を使って、JPEG decode 後の RGB を GPU 上で NPP の C3R resize に渡す方式を検証した。縮小済み RGB だけを CPU へ戻し、CPU 側で alpha 255 の RGBA8 へ変換した。Python API による事前確認も行ったが、製品経路の候補と API 設計の評価は C API を中心に進めた。

主な設計は次のとおりである。

```text
入力:
  HostMem 上の圧縮 JPEG

decode 出力:
  device memory 上の RGB

resize:
  NPP C3R linear resize

copy back:
  サムネイル寸法の RGB のみ

最終形式:
  caller-owned RGBA8
```

device buffer の slot 再利用は成立した。一方、decoder handle の再利用は比較した大半の条件で悪化したため採用しなかった。代表条件では約 300 images/s を記録した。

この段階で、opaque context、batch-only 呼び出し、caller-owned RGBA8 buffer、job ごとの result / status を持つ C ABI prototype も検討した。`OUTPUT_TOO_SMALL` の場合は必要寸法と必要 byte 数を返し、対象バッファへ書き込まず、batch の残りを継続する契約とした。

nvImageCodec の実装は GPU サムネイル補助レーンの候補になったが、最終的な Rust 統合には採用せず、C ABI と buffer ownership の設計資料として残した。

---

## 5. nvJPEG direct phased decode 再評価

初期 batched API とは別に、1 call を 1 sample とする phased decode 経路を作成した。framework 側の thread pool から worker へ分配し、各 worker が次を所有する方式である。

```text
CUDA stream / event
nvJPEG decoder / state / JPEG stream
pinned host buffer
device buffer
```

圧縮 JPEG bytes は HostMem から渡し、nvJPEG phased decode、NPP resize、サムネイル寸法の RGB copy back、CPU の RGB-to-RGBA8 変換までを行った。resource は worker 単位で再利用した。

固定の JPEG ベンチマークセットに対する平均値では、並列度を中程度まで上げると throughput が向上した。初期 PoC では progressive JPEG の一部が `UNSUPPORTED` となった。

```text
低い並列度:
  約 460 images/s

中程度の並列度:
  約 570 images/s
```

中程度の並列度が最良で、さらに worker 数を増やすと throughput は低下し、GPU memory 使用量との競合が増えた。高すぎる並列度では約 30 images/s まで悪化した。

この結果により、初期 batched API の低速さを nvJPEG 一般の性能とみなすべきではなく、phased decode と処理単位の並列化には DLL 化を検討する価値があると判断した。

---

## 6. nvJPEG C-DLL prototype

phased decode 経路を Rust から分離して利用するため、opaque context を持つ C ABI DLL を試作した。主要関数は次のとおりである。

```text
cbz_nvjpeg_create_context
cbz_nvjpeg_destroy_context
cbz_nvjpeg_decode_batch_to_rgba8
cbz_nvjpeg_status_name
cbz_nvjpeg_last_error
```

DLL は batch-only API とし、worker 数を context config で指定した。入力は HostMem 上の JPEG bytes、出力は caller-owned RGBA8 buffer とした。各 job は status、width、height、stride、required length を返す。`OUTPUT_TOO_SMALL` の job は書き込みを行わず、batch 内の他 job は継続する。resize は `FIT_WIDTH` の shrink-only とし、NPP linear resize を使い、alpha は 255 とした。

benchmark、timer、JSON report は DLL に持たせず、呼び出し側の責務とした。初期 prototype では progressive JPEG の一部が未対応だった。複数の並列度を比較した結果は次のとおりだった。

```text
低い並列度:     約 400 images/s
中程度の並列度: 約 490 images/s
高い並列度:     約 420 images/s
過剰な並列度:   約 30 images/s
```

後続実験では progressive JPEG 用の experimental hybrid backend を追加し、固定ベンチマークセットで未対応を解消した。ただし、製品経路では環境差、未対応 JPEG、JPEG 以外の画像を考慮する必要があるため、CPU fallback の必要性は変わらない。

---

## 7. Rust GPU thumbnail 統合

Rust 側から nvJPEG C-DLL を呼び出し、Library の初回サムネイル大量生成を補助する JPEG thumbnail batch decode 専用の GPU lane として統合した。Viewer の full decode や通常画像表示を置き換える設計にはしなかった。

役割分担は次のとおりである。

```text
Rust / CPU:
  ZIP / CBZ entry の展開
  入力の分類
  GPU job の投入
  RGB/RGBA 後処理
  WebP encode
  memory cache / disk cache の更新
  Ready 通知と UI 反映

nvJPEG DLL / GPU:
  JPEG batch decode
  NPP resize
  RGBA8 出力
```

JPEG 以外、GPU 未対応、GPU error は CPU 経路へ戻した。GPU finish worker は DLL 呼び出し後の WebP encode、cache 更新、完了通知を担当した。このため、GPU decode が高速でも、サムネイル生成全体が同じ比率で高速化するわけではない。

実験では worker 数、batch 上限、queue capacity、GPU memory budget、idle timeout を段階的に調整した。これらは実験専用の探索条件であり、公開版の設定値ではない。比較の結果、GPU decode を速くしても、CPU 側のアーカイブ展開、入力準備、WebP encode、cache 更新、UI 反映が全体 throughput を制約することが分かった。

---

## 8. Resource lifetime と idle shutdown

Rust 側の `GpuThumbnailServiceManager` は `Mutex<Option<Arc<GpuThumbnailService>>>` で service を保持した。job 自体は service を長期保持せず、submit 時だけ service を取得する。submit 中は local `Arc` が drop を防ぐ。pending job と active job がともに 0 になった時点で共通の短い idle deadline を設定し、idle が継続した場合だけ service を破棄する。次の job で DLL を動的 load し、service を lazy recreate する。

finish queue の pending 数は enqueue 前に予約し、queue が満杯または切断されていた場合は予約を必ず取り消す。finish worker が job を受け取った時点で pending を減らし、active を増やす。pending と active の両方から共通の idle 判定を行うことで、投入との競合による service の早期破棄を防いだ。定期的な context rotation は最終構成から撤廃した。

DLL context の破棄前には全 worker の CUDA event / stream を synchronize し、event、stream、nvJPEG decoder/state/JPEG stream、decode parameters、pinned host buffer、device buffer、nvJPEG handle を所有側で順序立てて破棄する。device buffer は `cudaFree` で解放する。

`cudaDeviceReset` は使用しなかった。これは process の CUDA primary context と、同一 process 内の他の CUDA 利用者へ影響し得るため、library の resource 解放として範囲が広すぎる。所有 resource を破棄しても、driver、runtime、内部 cache、primary context の都合により、OS の表示上で GPU memory が直ちに完全な 0 へ戻ることまでは保証しない。

---

## 9. 最終性能比較

最終的な比較は、数千冊規模の実コレクションに相当する大規模 Library workload で行った。GPU 補助レーンは CPU-only と比べて throughput が約 5% 高く、同程度の短縮率を確認した。

GPU 経路は動作し、GPU 利用率の上昇も確認した。この測定では GPU job の CPU fallback は発生しなかった。それでも CPU 負荷は依然として高く、全体差が小さいのは、アーカイブ展開、入力処理、queue、WebP encode、disk cache、Ready 通知、UI 反映などが CPU 側に残るためである。

単体 phased decode の約 570 images/s と、アプリ全体の約 5% 短縮は矛盾しない。前者は decode / resize 経路の throughput、後者は GPU 化していない処理を含む end-to-end の製品価値を測っている。

---

## 10. 製品投入を見送った理由

実験経路は技術的には成功したが、次の費用を正当化するほどアプリ全体の効果が大きくなかった。

```text
CUDA 対応環境と非対応環境の分岐
CUDA runtime と追加 DLL の配布・互換性管理
progressive JPEG を含む未対応 JPEG、JPEG 以外、GPU error に対する CPU fallback
GPU memory budget、queue、worker、idle shutdown の調整
GPU memory と L1 / L2 / SPAD など既存 cache 資源との競合
driver / runtime 差を含む障害解析
C ABI と Rust 側 loader の継続保守
既に十分高速な CPU 経路に対するユーザー体感効果の小ささ
```

そのため、公開版では次を行わない。

```text
nvJPEG を通常 JPEG decode 経路へ接続しない
Thumbnail / Viewer / Streaming 経路へ接続しない
cache 設計へ CUDA 固有状態を追加しない
CUDA 対応を製品機能として表明しない
配布物へ CUDA / nvJPEG DLL を追加しない
```

GPU 実験は技術的には成立したが、製品全体の価値と保守コストを比較し、公開版への投入は見送った。

---

## 11. 現在の公開版

公開版は CPU-only の画像 decode 構成を維持している。

```text
汎用 JPEG decode:
  zune-jpeg

Viewer / Thumbnail の縮小 JPEG decode:
  mozjpeg DCT scaling

GPU thumbnail service:
  なし

CUDA / nvJPEG DLL の build・package・配布:
  なし
```

本書に記載した C ABI DLL、GPU worker、idle shutdown の設計は実験履歴であり、現在の公開版に実装済みであることを示すものではない。

---

## 12. 将来の再評価条件

次のように前提が変わる場合は再評価の余地がある。

```text
nvJPEG が縮小 decode を正式に提供する
decode から resize、texture upload まで GPU 内で完結できる
GPU から RGBA を CPU へ戻す必要がなくなる
ZIP 展開や入力準備の CPU cost を削減できる
JPEG 以外も同じ GPU 経路で処理できる
progressive JPEG の fallback をさらに減らせる
アプリ全体で明確かつ再現可能な短縮率を得られる
CUDA 依存の配布と保守コストを受け入れる製品要件が生じる
GPU memory 使用量を抑えつつ phased decode の throughput を維持できる
異なる GPU と workload でも効果を再現できる
```

単体 benchmark の高速化だけでは採用条件を満たさない。再評価では end-to-end の処理時間、互換性、配布サイズ、resource lifetime、障害時の fallback を合わせて判断する。

---

## 13. まとめ

初期の nvJPEG batched full-decode 経路は CPU より遅かったが、これは nvJPEG 全般の限界ではなかった。nvImageCodec を経て、nvJPEG phased decode、NPP resize、C ABI DLL、Rust の GPU サムネイル補助レーンまで進めることで、単体経路は大幅に高速化できた。

一方、数千冊規模の実コレクションに相当する workload では、GPU 補助レーンによる短縮は約 5% に留まった。技術的成立と製品採用は別の判断であり、公開版では CPU-only 構成を維持する。今後は decode 回数の削減、cache 命中率、Page Map、Streaming 制御など、アプリ全体へ直接効く最適化を優先する。
