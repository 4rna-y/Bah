use async_channel::Sender;
use gpui::{Context, FontWeight, Render, Size, Window, div, prelude::*, px};

use crate::{
    app::launch_device_control_center_network,
    modules::system_controls::{
        ControlAction, ControlSnapshot, WifiConnectionEvent, WifiNetwork, WifiSecurity,
    },
    theme::{BarTheme, ui_font},
};

const POPOVER_MIN_WIDTH: f32 = 180.0;
const POPOVER_MAX_WIDTH: f32 = 360.0;
const POPOVER_PADDING: f32 = 8.0;
const ROW_HEIGHT: f32 = 32.0;
const ERROR_ROW_HEIGHT: f32 = 34.0;
const SEPARATOR_HEIGHT: f32 = 9.0;
const SECTION_TITLE_HEIGHT: f32 = 22.0;
const LIST_GAP: f32 = 2.0;
const NETWORKS_PER_GROUP: usize = 5;

pub struct NetworkPopover {
    controls: ControlSnapshot,
    actions: Sender<ControlAction>,
    errors: Vec<(Vec<u8>, String)>,
    connecting: Option<Vec<u8>>,
    theme: BarTheme,
}

impl NetworkPopover {
    pub fn new(controls: ControlSnapshot, actions: Sender<ControlAction>, theme: BarTheme) -> Self {
        Self {
            controls,
            actions,
            errors: Vec::new(),
            connecting: None,
            theme,
        }
    }

    pub fn set_controls(
        &mut self,
        controls: ControlSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.controls = controls;
        window.resize(window_size(&self.controls, &self.errors));
        cx.notify();
    }

    pub fn apply_wifi_event(
        &mut self,
        event: WifiConnectionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WifiConnectionEvent::Connecting { ssid } => {
                self.clear_error(&ssid);
                self.connecting = Some(ssid);
            }
            WifiConnectionEvent::Succeeded { ssid } => {
                self.clear_error(&ssid);
                if self.connecting.as_deref() == Some(ssid.as_slice()) {
                    self.connecting = None;
                }
            }
            WifiConnectionEvent::Failed { ssid, message } => {
                self.clear_error(&ssid);
                self.errors.push((ssid.clone(), message));
                if self.connecting.as_deref() == Some(ssid.as_slice()) {
                    self.connecting = None;
                }
            }
        }
        window.resize(window_size(&self.controls, &self.errors));
        cx.notify();
    }

    fn clear_error(&mut self, ssid: &[u8]) {
        self.errors.retain(|(candidate, _)| candidate != ssid);
    }

    fn dismiss_error(&mut self, ssid: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_error(&ssid);
        window.resize(window_size(&self.controls, &self.errors));
        cx.notify();
    }

    fn select_network(
        &mut self,
        network: WifiNetwork,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if network.connected || self.connecting.as_deref() == Some(network.ssid.as_slice()) {
            return;
        }
        if !network.saved && network.security != WifiSecurity::Open {
            launch_device_control_center_network(Some(network.ssid));
            let _ = self
                .actions
                .try_send(ControlAction::SetWifiDiscovery(false));
            window.remove_window();
            return;
        }

        self.clear_error(&network.ssid);
        self.connecting = Some(network.ssid.clone());
        if self
            .actions
            .try_send(ControlAction::ConnectWifi {
                ssid: network.ssid.clone(),
                password: None,
            })
            .is_err()
        {
            self.connecting = None;
            self.errors
                .push((network.ssid, "接続要求を送信できませんでした".to_string()));
        }
        window.resize(window_size(&self.controls, &self.errors));
        cx.notify();
    }

    fn open_more(&mut self, window: &mut Window) {
        launch_device_control_center_network(None);
        let _ = self
            .actions
            .try_send(ControlAction::SetWifiDiscovery(false));
        window.remove_window();
    }

    fn toggle_wifi(&mut self, cx: &mut Context<Self>) {
        if self.controls.wifi.available {
            let _ = self.actions.try_send(ControlAction::ToggleWifi);
        }
        cx.notify();
    }
}

impl Render for NetworkPopover {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let wired_active = self.controls.wired_active;
        let wifi_available = self.controls.wifi.available;
        let wifi_enabled = self.controls.wifi.enabled;
        let (saved_networks, other_networks) = visible_network_groups(&self.controls);
        let saved_networks_empty = saved_networks.is_empty();
        let other_networks_empty = other_networks.is_empty();
        let show_other_networks =
            wifi_available && wifi_enabled && (!other_networks_empty || saved_networks_empty);
        let connecting = self.connecting.clone();
        let errors = self.errors.clone();

        div()
            .size_full()
            .p(px(POPOVER_PADDING))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.background.alpha(1.0))
            .text_color(theme.foreground)
            .font(ui_font())
            .text_size(px(12.0))
            .flex()
            .flex_col()
            .gap(px(LIST_GAP))
            .when(wired_active, |list| {
                list.child(
                    div()
                        .h(px(ROW_HEIGHT))
                        .px(px(10.0))
                        .flex()
                        .items_center()
                        .font_weight(FontWeight::MEDIUM)
                        .child(div().flex_1().child("有線接続"))
                        .child(
                            div()
                                .text_size(px(15.0))
                                .text_color(gpui::rgb(0x63d297))
                                .child("✓"),
                        ),
                )
                .child(div().h(px(1.0)).my(px(4.0)).mx(px(6.0)).bg(theme.border))
            })
            .child(
                div()
                    .h(px(ROW_HEIGHT))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .child(div().flex_1().child("Wi-Fi"))
                    .child(
                        div()
                            .id("network-popover-wifi-toggle")
                            .w(px(34.0))
                            .h(px(18.0))
                            .p(px(2.0))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .when(wifi_enabled, |toggle| {
                                toggle.justify_end().bg(gpui::rgb(0x63d297))
                            })
                            .when(!wifi_enabled, |toggle| {
                                toggle.justify_start().bg(theme.border)
                            })
                            .when(!wifi_available, |toggle| toggle.opacity(0.45))
                            .hover(|style| style.opacity(0.82))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_wifi(cx)))
                            .child(div().size(px(14.0)).rounded_full().bg(theme.foreground)),
                    ),
            )
            .child(div().h(px(1.0)).my(px(4.0)).mx(px(6.0)).bg(theme.border))
            .when(!wifi_available, |list| {
                list.child(status_row("Wi-Fiは利用できません", theme))
            })
            .when(wifi_available && !wifi_enabled, |list| {
                list.child(status_row("Wi-Fiはオフです", theme))
            })
            .when(wifi_available && wifi_enabled, |list| {
                list.child(section_title("接続したことのあるネットワーク", theme))
            })
            .children(
                saved_networks
                    .into_iter()
                    .enumerate()
                    .flat_map(|(index, network)| {
                        let network_for_click = network.clone();
                        let is_connecting = connecting.as_deref() == Some(network.ssid.as_slice());
                        let error = errors
                            .iter()
                            .find(|(ssid, _)| *ssid == network.ssid)
                            .map(|(_, message)| message.clone());
                        let row = div()
                            .id(("wifi-network", index as u32))
                            .h(px(ROW_HEIGHT))
                            .px(px(10.0))
                            .rounded(px(5.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .cursor_pointer()
                            .when(is_connecting, |row| row.opacity(0.55))
                            .hover(|style| style.bg(theme.active_background))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_network(network_for_click.clone(), window, cx);
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(network.label),
                            )
                            .when(is_connecting, |row| {
                                row.child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(theme.muted_foreground)
                                        .child("接続中…"),
                                )
                            })
                            .when(network.connected, |row| {
                                row.child(
                                    div()
                                        .text_size(px(15.0))
                                        .text_color(gpui::rgb(0x63d297))
                                        .child("✓"),
                                )
                            });
                        let mut elements = vec![row.into_any_element()];
                        if let Some(message) = error {
                            let ssid = network.ssid;
                            elements.push(
                                div()
                                    .id(("wifi-error", index as u32))
                                    .min_h(px(ERROR_ROW_HEIGHT))
                                    .px(px(6.0))
                                    .py(px(5.0))
                                    .rounded(px(5.0))
                                    .flex()
                                    .items_start()
                                    .gap(px(6.0))
                                    .bg(theme.urgent_background)
                                    .child(
                                        div()
                                            .id(("dismiss-wifi-error", index as u32))
                                            .size(px(18.0))
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(px(4.0))
                                            .cursor_pointer()
                                            .hover(|style| style.bg(theme.active_background))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.dismiss_error(ssid.clone(), window, cx);
                                            }))
                                            .child("×"),
                                    )
                                    .child(div().flex_1().text_size(px(11.0)).child(message))
                                    .into_any_element(),
                            );
                        }
                        elements
                    }),
            )
            .when(
                wifi_available && wifi_enabled && saved_networks_empty,
                |list| list.child(status_row("なし", theme)),
            )
            .when(show_other_networks, |list| {
                list.child(div().h(px(1.0)).my(px(4.0)).mx(px(6.0)).bg(theme.border))
            })
            .when(show_other_networks, |list| {
                list.child(section_title("ほかのネットワーク", theme))
            })
            .children(
                other_networks
                    .into_iter()
                    .enumerate()
                    .flat_map(|(index, network)| {
                        let row_index = index + NETWORKS_PER_GROUP;
                        let network_for_click = network.clone();
                        let is_connecting = connecting.as_deref() == Some(network.ssid.as_slice());
                        let error = errors
                            .iter()
                            .find(|(ssid, _)| *ssid == network.ssid)
                            .map(|(_, message)| message.clone());
                        let row = div()
                            .id(("wifi-network", row_index as u32))
                            .h(px(ROW_HEIGHT))
                            .px(px(10.0))
                            .rounded(px(5.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .cursor_pointer()
                            .when(is_connecting, |row| row.opacity(0.55))
                            .hover(|style| style.bg(theme.active_background))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_network(network_for_click.clone(), window, cx);
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(network.label),
                            )
                            .when(is_connecting, |row| {
                                row.child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(theme.muted_foreground)
                                        .child("接続中…"),
                                )
                            })
                            .when(network.connected, |row| {
                                row.child(
                                    div()
                                        .text_size(px(15.0))
                                        .text_color(gpui::rgb(0x63d297))
                                        .child("✓"),
                                )
                            });
                        let mut elements = vec![row.into_any_element()];
                        if let Some(message) = error {
                            let ssid = network.ssid;
                            elements.push(
                                div()
                                    .id(("wifi-error", row_index as u32))
                                    .min_h(px(ERROR_ROW_HEIGHT))
                                    .px(px(6.0))
                                    .py(px(5.0))
                                    .rounded(px(5.0))
                                    .flex()
                                    .items_start()
                                    .gap(px(6.0))
                                    .bg(theme.urgent_background)
                                    .child(
                                        div()
                                            .id(("dismiss-wifi-error", row_index as u32))
                                            .size(px(18.0))
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(px(4.0))
                                            .cursor_pointer()
                                            .hover(|style| style.bg(theme.active_background))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.dismiss_error(ssid.clone(), window, cx);
                                            }))
                                            .child("×"),
                                    )
                                    .child(div().flex_1().text_size(px(11.0)).child(message))
                                    .into_any_element(),
                            );
                        }
                        elements
                    }),
            )
            .when(show_other_networks && other_networks_empty, |list| {
                list.child(status_row("ネットワークを検索中…", theme))
            })
            .child(
                div()
                    .id("network-popover-more")
                    .mt(px(4.0))
                    .h(px(ROW_HEIGHT))
                    .px(px(10.0))
                    .rounded(px(5.0))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.active_background))
                    .on_click(cx.listener(|this, _, window, _| this.open_more(window)))
                    .child("その他"),
            )
    }
}

fn visible_network_groups(controls: &ControlSnapshot) -> (Vec<WifiNetwork>, Vec<WifiNetwork>) {
    if !controls.wifi.available || !controls.wifi.enabled {
        return (Vec::new(), Vec::new());
    }

    let saved = controls
        .wifi_networks
        .iter()
        .filter(|network| network.saved)
        .take(NETWORKS_PER_GROUP)
        .cloned()
        .collect();
    let other = controls
        .wifi_networks
        .iter()
        .filter(|network| !network.saved)
        .take(NETWORKS_PER_GROUP)
        .cloned()
        .collect();
    (saved, other)
}

fn status_row(text: &str, theme: BarTheme) -> gpui::Div {
    div()
        .h(px(ROW_HEIGHT))
        .px(px(10.0))
        .flex()
        .items_center()
        .text_color(theme.muted_foreground)
        .child(text.to_string())
}

fn section_title(text: &str, theme: BarTheme) -> gpui::Div {
    div()
        .h(px(SECTION_TITLE_HEIGHT))
        .px(px(10.0))
        .flex()
        .items_end()
        .text_size(px(11.0))
        .text_color(theme.muted_foreground)
        .child(text.to_string())
}

pub fn window_size(controls: &ControlSnapshot, errors: &[(Vec<u8>, String)]) -> Size<gpui::Pixels> {
    let (saved_networks, other_networks) = visible_network_groups(controls);
    let visible_networks = saved_networks
        .iter()
        .chain(other_networks.iter())
        .collect::<Vec<_>>();
    let visible_errors = errors
        .iter()
        .filter(|(ssid, _)| visible_networks.iter().any(|network| network.ssid == *ssid))
        .collect::<Vec<_>>();
    let labels = visible_networks
        .iter()
        .copied()
        .map(|network| network.label.as_str())
        .chain(visible_errors.iter().map(|(_, message)| message.as_str()))
        .chain([
            "有線接続",
            "ネットワークを検索中…",
            "Wi-Fiは利用できません",
            "Wi-Fiはオフです",
            "接続したことのあるネットワーク",
            "ほかのネットワーク",
            "なし",
            "その他",
        ]);
    let content_width = labels.map(display_width).fold(0.0_f32, f32::max) + 64.0;
    let width = content_width.clamp(POPOVER_MIN_WIDTH, POPOVER_MAX_WIDTH);
    let mut height = POPOVER_PADDING * 2.0 + ROW_HEIGHT + SEPARATOR_HEIGHT + ROW_HEIGHT + 4.0;
    let mut child_count = 3_u32; // Wi-Fi toggle, separator, and "その他"
    if controls.wired_active {
        height += ROW_HEIGHT + SEPARATOR_HEIGHT;
        child_count += 2;
    }
    if !controls.wifi.available || !controls.wifi.enabled {
        height += ROW_HEIGHT;
        child_count += 1;
    } else {
        height += SECTION_TITLE_HEIGHT;
        child_count += 1;
        if saved_networks.is_empty() {
            height += ROW_HEIGHT;
            child_count += 1;
        } else {
            height += saved_networks.len() as f32 * ROW_HEIGHT;
            child_count += saved_networks.len() as u32;
        }

        let show_other_networks = !other_networks.is_empty() || saved_networks.is_empty();
        if show_other_networks {
            height += SEPARATOR_HEIGHT;
            child_count += 1;
            height += SECTION_TITLE_HEIGHT;
            child_count += 1;
            if other_networks.is_empty() {
                height += ROW_HEIGHT;
                child_count += 1;
            } else {
                height += other_networks.len() as f32 * ROW_HEIGHT;
                child_count += other_networks.len() as u32;
            }
        }
    }
    height += visible_errors.len() as f32 * ERROR_ROW_HEIGHT;
    child_count += visible_errors.len() as u32;
    height += (child_count.saturating_sub(1) as f32) * LIST_GAP;
    Size::new(px(width), px(height))
}

fn display_width(text: &str) -> f32 {
    text.chars()
        .map(|character| if character.is_ascii() { 7.0 } else { 14.0 })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::{visible_network_groups, window_size};
    use crate::modules::system_controls::{ControlSnapshot, WifiNetwork, WifiSecurity};

    fn network(label: &str, saved: bool) -> WifiNetwork {
        WifiNetwork {
            ssid: label.as_bytes().to_vec(),
            label: label.to_string(),
            strength: 80,
            connected: false,
            saved,
            security: WifiSecurity::Open,
        }
    }

    #[test]
    fn popover_size_grows_for_error_content() {
        let mut snapshot = ControlSnapshot::default();
        snapshot.wifi.available = true;
        snapshot.wifi.enabled = true;
        snapshot.wifi_networks.push(network("wifi", false));
        let normal = window_size(&snapshot, &[]);
        let with_error = window_size(
            &snapshot,
            &[(b"wifi".to_vec(), "接続できませんでした".to_string())],
        );
        assert!(with_error.height > normal.height);
        assert!(with_error.width >= normal.width);
    }

    #[test]
    fn visible_networks_are_limited_to_five_per_group() {
        let mut snapshot = ControlSnapshot::default();
        snapshot.wifi.available = true;
        snapshot.wifi.enabled = true;
        snapshot
            .wifi_networks
            .extend((0..6).map(|index| network(&format!("saved-{index}"), true)));
        snapshot
            .wifi_networks
            .extend((0..6).map(|index| network(&format!("other-{index}"), false)));

        let (saved, other) = visible_network_groups(&snapshot);

        assert_eq!(saved.len(), 5);
        assert_eq!(other.len(), 5);
        assert_eq!(saved[0].label, "saved-0");
        assert_eq!(other[0].label, "other-0");
        assert_eq!(saved[4].label, "saved-4");
        assert_eq!(other[4].label, "other-4");
    }

    #[test]
    fn popover_size_includes_titles_and_separator_for_two_groups() {
        let mut single_group = ControlSnapshot::default();
        single_group.wifi.available = true;
        single_group.wifi.enabled = true;
        single_group.wifi_networks = vec![network("saved-1", true), network("saved-2", true)];

        let mut two_groups = ControlSnapshot::default();
        two_groups.wifi.available = true;
        two_groups.wifi.enabled = true;
        two_groups.wifi_networks = vec![network("saved", true), network("other", false)];

        assert!(window_size(&two_groups, &[]).height > window_size(&single_group, &[]).height);
    }
}
