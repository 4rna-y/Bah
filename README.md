# bah

`bah` は、RustとGPUIで実装するHyprland専用のWayland Layer Shellステータスバーです。Waybar等をラップせず、GPUIによる描画とHyprland Unix socket IPCを直接使用します。

## 現在の機能

- 画面上端・左右アンカーのTop Layer Surface
- 設定したバー高の排他領域、透明なWindow背景と暗色半透明のバー背景、キーボード非フォーカス
- 左側のワークスペース表示（ウィンドウがあるものとアクティブなものだけを表示し、アクティブな番号部分にはフォーカス中アプリのアイコン、続けてウィンドウタイトルを表示）。Workspaceを左クリックするとそのWorkspaceへ遷移します。
- アクティブなWorkspaceを右クリックすると、フォーカス中アプリのDesktop Entryが宣言する`Actions`（LinuxのJump List / Quicklist標準）を表示し、選択したアクションを起動します。`Actions`を宣言していないアプリではメニューを表示しません。
- 右側の `YYYY-MM-DD HH:MM:SS` 時計。1秒ごとにGPUI Entityを更新します。
- 時計の右側に通知ボタンを表示し、未処理通知数をバッジで表示します。クリックすると画面右端に固定された、画面幅の35%・画面高の通知トレイを右からスライドインで開きます。
- 通知トレイ上部にWi-Fi、Bluetooth、既定の音声出力・入力、画面輝度のコントロールを表示します。Wi-FiとBluetoothは左クリックでOn/Off、右クリックで対応するデバイスコントロールセンターのページを開きます。音量・輝度スライダーはドラッグ中に現在値を表示します。
- 接続中のAirPodsはBluetoothとは別のアイコンで表示し、Popoverから左右平均の充電リングとTransparency／Adaptive／Noise Cancellationを操作できます。Bluetooth DCCには左右個別の残量と同じモード切替を表示します。
- `org.freedesktop.Notifications` のセッションD-Busサービスとして通知を受信し、右上（バー直下）の一時ポップアップとトレイへ表示します。アクション、期限、緊急度、進捗、置換、同期スタックタグ、履歴、通知一時停止に対応します。既存の通知デーモンが同名サービスを所有している場合は、そのデーモンとの競合を避けて通知受信を無効にします。
- `bah notifications` は dunstctl と同じ主要操作（close、history、count、pause、rule、reloadなど）を提供します。Cargo パッケージには既存のキーバインドを移行できる `dunstctl` 互換エントリも含まれます。
- `.socket.sock` による初期ワークスペース取得と、`.socket2.sock` のworkspace/focused-monitorイベントによる更新
- WindowsのAlt+Tabと同様に、通常のマップ済みウィンドウをMRU順で個別に切り替えるOverlay。Hyprlandの`toplevel-export`で一回だけ取得したプレビューを表示し、取得できない場合はアプリアイコンを表示します。
- `Super+V`から開くClipboard履歴。テキスト、画像、URI、その他のMIME表現をユーザー限定のXDGデータ領域へ保存し、再起動後も選択できます。
- IPCが利用不能でも、時計だけを表示して起動継続

## 開発環境と起動

Nix FlakeがRust、Wayland、fontconfig、JetBrainsMono Nerd Font、Noto Sans Mono CJK JP、libxkbcommon、Vulkanの依存関係を提供します。

```bash
nix develop
RUST_LOG=info cargo run
```

UI変更を保存時に自動ビルド・再起動する開発用ウォッチャーは、次のように起動します。

```bash
nix develop --command env RUST_LOG=info cargo watch -x run
```

これは実行中コードを差し替えるHot Reloadではなく、差分ビルド後にプロセスを再起動します。

設定ウィンドウだけを開くには、次を実行します。設定ウィンドウは1つだけ起動でき、すでに開いている場合はこのコマンドは何もせず終了します。

```bash
RUST_LOG=info cargo run -- window config
```

リリースビルドを直接起動する場合も、同じ引数形式です。

```bash
./bah
./bah window config
./bah window device-control-center
./bah window device-control-center bluetooth
```

`window device-control-center` は、常駐中のBahへDCC GUIの表示を要求します。`network`、`bluetooth`、`display`で開始ページを指定できます。DCCは画面中央のポップアップとして開き、Wi-Fiパスワード入力、Bluetoothペアリング、モニター配置、出力別壁紙の選択をマウスで操作できます。Barが起動している必要があります。

## Window Switcher

Bahを常駐起動した状態で、外部キーバインドから次のCLIを呼び出します。`cycle` / `cycle-reverse` は未表示ならOverlayを開いて次／前のMRU候補を選び、表示中なら選択だけを移動します。`commit`だけが選択ウィンドウへフォーカスを移します。マウスクリックも選択のみで、確定はしません。通常のAltキー解放をHyprland設定から`commit`へ割り当ててください。Escapeによるキャンセル操作は設けません。

開発環境からHyprlandが実行するバイナリを更新するには、リポジトリ直下で`./build.sh`を実行します。リリースビルドは`~/.local/lib/bah/bah`へ配置され、NixのWayland/Vulkan共有ライブラリを設定するランチャーを`~/.local/bin/bah`（互換名`Bah`）へ配置します。配置先は`BAH_INSTALL_DIR`（または`XDG_BIN_HOME`）で変更できます。

```bash
bah switcher cycle
bah switcher cycle-reverse
bah switcher select-next
bah switcher select-previous
bah switcher commit
bah switcher close
```

Hyprlandの設定例です。

```ini
bind = ALT, TAB, exec, bah switcher cycle
bind = ALT SHIFT, TAB, exec, bah switcher cycle-reverse
bindr = , Alt_L, exec, bah switcher commit
bindr = , Alt_R, exec, bah switcher commit
```

## デバイスコントロールの動作環境

通知トレイのデバイスコントロールは、NetworkManager、BlueZ、PipeWire/WirePlumber、systemd-logind、およびLinux backlight sysfsを使用します。AirPods機能には、ペアリング済みAirPodsとBlueZのAACP L2CAP接続権限が必要です。音声操作にはWirePlumber付属の`wpctl`が実行時に必要です。利用できないサービスやデバイスは個別に「利用不可」と表示され、通知トレイのほかの機能は継続動作します。

- AudioOut: PipeWireの既定Audio Sink
- AudioIn: PipeWireの既定Audio Source
- Brightness: `/sys/class/backlight`から優先デバイスを選択し、logind経由で変更

Hyprlandセッションから同じコマンドを実行してください。起動後は次でLayer Surfaceを確認できます。

```bash
hyprctl layers
```

初回起動ではGPUI/WGPUのGPU初期化に数十秒かかることがあります。

詳細ログは次のように有効化できます。

```bash
RUST_LOG=debug cargo run
```

任意の設定ファイルは `$XDG_CONFIG_HOME/bah/config.toml` です。

```toml
bar_height = 36.0
# `bah wallpaper set` により絶対パスが書き込まれます。
# wallpaper = "/home/user/Pictures/wallpaper.png"
# DCCのディスプレイページで設定する出力別の壁紙です。
# [wallpapers]
# DP-1 = "/home/user/Pictures/external.png"

[notifications]
# dunst の既定値と同じ上限・履歴数。critical_timeout_seconds = 0 は手動で閉じるまで表示します。
popup_width = 360.0
notification_limit = 20
history_length = 20
low_timeout_seconds = 10
normal_timeout_seconds = 10
critical_timeout_seconds = 0
pause_level = 0

[[notifications.rules]]
name = "quiet-network"
enabled = true
app_name = "NetworkManager アプレット"
skip_popup = true
```

## Clipboard履歴

Clipboard履歴は`$XDG_DATA_HOME/bah/clipboard`（未設定時は`~/.local/share/bah/clipboard`）に、ユーザーだけが読める権限で保存されます。テキストは1行プレビュー、画像はパネル幅に合わせてアスペクト比を保って表示されます。選択すると表示前にフォーカスされていたウィンドウへCtrl+Vを送ります。貼り付けを受けない画面では、選択内容は通常Clipboardに残るため、次回のCtrl+Vで利用できます。

```toml
[clipboard]
max_entries = 100
max_entry_bytes = 33554432  # 32 MiB
max_total_bytes = 268435456 # 256 MiB
```

Bahを常駐起動した状態で、Hyprland 0.55以降のLua設定へ次を追加してください。パネルを開いている間は`↑`/`↓`で選択を移動し、`Enter`で貼り付け、`Escape`または`Super+V`で閉じられます。

```lua
hl.bind("SUPER + V", hl.dsp.exec_cmd("bah clipboard toggle"))
hl.define_submap("clipboard", function()
  local close_clipboard = function()
    hl.dispatch(hl.dsp.exec_cmd("bah clipboard close"))
    hl.dispatch(hl.dsp.submap("reset"))
  end
  hl.bind("escape", close_clipboard)
  hl.bind("SUPER + V", close_clipboard)
  hl.bind("up", hl.dsp.exec_cmd("bah clipboard previous"))
  hl.bind("down", hl.dsp.exec_cmd("bah clipboard next"))
  hl.bind("return", hl.dsp.exec_cmd("bah clipboard select"))
end)
```

`bah clipboard open`、`close`、`toggle`、`previous`、`next`、`select`に加え、永続履歴を消去する`bah clipboard clear`を利用できます。履歴機能には`wl-paste`と`wl-copy`（wl-clipboard）が必要です。

## スクリーンショット

`Super+Shift+S`で矩形スクリーンショットを開始します。Bah は選択 UI を開く前に全画面を固定取得するため、選択中に画面が変化しても、保存される画像はショートカットを押した時点の内容です。複数モニターをまたぐ矩形も選択できます。Escape でキャンセルします。

PNG は `$XDG_PICTURES_DIR/Screenshots` に保存されます。`XDG_PICTURES_DIR` が環境変数にない場合は `~/.config/user-dirs.dirs` を参照し、最後は `~/Pictures/Screenshots` にフォールバックします。保存した PNG は Clipboard にもコピーされ、Bah の Clipboard 履歴へ取り込まれます。

Bah を常駐起動した状態で、Hyprland Lua 設定に以下を追加してください。

```lua
hl.bind("SUPER + SHIFT + S", hl.dsp.exec_cmd(bah .. " screenshot"))
```

この機能には実行時に `grim` と `slurp`、Clipboard コピーに `wl-copy` が必要です。Nix 開発環境には含まれます。

## 通知デーモンの移行

Bah が通知D-Bus名を所有する必要があるため、Hyprlandの起動設定から `dunst` を外し、代わりに `bah` を起動してください。現在の設定では次の行を置き換えます。

```lua
-- hl.exec_cmd("dunst")
hl.exec_cmd("bah")
```

同じセッションでdunstがすでに実行中なら、Bahを起動する前に終了します。通知操作は次のように行えます。

```bash
bah notifications count
bah notifications set-paused toggle
bah notifications history
# 互換エントリをインストール済みの場合も同じ操作です。
dunstctl close-all
```

`reload` はdunstrcではなくBahのTOMLを再読込します。ルールは上から順に適用され、`app_name`、`summary`、`category`、`desktop_entry`、`urgency`で照合して、期限、pause override、ポップアップ抑止、履歴除外、スタックタグを変更できます。

## 壁紙

壁紙はBarとは別のBottom Layer Surfaceとして描画されるため、通常のアプリとBarの背後に表示され、キーボード・ポインター入力を取得しません。画像は出力全体へアスペクト比を保って`cover`表示します。

```bash
./bah wallpaper set resrc/wallpaper.png
./bah wallpaper unset
```

`set` はパスを正規化して設定ファイルへ保存し、既存のBah壁紙Layerを終了して新しいLayerをバックグラウンドで起動します。`unset` は設定を削除してLayerを終了します。壁紙Layerだけを（設定済みのパスで）起動したいときは `./bah wallpaper` を使えます。

通常の `bah` 起動時も、共通または出力別の壁紙が設定済みなら壁紙Layerを自動で起動します。

静止画形式（PNG、JPEG、WebPなど）に加え、GIFとアニメーションWebPでは各デコード済みフレームを順に描画します。MP4、WebM、MKV、AVI、MOV、M4VはFFmpegでデコードし、30fpsでLayerへ渡します。動画は音声なしで繰り返し再生されます。Nix開発環境にはFFmpegを含めています。Nix環境外で実行する場合は、`ffmpeg`と`ffprobe`を`PATH`から実行可能にしてください。

## ディスプレイ

`bah window device-control-center display`、またはDCC GUIの「Display」ページから、接続中のモニターをドラッグして配置できます。選択したモニターを「メインモニターにする」と、当該モニターを`0x0`に固定したまま他モニターの相対位置を維持します。適用時には`~/.config/hypr/bah_displays.lua`を生成し、`hyprland.lua`へBah管理の`require("bah_displays")`を追加します。workspace 1はメインモニターへ割り当てられます。

壁紙は出力ごとに選択できます。個別設定がない出力は従来の共通`wallpaper`を使用します。

## 視認性と外観

BarとNotification TrayのWindow背景は透明ですが、描画するルート要素には `RGB(18, 18, 22)`、不透明度約72%の暗色背景を常に描画します。Popoverは約88%、DCC・通知カードは約94%へ密度を上げ、壁紙上でも情報階層と可読性を保ちます。

- 主文字色: `RGB(245, 245, 247)`（時計、アクティブワークスペース）
- 副文字色: `RGB(202, 202, 210)`（非アクティブワークスペース）
- アクティブワークスペース: 太字、明るい文字、半透明の明色背景、小さい角丸
- 下側境界線: 明色・約12%不透明・1論理ピクセル

文字色は壁紙の輝度に応じて切り替えません。これは白黒が混在する壁紙でも外観と視認性を安定させるためです。GPUI revisionには独立したテキストシャドウAPIがないため、テキストシャドウは使用せず、バー背景でコントラストを確保します。

白い壁紙を通常モードの最悪条件として合成すると、バー背景は概ね `RGB(84, 84, 87)` になります。主文字は約 `6.9:1`、副文字は約 `4.6:1` のコントラストを目標とし、暗い壁紙ではさらに高くなります。

壁紙を変更できる環境では、以下を手動確認してください。blurの有無にかかわらず、時計とアクティブワークスペースが読み取れ、非アクティブワークスペースが判別できることを確認します。

- 白い壁紙
- 黒い壁紙
- 白黒が混在する壁紙
- 細かい模様の壁紙
- 彩度の高い壁紙

高コントラストモードは以下で有効にします。背景を約96%不透明、文字と境界線をより明確にします。

```bash
BAH_HIGH_CONTRAST=1 cargo run
```

透明度のみを完全に無効化する場合は以下を使用します。Hyprlandのblur設定とは独立して背景を完全不透明にします。

```bash
BAH_DISABLE_TRANSPARENCY=1 cargo run
```

より透明なShellを試す場合は、BarとNotification Trayだけを約50%不透明にします。明るい壁紙かつblurなしでは可読性を保証しないため、任意の表示モードです。

```bash
BAH_GLASS=1 cargo run
```

これらの環境変数は `1`、`true`、または `yes` を受け付けます。それ以外の値や解析失敗時は無効（通常モード）へフォールバックします。`BAH_DISABLE_TRANSPARENCY`、`BAH_HIGH_CONTRAST`、`BAH_GLASS` を併用した場合は、この順で優先されます。

Bah 自身の常駐メモリ使用量を INFO ログへ1秒ごとに出力するには、起動時に次を指定します。

```bash
BAH_MEMUSG=1 RUST_LOG=info cargo run
./bah --memusg
```

環境変数は `1` の場合だけ有効です。CLIの`--memusg`は`BAH_MEMUSG=1`に相当し、`RUST_LOG`未指定時はメモリ測定ログだけをINFOで表示します。通常は Linux の `/proc/self/smaps_rollup` からRSS、PSS、private、shared、anonymous、swapをKiBとMiBで表示します。`smaps_rollup`を利用できない環境では、`/proc/self/status`の`VmRSS`だけを表示します。共有ページを按分した実質的な負担を見る場合はRSSだけでなくPSSも確認してください。

## GPUバックエンドとメモリ軽量化

BahのGPUI/WGPUはデフォルトでVulkanとOpenGLの両方を初期化します。WGPUバックエンドはプロセス単位の`WGPU_BACKEND`、またはCLIの`--wgpu-backend`で選択できます。Vulkanだけに限定してメモリ使用量を抑えるには`--wgpu-backend vulkan`を指定します。Vulkanを利用できない環境では`--wgpu-backend gl`でOpenGLだけを使えます。CLI指定は既存の`WGPU_BACKEND`環境変数より優先されます。

複数GPUと複数のVulkan ICDがインストールされた環境では、使用しないGPUドライバの列挙だけで常駐メモリが大きく増えることがあります。NixOS x86_64上でIntel GPUだけを使用する場合は、Bahの起動設定に次を追加するとIntel Vulkan ICDだけを読み込みます。この値はマシン固有なので、システム全体ではなくBahのプロセスにだけ指定してください。

```bash
VK_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d/intel_icd.x86_64.json ./bah
```

同じ指定はBahのCLIオプションでも行えます。ハイフン区切りを正式形とし、`--vk_driver_files`も互換エイリアスとして受け付けます。CLI指定は既存の`VK_DRIVER_FILES`環境変数より優先されます。

```bash
./bah --memusg --wgpu-backend vulkan \
  --vk-driver-files /run/opengl-driver/share/vulkan/icd.d/intel_icd.x86_64.json
```

設定後に起動できない場合やGPU構成を変更した場合は、`VK_DRIVER_FILES`を外してドライバの自動探索へ戻します。Vulkan自体を利用できない場合の明示的なフォールバックは次のとおりです。

```bash
env -u VK_DRIVER_FILES WGPU_BACKEND=gl ./bah
```

メモリを比較するときはrelease版を起動し、初期化が完了するまで10秒待ってから5回計測した中央値を使用します。Intel ICD限定時の目安はアイドルRSS 65 MiB以下です。

## Hyprland設定（0.55以降）

Hyprland 0.55以降ではLua設定を使用します。`bah` namespaceに対するblur、半透明ピクセルの扱い、Layer Surfaceアニメーション無効化の例です。これはREADME上の設定例であり、ユーザーのHyprland設定ファイルを直接変更しません。

```lua
hl.config({
  decoration = {
    blur = {
      enabled = true,
      size = 4,
      passes = 2,
    },
  },
})

hl.layer_rule({
  match = { namespace = "bah" },
  blur = true,
  ignore_alpha = 0.1,
  no_anim = true,
})
```

`no_anim = true` を削除し、`animation = "layersIn"` を指定するとLayer Surface固有のアニメーションを選べます。利用可能なスタイルはHyprland設定に依存します。古いHyprlang設定を使うHyprlandでは、同等の旧 `layerrule` 構文をそのバージョンの公式ドキュメントで確認してください。

## GPUIの固定revision

GPUIはcrates.io版ではなく、forkしたZedの[`wgpu-backend` ブランチ](https://github.com/4rna-y/zed/tree/wgpu-backend)のGit revision `f005b687a4287536540fe71b898fe63a176ed0d3` に固定しています。

このforkは `WGPU_BACKEND` による明示的なバックエンド選択を尊重します。また、内部レンダラの非ゼロ初期サイズを保ったまま、対向アンカー方向ではLayer Shellプロトコルへサイズ0（コンポジタ決定）を送ります。

必要な修正は固定revisionに含まれているため、`nix develop`によるCargo checkoutへの後パッチは行いません。

## 制約と次の段階

- 1つのLayer Surfaceをデフォルト出力へ作成します。マルチモニタごとのバー生成は未実装です。
- Waylandには実行中アプリの任意のアプリ内メニューを他プロセスが取得する共通プロトコルはありません。このため右クリックメニューはDesktop Entry仕様の`Actions`に限定されます。これはアプリパッケージが提供するJump List / Quicklistの標準形式です。
- 音量のデバイス選択、Bluetooth機器の解除、システムトレイ、設定画面、動的プラグインは未実装です。
- IPCイベントワーカーの再接続は未実装です。接続断はログへ出力され、時計は継続します。
