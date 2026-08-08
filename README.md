# bah

`bah` は、RustとGPUIで実装するHyprland専用のWayland Layer Shellステータスバーです。Waybar等をラップせず、GPUIによる描画とHyprland Unix socket IPCを直接使用します。

## 現在の機能

- 画面上端・左右アンカーのTop Layer Surface
- 設定したバー高の排他領域、透明なWindow背景と暗色半透明のバー背景、キーボード非フォーカス
- 左側のワークスペース表示（ウィンドウがあるものとアクティブなものだけを表示し、アクティブな番号部分にはフォーカス中アプリのアイコン、続けてウィンドウタイトルを表示）。Workspaceを左クリックするとそのWorkspaceへ遷移します。
- アクティブなWorkspaceを右クリックすると、フォーカス中アプリのDesktop Entryが宣言する`Actions`（LinuxのJump List / Quicklist標準）を表示し、選択したアクションを起動します。`Actions`を宣言していないアプリではメニューを表示しません。
- 右側の `YYYY-MM-DD HH:MM:SS` 時計。1秒ごとにGPUI Entityを更新します。
- 時計の右側に通知ボタンを表示し、未処理通知数をバッジで表示します。クリックすると画面右端に固定された、画面幅の35%・画面高の通知トレイを右からスライドインで開きます。
- 通知トレイ上部にWi-Fi、Bluetooth、既定の音声出力・入力、画面輝度のコントロールを表示します。Wi-FiとBluetoothは左クリックでOn/Off、右クリックでデバイスコントロールセンターを開きます。音量・輝度スライダーはドラッグ中に現在値を表示します。
- `org.freedesktop.Notifications` のセッションD-Busサービスとして通知を受信し、トレイで個別削除または一括削除できます。既存の通知デーモンが同名サービスを所有している場合は、そのデーモンとの競合を避けて通知受信を無効にします。
- `.socket.sock` による初期ワークスペース取得と、`.socket2.sock` のworkspace/focused-monitorイベントによる更新
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
```

`window device-control-center` は現段階では空の通常Windowだけを表示します。通知トレイのWi-Fi／Bluetooth右クリック、または音声入出力のデバイス選択ボタンからも起動できます。Hyprland上では、BahがPIDと`app_id`で自ウィンドウを特定してfloat dispatcherを送るため、個別のWindow Ruleや`Hyprland.lua`設定なしで浮動表示されます。

## デバイスコントロールの動作環境

通知トレイのデバイスコントロールは、NetworkManager、BlueZ、PipeWire/WirePlumber、systemd-logind、およびLinux backlight sysfsを使用します。音声操作にはWirePlumber付属の`wpctl`が実行時に必要です。利用できないサービスやデバイスは個別に「利用不可」と表示され、通知トレイのほかの機能は継続動作します。

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
```

## 視認性と外観

バーのWindow背景は透明ですが、描画するルート要素には `RGB(18, 18, 22)`、不透明度約72%の暗色背景を常に描画します。これにより、Hyprlandのblurが無効でも明るい・暗い・白黒混在・細かい模様・高彩度の壁紙上で文字を壁紙へ直接重ねずに表示します。

- 主文字色: `RGB(245, 245, 247)`（時計、アクティブワークスペース）
- 副文字色: `RGB(202, 202, 210)`（非アクティブワークスペース）
- アクティブワークスペース: 太字、明るい文字、半透明の明色背景、小さい角丸
- 下側境界線: 明色・約12%不透明・1論理ピクセル

文字色は壁紙の輝度に応じて切り替えません。これは白黒が混在する壁紙でも外観と視認性を安定させるためです。GPUI revisionには独立したテキストシャドウAPIがないため、テキストシャドウは使用せず、バー背景でコントラストを確保します。

白い壁紙を通常モードの最悪条件として合成すると、バー背景は概ね `RGB(84, 84, 87)` になり、主文字色は約 `6.9:1`、副文字色は約 `4.6:1` のコントラストになります。暗い壁紙では背景がさらに暗くなるため、これより高いコントラストになります。

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

両方の環境変数は `1`、`true`、または `yes` を受け付けます。それ以外の値や解析失敗時は無効（通常モード）へフォールバックします。

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
- 音量、ネットワーク、Bluetooth、システムトレイ、設定画面、動的プラグインは未実装です。
- IPCイベントワーカーの再接続は未実装です。接続断はログへ出力され、時計は継続します。
