# L1 / L2 Streaming Cache

## 実装対応

| 層 | メモリ | 概念 | 主な実装 |
|---|---|---|---|
| L2 | RAM | Policy | `SimpleStreamingCachePolicy` |
| L2 | RAM | Planner | `StreamingCachePlanner` |
| L2 | RAM | Manager | `ViewerWorkerManager` |
| L2 | RAM | Cache | `RgbaPageCache` |
| L1 | VRAM | Warmup Planner | `plan_gpu_warmup()` |
| L1 | VRAM | Future | `GpuWarmupCache` |
| L1 | VRAM | History | `GpuTextureHistory` |

以降は、本文を簡潔にするため次のショートネームを使用する。

```text
SimpleStreamingCachePolicy → Policy
StreamingCachePlanner      → Planner
ViewerWorkerManager        → Manager
RgbaPageCache              → Cache
plan_gpu_warmup()           → Warmup Planner
GpuWarmupCache             → Future
GpuTextureHistory          → History
```

---

## 概要

現在のキャッシュ構成は、

```text
L2
    RAM上のRGBA Cache
        ↓
L1
    VRAM上のGPU Texture Cache
```

の2層で構成される。

L2は、

```text
Policy
Planner
Manager
Cache
```

によって、表示候補ページのRGBAをRAM上へ準備する。

L1は、

```text
Future
History
```

によって、表示前後のGPU TextureをVRAM上に保持する。

---

## 用語

```text
L1
    VRAM上のGPU Texture Cache

L2
    RAM上のRGBA Cache
```

L1は描画に直接利用できる最終キャッシュである。

L2はL1へTextureを供給する中間キャッシュである。

CPUデコード結果が直接表示に使われるinteractive経路は存在するが、
本書ではストリーミングキャッシュのL1 / L2構成を対象とする。

---

# L2：RAM RGBA Cache

## Policy

Policyは、

```text
次に準備すべきページ順
```

を決定する。

出力はDesired Sequenceである。

Page MapありAUTOでは、表示 unit を起点にした順序を同じ出力形式で返す。
Single / Spread / Page Mapなしでは、従来の physical page Policy を使う。

入力は、

```text
current_page
total_pages
visible_pages
```

だけである。

Policyは、

```text
cache
inflight
worker
byte budget
```

の状態を持たない。

現在のDesired Sequenceは、

```text
visible
current
forward burst
backward 1ページ
```

を繰り返す。

前方を厚くしつつ、後方も完全には捨てない。

---

## Planner

Plannerは、

```text
Desired Sequence
Cache状態
inflight状態
too-large状態
visible / protected状態
worker capacity
byte budget
```

を入力として受け取る。

役割は、

```text
何を作るか
何を退避候補とするか
完了したBG結果を現在のCacheへ採用するか
```

を決定すること。

出力は、

```text
dispatch_pages
evict_candidates
stop_reason
completion_admission_plan
```

である。

Planner自身は、

```text
decode
worker dispatch
cache insert
cache eviction
```

を実行しない。

---

## Plannerの基本動作

### Cacheに空きがある場合

Desired Sequence上位から、

```text
cacheに存在しない
inflightではない
too-largeではない
visible / protectedではない
```

ページをdispatch候補にする。

---

### Cacheが満杯の場合

現在保持している退避可能ページの最低優先順位と比較する。

```text
現在のworst rankより高優先のmissing page
```

だけをdispatch候補にする。

順位改善にならない場合は、新規decodeを止める。

---

### BG完了時

BG開始後に Desired Sequence が変化している可能性がある。

完了 page を現在の順位で再評価する。

completed page より低優先の退避候補だけで
実容量を確保できる場合に admit する。

採用時の退避候補も Planner が決める。

順位改善にならない結果は Cache へ入れず破棄する。

drop は Too-Large や decode failure ではない。

---

## Manager

Managerは、

```text
Policy
Planner
BG Worker
Cache
```

を接続する。

役割は、

```text
snapshot受領
cache上限反映
plan作成
completion admission plan取得
worker dispatch
result回収
stale判定
cache反映
admit/dropの実行
admit/dropのどちらでも再補充へ進む
再補充
```

である。

ManagerはPlannerの結果を実行する。

Manager内で別の優先順位を再計算しない。

---

## Cache

Cacheは、

```text
表示候補ページのRGBA
```

をRAM上に保持する。

役割は、

```text
保持
検索
挿入
退避
容量管理
```

である。

Cache自身はDesired Sequenceを知らない。

優先順位やdispatch方針を決定しない。
completion admission の採否もしない。

---

## Protected Pages

現在の実装では、

```text
visible pages
```

をprotectedとして扱う。

protected pageは、

```text
Plannerによる退避
cache上限変更時の退避
completion admission での退避候補
```

から保護される。

---

## Too-Large Pages

RGBAがL2へ収まらないページ、またはinsert後に保持されなかったページは、

```text
TooLargeForBgRgba
InsertDidNotSurvive
```

として記録される。

これらは同一条件で繰り返しdispatchしない。

interactive表示経路は別であり、表示不能を意味しない。

completion admission の drop は別扱いであり、TooLargeForBgRgba には記録しない。

---

# L1：VRAM GPU Texture Cache

## 概要

L1は、表示用GPU Textureを保持する層である。

現在は責務を二つに分ける。

```text
Future
    未来ページ

History
    表示済みページ
```

両者は同じGPU Textureを保持するが、用途は異なる。

---

## Warmup Planner

Warmup Plannerは、

```text
L2のready entry
現在のFuture
History
未来ページの表示要件
L1容量
```

未来ページの表示要件は、既存L1 Textureの保持・適合判定に使う。
L2 Ready entry は、新規upload候補の生成にだけ使う。

を入力として受け取る。

役割は、

```text
何をL1へuploadするか
何をstaleとして退避するか
何を順位改善で置換するか
```

を決定すること。

出力は、

```text
stale_evict_candidates
replacement_evict_candidates
upload_candidate
```

である。

一度のplanで選ぶuploadは最大1件である。

実際のTexture生成はPlannerでは行わない。

---

## Future

Futureは、

```text
現在表示より先のページ
```

を保持する未来専用GPU L1である。

L2は、新規TextureをL1へuploadする際のRGBA供給元である。
一度L1へuploadされたTextureは、対応するRGBAがL2からevictされても、
それだけを理由に直ちに削除しない。
L1 Textureの寿命は、Desired Future / History、現在の表示要件、
L1容量、およびL1内の優先順位によって決定する。

基本動作は、

```text
空きがある間は近い未来ページを追加
満杯時は遠い保持ページより近い候補だけを置換
```

である。

毎frame、理想集合への完全同期は行わない。

現在表示より過去になったentryや、表示条件に合わなくなったentryはstaleとして退避する。

---

## History

Historyは、

```text
表示に使用されたGPU Texture
```

を保持する履歴側GPU L1である。

HistoryもL2 Readyの有無で寿命を決めない。
History容量、LRU、表示要件で管理する。

役割は、

```text
戻る操作での再利用
同一ページの再表示
見開き片側の部分再利用
表示条件を満たすTextureの検索
```

である。

LRUで容量を管理する。

---

## WarmupからHistoryへの昇格

FutureのTextureが実際の表示に採用された場合、

```text
Future
    ↓ promote
History
```

へ移動する。

同じTextureを再uploadしない。

これにより、

```text
未来として準備
→ 表示に使用
→ 履歴として保持
```

というライフサイクルになる。

---

## Textureの適合判定

L1はページ番号だけでは判定しない。

```text
page
render signature
required display size
quality
max texture size
```

を考慮し、

```text
Exact
Suitable
Miss
```

を判定する。

小さすぎるTextureは再利用しない。

---

# 全体データフロー

```text
current / visible state
        ↓
Policy
        ↓
Desired Sequence
        ↓
Planner
        ↓
dispatch_pages / evict_candidates / completion_admission_plan
        ↓
Manager
        ↓
BG Worker
        ↓ completion
Planner completion admission
        ↓
Manager
        ↓
L2 Cache / drop
        ↓
Warmup Planner
        ↓
Future
        ↓ display commit
History
        ↓
画面表示 / 再利用
```

---

# 責務境界

## Policyは優先順位を決める

```text
何が重要か
```

を決める。

---

## Plannerは計画を決める

```text
何を作るか
何を退避候補とするか
完了したBG結果を採用するか
```

を決める。

---

## Managerは実行する

```text
いつworkerへ流すか
結果をいつcacheへ反映するか
completion admission plan をどう実行するか
```

を担当する。

---

## CacheはRAM上で保持する

```text
何を持っているか
```

を管理する。

---

## Warmup PlannerはL1準備を計画する

```text
どの未来ページをL1へ上げるか
```

を決める。

---

## Futureは未来を保持する

```text
これから表示するTexture
```

を管理する。

---

## Historyは過去を保持する

```text
すでに表示したTexture
```

を管理する。

---

# 現在の設計原則

```text
Policyはcacheを知らない
Plannerはdecodeしない
Managerは優先順位を再定義しない
Managerは完了結果の優先順位を独自判断しない
Cacheは方針を決めない
Cacheは完了結果の採用方針を持たない
L1 Warmupは未来だけを扱う
Historyは表示済みだけを扱う
```

L2側とL1側は、

```text
L2
```

を境界として接続する。

GPU L1はL2を供給元とするが、L2側のPolicyやPlannerへL1固有責務を持ち込まない。

---

# 結論

現在のキャッシュ構成は、

```text
L2
Policy
↓
Planner
↓
Manager
↓
Cache

L1
Warmup Planner
↓
Future
↓
History
```

の責務分離によって構成される。

L2は、

```text
表示候補ページのRGBAをRAM上へ準備する
```

ことを担当する。

L1は、

```text
未来TextureをVRAM上へ準備し
表示後は履歴として再利用する
```

ことを担当する。

L1とL2は上下関係ではあるが、同じ方針を重複して持たない。

```text
L2
    どのRGBAを準備するか

L1
    どのTextureを保持するか
```

をそれぞれ独立して決定する。

アルゴリズム変更は、このL1 / L2境界と各責務境界を越えないことを原則とする。
