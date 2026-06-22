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
  +-- 優先サムネイル生成
  |     |
  |     +-- サムネイル生成
  |     |
  |     +-- FAST Page Map生成
  |             |
  |             +-- 成功: Page Map保存
  |             |
  |             +-- 未対応/失敗: SLOW Page Mapへ委譲
  |
  +-- リトライサムネイル生成
  |     |
  |     +-- サムネイル再生成のみ
  |
  +-- SLOW Page Map生成
        |
        +-- FASTで作れなかったPage Mapをバックグラウンドで生成
```

重要なのは、リトライサムネイル生成と SLOW Page Map 生成を混ぜないことである。

---

## 優先サムネイル生成

優先サムネイル生成は、Library が本を表示するための通常経路である。

この経路では、サムネイル生成と同じ機会に、可能であれば FAST Page Map 生成も試みる。

```text
優先サムネイル生成
  |
  +-- サムネイルを作る
  |
  +-- FAST Page Mapを作る
```

この経路の目的は、Library の初回表示や通常表示をできるだけ早く成立させることである。

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

つまり、優先サムネイル生成では、サムネイルが主、FAST Page Map は付随処理である。

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

## リトライサムネイル生成

リトライサムネイル生成は、サムネイル生成に失敗した本を再試行するための経路である。

この経路の責務は、サムネイルの再生成だけである。

```text
リトライサムネイル生成
  |
  +-- サムネイルを再生成する
```

リトライサムネイル生成では、Page Map 生成を行わない。

理由は、サムネイルのリトライを Page Map の再試行経路にしてしまうと、Library の軽い復旧処理が暗黙の全ページ走査になってしまうためである。

```text
リトライサムネイル生成でやること:
  - サムネイル再生成

リトライサムネイル生成でやらないこと:
  - FAST Page Map再生成
  - SLOW Page Map生成
  - 全ページ走査
```

Page Map の失敗は、サムネイル失敗とは別の問題である。

そのため、サムネイルのリトライ経路に Page Map の責務を持たせない。

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

そのため、Library表示や読書開始を妨げてはいけない。

```text
SLOW Page Map生成
  |
  +-- バックグラウンドで実行
  +-- サムネイル表示を妨げない
  +-- Viewer起動を妨げない
  +-- 読書を妨げない
```

SLOW Page Map は、Page Map の補完処理であり、サムネイル生成のリトライ処理ではない。

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
  |     +-- FAST生成可能
  |     |     => Mapped
  |     |
  |     +-- FAST生成不可
  |           => Unavailable
```

Viewer は、読書中に SLOW Page Map 生成を開始しない。

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

一方で、DRM保護されたEPUBなど、形式として読めないものは恒久失敗として扱い、無限にリトライしない。

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
優先サムネイル生成
  = サムネイル + FAST Page Map

リトライサムネイル生成
  = サムネイルのみ

SLOW Page Map生成
  = FAST Page Map失敗時のフォールバック

Viewer
  = 起動時にPage Map利用可否を確定
  = 読書中にSLOW Page Mapを起動しない
```

最も重要な原則は次である。

> Page Map は読書体験を改善するが、読書の必須条件にしてはいけない。
