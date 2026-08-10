use async_channel::Receiver;
use gpui::{
    Context, FocusHandle, KeyDownEvent, MouseButton, Render, Window, div, prelude::*, px, rgb, rgba,
};

use crate::{
    app::{DeviceControlCenterLock, DeviceControlCenterRoute, DeviceControlCenterRouteServer},
    modules::system_controls::{
        ActiveNetwork, ControlAction, ControlChannels, ControlSnapshot, IpSettings,
        NetworkSettings, NetworkSettingsEvent, WifiConnectionEvent, WifiNetwork, WifiSecurity,
        start_worker,
    },
    theme::ui_font,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
enum InputField {
    Password,
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
            Self::Ipv4Address => 1,
            Self::Ipv4Subnet => 2,
            Self::Ipv4Gateway => 3,
            Self::Ipv4PrimaryDns => 4,
            Self::Ipv4SecondaryDns => 5,
            Self::Ipv6Address => 6,
            Self::Ipv6Prefix => 7,
            Self::Ipv6Gateway => 8,
            Self::Ipv6PrimaryDns => 9,
            Self::Ipv6SecondaryDns => 10,
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

/// The standalone device control center currently has one real page. Keeping
/// the sidebar separate makes adding the remaining device pages non-breaking.
pub struct DeviceControlCenter {
    _lock: DeviceControlCenterLock,
    _route_server: DeviceControlCenterRouteServer,
    controls: ControlChannels,
    snapshot: ControlSnapshot,
    selected_ssid: Option<Vec<u8>>,
    modal: Option<Modal>,
    details: Option<NetworkDetails>,
    active_input: Option<InputField>,
    activate_requested: bool,
    input_focus: FocusHandle,
}

impl DeviceControlCenter {
    pub fn new(
        lock: DeviceControlCenterLock,
        route: DeviceControlCenterRoute,
        route_updates: Receiver<DeviceControlCenterRoute>,
        route_server: DeviceControlCenterRouteServer,
        cx: &mut Context<Self>,
    ) -> Self {
        let controls = start_worker();
        let updates = controls.updates.clone();
        let wifi_events = controls.wifi_events.clone();
        let network_settings_events = controls.network_settings_events.clone();
        let _ = controls
            .actions
            .try_send(ControlAction::SetWifiDiscovery(true));

        cx.spawn(async move |center, cx| {
            while let Ok(snapshot) = updates.recv().await {
                if center
                    .update(cx, |center, cx| {
                        center.snapshot = snapshot;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        cx.spawn(async move |center, cx| {
            while let Ok(route) = route_updates.recv().await {
                if center
                    .update(cx, |center, cx| {
                        center.apply_route(route);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        cx.spawn(async move |center, cx| {
            while let Ok(event) = wifi_events.recv().await {
                if center
                    .update(cx, |center, cx| {
                        center.apply_connection_event(event);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        cx.spawn(async move |center, cx| {
            while let Ok(event) = network_settings_events.recv().await {
                if center
                    .update(cx, |center, cx| {
                        center.apply_network_settings_event(event);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();

        Self {
            _lock: lock,
            _route_server: route_server,
            controls,
            snapshot: ControlSnapshot::default(),
            selected_ssid: route.ssid,
            modal: None,
            details: None,
            active_input: None,
            activate_requested: false,
            input_focus: cx.focus_handle(),
        }
    }

    fn apply_route(&mut self, route: DeviceControlCenterRoute) {
        self.selected_ssid = route.ssid;
        self.modal = None;
        self.details = None;
        self.active_input = None;
        self.activate_requested = true;
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
        {
            if let Some(Modal::Password {
                message,
                connecting,
                ..
            }) = self.modal.as_mut()
            {
                *connecting = false;
                *message = Some("接続要求を送信できませんでした".to_string());
            }
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
            }
            return;
        }
        if event.keystroke.key == "enter" && self.active_input == Some(InputField::Password) {
            self.connect_password_network(cx);
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
        let focused = self.active_input == Some(field);
        let value = self.input_value(field);
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(0xaeb1bd))
                    .child(label),
            )
            .child(
                div()
                    .id(("dcc-network-input", field.element_id()))
                    .h(px(34.0))
                    .px(px(9.0))
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(if focused {
                        rgb(0x7da7ff)
                    } else {
                        rgb(0x444752)
                    })
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
        let automatic = settings.automatic;
        let manual = !automatic;
        div()
            .p(px(12.0))
            .rounded(px(7.0))
            .bg(rgb(0x272932))
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
                                    .text_color(rgb(0xaeb1bd))
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
                                    .bg(if manual { rgb(0x315b46) } else { rgb(0x444752) })
                                    .hover(|style| style.bg(rgb(0x526577)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_manual(ipv6, !manual, cx)
                                    }))
                                    .child(
                                        div()
                                            .size(px(18.0))
                                            .rounded_full()
                                            .bg(rgb(0xf5f5f7))
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
        let muted = rgb(0xaeb1bd);
        let panel = rgb(0x202128);
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
            .bg(rgb(0x17181e))
            .text_color(rgb(0xf5f5f7))
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
                            .border_color(rgb(0x343640))
                            .child(
                                div()
                                    .h(px(36.0))
                                    .px(px(10.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .bg(rgb(0x343640))
                                    .child("ネットワーク"),
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
                                            .bg(if wifi_enabled { rgb(0x315b46) } else { rgb(0x444752) })
                                            .hover(|style| style.bg(rgb(0x526577)))
                                            .on_click(cx.listener(|this, _, _, cx| this.toggle_wifi(cx)))
                                            .child(
                                                div()
                                                    .size(px(22.0))
                                                    .rounded_full()
                                                    .bg(rgb(0xf5f5f7))
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
                                                            .bg(rgb(0x343640))
                                                            .child(div().flex_1().child(network.label))
                                                            .when_some(route_badge, |row, badge| {
                                                                row.child(
                                                                    div()
                                                                        .text_color(rgb(0x63d297))
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
                                                                    .hover(|style| style.bg(rgb(0x526577)))
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
                                                            .bg(if is_selected { rgb(0x343640) } else { panel })
                                                            .hover(|style| style.bg(rgb(0x343640)))
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
                                                            .bg(if is_selected { rgb(0x343640) } else { panel })
                                                            .hover(|style| style.bg(rgb(0x343640)))
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
            .when_some(details, |root, details| {
                root.child(self.render_network_details_modal(details, cx))
            })
            .when_some(modal, |root, modal| {
                root.child(self.render_modal(modal, window, cx))
            })
    }
}

impl DeviceControlCenter {
    fn render_network_details_modal(
        &self,
        details: NetworkDetails,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let panel = rgb(0x282a33);
        let muted = rgb(0xaeb1bd);
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
                    .rounded(px(8.0))
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
                                            .rounded(px(5.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .hover(|style| style.bg(rgb(0x343640)))
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
                                    .rounded(px(5.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .opacity(if details.saving { 0.45 } else { 1.0 })
                                    .when(!details.saving, |button| {
                                        button
                                            .cursor_pointer()
                                            .hover(|style| style.bg(rgb(0x315b46)))
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
                                .text_color(rgb(0xf2a0a0))
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
        let panel = rgb(0x282a33);
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
                    .rounded(px(8.0))
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
                                    .text_color(rgb(0xaeb1bd))
                                    .child("このネットワークにはパスワードが必要です"),
                            )
                            .child(
                                div()
                                    .id("dcc-wifi-password")
                                    .h(px(36.0))
                                    .px(px(10.0))
                                    .rounded(px(5.0))
                                    .border_1()
                                    .border_color(
                                        if self.active_input == Some(InputField::Password) {
                                            rgb(0x7da7ff)
                                        } else {
                                            rgb(0x444752)
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
                                        .text_color(rgb(0xf2a0a0))
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
                                            .rounded(px(5.0))
                                            .cursor_pointer()
                                            .hover(|style| style.bg(rgb(0x3b3e48)))
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
                                            .rounded(px(5.0))
                                            .cursor_pointer()
                                            .bg(rgb(0x315b46))
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
                                        .text_color(rgb(0xaeb1bd))
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
                                                .rounded(px(5.0))
                                                .cursor_pointer()
                                                .hover(|style| style.bg(rgb(0x3b3e48)))
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
                                                .rounded(px(5.0))
                                                .cursor_pointer()
                                                .bg(rgb(0x315b46))
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
    use super::default_route_badge;
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
}
