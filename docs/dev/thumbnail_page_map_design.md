# サムネイル生成と Page Map 生成の構成概要

## 目的

CBZ Viewer では、Library 表示のためにサムネイルを生成し、Viewer の読書体験を安定させるために Page Map を生成する。

サムネイルと Page Map は、どちらも本の内容を調べる処理から得られるが、責務は異なる。

* サムネイルは Library のための代表画像
* Page Map は Viewer のためのページ構造情報

Page Map は、読書の必須条件ではない。
本を読むために必ず存在しなければならないデータではなく、進捗表示、AUTO見開き、Streaming/cache計画を安定させるための補助データである。

---

## 全体構成

サムネイル生成と Page Map 生成は、次の3つの経路に分かれる。

```text
Library
  |
  +-- 通常サムネイル生成
  |     |
  |     +-- サムネイル生成
  |     |
  |     +-- FAST Page Map生成
  |             |
  |             +-- 成功: Page Map保存
  |             |
  |             +-- 未対応/失敗: SLOW Page Mapへ委譲
  |
  +-- Library 3秒周期差分scan
  |     |
  |     +-- source変更検知・サムネイル復旧
  |
  +-- SLOW Page Map生成
        |
        +-- FASTで作れなかったPage Mapを低速枠で生成
```

Library の差分scanによるサムネイル復旧と SLOW Page Map は別の責務である。
Page Map はサムネイル成功を Page Map の失敗で取り消さず、Page Map の失敗で
Library UX を壊さない。FAST / SLOW / RAR / CBR / PageMapCoordinator の設計は維持する。

---

## Page Map cache unavailable時

Page Map disk cacheはthumbnail workerの必須起動条件ではない。
primaryとfallbackの両方でPage Map cacheをopenできない場合はwarningを1回記録し、
worker lifetime中をthumbnail-only modeとしてrequest loopを継続する。

```text
thumbnail cache open成功
Page Map cache open失敗
  |
  +-- thumbnail生成とLibrary表示を継続
  +-- Library background FAST/SLOW Page Map生成をskip
```

このmodeではPage Map cache lookup、FAST、SLOW予約、Page Map deferred persistenceを開始しない。
runtime中のPage Map cache自動復旧は保証せず、application再起動後にcache openを再試行する。

ここでいう thumbnail-only mode は Page Map cache unavailable 時の動作モードであり、
サムネイル復旧の専用経路を意味しない。

Viewer bootstrapはthumbnail workerとは独立した既存fallbackを維持し、
Page Mapを利用できないsessionでも通常読書を継続する。

---

## 通常サムネイル生成

通常サムネイル生成は、Library が本を表示するための通常経路である。

この経路では、サムネイル生成と同じ機会に、可能であれば FAST Page Map 生成も試みる。

```text
通常サムネイル生成
  |
  +-- サムネイルを作る
  |
  +-- FAST Page Mapを作る
```

この経路の目的は、Library の初回表示や通常表示を成立させることである。

サムネイル生成に成功すれば、Library はその本を表示できる。

FAST Page Map 生成に成功すれば、Viewer はその本を開いたときに Page Map を利用できる。

ただし、FAST Page Map 生成に失敗しても、サムネイル生成の成功を取り消してはいけない。

```text
サムネイル成功 + FAST Page Map成功
  => サムネイル表示
  => Page Map保存

サムネイル成功 + FAST Page Map未対応/失敗
  => サムネイル表示
  => Page MapはSLOWへ委譲可能

サムネイル失敗
  => Library上では本を開ける対象にしない、または失敗扱い
```

つまり、通常サムネイル生成では、サムネイルが主、FAST Page Map は付随処理である。

通常サムネイルの生成・保存幅は `THUMB_STORAGE_WIDTH = 500` とする。Image / Video の通常
Thumbnail は `AppSettings::storage_width()` を経由して500px幅で生成し、Memory / Disk Cache の
通常Thumbnail成果物として扱う。表示サイズとは分離し、表示幅を変更しても通常Thumbnailの
保存幅は変えない。

Library のサムネイル表示幅は `AppSettings.thumb_display_w` を正本とし、次の範囲へclampする。

```text
THUMB_DISPLAY_MIN     = 120
THUMB_DISPLAY_DEFAULT = 200
THUMB_DISPLAY_MAX     = 660
THUMB_DISPLAY_STEP    = 20
```

有効な表示幅は `120, 140, 160, ... 620, 640, 660` である。高さは従来のサムネイル比率
`180:260` を維持し、表示幅から算出する。Library Settings のカードサイズと Library grid 上の
Ctrl+Wheel は同じ `thumb_display_w` を更新する。Ctrl+Wheel 1ノッチは `±20px` とし、変更後は
即時反映してsettingsへ保存する。通常Wheelは縦スクロールのまま維持し、Ctrl+Wheel時だけ
サムネイルサイズ変更へ振り向ける。

通常Thumbnailの表示は500px保存成果物を使用する。したがって表示幅 `<= 500px` では500px
Thumbnailを縮小表示し、表示幅 `> 500px` では現行仕様として500px Thumbnailを拡大表示する。
500pxを超える表示幅に対して元sourceから高解像度Thumbnailを直接decodeするruntime-only経路は
現行仕様には存在しない。Previewは後述のとおり通常Thumbnailとは別経路で表示幅へ追従する。

Library grid の横方向レイアウトは、表示中の `thumb_w / thumb_h` を変更せず、余った横幅だけを
余白へ配分する。列数は `theme::GRID_GAP` を最低列間gapとして確保できる最大値を使う。

```text
cols * cell_width + (cols - 1) * GRID_GAP <= available_width
```

最低1列を保証する。基準幅を差し引いた余剰 `extra` は、左右端2箇所と `cols - 1` 個の列間、
合計 `cols + 1` 箇所へ同量ずつ配る。

```text
base_width  = cols * cell_width + (cols - 1) * GRID_GAP
extra       = max(0, available_width - base_width)
side_margin = extra / (cols + 1)
actual_gap  = GRID_GAP + side_margin
```

このため左右端余白は同じになり、`actual_gap >= GRID_GAP` を常に満たす。最終行のitem数が
`cols` 未満でも、その行だけ再均等配置せず、通常の `col 0, col 1, ...` の列位置を維持する。
縦方向のVirtual Grid、`visible_start / visible_end / visible_range`、Texture要求、Preview、selection、
hit-test のindex契約は変更しない。描画・hover・click・double click・selection・context menu・
Preview・drag/drop は従来どおり同じcell Rectを使用する。`scroll_selected_into_view` 側の列数も
同じ共通列数算出を使用し、Ctrl+Wheel後の次frameで列数と横余白を再計算する。

VideoFile の通常サムネイルは代表フレーム 5% の1枚を維持する。Library の Hover Preview は
runtime-only の表示補助であり、Thumbnail、Page Map、artifact、failure-cache のいずれの
成果物・製品機能にも含めない。

Library の VideoFile thumbnail では、既存 filename HUD card 相当の領域を X 軸 hover-only
Scrub、それ以外の thumbnail を既存の Hover Auto Preview とする。Scrub は既存 Auto と同じ
10%～90%、10%刻みの9 sceneを使い、thumbnail内の local X を scene index に変換する。
filename HUD card 相当の仮想領域は HUD ON/OFF で同じ位置・高さとし、HUD OFF でも hit area として
有効にする。初回 hover のみ300ms delayを適用し、active後のAuto/Scrub切替は同じ
VideoPreview session 内で即時に行う。

Auto と Scrub は同じ runtime-only Preview worker、保持中の VideoDecoder、replaceable
mailbox、single-in-flight、stale/revision、repaint 経路を共有する。Scrub 中は最新の X
scene だけを保持し、decode 中に queue/backlog を作らない。Scrub の失敗は通常 Video
thumbnail の5%生成、Thumbnail Worker/cache/failure/artifact、Page Map、Global Goalへ
流さない。

Library Preview worker は VideoFile 専用ではなく、Video、Static Book、Animated WebP の
runtime-only Preview を共通に扱う。Static Book は Page Map が利用可能な場合に filename
HUD で全ページを Scrub し、multi-page Book では page axis を優先する。Animated WebP は
thumbnail body の Hover Auto Preview と filename HUD の time Scrub を使い、1ページのBook
では時間軸、multi-page Book ではページ軸を使用する。

これらの Preview は single Preview worker と最新要求を優先する経路で処理する runtime-only
の表示補助であり、Preview 結果を Thumbnail、Disk Cache、Page Map の成果物へ保存・反映しない。

Preview task の `target_width` は通常Thumbnailの固定保存幅500pxではなく、現在の
`AppSettings::clamped_display_w()` を使用する。Library `show()` へ `preview_target_width` として渡し、
Video Auto / Video Scrub / Animated WebP Auto / Animated WebP time Scrub / Static Book page Scrub の
各runtime Previewで共通して現在表示幅へ追従する。

```text
表示120px -> Preview decode 120px
表示200px -> Preview decode 200px
表示500px -> Preview decode 500px
表示660px -> Preview decode 660px
```

サムネイル表示サイズを変更した場合、以後のPreview要求は新しい表示幅をtargetにする。Previewは
引き続きruntime-onlyであり、500pxの通常Thumbnail Disk Cacheを上書きせず、Preview結果の
Disk Cache保存、cache version変更、Thumbnail artifactへの昇格は行わない。

---

## Thumbnail Workerの並列制御

Thumbnail Worker の Image lane と Video lane は、同じ `Global Thumbnail Semaphore` を
共有する。worker 生成時の `base_goal` は `std::thread::available_parallelism()` を基準にし、
取得できない場合は `2` を使い、最終値を `2..=8`（Global Thumbnail Permits は `2～8`）に
clamp する。この値は厳密な thread 数ではなく、同時に許可できる負荷の permit 予算である。
root 選択前の初期 `Global Goal` は `base_goal` とし、worker は再生成せず、選択された
Library root の storage medium に追従して更新する。

```text
available_parallelism()
    fallback = 2
    clamp = 2～8
    base_goal = clamp 後の値

Library root の medium       Global Goal
SSD / Unknown                base_goal
HDD                         max(1, base_goal / 2)
```

各処理の負荷は次のように数える。

```text
Image            => 1 slot
Video            => 1 slot
```

例えば `base_goal = 8` の状態で SSD root から HDD root へ移動すると、選択 path の
medium が HDD と判定された時点で `Global Goal = 4` になる。Unknown または SSD root
へ戻ると `Global Goal = 8` に戻る。root 変更の実経路は
`LibraryState::start_load_dir_async` であり、既存の `clear_pending_tasks()` に続けて
worker state の Goal を更新する。Goal 値が変わったときだけ、path、medium、base_goal、
global_goal を含む1行のログを出す。

物理 `Semaphore` の容量は `base_goal` のまま保持する。lane state は `global_goal` と
Image / Video の running 合計を共有 Mutex 内で管理し、Semaphore の permit を取得した
後にも Goal を再確認してから running として確定する。Goal 縮小時に実行中の処理を
preempt / cancel せず、running 合計が新しい Goal 以下になるまで新規開始を抑制する。
処理完了時の既存 wake で自然に再評価し、Goal 拡張時は Goal 更新の wake で待機 lane を
速やかに再評価する。

Video は1件ずつに固定されず、permit が空いている限り複数件を in-flight にできる。
Image / Video それぞれの同時実行数は `0..=Global`、つまり Image min = 0 / max = Global、
Video min = 0 / max = Global であり、両 lane の使用 permit 合計は常に `Global` 以下である。
例えば Global = 8 で Video だけにbacklogがあれば、Image 0 / Video 8 まで成立する。
これは固定8並列ではなく、Global permit の空き状況に従う。Image と Video の間に
7:1、4:4、50:50 などの固定比率や固定 slot を設けない。
旧 `VIDEO_LANE_SLOTS`、Video 専用 Semaphore、Video を1並列に固定する制御は廃止済みで、
現行設計には存在しない。

Image と Video は独立した queue を持つが、permit の取得は共有仲裁を通る。専用の
Scheduler thread は作らず、それぞれの lane worker が共有状態を見て Global permit を
取りに行く。仲裁は、完了して permit を返した lane を優先し、laneごとの
`image_completion_credit` / `video_completion_credit` でその進行を記録する。credit は
その lane の次の開始で1つ消費され、同じ lane に `pending_requests` が残る間は、完了の
たびに失われずに蓄積される。例えば Image が2件完了して Image backlog が残っていれば、
`image_completion_credit += 2` 相当となり、その後 Image を2件起動するまでに1件ずつ
消費される。解放された枠は、同じ lane にbacklogが残る限り原則として同じ laneへ返す。

仲裁の優先順位は次のとおりである。

```text
1. pending/waiting があり running == 0 の後着 lane
   → start_pending で次の実行機会を1回保証
2. completion credit がある lane
   → 解放された枠を同じ laneへ返す
3. 同 lane にbacklogがない
   → 反対 laneへ枠を移譲
4. 両 lane ともbacklogがない
   → idle
```

`start_pending` は後着 lane の開始を1回保証するための一時的な印であり、固定 slot の
予約ではない。ある lane に pending/waiting があり、その lane の running == 0 で、反対
lane が Global permit を使用中のときに設定される。開始できた時点で消費され、待機や
実行状態が変われば再評価される。
`waiting` は Global permit 待ち状態であり、lane 全体のbacklog数ではない。実際のbacklog
判定には lane 別の `pending_requests` を使う。`pending_requests` は、public lane API が
requestをenqueueしてから lane worker が retire するまでの要求数を表す。enqueue時に
加算し、queue への送信に失敗した場合は rollback する。worker が要求を引き取った後は
RAII guard が early-continue を含む各経路で retire を担当するため、一時的に
`waiting == 0` になっただけで backlog の終了を判定してはいけない。completion credit の
加算は同 lane の `pending_requests > 0` を基準にする。lane が本当に idle になった
`pending_requests == 0 && waiting == 0 && running == 0` の境界では、その lane の
不要な completion credit を破棄する。Clear / Remove / Shutdown および lane reset では、
`start_pending`、completion credit などの transient state も破棄する。

Video は Global permit を取得してから JoinSet に投入する。permit 待ちの Video task を
先に大量生成して waiter を増やすことはしない。すでに走っている Video は独立した複数の
in-flight として完了を返し、完了 lane 優先と shared arbitration を通じて次の開始を促す。
現行の Video thumbnail は代表フレーム1枚であり、同一動画から5%、10%、15%など複数の
フレームを生成する機能は未実装である。

重複抑止の flight registry には、要求の識別子だけでなく unique flight id も含める。
Clear / Remove 後に同じ source の新しい flight が登録された場合でも、古い flight の
遅れて到着した完了が新しい登録を消さないためである。これは結果を UI に反映しない
`Stale` / `VideoStale` や source revision の検証とは目的が異なる。前者は in-flight
registry の所有権を守り、後者は古い source の結果を採用しないための仕組みである。

この並列制御はサムネイルの実行資源だけを扱う。Library の3秒周期差分scanは仕様を変えず、
新規・変更 source を同じ Global 仲裁経路へ送る。同一 revision の retry を再導入しない。
FAST / SLOW Page Map、PageMapCoordinator、RAR / CBR の SLOW 経路、Viewer が読書中に
Page Map の有無を変えない制約も変更しない。複数フレームの Video thumbnail は将来検討で
あり、現行仕様では未実装である。Thumbnail のGlobal仲裁はSLOW Page Mapの責務や
予約枠を変更するものではなく、Page Map設計そのものとは分離されている。

---

## FAST Page Map

FAST Page Map は、本全体のページ情報を軽量に取得できる場合に使う経路である。

目的は、全ページを本格的にデコードせずに、Viewer が必要とする最低限のページ情報を得ることである。

Page Map が保持する情報は、ページ単位の軽量メタデータである。

```text
Page Map
  |
  +-- page index
  +-- image format
  +-- width
  +-- height
```

Page Map は画像本体を保持しない。

デコード済み画像も保持しない。

レンダリング結果も保持しない。

FAST Page Map は、軽量メタデータ取得が成立する形式でだけ Ready になる。

```text
FAST Page Map
  |
  +-- 全ページの軽量メタデータ取得に成功
  |     => Ready
  |
  +-- 軽量メタデータで扱えない形式が含まれる
  |     => RequiresComplete
  |
  +-- 読み取り失敗
        => Failed
```

Ready の場合だけ、Page Map として保存する。

RequiresComplete の場合は、必要に応じて SLOW Page Map に回す。

Failed の場合は、通常読書へのフォールバックを妨げない。

---

## Library差分scanによるサムネイル復旧

サムネイル復旧は、静止画・動画それぞれに専用の時間ベース経路を持たせず、
Library の差分scanに一本化する。Library は3秒周期で現在のsourceを再scanし、
一覧に新規検知されたsourceと、既存sourceの `size` または `modified` が変化した
sourceを検出する。

```text
Library 3秒周期差分scan
  |
  +-- 新規sourceを検知
  |     => 通常サムネイル生成を要求
  |
  +-- 既存sourceの size / modified 変化を検知
        => source revision変更として扱う
        => 旧thumbnail state / cacheを無効化
        => Failed状態を解除
        => 新revisionの通常サムネイルworker経路を要求
```

差分scanは一覧全体のthumbnail stateを消去しない。変更のないsourceは既存状態を
維持し、追加・変更されたsourceだけを通常のサムネイル要求へ戻す。新revisionでは
通常のartifact処理が行われるため、Page Map cache lookupを含む通常のPage Map評価も
再び行われ得る。したがって、source revision変更後にPage Mapが再評価されることは、
サムネイル復旧の別経路ではなく、通常の新revision処理の一部である。

同一revisionに対する時間ベースの再試行は行わない。サムネイル生成中にsourceが
変化した場合も、古い要求や転送中の結果を採用せず、差分scanが検知した新revisionを
同じ無効化・通常生成の流れへ通す。処理中のsource変更からの復旧経路はこの流れに
一本化する。

source kindごとの復旧対象は、Archive / Bookでは同一path/idのsnapshot変更時に
旧texture、要求状態、失敗状態を無効化し、`force_reload`を維持したまま通常要求へ戻す。
ImageFileも `size` / `modified` の変更で同じ状態無効化を行う。FolderBookは変更された
sourceの既存book stateを破棄して通常要求へ戻す。VideoFileは旧`video_states`と要求中の
snapshotを破棄し、古い結果を採用せず通常のvideo laneから再生成する。

Failure Disk Cacheも `size` / `modified` に基づくsource revision単位で扱う。
転送途中の旧revisionに記録された失敗は、新revisionの生成判定を阻害せず、既存の
revision判定とobsolete artifact pruneによって旧revisionとして扱われる。

```text
サムネイル復旧で行うこと:
  - 3秒周期のLibrary差分scan
  - 新規sourceの通常サムネイル要求
  - size / modified 変化によるsource revision更新
  - 旧thumbnail state / cacheの無効化とFailed解除
  - 新revisionの通常サムネイルworker経路

サムネイル復旧で行わないこと:
  - 同一revisionの時間ベース再試行
  - サムネイル復旧専用処理としてSLOW Page Mapを直接起動すること
  - Viewer読書中のPage Map生成
```

この復旧経路でも、Thumbnail と Page Map は独立した成果物である。Thumbnail が
成功した後に Page Map が失敗しても、Thumbnail 成功を取り消さない。

---

## SLOW Page Map生成

SLOW Page Map生成は、FAST Page Mapで作れなかった本に対するフォールバック経路である。

```text
FAST Page Map
  |
  +-- Ready
  |     => 保存して完了
  |
  +-- RequiresComplete
  |     => SLOW Page Mapへ
  |
  +-- Failed
        => 必要に応じて失敗扱い
```

SLOW Page Mapでは、軽量メタデータだけではなく、より重い読み取りや通常のメタデータ取得を使って、Page Map作成を試みる。

この経路はサムネイル生成より時間がかかる可能性がある。

SLOW Page Map の予約・実行・完了通知は PageMapCoordinator で調整する。
RAR / CBR は引き続きこの低速枠の Page Map 経路で扱い、FAST 経路へ一般化しない。

そのため、Library表示や読書開始を妨げてはいけない。

```text
SLOW Page Map生成
  |
  +-- バックグラウンドで実行
  +-- サムネイル表示を妨げない
  +-- Viewer起動を妨げない
  +-- 読書を妨げない
```

SLOW Page Map は、Page Map の補完処理であり、サムネイルの復旧処理ではない。

---

## Viewerでの利用

Viewer は、起動時に利用可能な Page Map があれば使用する。

```text
Viewer起動
  |
  +-- 保存済みPage Mapあり
  |     => Mapped
  |
  +-- 保存済みPage Mapなし
        +-- source kind別FAST生成を試行
              |
              +-- ZIP / CBZ、EPUB等
              |     |
              |     +-- FAST Ready
              |     |     => Mapped
              |     |
              |     +-- FAST不可 / RequiresComplete
              |           => Unavailable
              |
              +-- FolderBook
                    |
                    +-- FAST Ready
                    |     => Mapped
                    |
                    +-- FAST RequiresComplete
                          => bootstrap中に同期SLOWを実行
                          => 成功: Mapped
                          => 失敗: Unavailable
```

FolderBook は、内部に JPEG、PNG、WebP、TIFF など複数の画像形式が混在することが通常である。
FAST Page Map は、全ページについて軽量 metadata 取得が成立した場合だけ `Ready` になる。
1ページでも FAST 経路だけで descriptor を確定できなければ `RequiresComplete` になり得る。

FolderBook は archive 展開を必要とせず、各画像 file へ直接アクセスできる。
この形式固有の性質を利用し、FolderBook に限って Viewer bootstrap 中の同期 SLOW を許可する。
目的は、読書開始前に完全な Page Map を確定し、AUTO 見開き、進捗表示、
Streaming 計画を session 開始時から安定させることである。

これは FolderBook が常に SLOW という意味ではない。
FAST 対応画像だけで構成される場合や、画像形式が混在していても全 page descriptor を
FAST で取得できる場合は `Ready` となり、同期 SLOW は実行しない。
この例外を ZIP / CBZ、RAR / CBR、EPUB などへ一般化しない。

Viewer は、読書 session 開始後に SLOW Page Map 生成を開始しない。
FolderBook の同期 SLOW は ViewerState を構築する前の bootstrap 処理であり、
読書中の background SLOW ではない。

読書中に Page Map の有無が変わると、進捗表示、AUTO見開き、Streaming/cache計画が途中で変わる可能性がある。

そのため、Viewer は本を開いた時点で、そのセッションにおける Page Map 利用可否を確定する。

```text
Viewer読書セッション
  |
  +-- 開始時にMapped
  |     => そのセッションではPage Mapありとして扱う
  |
  +-- 開始時にUnavailable
        => そのセッションではPage Mapなしとして扱う
```

Page Map がなくても、Viewer は通常の読書経路で本を開く。

---

## 形式ごとのPage Map生成

Page Map は、Viewer が実際に読むページ順と一致している必要がある。

形式ごとに、ページ順の決め方は異なる。

```text
ZIP / CBZ
  => archive内画像をnatural sortした順序

RAR / CBR
  => readerが使うarchive page order

FolderBook
  => readerが使うフォルダ内画像順

EPUB画像本
  => EPUB自身の読書順
```

EPUB画像本では、archive内のファイル名を natural sort してはいけない。

EPUB は、自身の読書順を持つ。

```text
EPUB画像本
  |
  +-- META-INF/container.xml
  +-- OPF package document
  +-- manifest
  +-- spine
  +-- XHTML内の画像参照順
```

この順序に従うことで、Page Map と実際の読書順が一致する。

---

## 失敗時の責務分離

サムネイル失敗と Page Map 失敗は、扱いを分ける。

```text
サムネイル失敗
  => Library表示やopenable判定に影響する

Page Map失敗
  => 高度な読書補助が使えないだけ
  => 通常読書へフォールバックする
```

Page Map が失敗しても、本が読めるなら Viewer は開けるべきである。

一方で、DRM保護されたEPUBなど、形式として読めないものは恒久失敗として扱い、無期限の自動再試行を行わない。

---

## 保存と削除

生成された Page Map は、本に紐づく artifact として保存する。

```text
本
  |
  +-- thumbnail artifact
  +-- page map artifact
```

Page Map は、元の本の revision と対応づける。

本が変更された場合、既存の Page Map は無効になる可能性がある。

Library操作で本を削除または名前変更した場合、Page Map artifact も本に追従して扱う。

Page Map は画像cacheではない。

Page Map は、本の構造を表すメタデータである。

---

## 構成まとめ

```text
通常サムネイル生成
  = サムネイル + FAST Page Map

Library 3秒周期差分scan
  = source変更検知 + サムネイル復旧

SLOW Page Map生成
  = FAST Page Map失敗時のフォールバック

Viewer
  = 起動時にPage Map利用可否を確定
  = 読書中にSLOW Page Mapを起動しない
```

最も重要な原則は次である。

> Page Map は読書体験を改善するが、読書の必須条件にしてはいけない。
