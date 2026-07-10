# SPAD: Adjacent Book Scratchpad

## 実装対応

| 構成要素 | メモリ / 実行場所 | 概念 | 主な実装 |
|---|---|---|---|
| SPAD Target State | RAM / UI thread | Next / Prev target | `SpadTargetState` |
| Target Layout Hint | RAM / UI thread | 既存Page Map cacheによるtarget layout | `SpadTargetLayoutHint`, `SpadResolvedDecodeTarget` |
| Next Worker | worker thread | 次本専用RGBA decode | `spad-next`, `spad_next_shared` |
| Prev Worker | worker thread | 前本専用RGBA decode | `spad-prev`, `spad_prev_shared` |
| SPAD Queue | worker queue | side別single-slot request queue | `send_spad_next_request()`, `send_spad_prev_request()` |
| SPAD Result Receiver | UI thread | side別result drain | `try_recv_spad_next()`, `try_recv_spad_prev()` |
| Ready Scratchpad | RAM | promotion候補のRGBA | `ready_pages`, `SpadReadyPage` |
| Promotion | UI thread | 本移動後のL1 Future化 | `take_spad_ready_pages_for_target()`, `promote_spad_ready_pages_to_l1_future()` |
| L1 Future | VRAM | promotion先 | `GpuWarmupCache` |

以降は、本文を簡潔にするため次のショートネームを使用する。

```text
SpadTargetState          -> Target
SpadTargetLayoutHint     -> Target layout hint
SpadResolvedDecodeTarget -> Resolved decode target
GpuWarmupCache           -> L1 Future
```

---

## 概要

SPAD is an adjacent-book scratchpad that prepares a small number of RGBA pages from the next and previous books.

SPADは、次本・前本の近いページを少量だけRGBAとして保持する隣接本用Scratchpadである。

SPADは現在本のcacheではない。現在本に対してはL2がStreaming RGBA Cacheとして動作する。

```text
L2
    現在本のStreaming RGBA Cache

L1 Future
    現在本の未来Texture

L1 History
    表示済みTexture

SPAD
    次本 / 前本のRGBA scratchpad
    本移動後にだけL1 Futureへpromotionする準備領域
```

目的は、本移動直後の待ち時間を軽減することにある。SPADのRGBAは本移動前にはTexture化しない。

---

## 用語

```text
SPAD / Scratchpad
    隣接本用の小さなRGBA保持領域。

Target
    次本または前本のdecode対象。Next targetとPrev targetは別々の本である。

Ready page
    static RGBA decodeに成功し、promotion候補として保持されているページ。

Failed page
    decode失敗、range外、budget不採用などにより再dispatchを抑止するページ。

Exhausted
    targetが明示的にdispatch停止状態になったこと。
    すべてのdispatch停止理由を表すわけではない。

Inflight
    side別workerへ送信済みで、完了を待つrequest。

Promotion
    本移動後にReady RGBAをL1 FutureへTexture insertする処理。

Target layout hint
    既存Page Map cacheから得たtarget本の表示単位情報。

RenderSignature
    RGBAを生成したdecode条件の署名。

DisplayRequirement
    現在表示に必要な最低decode条件。
```

---

## Target管理

TargetはNextとPrevで独立している。

```text
Next target
    次本

Prev target
    前本
```

各Targetはtarget path、entry page、page_count、ready pages、failed pages、current bytes、byte budget、exhaustedを保持する。SPAD全体はsessionとgenerationを持ち、inflightはNext/Prevで別に保持する。予算もTarget別であり、片側が明示的にexhaustedでも反対側は継続できる。

---

## Worker構成

SPADは2本の専用workerを持つ。

```text
spad-next
    Next本専用worker

spad-prev
    Prev本専用worker
```

Next/Prevは別々の本なので、別queue・別result receiver・別inflightで並列decodeする。

```text
spad_next_shared -> spad-next -> spad_next_result_rx
spad_prev_shared -> spad-prev -> spad_prev_result_rx
```

shared SPAD queueは使わない。Next dispatchを先に試して優先性を維持するが、Prev workerがidleなら同一frameでPrevもdispatchできる。L2 BG workerはSPAD jobに使わない。

---

## 開始条件とdispatch順

SPAD dispatchは、次のglobal gateを通過した後にside別で判定する。

```text
L2 settled済み
L2 statusのBookIdが現在Viewerと一致
L2 statusのgenerationが現在Viewerと一致
```

各sideはside別inflightがなく、targetが存在し、targetが未exhaustedで、target budgetとdispatch可能な候補pageが残る場合にdispatch可能である。

処理順は次のとおりである。

```text
1. result drain
    next result
    prev result

2. dispatch
    nextを先に試す
    prevもidleなら同一frameで試す
```

Next優先は、Next専用workerを確保し、Next dispatchを先に試すことで維持する。PrevはNextのworker slotを奪わない。

---

## Decode sizeとtarget layout

SPADはPage Mapを新規作成しない。既存Page Map cacheがある場合だけ参照する。

```text
Page Map cache hit
    target本のAutoSpreadPlanでentry pageの表示単位を解決する
    単ページならfull幅decode
    見開きならhalf幅decode

Page Map cache miss
    現在本layout基準のdecodeへfallbackする
```

cache hit時のtarget decode sizeは、dispatchとtarget別budget算定で同じResolved decode targetから使用する。AUTOでresume pageが見開き右ページを指す場合は、表示単位のfirst pageをschedule anchorに補正する。

SPADは次を行わない。

```text
Page Mapの新規作成
ZIP / Folder / EPUB FAST生成
RAR slow complete
Viewer bootstrapのSPADからの直接利用
```

layout hintが得られない場合、またはpromotion時にsignatureが一致しない場合は、通常decodeが表示保証のfallbackになる。

---

## Ready / Failed / Budget / Exhausted

`SpadTargetState`はtargetごとに次を管理する。

```text
ready
    promotion候補のstatic RGBA。

failed
    decode失敗、range外、budget不採用などの再dispatch抑止。

budget
    targetごとのRGBA byte上限。target layoutに対応する2ページ保証を含む。

exhausted
    targetが明示的にdispatch停止状態になったこと。すべての停止理由を表さない。
```

decode失敗やrange外は当該pageを`failed`へ記録する。候補枯渇はdispatch可能候補なしとして扱う。SPADのdispatch停止は、exhausted、候補pageなし、budget不足、ready / failed済みなどの組み合わせで決まる。

NextとPrevは別Targetなので、片側が明示的にexhaustedでも反対側は継続できる。

---

## Stale result drop

side別result receiverから得た結果は、対応するsideのinflightと照合する。expected side、inflight side、request_id、session、generation、target path、page_count / page range、ready / failed duplicate、static frame、target budgetを検証する。

target変更や本移動後にすでに開始済みのdecodeを物理cancelすることは必須ではない。旧結果は到着時にstaleとしてdropする。

---

## Promotion

SPAD promotionは本移動後だけ行う。

```text
本移動
    ↓
target pathが一致するReady RGBAを取り出す
    ↓
新ViewerState作成
    ↓
新しい表示条件でRenderSignature / DisplayRequirementを検証
    ↓
適合するものだけL1 Futureへinsert
    ↓
不適合はdropし、通常decodeへfallback
```

SPAD targetが持つpage_count hintは、新ViewerStateの`persistent.page_count`が未確定の場合に採用できる。採用時はspread snapshotをinvalidateする。

SPAD結果はL2 Cacheへ入れない。本移動前にL1へTexture insertしない。Texture管理はpromotion後のL1 Futureの責務である。

---

## L1 / L2との責務境界

```text
L2
    現在本のStreaming RGBA Cache
    Policy / Planner / Manager / Cache

SPAD
    隣接本のRGBA scratchpad
    target / ready / failed / budget / exhausted / promotion
```

SPADはL2 Policy / Planner / Manager / Cacheへ混ぜない。

```text
SPAD結果をL2へseed insertしない
L2 BG workerをSPAD jobに使わない
SPAD worker側でTexture生成しない
本移動前にL1へinsertしない
```

---

## 全体データフロー

```text
AdjacentBooks snapshot
        ↓
SPAD target configure
        ↓
existing Page Map cache read only (optional)
        ↓
L2 settled gate
        ↓
SPAD dispatch
    ├─ spad-next worker
    └─ spad-prev worker
        ↓
side-specific result drain
        ↓
ready scratchpad / failed / dispatch stop state
        ↓
book navigation
        ↓
take ready for target
        ↓
RenderSignature / DisplayRequirement check
        ↓
L1 Future promotion or drop
        ↓
display commit / fallback decode
```

---

## 設計原則

- SPADは隣接本だけを扱う。
- SPADは現在本L2に関与しない。
- SPADはRGBA scratchpadでありTexture cacheではない。
- SPADは本移動後にのみL1 Futureへpromotionする。
- Next/Prevは別Targetとして並列decodeする。
- Page Mapは既存cacheを参照するだけで、新規作成しない。
- stale resultはdropし、物理cancelは必須にしない。
- fallback decodeを表示保証の最終経路とする。
