# Viewer Windowed最大化起動（Windows 11）

> この文書は、初回調査時の判断と、その後の再調査・採用結果を記録している。
> 現在の採用方式は「最終採用構成」を参照すること。

## 現在の結論

Viewer の通常 Windowed 位置・サイズと最大化状態は分離して保存する。

最大化状態で起動する場合は、

```text
保存済み Viewer 矩形が属する monitor rect で非最大化起動
→ 遅延 Maximized(true)
→ Win32 最大化完了を確認
→ rcNormalPosition だけを保存済み通常矩形へ補正
```

を採用する。

これにより、

```text
点滅なし
最大化アニメーションなし
最大化解除時に前回通常矩形へ復帰
```

を両立する。

---

# 第1次調査：最大化起動時の点滅とアニメーション

## 当時の症状

ライブラリから Windowed モードでビューアを起動する際、

* 起動時の点滅
* 最大化アニメーション
* マルチモニタ環境での初期位置ずれ

が発生した。

## 試した案

以下を試したが解決しなかった。

* Visible(false) → Visible(true)
* 安定フレーム待機
* Glow バックエンド
* SetWindowPlacement + ShowWindow
* DWM 制御

結論:

```text
Windows / eframe 初期化時の描画起因
```

であり、完全な抑制は困難だった。

### 最大化アニメの比較

以下で挙動が異なることを確認した。

```rust
with_maximized(true)
```

結果:

```text
アニメなし
点滅あり
```

```rust
ViewportCommand::Maximized(true)
```

結果:

```text
アニメあり
点滅なし
```

点滅抑制を優先し、遅延最大化を採用した。

## 当時の採用構成

ライブラリウィンドウ中心点からモニターを取得する。

```text
MonitorFromPoint()
↓
GetMonitorInfo()
↓
viewer_monitor_rect
```

を Viewer に渡す。

Viewer は起動時に、

```rust
with_position(monitor_origin)
with_inner_size(monitor_size)
with_maximized(false)
```

で立ち上げ、起動後に

```rust
ViewportCommand::Maximized(true)
```

を送信する。

## 当時の採用理由

* 点滅が最も少ない
* 同一モニターで起動できる
* 最大化状態を維持できる
* 実装が比較的単純
* Windows API 依存を最小化できる

## 当時残った制約

この方式で点滅と最大化アニメーションは抑制できた。

一方で、最大化起動前の通常ウィンドウ矩形として monitor rect 相当を使用するため、最大化解除時に「前回ユーザーが使っていた通常 Windowed 矩形」へ戻すことは扱っていなかった。

また、Viewer の通常位置・サイズ・最大化状態を前回終了時から復元する仕様も持っていなかった。

### 不採用案

#### 非表示起動

```text
Visible(false)
Visible(true)
```

複雑化するが効果なし。

#### Windows API 最大化

```text
SetWindowPlacement
ShowWindow
```

保守コストに対して効果が小さい。

#### DWM 制御

```text
DWMWA_TRANSITIONS_FORCEDISABLED
```

Windows 11 では効果なし。

#### Glow バックエンド

根本解決にならない。

#### builder 最大化

```rust
with_maximized(true)
```

点滅が再発する。

---

# 第2次調査：前回Windowed状態の復元とrestore rect補正

2026-06

## 発端

従来の Windowed Viewer は、画面いっぱいに近い通常ウィンドウを作ってから最大化していた。

その結果、通常状態なのに最大化して見え、Windows の最大化ボタンを押すとわずかに拡大するという UX 上の違和感があった。

前回終了時の通常 Windowed 位置・サイズ・最大化状態を復元する方針へ変更した。

## eframe persist_window の評価

`eframe` の `persist_window` による PoC も試したが、現在の Library / Viewer 別プロセス構成では、Viewer の前回位置・サイズ復元を確認できなかった。

そのため、Viewer 用 session として自前保存を採用した。

保存項目は次の 5 つに分けた。

```text
viewer_window_x
viewer_window_y
viewer_window_w
viewer_window_h
viewer_window_maximized
```

通常位置・サイズと最大化状態は別々に保存する。

この方向性は、`egui Issue #3494` で議論されている「ウィンドウ状態の分離」に近い。

## 自前保存の意味

保存値の意味は次のとおり。

```text
位置
→ 通常 Windowed 時の outer_rect.min

サイズ
→ 通常 Windowed 時の inner_rect.size

最大化状態
→ Windowed 時の viewport.maximized
```

保存条件は次のとおり。

```text
通常矩形
→ 非Fullscreen・非最大化時だけ更新

最大化状態
→ 非Fullscreen時だけ更新

Fullscreen中
→ Windowed 最大化状態を更新しない
```

最大化時のモニター全面サイズを、通常 Windowed 矩形として保存しない。

## 新しい最大化起動シーケンス

最終的な起動シーケンスは次のとおり。

```text
1. 保存済み通常 Viewer 矩形が属するモニターを取得
2. その monitor rect 相当で非最大化起動
3. 1 フレーム後に ViewportCommand::Maximized(true)
4. Win32 最大化完了を確認
5. GetWindowPlacement
6. rcNormalPosition だけを保存済み通常矩形へ補正
7. SetWindowPlacement
```

最大化完了条件は次の 3 つを同時に確認する。

```text
egui viewport.maximized == true
IsZoomed(hwnd) != 0
WINDOWPLACEMENT.showCmd == SW_SHOWMAXIMIZED
```

最大化完了を短時間だけ再試行し、無限再試行は行わない。

## SetWindowPlacement 再評価の経緯

過去に `SetWindowPlacement` を不採用にしたのは、点滅や最大化アニメーション自体を抑える効果が小さかったためだった。

今回の用途は異なり、

```text
最大化処理
→ monitor rect + 遅延 Maximized(true)

SetWindowPlacement
→ 最大化解除時の restore rect 補正だけに使用
```

として用途を限定したことで、効果が確認できた。

つまり、過去の不採用判断を覆したのではなく、用途を限定して再評価した結果として採用した。

## rcNormalPosition の注意点

`GetWindowPlacement` で取得した既存状態を維持し、`rcNormalPosition` だけを変更する。

座標系は次のとおり。

```text
rcNormalPosition
→ ワークスペース座標

保存位置
→ スクリーン座標相当

rcWork と rcMonitor の差分で補正する
```

サイズは次のとおり。

```text
保存サイズ
→ inner size

rcNormalPosition
→ outer rect

AdjustWindowRectEx で inner → outer へ変換する
```

`WINDOWPLACEMENT` の座標を `SetWindowPos` へ流用しない。

## モニター選択

最終仕様は次のとおり。

```text
通常Windowed
→ 保存済み Viewer 位置・サイズ・最大化状態を復元

最大化Windowed
→ 保存済み Viewer 通常矩形が属するモニターを優先

Library から Full 起動
→ 保存済み Viewer 通常矩形が属するモニターを優先

保存値なし・無効
→ Library 由来のモニター情報へフォールバック

F11
→ 現在 Viewer が存在するモニターで Fullscreen
```

設定化は見送った。

理由は、Windowed と Full を前回 Viewer モニターへ追従させれば、設定を増やさず運用で一貫した挙動になるため。

## 起動引数整理

旧内部引数は削除した。

最大化状態の真実は次の 1 つに集約した。

```text
session.viewer_window_maximized
```

残した起動情報は次のとおり。

```text
viewer_window_pos
→ 初回 Windowed 位置フォールバック

viewer_monitor_rect
→ Library モニターの一般フォールバック

viewer_fullscreen_target
→ Library から直接 Full 起動する際の初期 Viewport
```

---

# 最終採用構成

## 保存

```text
通常位置
→ outer_rect.min

通常サイズ
→ inner_rect.size

最大化状態
→ viewport.maximized

通常矩形と最大化状態は分離保存
```

## 通常 Windowed 起動

```text
保存済み Viewer 矩形
→ Library 由来位置
→ OS 既定
```

## 最大化 Windowed 起動

```text
保存済み Viewer 矩形が属する monitor rect
→ 非最大化起動
→ 遅延最大化
→ Win32 最大化完了確認
→ rcNormalPosition 補正
```

## Fullscreen 起動

```text
Library から Full
→ 前回 Viewer モニター優先
→ 取得不能なら Library モニター

F11
→ 現在 Viewer がいるモニター
```

## 最大化状態の真実

```text
session.viewer_window_maximized
```

---

# 実機結果

* 点滅なし
* 最大化アニメーションなし
* 最大化状態で正しく起動
* 最大化解除で保存済み通常矩形へ戻る
* 再最大化後も restore rect を維持
* サブモニターで前回 Viewer 位置を維持
* Library → Full も前回 Viewer モニターへ追従
* F11 往復に影響なし

---

# 既知の後続課題

* Library と Viewer の session 同時書き込み競合
* Library の毎フレーム位置・サイズ取得の廃止
* DPI 差が大きいマルチモニター環境での再検証
* タスクバーが上・左にある環境での座標補正確認

これらは現在の採用を妨げる課題ではない。

---

# 参考・着想元

* egui Issue #3494
* WINDOWPLACEMENT / rcNormalPosition の API 用途
* SetWindowPlacement を restore rect 補正へ限定して再評価する助言
