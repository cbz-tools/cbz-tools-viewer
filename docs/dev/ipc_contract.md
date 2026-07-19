# Viewer IPC Contract

## 目的

Library と Viewer は別プロセスで動作する。

Viewer は表示を担当し、
Library は本一覧・お気に入り・削除状態を管理する。

Viewer は要求を送信し、
Library が結果を決定する。

---

## 責務

### Library

以下を所有する。

* 本一覧
* 本順序
* フィルタ状態
* お気に入り状態
* 削除状態

Library が唯一の真実（Source of Truth）である。

---

### Viewer

以下を担当する。

* 本の表示
* ページ移動
* UI操作

Viewer は Library 状態を直接変更しない。

---

## Viewer → Library

### 状態取得

```text
RequestViewerState
```

現在本のお気に入り状態を取得する。

---

### お気に入り切替

```text
FavoriteToggle
```

現在本のお気に入り状態を切り替える。

---

### 本移動

```text
RequestNextBook
RequestPrevBook
```

現在本を基準に次本・前本を要求する。

Library の本順序は Archive と FolderBook を対象とし、
Folder と ImageFile は対象外とする。

---

### 隣接本取得

```text
RequestAdjacentBooks
```

削除ダイアログや境界プレビュー用の隣接本情報を取得する。

prev / next は Archive または FolderBook になり得る。

---

### 削除

```text
Delete
DeleteAndNext
```

現在本の削除を要求する。

Archive は file delete、
FolderBook は directory delete とする。

---

## Library → Viewer

### 状態応答

```text
ResponseViewerState
```

Viewer 状態を返す。

現在はお気に入り状態を含む。

---

### お気に入り更新応答

```text
FavoriteToggleResponse
```

更新後のお気に入り状態を返す。

---

### 本移動

```text
NavigateTo
```

移動先の本を返す。

Library が移動先を決定する。

対象は Archive と FolderBook である。

---

### 削除完了

```text
Deleted
```

削除結果と削除後の遷移先を返す。

DeleteAndNext は現在の Archive または FolderBook を削除し、
既存契約に従って次の本へ遷移する。

---

### 隣接本情報

```text
AdjacentBooks
```

前後の本を返す。

prev / next は Archive または FolderBook になり得る。
境界サムネイル / 前後本プレビューも同じ順序を利用する。

---

### 本なし

```text
NoMoreBooks
```

移動先が存在しない。

---

### エラー

```text
Error
```

要求処理に失敗した。

---

## エラー分類

### Retry可能

```text
SnapshotUnavailable
SnapshotPathMismatch
```

Viewer は再要求可能。

`SnapshotUnavailable` と `SnapshotPathMismatch` は、
retry可能な型契約を維持するための予約codeである。
現行navigation resolutionで通常送出されるerrorであるとは限らない。

---

### Retry不可

```text
FileNotFound
DeleteFailed
AccessDenied
InvalidRequest
Unknown
```

要求は失敗として終了する。

---

## Navigation Resolution

本移動は Library が解決する。

```text
Viewer
↓
RequestNextBook
↓
Library
↓
NavigateTo
↓
Viewer
```

Viewer は移動先を決定しない。

---

## Favorite Resolution

お気に入り状態は Library が管理する。

```text
Viewer
↓
FavoriteToggle
↓
Library
↓
FavoriteToggleResponse
↓
Viewer
```

Viewer は表示のみ更新する。

---

## Snapshot

Library は IPC 処理用に読み取り専用 Snapshot を保持する。

Snapshot に含める本は Archive と FolderBook とする。
Folder と ImageFile は対象外とする。

目的は、

```text
UIスレッド
↓
IPCスレッド
```

の分離である。

Snapshot は Library の内部実装であり、
Viewer は直接参照しない。

Snapshot が空・未同期・不一致の場合も、
Archive と FolderBook を本移動候補として扱う。

現行navigation resolutionでは、snapshotが空、未同期、またはcurrent pathと不一致の場合、
Library process内でfallback book orderを解決する。
fallbackが成立する限り、Viewerへ`SnapshotUnavailable`または
`SnapshotPathMismatch`を返さない。

fallback後もLibraryがSource of Truthであり、navigation resolverである。
Viewerがfilesystem順序を決めることはない。
両error codeは予約済み・互換維持用のcontractとして残し、
将来のfallback不能経路やtransport contractで利用される可能性を排除しない。

---

## 設計原則

```text
Library が状態を管理する

Viewer は要求を送信する

Library が結果を決定する
```

この原則を維持する。
