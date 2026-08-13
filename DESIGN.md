# Bah デザインシステム

## 目的

Bah は、壁紙と共存する軽量なWaylandシェルUIである。Bar、Notification Tray、Popover、通知、DCC、Settingsのどれを開いても、同じ暗色ガラス材と落ち着いた情報階層として認識できることを目指す。透明感よりも、blurが無効な環境を含む可読性と操作状態の明瞭さを優先する。

## 面の階層

Window背景は透明にし、描画するルート要素が意味ごとの背景を持つ。個別の画面が独自の暗色や不透明度を定義してはならない。

| 面 | 対象 | 通常時 | 役割 |
| --- | --- | --- | --- |
| Shell | Bar、Notification Tray | `RGB(18,18,22)` 72% | 常時表示・画面端に接するUI |
| Floating | Popover、Tooltip、Jump/Workspace Menu | 同色88% | 壁紙上の短い文脈操作 |
| Dialog | DCC、通知カード、モーダル | 同色94% | 集中して読む・操作するUI |
| Window | Settings | 同色100% | 通常ウィンドウ |

`BAH_GLASS=1` はShellだけを50%にする任意モードである。明るい壁紙かつblurなしではコントラストを保証しないため、日常の既定値にはしない。

## 色と状態

- 主文字は `RGB(245,245,247)`、副文字は `RGB(202,202,210)`。壁紙の明暗による動的な反転は行わない。
- 面上の明色オーバーレイはcontainer 6%、hover 12%、selected 18%、pressed 24%、border 12%、strong border 22%を使う。
- 選択・有効状態は、背景だけに依存せず、文字色、太字、位置、チェック、ラベルを組み合わせる。
- 緑は成功、青は入力フォーカス、赤はcritical・危険・失敗に限る。大きなボタンやカード全体を高彩度に塗らない。
- 入力不能は45%不透明度、フォーカスは青い境界、エラーは赤い境界またはメッセージで表す。
- 角丸は操作要素6px、カード・Popover・モーダル8pxを基本とする。強い影は使わない。

## 情報設計

- Notification Trayは頻繁な確認と即時操作のためのShell。Wi-Fi、Bluetooth、音量、輝度、通知履歴を扱う。
- Popoverは起点となるBarアイコンに紐づく短い文脈操作。詳細な設定はDCCへ遷移する。
- DCCは詳細設定ハブ。現在のサイドバーとページ構造を維持し、カード・行・入力・モーダルに同じ面と状態トークンを適用する。
- 通知PopupはDialog面で内容を読み、criticalは赤いマーカーに加えて表示位置・要約で区別する。

## モーションとアクセシビリティ

- Notification Trayは右端から220msのease-outで表示・非表示する。モーション削減設定時は最終位置を直接表示する。
- `BAH_HIGH_CONTRAST=1`: Shellを約96%不透明、Floatingを約98%、DialogとWindowを完全不透明にし、文字と境界を明るくする。
- `BAH_DISABLE_TRANSPARENCY=1`: すべての面を完全不透明にする。High Contrastと併用した場合はHigh Contrast配色を優先する。
- `BAH_GLASS=1`: Shellを50%不透明にする。High Contrastまたは透明無効が有効なら無視する。
- 環境変数は `1`、`true`、`yes` を受け付け、それ以外または未設定は無効として扱う。

## 実装上のルール

- 色、面、余白、角丸、フォントサイズ、バー高、モーションは `src/theme.rs` の `BahTheme` に集約する。既存の `BarTheme` 名は互換エイリアスである。
- 新しいUIは `SurfaceRole` を選び、固定の `rgb()`、個別の `.alpha(1.0)`、外部テーマエンジンを追加しない。
- GPUIの色値は固定revisionの `rgba(0xRRGGBBAA)` 形式を使用する。
- GPUI側で独自blur、壁紙解析、動的な文字色反転、独自GPU blur、二重描画テキストシャドウを実装しない。blurはHyprlandへ委譲する。
- 例外としてWindow Switcherは、選択候補を識別するためにHyprlandの`toplevel-export`プロトコルから**一回だけ**toplevelのスナップショットを取得してよい。連続キャプチャ・画面全体キャプチャ・キャプチャ結果からの壁紙解析は行わない。取得不能時はアプリのアイコンと名称を使う。
- Layer Shell namespaceは `bah` を使い、Hyprland設定はユーザーが管理する。設定例はREADMEに記載する。

## 手動確認

blurの有無と各表示モードで、Bar、Tray、Popover、通知Popup、Tooltip/Menu、DCCの全ページ・モーダル、Settingsを確認する。

- 白い壁紙、黒い壁紙、白黒混在、細かい模様、高彩度の壁紙
- hover、pressed、selected、disabled、focus、success、critical/error
- Trayの表示・非表示と、モーション削減設定時の直接表示
- Popover/DCCの入力grab、外側クリックによるdismiss、通知操作
- Window SwitcherのMRU順、スナップショット取得不可時のアイコンfallback、選択枠、`commit`時だけのフォーカス移動。Escape等のキャンセルキーは予約しない。
