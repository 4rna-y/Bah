# hyprbar

`hyprbar` は、RustとGPUIで実装するHyprland専用のWayland Layer Shellステータスバーです。Waybar等をラップせず、GPUIによる描画とHyprland Unix socket IPCを直接使用します。

## 現在の機能

- 画面上端・左右アンカーのTop Layer Surface
- 設定したバー高の排他領域、透明なWindow背景と暗色半透明のバー背景、キーボード非フォーカス
- 左側のワークスペース表示（ウィンドウがあるものとアクティブなものだけを表示し、アクティブな番号部分にはフォーカス中アプリのアイコン、続けてウィンドウタイトルを表示）
- 右側の `YYYY-MM-DD HH:MM:SS` 時計。1秒ごとにGPUI Entityを更新します。
- `.socket.sock` による初期ワークスペース取得と、`.socket2.sock` のworkspace/focused-monitorイベントによる更新
- IPCが利用不能でも、時計だけを表示して起動継続

## 開発環境と起動

Nix FlakeがRust、Wayland、fontconfig、JetBrainsMono Nerd Font、libxkbcommon、Vulkanの依存関係を提供します。

```bash
nix develop
RUST_LOG=info cargo run
```

Hyprlandセッションから同じコマンドを実行してください。起動後は次でLayer Surfaceを確認できます。

```bash
hyprctl layers
```

初回起動ではGPUI/WGPUのGPU初期化に数十秒かかることがあります。

詳細ログは次のように有効化できます。

```bash
RUST_LOG=debug cargo run
```

任意の設定ファイルは `$XDG_CONFIG_HOME/hyprbar/config.toml` です。

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
HYPRBAR_HIGH_CONTRAST=1 cargo run
```

透明度のみを完全に無効化する場合は以下を使用します。Hyprlandのblur設定とは独立して背景を完全不透明にします。

```bash
HYPRBAR_DISABLE_TRANSPARENCY=1 cargo run
```

両方の環境変数は `1`、`true`、または `yes` を受け付けます。それ以外の値や解析失敗時は無効（通常モード）へフォールバックします。

## Hyprland設定（0.55以降）

Hyprland 0.55以降ではLua設定を使用します。`hyprbar` namespaceに対するblur、半透明ピクセルの扱い、Layer Surfaceアニメーション無効化の例です。これはREADME上の設定例であり、ユーザーのHyprland設定ファイルを直接変更しません。

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
  match = { namespace = "hyprbar" },
  blur = true,
  ignore_alpha = 0.1,
  no_anim = true,
})
```

`no_anim = true` を削除し、`animation = "layersIn"` を指定するとLayer Surface固有のアニメーションを選べます。利用可能なスタイルはHyprland設定に依存します。古いHyprlang設定を使うHyprlandでは、同等の旧 `layerrule` 構文をそのバージョンの公式ドキュメントで確認してください。

## GPUIの固定revisionとローカルパッチ

GPUIはcrates.io版ではなく、Zed IndustriesのGit revision `4aad57fd1f002f9feeea2b7fb6229ccbcd576cb1` に固定しています。GPUIのこのrevisionでは、対向するLayer Shellアンカーにも初期レンダラ幅をそのまま送るため、バーが1px幅になります。

`patches/gpui-layer-shell-stretch.patch` は、内部レンダラの非ゼロ初期サイズを保ったまま、対向アンカー方向だけLayer Shellプロトコルへサイズ0（コンポジタ決定）を送る最小パッチです。`nix develop` はプロジェクト内の隔離Cargoキャッシュへ固定sourceを取得し、このパッチを一度だけ適用します。

## 制約と次の段階

- 1つのLayer Surfaceをデフォルト出力へ作成します。マルチモニタごとのバー生成は未実装です。
- ワークスペースクリックによるdispatcher実行は未実装です。ただし各要素はworkspace IDで識別されています。
- 音量、ネットワーク、Bluetooth、トレイ、通知、設定画面、動的プラグインは未実装です。
- IPCイベントワーカーの再接続は未実装です。接続断はログへ出力され、時計は継続します。
