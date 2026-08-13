use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use gpui::{
    Context, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Render, Window, div, img, prelude::*, px, rgba,
};
use log::error;

use crate::{
    app::{DeviceControlCenterPage, DeviceControlCenterRoute},
    config::Config,
    hyprland::display::DisplayLayout,
    modules::airpods::AirPodsListeningMode,
    modules::system_controls::{
        ActiveNetwork, BluetoothDevice, BluetoothEvent, BluetoothPairingPrompt,
        BluetoothPairingResponse, ControlAction, ControlChannels, ControlSnapshot, IpSettings,
        NetworkSettings, NetworkSettingsEvent, WifiConnectionEvent, WifiNetwork, WifiSecurity,
    },
    theme::{BarTheme, SurfaceRole, ui_font},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
enum InputField {
    Password,
    BluetoothPin,
    Ipv4Address,
    Ipv4Subnet,
    Ipv4Gateway,
    Ipv4PrimaryDns,
    Ipv4SecondaryDns,
    Ipv6Address,
    Ipv6Prefix,
    Ipv6Gateway,
    Ipv6PrimaryDns,
    Ipv6SecondaryDns,
}

impl InputField {
    fn element_id(self) -> u32 {
        match self {
            Self::Password => 0,
            Self::BluetoothPin => 1,
            Self::Ipv4Address => 2,
            Self::Ipv4Subnet => 3,
            Self::Ipv4Gateway => 4,
            Self::Ipv4PrimaryDns => 5,
            Self::Ipv4SecondaryDns => 6,
            Self::Ipv6Address => 7,
            Self::Ipv6Prefix => 8,
            Self::Ipv6Gateway => 9,
            Self::Ipv6PrimaryDns => 10,
            Self::Ipv6SecondaryDns => 11,
        }
    }
}

fn default_route_badge(network: &ActiveNetwork) -> Option<&'static str> {
    match (network.default_ipv4, network.default_ipv6) {
        (true, true) => Some("✓ IPv4 / IPv6"),
        (true, false) => Some("✓ IPv4"),
        (false, true) => Some("✓ IPv6"),
        (false, false) => None,
    }
}

fn is_wallpaper_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "webp"
                | "gif"
                | "mp4"
                | "webm"
                | "mkv"
                | "avi"
                | "mov"
                | "m4v"
        )
    )
}

fn select_wallpaper_with_zenity() -> anyhow::Result<Option<PathBuf>> {
    let output = Command::new("zenity")
        // Zenity is GTK's native file chooser. Force its Wayland backend so
        // no XWayland or portal parent surface is involved.
        .env("GDK_BACKEND", "wayland")
        .args([
            "--file-selection",
            "--title=壁紙を選択",
            "--file-filter=画像 | *.png *.jpg *.jpeg *.webp *.gif",
            "--file-filter=動画 | *.mp4 *.webm *.mkv *.avi *.mov *.m4v",
        ])
        .output()
        .map_err(|error| anyhow::anyhow!("Waylandファイルブラウザを起動できません: {error}"))?;

    if output.status.success() {
        let path = String::from_utf8(output.stdout)
            .map_err(|error| anyhow::anyhow!("選択されたパスを読み取れません: {error}"))?;
        let path = PathBuf::from(path.trim());
        return path
            .canonicalize()
            .map(Some)
            .map_err(|error| anyhow::anyhow!("選択されたファイルを開けません: {error}"));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(anyhow::anyhow!(if message.is_empty() {
        "Waylandファイルブラウザが終了しました".to_string()
    } else {
        format!("Waylandファイルブラウザを起動できません: {message}")
    }))
}

#[derive(Clone)]
enum Modal {
    Password {
        network: WifiNetwork,
        password: String,
        message: Option<String>,
        connecting: bool,
    },
    ConfirmNetworkSettings,
}

#[derive(Clone)]
struct NetworkDetails {
    network: ActiveNetwork,
    settings: NetworkSettings,
    message: Option<String>,
    saving: bool,
}

#[derive(Clone)]
struct BluetoothPairingDialog {
    device_path: String,
    device_label: String,
    prompt: BluetoothPairingPrompt,
    pin_code: String,
}

#[derive(Clone, Debug)]
struct MonitorDrag {
    name: String,
    start_pointer_x: f32,
    start_pointer_y: f32,
    start_x: i32,
    start_y: i32,
    preview_scale: f32,
}

pub struct DeviceControlCenter {
    controls: ControlChannels,
    snapshot: ControlSnapshot,
    page: DeviceControlCenterPage,
    selected_ssid: Option<Vec<u8>>,
    modal: Option<Modal>,
    details: Option<NetworkDetails>,
    bluetooth_pairing: Option<BluetoothPairingDialog>,
    bluetooth_operations: std::collections::HashMap<String, bool>,
    bluetooth_message: Option<String>,
    active_input: Option<InputField>,
    activate_requested: bool,
    input_focus: FocusHandle,
    display_layout: Option<DisplayLayout>,
    display_original: Option<DisplayLayout>,
    display_wallpapers: BTreeMap<String, PathBuf>,
    display_selected: Option<String>,
    display_drag: Option<MonitorDrag>,
    display_message: Option<String>,
    display_applying: bool,
    theme: BarTheme,
}

impl DeviceControlCenter {
    pub fn new_popover(
        controls: ControlChannels,
        snapshot: ControlSnapshot,
        route: DeviceControlCenterRoute,
        theme: BarTheme,
        cx: &mut Context<Self>,
    ) -> Self {
        let _ = controls.actions.try_send(ControlAction::SetWifiDiscovery(
            route.page == DeviceControlCenterPage::Network,
        ));
        let _ = controls
            .actions
            .try_send(ControlAction::SetBluetoothDiscovery(
                route.page == DeviceControlCenterPage::Bluetooth,
            ));
        let display_layout = crate::hyprland::display::load_layout().ok();
        let display_selected = display_layout.as_ref().map(|layout| layout.main.clone());
        let display_wallpapers = Config::load().unwrap_or_default().wallpapers;
        Self {
            controls,
            snapshot,
            page: route.page,
            selected_ssid: route.ssid,
            modal: None,
            details: None,
            bluetooth_pairing: None,
            bluetooth_operations: std::collections::HashMap::new(),
            bluetooth_message: None,
            active_input: None,
            activate_requested: false,
            input_focus: cx.focus_handle(),
            display_original: display_layout.clone(),
            display_layout,
            display_wallpapers,
            display_selected,
            display_drag: None,
            display_message: None,
            display_applying: false,
            theme,
        }
    }

    pub fn set_snapshot(&mut self, snapshot: ControlSnapshot, cx: &mut Context<Self>) {
        self.snapshot = snapshot;
        cx.notify();
    }

    pub fn apply_wifi_update(&mut self, event: WifiConnectionEvent, cx: &mut Context<Self>) {
        self.apply_connection_event(event);
        cx.notify();
    }

    pub fn apply_bluetooth_update(&mut self, event: BluetoothEvent, cx: &mut Context<Self>) {
        self.apply_bluetooth_event(event);
        cx.notify();
    }

    pub fn apply_network_settings_update(
        &mut self,
        event: NetworkSettingsEvent,
        cx: &mut Context<Self>,
    ) {
        self.apply_network_settings_event(event);
        cx.notify();
    }

    fn set_page(&mut self, page: DeviceControlCenterPage) {
        if self.page == page {
            return;
        }
        self.page = page;
        self.modal = None;
        self.details = None;
        self.bluetooth_pairing = None;
        self.active_input = None;
        let _ = self
            .controls
            .actions
            .try_send(ControlAction::SetWifiDiscovery(
                page == DeviceControlCenterPage::Network,
            ));
        let _ = self
            .controls
            .actions
            .try_send(ControlAction::SetBluetoothDiscovery(
                page == DeviceControlCenterPage::Bluetooth,
            ));
        if page == DeviceControlCenterPage::Display {
            self.refresh_display_state();
        }
    }

    fn refresh_display_state(&mut self) {
        match crate::hyprland::display::load_layout() {
            Ok(layout) => {
                self.display_selected = Some(layout.main.clone());
                self.display_original = Some(layout.clone());
                self.display_layout = Some(layout);
                self.display_wallpapers = Config::load().unwrap_or_default().wallpapers;
                self.display_message = None;
            }
            Err(error) => {
                self.display_layout = None;
                self.display_message = Some(format!("モニター情報を取得できません: {error}"));
            }
        }
    }

    fn begin_monitor_drag(
        &mut self,
        name: String,
        preview_scale: f32,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(monitor) = self
            .display_layout
            .as_ref()
            .and_then(|layout| layout.monitor(&name))
        else {
            return;
        };
        self.display_selected = Some(name.clone());
        self.display_drag = Some(MonitorDrag {
            name,
            start_pointer_x: event.position.x.into(),
            start_pointer_y: event.position.y.into(),
            start_x: monitor.x,
            start_y: monitor.y,
            preview_scale,
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn move_monitor_drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.display_drag.clone() else {
            return;
        };
        let x = drag.start_x
            + ((f32::from(event.position.x) - drag.start_pointer_x) / drag.preview_scale).round()
                as i32;
        let y = drag.start_y
            + ((f32::from(event.position.y) - drag.start_pointer_y) / drag.preview_scale).round()
                as i32;
        if let Some(layout) = self.display_layout.as_mut() {
            layout.move_monitor(&drag.name, x, y);
            cx.notify();
        }
    }

    fn end_monitor_drag(&mut self, cx: &mut Context<Self>) {
        if self.display_drag.take().is_some() {
            cx.notify();
        }
    }

    fn set_main_monitor(&mut self, name: String, cx: &mut Context<Self>) {
        if let Some(layout) = self.display_layout.as_mut()
            && layout.normalize_main(&name)
        {
            self.display_selected = Some(name);
            cx.notify();
        }
    }

    fn choose_wallpaper(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(output) = self.display_selected.clone() else {
            return;
        };
        let wallpapers = self.display_wallpapers.clone();
        let result = thread::Builder::new()
            .name("bah-wallpaper-picker".to_string())
            .spawn(move || {
                // The DCC is an input-grabbing popup. Unmap it before the
                // native chooser asks Wayland for focus.
                thread::sleep(Duration::from_millis(50));
                match select_wallpaper_with_zenity() {
                    Ok(Some(path)) if path.is_file() && is_wallpaper_file(&path) => {
                        match Config::load() {
                            Ok(mut config) => {
                                let mut wallpapers = wallpapers;
                                wallpapers.insert(output, path);
                                config.wallpapers = wallpapers;
                                if let Err(error) = config.save() {
                                    error!("could not save selected wallpaper: {error}");
                                }
                            }
                            Err(error) => {
                                error!("could not load configuration for wallpaper: {error}")
                            }
                        }
                    }
                    Ok(Some(path)) => error!(
                        "selected file is not a supported wallpaper: {}",
                        path.display()
                    ),
                    Ok(None) => {}
                    Err(error) => error!("Wayland file browser failed: {error}"),
                }
                crate::app::request_device_control_center(DeviceControlCenterRoute {
                    page: DeviceControlCenterPage::Display,
                    ssid: None,
                });
            });
        if let Err(error) = result {
            self.display_message = Some(format!("壁紙選択を開始できませんでした: {error}"));
            cx.notify();
            return;
        }
        window.remove_window();
    }

    fn clear_wallpaper(&mut self, cx: &mut Context<Self>) {
        if let Some(name) = &self.display_selected {
            self.display_wallpapers.remove(name);
            cx.notify();
        }
    }

    fn apply_display_changes(&mut self, cx: &mut Context<Self>) {
        if self.display_applying {
            return;
        }
        let Some(layout) = self.display_layout.clone() else {
            return;
        };
        if layout.overlaps() {
            self.display_message =
                Some("モニターが重なっています。重ならないように配置してください。".into());
            cx.notify();
            return;
        }
        self.display_applying = true;
        self.display_message = None;
        let original_config = Config::load().unwrap_or_default();
        let mut updated_config = original_config.clone();
        updated_config.wallpapers = self.display_wallpapers.clone();
        let result = updated_config
            .save()
            .and_then(|()| crate::hyprland::display::apply_layout(&layout));
        match result {
            Ok(()) => {
                if let Err(error) = crate::app::restart_wallpaper_process() {
                    self.display_message = Some(format!(
                        "配置は保存しましたが、壁紙を更新できません: {error}"
                    ));
                } else {
                    self.display_message = Some("ディスプレイ設定を適用しました。".into());
                }
                self.display_original = Some(layout);
            }
            Err(error) => {
                let _ = original_config.save();
                self.display_message = Some(format!("適用できませんでした: {error}"));
            }
        }
        self.display_applying = false;
        cx.notify();
    }
    fn select_network(&mut self, network: WifiNetwork, cx: &mut Context<Self>) {
        self.selected_ssid = Some(network.ssid.clone());
        self.details = None;
        self.active_input = None;
        if network.security == WifiSecurity::Unsupported && !network.saved {
            self.modal = Some(Modal::Password {
                network,
                password: String::new(),
                message: Some("このネットワークの認証方式には対応していません".to_string()),
                connecting: false,
            });
        } else if network.security == WifiSecurity::Personal && !network.saved {
            self.modal = Some(Modal::Password {
                network,
                password: String::new(),
                message: None,
                connecting: false,
            });
        } else {
            self.request_connection(network, None);
        }
        cx.notify();
    }

    fn request_connection(&mut self, network: WifiNetwork, password: Option<String>) {
        if let Some(Modal::Password {
            message,
            connecting,
            ..
        }) = self.modal.as_mut()
        {
            *message = None;
            *connecting = true;
        }
        if self
            .controls
            .actions
            .try_send(ControlAction::ConnectWifi {
                ssid: network.ssid,
                password,
            })
            .is_err()
            && let Some(Modal::Password {
                message,
                connecting,
                ..
            }) = self.modal.as_mut()
        {
            *connecting = false;
            *message = Some("接続要求を送信できませんでした".to_string());
        }
    }

    fn connect_password_network(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Password {
            network,
            password,
            connecting,
            message,
        }) = self.modal.as_ref()
        else {
            return;
        };
        if *connecting || network.security == WifiSecurity::Unsupported {
            return;
        }
        if password.is_empty() {
            if let Some(Modal::Password { message, .. }) = self.modal.as_mut() {
                *message = Some("パスワードを入力してください".to_string());
            }
        } else {
            let _ = message;
            self.request_connection(network.clone(), Some(password.clone()));
        }
        cx.notify();
    }

    fn apply_connection_event(&mut self, event: WifiConnectionEvent) {
        let Some(Modal::Password { network, .. }) = self.modal.as_ref() else {
            return;
        };
        let selected_ssid = network.ssid.clone();
        match event {
            WifiConnectionEvent::Connecting { ssid } if ssid == selected_ssid => {
                if let Some(Modal::Password {
                    connecting,
                    message,
                    ..
                }) = self.modal.as_mut()
                {
                    *connecting = true;
                    *message = None;
                }
            }
            WifiConnectionEvent::Succeeded { ssid } if ssid == selected_ssid => {
                self.modal = None;
                self.active_input = None;
            }
            WifiConnectionEvent::Failed { ssid, message } if ssid == selected_ssid => {
                if let Some(Modal::Password {
                    connecting,
                    message: modal_message,
                    ..
                }) = self.modal.as_mut()
                {
                    *connecting = false;
                    *modal_message = Some(message);
                }
            }
            _ => {}
        }
    }

    fn apply_network_settings_event(&mut self, event: NetworkSettingsEvent) {
        let Some(details) = self.details.as_mut() else {
            return;
        };
        let Some(uuid) = details.network.connection_uuid.as_deref() else {
            return;
        };
        match event {
            NetworkSettingsEvent::Applied { connection_uuid } if connection_uuid == uuid => {
                details.saving = false;
                details.message = Some("設定を保存し、接続を再確立しました".to_string());
            }
            NetworkSettingsEvent::Failed {
                connection_uuid,
                message,
            } if connection_uuid == uuid => {
                details.saving = false;
                details.message = Some(message);
            }
            _ => {}
        }
    }

    fn toggle_wifi(&mut self, cx: &mut Context<Self>) {
        if self.snapshot.wifi.available {
            let _ = self.controls.actions.try_send(ControlAction::ToggleWifi);
            cx.notify();
        }
    }

    fn toggle_bluetooth(&mut self, cx: &mut Context<Self>) {
        if self.snapshot.bluetooth.available {
            let _ = self
                .controls
                .actions
                .try_send(ControlAction::ToggleBluetooth);
            cx.notify();
        }
    }

    fn set_airpods_mode(&mut self, mode: AirPodsListeningMode, cx: &mut Context<Self>) {
        if self.snapshot.airpods.ready {
            self.snapshot.airpods.listening_mode = Some(mode);
            self.snapshot.airpods.message = None;
            if self
                .controls
                .actions
                .try_send(ControlAction::SetAirPodsListeningMode(mode))
                .is_err()
            {
                self.snapshot.airpods.message =
                    Some("AirPodsへの操作要求を送信できませんでした".to_string());
            }
        }
        cx.notify();
    }

    fn select_bluetooth_device(&mut self, device: BluetoothDevice, cx: &mut Context<Self>) {
        if self.bluetooth_operations.contains_key(&device.path) {
            return;
        }
        let action = if device.connected {
            ControlAction::DisconnectBluetooth {
                device_path: device.path,
            }
        } else if device.paired {
            ControlAction::ConnectBluetooth {
                device_path: device.path,
            }
        } else {
            ControlAction::PairBluetooth {
                device_path: device.path,
            }
        };
        if self.controls.actions.try_send(action).is_err() {
            self.bluetooth_message = Some("Bluetoothの操作要求を送信できませんでした".to_string());
        }
        cx.notify();
    }

    fn apply_bluetooth_event(&mut self, event: BluetoothEvent) {
        match event {
            BluetoothEvent::OperationStarted {
                device_path,
                pairing,
            } => {
                self.bluetooth_operations.insert(device_path, pairing);
                self.bluetooth_message = None;
            }
            BluetoothEvent::PairingPrompt {
                device_path,
                prompt,
            } => {
                let device_label = self
                    .snapshot
                    .bluetooth_devices
                    .iter()
                    .find(|device| device.path == device_path)
                    .map(|device| device.label.clone())
                    .unwrap_or_else(|| "Bluetoothデバイス".to_string());
                self.bluetooth_pairing = Some(BluetoothPairingDialog {
                    device_path,
                    device_label,
                    prompt,
                    pin_code: String::new(),
                });
                self.active_input = None;
            }
            BluetoothEvent::Succeeded { device_path } => {
                self.bluetooth_operations.remove(&device_path);
                self.bluetooth_pairing = None;
                self.active_input = None;
            }
            BluetoothEvent::Failed {
                device_path,
                message,
            } => {
                self.bluetooth_operations.remove(&device_path);
                self.bluetooth_pairing = None;
                self.active_input = None;
                self.bluetooth_message = Some(message);
            }
        }
    }

    fn respond_bluetooth_pairing(&mut self, accepted: bool, cx: &mut Context<Self>) {
        let Some(dialog) = self.bluetooth_pairing.take() else {
            return;
        };
        let response = match dialog.prompt {
            BluetoothPairingPrompt::PinCode | BluetoothPairingPrompt::Passkey if accepted => {
                if dialog.pin_code.trim().is_empty() {
                    self.bluetooth_pairing = Some(dialog);
                    self.bluetooth_message =
                        Some("PINまたはパスキーを入力してください".to_string());
                    cx.notify();
                    return;
                }
                BluetoothPairingResponse::PinCode {
                    device_path: dialog.device_path,
                    pin_code: dialog.pin_code,
                }
            }
            BluetoothPairingPrompt::DisplayPinCode { .. }
            | BluetoothPairingPrompt::DisplayPasskey { .. }
                if accepted =>
            {
                self.active_input = None;
                cx.notify();
                return;
            }
            _ if accepted => BluetoothPairingResponse::Confirm {
                device_path: dialog.device_path,
                accepted: true,
            },
            _ => BluetoothPairingResponse::Cancel {
                device_path: dialog.device_path,
            },
        };
        if self
            .controls
            .bluetooth_pairing_responses
            .try_send(response)
            .is_err()
        {
            self.bluetooth_message = Some("ペアリング応答を送信できませんでした".to_string());
        }
        self.active_input = None;
        cx.notify();
    }

    fn open_details(&mut self, network: ActiveNetwork, cx: &mut Context<Self>) {
        self.details = Some(NetworkDetails {
            settings: network.settings.clone(),
            network,
            message: None,
            saving: false,
        });
        self.active_input = None;
        cx.notify();
    }

    fn close_details(&mut self, cx: &mut Context<Self>) {
        self.details = None;
        self.active_input = None;
        cx.notify();
    }

    fn focus_input(&mut self, field: InputField, window: &mut Window, cx: &mut Context<Self>) {
        self.active_input = Some(field);
        window.focus(&self.input_focus, cx);
        cx.notify();
    }

    fn input_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_focus.is_focused(window) || event.keystroke.modifiers.modified() {
            return;
        }
        if event.keystroke.key == "escape" {
            if self.modal.is_some() {
                self.close_modal(cx);
            } else if self.bluetooth_pairing.is_some() {
                self.respond_bluetooth_pairing(false, cx);
            }
            return;
        }
        if event.keystroke.key == "enter" && self.active_input == Some(InputField::Password) {
            self.connect_password_network(cx);
            return;
        }
        if event.keystroke.key == "enter" && self.active_input == Some(InputField::BluetoothPin) {
            self.respond_bluetooth_pairing(true, cx);
            return;
        }
        let Some(field) = self.active_input else {
            return;
        };
        let value = self.input_value_mut(field);
        let Some(value) = value else {
            return;
        };
        match event.keystroke.key.as_str() {
            "backspace" => {
                value.pop();
                cx.notify();
            }
            key if key.chars().count() == 1 => {
                value.push_str(key);
                cx.notify();
            }
            _ => {}
        }
    }

    fn input_value_mut(&mut self, field: InputField) -> Option<&mut String> {
        if field == InputField::Password {
            return match self.modal.as_mut() {
                Some(Modal::Password { password, .. }) => Some(password),
                _ => None,
            };
        }
        if field == InputField::BluetoothPin {
            return self
                .bluetooth_pairing
                .as_mut()
                .map(|dialog| &mut dialog.pin_code);
        }
        let details = self.details.as_mut()?;
        let (settings, is_ipv6) = match field {
            InputField::Ipv4Address
            | InputField::Ipv4Subnet
            | InputField::Ipv4Gateway
            | InputField::Ipv4PrimaryDns
            | InputField::Ipv4SecondaryDns => (&mut details.settings.ipv4, false),
            _ => (&mut details.settings.ipv6, true),
        };
        if settings.automatic {
            return None;
        }
        Some(match (field, is_ipv6) {
            (InputField::Ipv4Address | InputField::Ipv6Address, _) => &mut settings.address,
            (InputField::Ipv4Subnet | InputField::Ipv6Prefix, _) => &mut settings.subnet_or_prefix,
            (InputField::Ipv4Gateway | InputField::Ipv6Gateway, _) => &mut settings.gateway,
            (InputField::Ipv4PrimaryDns | InputField::Ipv6PrimaryDns, _) => {
                &mut settings.primary_dns
            }
            _ => &mut settings.secondary_dns,
        })
    }

    fn set_manual(&mut self, ipv6: bool, manual: bool, cx: &mut Context<Self>) {
        let Some(details) = self.details.as_mut() else {
            return;
        };
        let settings = if ipv6 {
            &mut details.settings.ipv6
        } else {
            &mut details.settings.ipv4
        };
        settings.automatic = !manual;
        details.message = None;
        cx.notify();
    }

    fn request_save_network_settings(&mut self, cx: &mut Context<Self>) {
        let Some(details) = self.details.as_ref() else {
            return;
        };
        if details.saving {
            return;
        }
        if details.network.connection_uuid.is_none() {
            if let Some(details) = self.details.as_mut() {
                details.message = Some("接続プロファイルが見つかりません".to_string());
            }
        } else {
            self.modal = Some(Modal::ConfirmNetworkSettings);
            self.active_input = None;
        }
        cx.notify();
    }

    fn save_network_settings(&mut self, cx: &mut Context<Self>) {
        let Some(details) = self.details.as_mut() else {
            return;
        };
        let Some(connection_uuid) = details.network.connection_uuid.clone() else {
            return;
        };
        details.saving = true;
        details.message = None;
        self.modal = None;
        if self
            .controls
            .actions
            .try_send(ControlAction::UpdateNetworkSettings {
                connection_uuid,
                settings: details.settings.clone(),
            })
            .is_err()
        {
            details.saving = false;
            details.message = Some("設定の保存要求を送信できませんでした".to_string());
        }
        cx.notify();
    }

    fn close_modal(&mut self, cx: &mut Context<Self>) {
        self.modal = None;
        self.active_input = None;
        cx.notify();
    }

    fn input_value(&self, field: InputField) -> String {
        if field == InputField::Password {
            return match self.modal.as_ref() {
                Some(Modal::Password { password, .. }) => "•".repeat(password.chars().count()),
                _ => String::new(),
            };
        }
        if field == InputField::BluetoothPin {
            return self
                .bluetooth_pairing
                .as_ref()
                .map(|dialog| dialog.pin_code.clone())
                .unwrap_or_default();
        }
        let Some(details) = self.details.as_ref() else {
            return String::new();
        };
        let settings = match field {
            InputField::Ipv4Address
            | InputField::Ipv4Subnet
            | InputField::Ipv4Gateway
            | InputField::Ipv4PrimaryDns
            | InputField::Ipv4SecondaryDns => &details.settings.ipv4,
            _ => &details.settings.ipv6,
        };
        match field {
            InputField::Ipv4Address | InputField::Ipv6Address => settings.address.clone(),
            InputField::Ipv4Subnet | InputField::Ipv6Prefix => settings.subnet_or_prefix.clone(),
            InputField::Ipv4Gateway | InputField::Ipv6Gateway => settings.gateway.clone(),
            InputField::Ipv4PrimaryDns | InputField::Ipv6PrimaryDns => settings.primary_dns.clone(),
            _ => settings.secondary_dns.clone(),
        }
    }

    fn input_row(
        &self,
        label: &'static str,
        field: InputField,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let focused = self.active_input == Some(field);
        let value = self.input_value(field);
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
            .child(
                div()
                    .id(("dcc-network-input", field.element_id()))
                    .h(px(34.0))
                    .px(px(9.0))
                    .rounded(theme.control_radius)
                    .border_1()
                    .border_color(if focused { theme.focus } else { theme.border })
                    .opacity(if disabled { 0.45 } else { 1.0 })
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            if !disabled {
                                this.focus_input(field, window, cx);
                            }
                        }),
                    )
                    .child(value),
            )
    }

    fn ip_settings_panel(
        &self,
        title: &'static str,
        settings: &IpSettings,
        ipv6: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let automatic = settings.automatic;
        let manual = !automatic;
        div()
            .p(px(12.0))
            .rounded(theme.panel_radius)
            .bg(theme.container_background)
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(16.0)).child(title))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme.muted_foreground)
                                    .child("手動設定"),
                            )
                            .child(
                                div()
                                    .id(("dcc-ip-manual-toggle", if ipv6 { 6_u32 } else { 4_u32 }))
                                    .h(px(24.0))
                                    .w(px(48.0))
                                    .p(px(3.0))
                                    .rounded(px(12.0))
                                    .cursor_pointer()
                                    .bg(if manual {
                                        theme.active_background
                                    } else {
                                        theme.border
                                    })
                                    .hover(|style| style.bg(theme.hover_background))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_manual(ipv6, !manual, cx)
                                    }))
                                    .child(
                                        div()
                                            .size(px(18.0))
                                            .rounded_full()
                                            .bg(theme.foreground)
                                            .when(manual, |knob| knob.ml_auto()),
                                    ),
                            ),
                    ),
            )
            .child(self.input_row(
                "アドレス",
                if ipv6 {
                    InputField::Ipv6Address
                } else {
                    InputField::Ipv4Address
                },
                automatic,
                cx,
            ))
            .child(self.input_row(
                if ipv6 {
                    "サブネットプレフィックスの長さ"
                } else {
                    "サブネットマスク"
                },
                if ipv6 {
                    InputField::Ipv6Prefix
                } else {
                    InputField::Ipv4Subnet
                },
                automatic,
                cx,
            ))
            .child(self.input_row(
                "ゲートウェイ",
                if ipv6 {
                    InputField::Ipv6Gateway
                } else {
                    InputField::Ipv4Gateway
                },
                automatic,
                cx,
            ))
            .child(self.input_row(
                "優先DNS",
                if ipv6 {
                    InputField::Ipv6PrimaryDns
                } else {
                    InputField::Ipv4PrimaryDns
                },
                automatic,
                cx,
            ))
            .child(self.input_row(
                "代替DNS",
                if ipv6 {
                    InputField::Ipv6SecondaryDns
                } else {
                    InputField::Ipv4SecondaryDns
                },
                automatic,
                cx,
            ))
    }
}

impl Render for DeviceControlCenter {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.activate_requested {
            self.activate_requested = false;
            window.activate_window();
        }
        let theme = self.theme;
        let muted = theme.muted_foreground;
        let panel = theme.container_background;
        let page = self.page;
        let wifi_enabled = self.snapshot.wifi.enabled;
        let networks = self.snapshot.wifi_networks.clone();
        let active_networks = self.snapshot.active_networks.clone();
        let selected = self.selected_ssid.clone();
        let details = self.details.clone();
        let modal = self.modal.clone();

        let saved_networks: Vec<WifiNetwork> = networks
            .iter()
            .filter(|network| network.saved && !network.connected)
            .cloned()
            .collect();
        let other_networks: Vec<WifiNetwork> = networks
            .iter()
            .filter(|network| !network.saved && !network.connected)
            .cloned()
            .collect();

        div()
            .size_full()
            .relative()
            .bg(theme.surface(SurfaceRole::Dialog))
            .text_color(theme.foreground)
            .font(ui_font())
            .child(
                div()
                    .size_full()
                    .flex()
                    .child(
                        div()
                            .w(px(210.0))
                            .p(px(18.0))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .border_r_1()
                            .border_color(theme.border)
                            .gap(px(6.0))
                            .child(
                                div()
                                    .id("dcc-page-network")
                                    .h(px(36.0))
                                    .px(px(10.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .bg(if page == DeviceControlCenterPage::Network {
                                        theme.active_background
                                    } else {
                                        theme.container_background
                                    })
                                    .hover(|style| style.bg(theme.hover_background))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_page(DeviceControlCenterPage::Network);
                                        cx.notify();
                                    }))
                                    .child("ネットワーク"),
                            )
                            .child(
                                div()
                                    .id("dcc-page-bluetooth")
                                    .h(px(36.0))
                                    .px(px(10.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .bg(if page == DeviceControlCenterPage::Bluetooth {
                                        theme.active_background
                                    } else {
                                        theme.container_background
                                    })
                                    .hover(|style| style.bg(theme.hover_background))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_page(DeviceControlCenterPage::Bluetooth);
                                        cx.notify();
                                    }))
                                    .child("Bluetooth"),
                            )
                            .child(
                                div()
                                    .id("dcc-page-display")
                                    .h(px(36.0))
                                    .px(px(10.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .bg(if page == DeviceControlCenterPage::Display {
                                        theme.active_background
                                    } else {
                                        theme.container_background
                                    })
                                    .hover(|style| style.bg(theme.hover_background))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_page(DeviceControlCenterPage::Display);
                                        cx.notify();
                                    }))
                                    .child("ディスプレイ"),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .min_h(px(0.0))
                            .relative()
                            .p(px(28.0))
                            .flex()
                            .flex_col()
                            .gap(px(16.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(10.0))
                                            .child(div().text_size(px(24.0)).child("ネットワーク")),
                                    )
                                    .child(
                                        div()
                                            .id("dcc-wifi-toggle")
                                            .h(px(28.0))
                                            .w(px(58.0))
                                            .p(px(3.0))
                                            .rounded(px(14.0))
                                            .cursor_pointer()
                                            .bg(if wifi_enabled { theme.active_background } else { theme.border })
                                            .hover(|style| style.bg(theme.hover_background))
                                            .on_click(cx.listener(|this, _, _, cx| this.toggle_wifi(cx)))
                                            .child(
                                                div()
                                                    .size(px(22.0))
                                                    .rounded_full()
                                                    .bg(theme.foreground)
                                                    .when(wifi_enabled, |knob| knob.ml_auto()),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .gap(px(18.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .flex()
                                            .flex_col()
                                            .gap(px(12.0))
                                            .child(
                                                div()
                                                    .max_h(px(260.0))
                                                    .p(px(12.0))
                                                    .rounded(px(7.0))
                                                    .bg(panel)
                                                    .flex()
                                                    .flex_col()
                                                    .gap(px(8.0))
                                                    .child(div().text_color(muted).child("接続したことのあるネットワーク"))
                                                    .child(
                                                div()
                                                    .id("dcc-saved-wifi-list")
                                                    .flex_1()
                                                    .min_h(px(0.0))
                                                    .overflow_y_scroll()
                                                    .flex()
                                                    .flex_col()
                                                    .gap(px(3.0))
                                                    .children(active_networks.into_iter().enumerate().map(|(index, network)| {
                                                        let network_for_details = network.clone();
                                                        let route_badge = default_route_badge(&network);
                                                        div()
                                                            .id(("dcc-active-network", index as u32))
                                                            .h(px(40.0))
                                                            .px(px(10.0))
                                                            .rounded(px(5.0))
                                                            .flex()
                                                            .items_center()
                                                            .gap(px(8.0))
                                                            .bg(theme.active_background)
                                                            .child(div().flex_1().child(network.label))
                                                            .when_some(route_badge, |row, badge| {
                                                                row.child(
                                                                    div()
                                                                        .text_color(theme.success)
                                                                        .text_size(px(12.0))
                                                                        .child(badge),
                                                                )
                                                            })
                                                            .child(
                                                                div()
                                                                    .id(("dcc-network-details", index as u32))
                                                                    .w(px(26.0))
                                                                    .h(px(26.0))
                                                                    .rounded_full()
                                                                    .flex()
                                                                    .items_center()
                                                                    .justify_center()
                                                                    .cursor_pointer()
                                                                    .hover(|style| style.bg(theme.hover_background))
                                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                                        this.open_details(network_for_details.clone(), cx)
                                                                    }))
                                                                    .child("i"),
                                                            )
                                                    }))
                                                    .children(saved_networks.into_iter().enumerate().map(|(index, network)| {
                                                        let is_selected = selected.as_deref() == Some(network.ssid.as_slice());
                                                        let network_for_click = network.clone();
                                                        div()
                                                            .id(("dcc-saved-wifi", index as u32))
                                                            .h(px(40.0))
                                                            .px(px(10.0))
                                                            .rounded(px(5.0))
                                                            .flex()
                                                            .items_center()
                                                            .cursor_pointer()
                                                            .bg(if is_selected { theme.active_background } else { panel })
                                                            .hover(|style| style.bg(theme.hover_background))
                                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                                this.select_network(network_for_click.clone(), cx)
                                                            }))
                                                            .child(div().flex_1().child(network.label))
                                                    })),
                                                    )
                                            )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h(px(0.0))
                                            .p(px(12.0))
                                            .rounded(px(7.0))
                                            .bg(panel)
                                            .flex()
                                            .flex_col()
                                            .gap(px(8.0))
                                            .child(div().text_color(muted).child("ほかのネットワーク"))
                                            .when(!wifi_enabled, |list| list.child(div().text_color(muted).child("Wi-Fiはオフです")))
                                            .when(wifi_enabled && networks.is_empty(), |list| list.child(div().text_color(muted).child("ネットワークを検索中…")))
                                            .child(
                                                div()
                                                    .id("dcc-other-wifi-list")
                                                    .flex_1()
                                                    .min_h(px(0.0))
                                                    .overflow_y_scroll()
                                                    .flex()
                                                    .flex_col()
                                                    .gap(px(3.0))
                                                    .children(other_networks.into_iter().enumerate().map(|(index, network)| {
                                                        let is_selected = selected.as_deref() == Some(network.ssid.as_slice());
                                                        let network_for_click = network.clone();
                                                        div()
                                                            .id(("dcc-other-wifi", index as u32))
                                                            .h(px(40.0))
                                                            .px(px(10.0))
                                                            .rounded(px(5.0))
                                                            .flex()
                                                            .items_center()
                                                            .cursor_pointer()
                                                            .bg(if is_selected { theme.active_background } else { panel })
                                                            .hover(|style| style.bg(theme.hover_background))
                                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                                this.select_network(network_for_click.clone(), cx)
                                                            }))
                                                            .child(div().flex_1().child(network.label))
                                                    })),
                                            )
                                    )
                                    ),
                    ),
            )
            )
            .when(page == DeviceControlCenterPage::Bluetooth, |root| {
                root.child(
                    div()
                        .id("dcc-bluetooth-page")
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left(px(210.0))
                        .child(self.render_bluetooth_page(cx)),
                )
            })
            .when(page == DeviceControlCenterPage::Display, |root| {
                root.child(
                    div()
                        .id("dcc-display-page")
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left(px(210.0))
                        .child(self.render_display_page(cx)),
                )
            })
            .when_some(details, |root, details| {
                root.child(self.render_network_details_modal(details, cx))
            })
            .when_some(modal, |root, modal| {
                root.child(self.render_modal(modal, window, cx))
            })
            .when_some(self.bluetooth_pairing.clone(), |root, dialog| {
                root.child(self.render_bluetooth_pairing_modal(dialog, window, cx))
            })
            .into_any_element()
    }
}

impl DeviceControlCenter {
    fn render_display_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let panel = theme.container_background;
        let muted = theme.muted_foreground;
        let selected_name = self.display_selected.clone();
        let Some(layout) = self.display_layout.clone() else {
            return div()
                .size_full()
                .p(px(28.0))
                .bg(theme.dialog_background)
                .text_color(theme.foreground)
                .font(ui_font())
                .child(div().text_size(px(24.0)).child("ディスプレイ"))
                .child(div().mt(px(16.0)).text_color(theme.error).child(
                    self.display_message.clone().unwrap_or_else(|| {
                        "モニター情報を取得できません。Hyprland上で実行してください。".into()
                    }),
                ));
        };

        let min_x = layout
            .monitors
            .iter()
            .map(|monitor| monitor.x)
            .min()
            .unwrap_or(0);
        let min_y = layout
            .monitors
            .iter()
            .map(|monitor| monitor.y)
            .min()
            .unwrap_or(0);
        let max_x = layout
            .monitors
            .iter()
            .map(|monitor| monitor.x + monitor.logical_size().0)
            .max()
            .unwrap_or(1);
        let max_y = layout
            .monitors
            .iter()
            .map(|monitor| monitor.y + monitor.logical_size().1)
            .max()
            .unwrap_or(1);
        let preview_scale = (520.0 / (max_x - min_x).max(1) as f32)
            .min(250.0 / (max_y - min_y).max(1) as f32)
            .min(0.22)
            .max(0.035);
        let monitors = layout.monitors.clone();
        let canvas = div()
            .id("dcc-display-layout")
            .relative()
            .h(px(282.0))
            .w_full()
            .rounded(theme.panel_radius)
            .bg(theme.window_background)
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.move_monitor_drag(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_monitor_drag(cx)),
            )
            .children(monitors.iter().enumerate().map(|(index, monitor)| {
                let (width, height) = monitor.logical_size();
                let left = 16.0 + (monitor.x - min_x) as f32 * preview_scale;
                let top = 16.0 + (monitor.y - min_y) as f32 * preview_scale;
                let panel_width = (width as f32 * preview_scale).max(52.0);
                let panel_height = (height as f32 * preview_scale).max(38.0);
                let name = monitor.name.clone();
                let selected = selected_name.as_deref() == Some(name.as_str());
                let main = layout.main == name;
                div()
                    .id(("dcc-display-monitor", index as u32))
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(panel_width))
                    .h(px(panel_height))
                    .p(px(6.0))
                    .rounded(theme.control_radius)
                    .cursor_pointer()
                    .bg(if selected {
                        theme.active_background
                    } else {
                        theme.container_background
                    })
                    .border_1()
                    .border_color(if main {
                        theme.success
                    } else {
                        theme.strong_border
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event, _, cx| {
                            this.begin_monitor_drag(name.clone(), preview_scale, event, cx);
                        }),
                    )
                    .child(div().text_size(px(12.0)).text_center().child(if main {
                        format!("{} ★", monitor.name)
                    } else {
                        monitor.name.clone()
                    }))
                    .child(
                        div()
                            .mt(px(2.0))
                            .text_size(px(10.0))
                            .text_color(muted)
                            .text_center()
                            .child(format!("{} × {}", monitor.x, monitor.y)),
                    )
            }));

        let selected_monitor = selected_name
            .as_deref()
            .and_then(|name| layout.monitor(name))
            .cloned();
        let selected_is_main = selected_monitor
            .as_ref()
            .is_some_and(|monitor| monitor.name == layout.main);
        let wallpaper_label = selected_name.as_ref().and_then(|name| {
            self.display_wallpapers.get(name).map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            })
        });
        let overlap_message = layout
            .overlaps()
            .then_some("モニターが重なっています。適用前に分離してください。");

        div()
            .size_full()
            .p(px(28.0))
            .bg(theme.dialog_background)
            .text_color(theme.foreground)
            .font(ui_font())
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(24.0)).child("ディスプレイ"))
                    .child(
                        div()
                            .id("dcc-display-refresh")
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(theme.control_radius)
                            .cursor_pointer()
                            .bg(panel)
                            .hover(|style| style.bg(theme.hover_background))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh_display_state();
                                cx.notify();
                            }))
                            .child("再読み込み"),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(muted)
                    .child("モニターをドラッグして配置を変更します。近い辺には自動で吸着します。"),
            )
            .child(canvas)
            .when_some(selected_monitor, |page, monitor| {
                let monitor_name = monitor.name.clone();
                page.child(
                    div()
                        .p(px(14.0))
                        .rounded(theme.panel_radius)
                        .bg(panel)
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(div().child(format!(
                                    "{}  {}×{}",
                                    monitor.name, monitor.width, monitor.height
                                )))
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(muted)
                                        .child(format!("位置: {} × {}", monitor.x, monitor.y)),
                                ),
                        )
                        .child(
                            div()
                                .id("dcc-display-main")
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_main_monitor(monitor_name.clone(), cx);
                                }))
                                .child(
                                    div()
                                        .size(px(16.0))
                                        .rounded(px(3.0))
                                        .border_1()
                                        .border_color(if selected_is_main {
                                            theme.success
                                        } else {
                                            theme.strong_border
                                        })
                                        .bg(if selected_is_main {
                                            theme.active_background
                                        } else {
                                            theme.window_background
                                        })
                                        .text_center()
                                        .text_size(px(12.0))
                                        .child(if selected_is_main { "✓" } else { "" }),
                                )
                                .child("メインモニターにする"),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(div().text_color(muted).child("壁紙:"))
                                .child(
                                    div().flex_1().min_w(px(0.0)).text_size(px(12.0)).child(
                                        wallpaper_label.unwrap_or_else(|| {
                                            "個別設定なし（共通壁紙を使用）".into()
                                        }),
                                    ),
                                )
                                .child(
                                    div()
                                        .id("dcc-display-wallpaper-choose")
                                        .px(px(9.0))
                                        .py(px(5.0))
                                        .rounded(px(4.0))
                                        .cursor_pointer()
                                        .bg(theme.container_background)
                                        .hover(|style| style.bg(theme.hover_background))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.choose_wallpaper(window, cx);
                                        }))
                                        .child("選択"),
                                )
                                .child(
                                    div()
                                        .id("dcc-display-wallpaper-clear")
                                        .px(px(9.0))
                                        .py(px(5.0))
                                        .rounded(px(4.0))
                                        .cursor_pointer()
                                        .bg(theme.container_background)
                                        .hover(|style| style.bg(theme.hover_background))
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.clear_wallpaper(cx)),
                                        )
                                        .child("解除"),
                                ),
                        ),
                )
            })
            .when_some(overlap_message, |page, message| {
                page.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.error)
                        .child(message),
                )
            })
            .when_some(self.display_message.clone(), |page, message| {
                page.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.muted_foreground)
                        .child(message),
                )
            })
            .child(div().flex_1())
            .child(
                div().flex().justify_end().child(
                    div()
                        .id("dcc-display-apply")
                        .px(px(16.0))
                        .py(px(8.0))
                        .rounded(theme.control_radius)
                        .cursor_pointer()
                        .opacity(if layout.overlaps() || self.display_applying {
                            0.45
                        } else {
                            1.0
                        })
                        .bg(theme.active_background)
                        .hover(|style| style.bg(theme.pressed_background))
                        .on_click(cx.listener(|this, _, _, cx| this.apply_display_changes(cx)))
                        .child(if self.display_applying {
                            "適用中…"
                        } else {
                            "適用"
                        }),
                ),
            )
    }

    fn render_airpods_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let panel = theme.container_background;
        let muted = theme.muted_foreground;
        let selected = self.snapshot.airpods.listening_mode;
        let ready = self.snapshot.airpods.ready;
        let pod = |label: &'static str, path: &'static str, percent: Option<u8>| {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.0))
                .child(
                    img(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path))
                        .size(px(54.0)),
                )
                .child(div().text_size(px(12.0)).text_color(muted).child(format!(
                        "{label}  {}",
                        percent
                            .map(|value| format!("{value}%"))
                            .unwrap_or_else(|| "—%".to_string())
                    )))
                .child(
                    div()
                        .w_full()
                        .my(px(4.0))
                        .h(px(5.0))
                        .rounded_full()
                        .bg(theme.border)
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative(percent.unwrap_or(0).min(100) as f32 / 100.0))
                                .rounded_full()
                                .bg(theme.success),
                        ),
                )
        };
        let mode_button = |id: &'static str,
                           label: &'static str,
                           icon: &'static str,
                           candidate: AirPodsListeningMode| {
            let is_selected = selected == Some(candidate);
            div()
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.0))
                .child(
                    div()
                        .id(format!("dcc-airpods-mode-{id}"))
                        .w_full()
                        .h(px(34.0))
                        .rounded(theme.control_radius)
                        .border_1()
                        .border_color(theme.border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(22.0))
                        .text_color(if is_selected { theme.foreground } else { muted })
                        .bg(if is_selected {
                            theme.active_background
                        } else {
                            theme.container_background
                        })
                        .opacity(if ready { 1.0 } else { 0.45 })
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.hover_background))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.set_airpods_mode(candidate, cx)),
                        )
                        .child(icon),
                )
                .child(
                    div()
                        .h(px(16.0))
                        .text_size(px(8.0))
                        .text_color(if is_selected { theme.foreground } else { muted })
                        .child(label),
                )
        };

        div()
            .p(px(12.0))
            .rounded(theme.panel_radius)
            .bg(panel)
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(div().text_size(px(14.0)).child("AirPods"))
            .child(
                div()
                    .flex()
                    .gap(px(18.0))
                    .child(pod(
                        "L",
                        "src/icon/airpods_l_icon.svg",
                        self.snapshot.airpods.left_percent,
                    ))
                    .child(pod(
                        "R",
                        "src/icon/airpods_r_icon.svg",
                        self.snapshot.airpods.right_percent,
                    )),
            )
            .child(
                div()
                    .h(px(54.0))
                    .flex()
                    .gap(px(5.0))
                    .child(mode_button(
                        "transparency",
                        "外音取り込み",
                        "\u{f07c5}",
                        AirPodsListeningMode::Transparency,
                    ))
                    .child(mode_button(
                        "adaptive",
                        "アダプティブ",
                        "\u{f2a2}",
                        AirPodsListeningMode::Adaptive,
                    ))
                    .child(mode_button(
                        "noise-cancellation",
                        "ノイズキャンセリング",
                        "\u{f0a45}",
                        AirPodsListeningMode::NoiseCancellation,
                    )),
            )
            .when_some(self.snapshot.airpods.message.clone(), |card, message| {
                card.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.error)
                        .child(message),
                )
            })
            .when(!ready && self.snapshot.airpods.message.is_none(), |card| {
                card.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(muted)
                        .child("AirPodsを準備中…"),
                )
            })
    }

    fn render_bluetooth_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let panel = theme.container_background;
        let muted = theme.muted_foreground;
        let enabled = self.snapshot.bluetooth.enabled;
        let available = self.snapshot.bluetooth.available;
        let devices = self.snapshot.bluetooth_devices.clone();
        let operations = self.bluetooth_operations.clone();
        let known_devices = devices
            .iter()
            .filter(|device| device.connected || device.paired)
            .cloned()
            .collect::<Vec<_>>();
        let other_devices = devices
            .iter()
            .filter(|device| !device.connected && !device.paired)
            .cloned()
            .collect::<Vec<_>>();
        let has_connected_devices = known_devices.iter().any(|device| device.connected);
        let airpods_connected = self.snapshot.airpods.connected;
        let airpods_card =
            airpods_connected.then(|| self.render_airpods_card(cx).into_any_element());
        // With no active connection, reserve only enough space for the header,
        // status text, and up to three remembered devices; the lower scan panel
        // gets the remaining vertical space.
        let compact_known_panel_height =
            (96.0 + known_devices.len().min(3) as f32 * 52.0).min(252.0);

        let device_row = |id: (&'static str, u32), device: BluetoothDevice| {
            let operation = operations.get(&device.path).copied();
            let status = if operation.is_some() {
                if operation == Some(true) {
                    "ペアリング中…".to_string()
                } else {
                    "処理中…".to_string()
                }
            } else if device.connected {
                "接続済み（クリックで切断）".to_string()
            } else if device.paired {
                "ペアリング済み（クリックで接続）".to_string()
            } else {
                "クリックでペアリング・接続".to_string()
            };
            let signal = device
                .rssi
                .map(|rssi| format!(" {rssi} dBm"))
                .unwrap_or_default();
            let label = device.label.clone();
            let click_device = device.clone();
            div()
                .id(id)
                .min_h(px(52.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(theme.control_radius)
                .flex()
                .items_center()
                .gap(px(8.0))
                .cursor_pointer()
                .opacity(if operation.is_some() { 0.55 } else { 1.0 })
                .bg(if device.connected {
                    theme.active_background
                } else {
                    panel
                })
                .hover(|style| style.bg(theme.hover_background))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_bluetooth_device(click_device.clone(), cx)
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .child(label)
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(muted)
                                .child(format!("{}{}", status, signal)),
                        ),
                )
        };

        div()
            .size_full()
            .p(px(28.0))
            .bg(theme.dialog_background)
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(24.0)).child("Bluetooth"))
                    .child(
                        div()
                            .id("dcc-bluetooth-toggle")
                            .h(px(28.0))
                            .w(px(58.0))
                            .p(px(3.0))
                            .rounded(px(14.0))
                            .cursor_pointer()
                            .opacity(if available { 1.0 } else { 0.45 })
                            .bg(if enabled {
                                theme.active_background
                            } else {
                                theme.border
                            })
                            .hover(|style| style.bg(theme.hover_background))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_bluetooth(cx)))
                            .child(
                                div()
                                    .size(px(22.0))
                                    .rounded_full()
                                    .bg(theme.foreground)
                                    .when(enabled, |knob| knob.ml_auto()),
                            ),
                    ),
            )
            .when_some(self.bluetooth_message.clone(), |page, message| {
                page.child(div().text_color(theme.error).child(message))
            })
            .when(!available, |page| {
                page.child(div().text_color(muted).child("Bluetoothは利用できません"))
            })
            .when(available && !enabled, |page| {
                page.child(div().text_color(muted).child("Bluetoothはオフです"))
            })
            .when(available && enabled, |page| {
                page.child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h(px(0.0))
                        .gap(px(18.0))
                        .when_some(airpods_card, |content, card| {
                            content.child(div().flex_none().child(card))
                        })
                        .child(
                            div()
                                .when(has_connected_devices, |panel| panel.flex_1())
                                .when(!has_connected_devices, |panel| {
                                    panel.h(px(compact_known_panel_height)).flex_none()
                                })
                                .min_h(px(0.0))
                                .p(px(12.0))
                                .rounded(theme.panel_radius)
                                .bg(panel)
                                .flex()
                                .flex_col()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .text_color(muted)
                                        .child("接続済み・ペアリング済みのデバイス"),
                                )
                                .when(known_devices.is_empty(), |list| {
                                    list.child(div().text_color(muted).child("なし"))
                                })
                                .child(
                                    div()
                                        .id("dcc-known-bluetooth-list")
                                        .flex_1()
                                        .min_h(px(0.0))
                                        .overflow_y_scroll()
                                        .flex()
                                        .flex_col()
                                        .gap(px(3.0))
                                        .children(known_devices.into_iter().enumerate().map(
                                            |(index, device)| {
                                                device_row(
                                                    ("dcc-known-bluetooth", index as u32),
                                                    device,
                                                )
                                            },
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_h(px(0.0))
                                .p(px(12.0))
                                .rounded(theme.panel_radius)
                                .bg(panel)
                                .flex()
                                .flex_col()
                                .gap(px(8.0))
                                .child(div().text_color(muted).child("ほかのデバイス"))
                                .when(other_devices.is_empty(), |list| {
                                    list.child(
                                        div().text_color(muted).child("周辺のデバイスを検索中…"),
                                    )
                                })
                                .child(
                                    div()
                                        .id("dcc-other-bluetooth-list")
                                        .flex_1()
                                        .min_h(px(0.0))
                                        .overflow_y_scroll()
                                        .flex()
                                        .flex_col()
                                        .gap(px(3.0))
                                        .children(other_devices.into_iter().enumerate().map(
                                            |(index, device)| {
                                                device_row(
                                                    ("dcc-other-bluetooth", index as u32),
                                                    device,
                                                )
                                            },
                                        )),
                                ),
                        ),
                )
            })
    }

    fn render_bluetooth_pairing_modal(
        &self,
        dialog: BluetoothPairingDialog,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let requires_input = matches!(
            &dialog.prompt,
            BluetoothPairingPrompt::PinCode | BluetoothPairingPrompt::Passkey
        );
        let (instruction, confirm_label) = match &dialog.prompt {
            BluetoothPairingPrompt::PinCode => {
                ("このデバイスのPINを入力してください".to_string(), "送信")
            }
            BluetoothPairingPrompt::Passkey => {
                ("数字のパスキーを入力してください".to_string(), "送信")
            }
            BluetoothPairingPrompt::Confirmation { passkey } => (
                format!("次のコードがデバイス側と一致することを確認してください: {passkey:06}"),
                "承認",
            ),
            BluetoothPairingPrompt::DisplayPinCode { pin_code } => (
                format!("デバイス側で次のPINを入力してください: {pin_code}"),
                "閉じる",
            ),
            BluetoothPairingPrompt::DisplayPasskey { passkey } => (
                format!("デバイス側に次のコードが表示されます: {passkey:06}"),
                "閉じる",
            ),
            BluetoothPairingPrompt::Authorization => (
                "このデバイスとのペアリングを許可しますか？".to_string(),
                "許可",
            ),
        };
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000099))
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(
                div()
                    .id("dcc-bluetooth-pairing-modal")
                    .w(px(440.0))
                    .p(px(20.0))
                    .rounded(theme.panel_radius)
                    .bg(theme.dialog_background)
                    .track_focus(&self.input_focus)
                    .on_key_down(
                        cx.listener(|this, event, window, cx| {
                            this.input_key_down(event, window, cx)
                        }),
                    )
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(div().text_size(px(20.0)).child("Bluetooth ペアリング"))
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(dialog.device_label),
                    )
                    .child(instruction)
                    .when(requires_input, |modal| {
                        modal.child(
                            div()
                                .id("dcc-bluetooth-pairing-input")
                                .h(px(36.0))
                                .px(px(10.0))
                                .rounded(theme.control_radius)
                                .bg(theme.window_background)
                                .cursor_text()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.focus_input(InputField::BluetoothPin, window, cx)
                                }))
                                .child(self.input_value(InputField::BluetoothPin)),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.0))
                            .when(
                                !matches!(
                                    &dialog.prompt,
                                    BluetoothPairingPrompt::DisplayPinCode { .. }
                                        | BluetoothPairingPrompt::DisplayPasskey { .. }
                                ),
                                |buttons| {
                                    buttons.child(
                                        div()
                                            .id("dcc-bluetooth-pairing-cancel")
                                            .h(px(34.0))
                                            .px(px(12.0))
                                            .rounded(theme.control_radius)
                                            .cursor_pointer()
                                            .hover(|style| style.bg(theme.hover_background))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.respond_bluetooth_pairing(false, cx)
                                            }))
                                            .child("キャンセル"),
                                    )
                                },
                            )
                            .child(
                                div()
                                    .id("dcc-bluetooth-pairing-confirm")
                                    .h(px(34.0))
                                    .px(px(12.0))
                                    .rounded(theme.control_radius)
                                    .cursor_pointer()
                                    .bg(theme.active_background)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.respond_bluetooth_pairing(true, cx)
                                    }))
                                    .child(confirm_label),
                            ),
                    ),
            )
    }

    fn render_network_details_modal(
        &self,
        details: NetworkDetails,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let panel = theme.dialog_background;
        let muted = theme.muted_foreground;
        let speed = details
            .network
            .speed_mbps
            .map(format_link_speed)
            .unwrap_or_else(|| "利用不可".to_string());

        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000099))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .p(px(24.0))
            .child(
                div()
                    .id("dcc-network-details-modal")
                    .w(px(520.0))
                    .h_full()
                    .max_h(px(720.0))
                    .track_focus(&self.input_focus)
                    .on_key_down(
                        cx.listener(|this, event, window, cx| {
                            this.input_key_down(event, window, cx)
                        }),
                    )
                    .overflow_y_scroll()
                    .p(px(20.0))
                    .rounded(theme.panel_radius)
                    .bg(panel)
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.0))
                                    .child(
                                        div()
                                            .id("dcc-details-close")
                                            .size(px(32.0))
                                            .rounded(theme.control_radius)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .hover(|style| style.bg(theme.hover_background))
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.close_details(cx)
                                                }),
                                            )
                                            .child("×"),
                                    )
                                    .child(div().text_size(px(20.0)).child("ネットワーク詳細")),
                            )
                            .child(
                                div()
                                    .id("dcc-save-network-settings")
                                    .size(px(32.0))
                                    .rounded(theme.control_radius)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .opacity(if details.saving { 0.45 } else { 1.0 })
                                    .when(!details.saving, |button| {
                                        button
                                            .cursor_pointer()
                                            .hover(|style| style.bg(theme.hover_background))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.request_save_network_settings(cx)
                                            }))
                                    })
                                    .child(if details.saving { "…" } else { "✓" }),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(16.0))
                            .child(details.network.label.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(muted)
                            .child(format!("リンクスピード: {speed}")),
                    )
                    .child(self.ip_settings_panel("IPv4", &details.settings.ipv4, false, cx))
                    .child(self.ip_settings_panel("IPv6", &details.settings.ipv6, true, cx))
                    .when_some(details.message.clone(), |panel, message| {
                        panel.child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.error)
                                .child(message),
                        )
                    }),
            )
    }

    fn render_modal(
        &self,
        modal: Modal,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let panel = theme.dialog_background;
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000099))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(
                div()
                    .id("dcc-modal")
                    .track_focus(&self.input_focus)
                    .on_key_down(
                        cx.listener(|this, event, window, cx| {
                            this.input_key_down(event, window, cx)
                        }),
                    )
                    .w(px(360.0))
                    .p(px(20.0))
                    .rounded(theme.panel_radius)
                    .bg(panel)
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .when(modal.clone().is_password(), |modal_view| {
                        let Modal::Password {
                            network,
                            password,
                            message,
                            connecting,
                        } = modal.clone()
                        else {
                            unreachable!()
                        };
                        modal_view
                            .child(div().text_size(px(18.0)).child(network.label))
                            .child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child("このネットワークにはパスワードが必要です"),
                            )
                            .child(
                                div()
                                    .id("dcc-wifi-password")
                                    .h(px(36.0))
                                    .px(px(10.0))
                                    .rounded(theme.control_radius)
                                    .border_1()
                                    .border_color(
                                        if self.active_input == Some(InputField::Password) {
                                            theme.focus
                                        } else {
                                            theme.border
                                        },
                                    )
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            this.focus_input(InputField::Password, window, cx)
                                        }),
                                    )
                                    .child(if password.is_empty() {
                                        "パスワード".to_string()
                                    } else {
                                        "•".repeat(password.chars().count())
                                    }),
                            )
                            .when_some(message, |modal_view, message| {
                                modal_view.child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(theme.error)
                                        .child(message),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .id("dcc-password-cancel")
                                            .h(px(34.0))
                                            .px(px(12.0))
                                            .rounded(theme.control_radius)
                                            .cursor_pointer()
                                            .hover(|style| style.bg(theme.hover_background))
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.close_modal(cx)),
                                            )
                                            .child("キャンセル"),
                                    )
                                    .child(
                                        div()
                                            .id("dcc-password-connect")
                                            .h(px(34.0))
                                            .px(px(12.0))
                                            .rounded(theme.control_radius)
                                            .cursor_pointer()
                                            .bg(theme.active_background)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.connect_password_network(cx)
                                            }))
                                            .child(if connecting {
                                                "接続中…"
                                            } else {
                                                "接続"
                                            }),
                                    ),
                            )
                    })
                    .when(
                        matches!(modal, Modal::ConfirmNetworkSettings),
                        |modal_view| {
                            modal_view
                                .child(
                                    div()
                                        .text_size(px(18.0))
                                        .child("ネットワーク設定を保存しますか？"),
                                )
                                .child(
                                    div()
                                        .text_color(theme.muted_foreground)
                                        .child("接続を一度再確立して、新しいIP設定を反映します。"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .justify_end()
                                        .gap(px(8.0))
                                        .child(
                                            div()
                                                .id("dcc-settings-cancel")
                                                .h(px(34.0))
                                                .px(px(12.0))
                                                .rounded(theme.control_radius)
                                                .cursor_pointer()
                                                .hover(|style| style.bg(theme.hover_background))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.close_modal(cx)
                                                }))
                                                .child("キャンセル"),
                                        )
                                        .child(
                                            div()
                                                .id("dcc-settings-confirm")
                                                .h(px(34.0))
                                                .px(px(12.0))
                                                .rounded(theme.control_radius)
                                                .cursor_pointer()
                                                .bg(theme.active_background)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.save_network_settings(cx)
                                                }))
                                                .child("保存して再接続"),
                                        ),
                                )
                        },
                    ),
            )
    }
}

impl Modal {
    fn is_password(&self) -> bool {
        matches!(self, Self::Password { .. })
    }
}

fn format_link_speed(speed_mbps: u64) -> String {
    if speed_mbps >= 1000 {
        let gigabits = speed_mbps as f64 / 1000.0;
        if gigabits.fract() == 0.0 {
            format!("{} Gbps", gigabits as u64)
        } else {
            format!("{gigabits:.1} Gbps")
        }
    } else {
        format!("{speed_mbps} Mbps")
    }
}

#[cfg(test)]
mod tests {
    use super::{default_route_badge, is_wallpaper_file};
    use crate::modules::system_controls::{ActiveNetwork, NetworkKind, NetworkSettings};

    fn network(default_ipv4: bool, default_ipv6: bool) -> ActiveNetwork {
        ActiveNetwork {
            label: "network".to_string(),
            kind: NetworkKind::Wired,
            interface: "eth0".to_string(),
            default_ipv4,
            default_ipv6,
            connection_uuid: None,
            speed_mbps: None,
            settings: NetworkSettings::default(),
        }
    }

    #[test]
    fn route_badges_only_mark_default_routes() {
        assert_eq!(default_route_badge(&network(false, false)), None);
        assert_eq!(default_route_badge(&network(true, false)), Some("✓ IPv4"));
        assert_eq!(default_route_badge(&network(false, true)), Some("✓ IPv6"));
        assert_eq!(
            default_route_badge(&network(true, true)),
            Some("✓ IPv4 / IPv6")
        );
    }

    #[test]
    fn wallpaper_picker_only_shows_supported_media_files() {
        assert!(is_wallpaper_file("wallpaper.PNG".as_ref()));
        assert!(is_wallpaper_file("movie.mkv".as_ref()));
        assert!(!is_wallpaper_file("notes.txt".as_ref()));
    }
}
