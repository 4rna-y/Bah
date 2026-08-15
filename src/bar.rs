use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use async_channel::{Receiver, Sender};

use gpui::{
    App, Bounds, Context, FontWeight, InteractiveElement, MouseDownEvent, Pixels, Point, Render,
    Size, Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowHandle,
    WindowKind, WindowOptions, canvas, div, img, layer_shell::*, point, popup::*, prelude::*, px,
};
use log::{error, info, warn};

use crate::{
    airpods_popover::{AirPodsPopover, window_size as airpods_popover_window_size},
    clipboard::{ClipboardPublisher, SharedClipboardHistory},
    clipboard_panel::ClipboardPanel,
    hyprland::{
        IpcUpdate, JumpListAction, WorkspaceWindow, close_window, focus_window,
        launch_jump_list_action, set_keybind_submap, switch_to_workspace,
    },
    modules::{
        clock::Clock,
        notifications::{
            CloseReason, NotificationEvent, NotificationStore, SharedNotificationStore,
            emit_notification_closed,
        },
        system_controls::{
            BatteryStatus, ControlChannels, ControlSnapshot, CpuStatus, LevelStatus, MemoryStatus,
            NetworkKind, NetworkRoute, ToggleStatus,
        },
        workspaces::Workspaces,
    },
    network_popover::{NetworkPopover, window_size as network_popover_window_size},
    notification_popup::NotificationPopupStack,
    notification_tray::{NotificationTray, NotificationTrayDismissTarget, TRAY_PANEL_WIDTH_RATIO},
    theme::{BarTheme, SurfaceRole, ui_font},
    window_switcher::{SwitcherState, WindowSwitcher},
};

/// GPUI entity that owns bar-local state and redraw scheduling.
pub struct Bar {
    clock: Clock,
    workspaces: Option<Workspaces>,
    notification_sender: Sender<NotificationEvent>,
    notifications: SharedNotificationStore,
    theme: BarTheme,
    jump_menu: Option<JumpMenu>,
    workspace_menu: Option<WorkspaceMenu>,
    jump_menu_resize_pending: bool,
    status_tooltip_hover: Option<&'static str>,
    status_tooltip_generation: u64,
    status_tooltip_popup: Option<WindowHandle<StatusTooltip>>,
    notification_tray: Option<WindowHandle<NotificationTray>>,
    notification_tray_dismiss_target: Option<WindowHandle<NotificationTrayDismissTarget>>,
    notification_popup: Option<WindowHandle<NotificationPopupStack>>,
    network_popover: Option<WindowHandle<NetworkPopover>>,
    device_control_center: Option<WindowHandle<crate::device_control_center::DeviceControlCenter>>,
    device_control_center_anchor: WindowHandle<DeviceControlCenterAnchor>,
    _device_control_center_route_server: crate::app::DeviceControlCenterRouteServer,
    pending_device_control_center_route: Option<crate::app::DeviceControlCenterRoute>,
    airpods_popover: Option<WindowHandle<AirPodsPopover>>,
    window_switcher: Option<WindowHandle<WindowSwitcher>>,
    _window_switcher_command_server: crate::app::WindowSwitcherCommandServer,
    clipboard_panel: Option<WindowHandle<ClipboardPanel>>,
    clipboard: SharedClipboardHistory,
    clipboard_publisher: Arc<Mutex<ClipboardPublisher>>,
    _clipboard_command_server: crate::app::ClipboardCommandServer,
    _screenshot_command_server: crate::app::ScreenshotCommandServer,
    screenshot_active: bool,
    airpods_icon_bounds: Rc<RefCell<Bounds<Pixels>>>,
    bluetooth_icon_bounds: Rc<RefCell<Bounds<Pixels>>>,
    network_icon_bounds: Rc<RefCell<Bounds<Pixels>>>,
    controls: ControlChannels,
    control_snapshot: ControlSnapshot,
    bar_display_id: Option<gpui::DisplayId>,
}

const JUMP_MENU_ROW_HEIGHT: f32 = 28.0;
const JUMP_MENU_WIDTH: f32 = 220.0;
const JUMP_MENU_BORDER_WIDTH: f32 = 1.0;
const STATUS_ICON_FRAME_SIZE: f32 = 24.0;
// The status glyphs come from JetBrainsMono Nerd Font Mono. Their outlines
// have different heights even when rendered at the same text size, so scale
// each selected glyph to the 928-unit height of the regular battery glyph.
const STATUS_ICON_REFERENCE_VISIBLE_HEIGHT: f32 = 928.0;
const STATUS_ICON_UNITS_PER_EM: f32 = 1000.0;
// Scale the MDI glyphs to the same 12.1px visible height as `md-battery`
// at the bar's standard 13px status-icon size.
const CPU_ICON_SCALE: f32 = 1.74;
const MEMORY_ICON_SCALE: f32 = 1.55;
const CPU_ICON: &str = "\u{f061a}";
const MEMORY_ICON: &str = "\u{f035b}";
const BLUETOOTH_ICON: &str = "\u{f293}";
const STATUS_TOOLTIP_SHOW_DELAY: Duration = Duration::from_millis(500);
const STATUS_TOOLTIP_FONT_SIZE: f32 = 11.0;
const STATUS_TOOLTIP_LINE_HEIGHT: f32 = 17.0;
const STATUS_TOOLTIP_HORIZONTAL_PADDING: f32 = 8.0;
const STATUS_TOOLTIP_VERTICAL_PADDING: f32 = 5.0;
const STATUS_TOOLTIP_BORDER_WIDTH: f32 = 1.0;
const STATUS_TOOLTIP_ASCII_CHARACTER_WIDTH: f32 = 8.0;
const DEVICE_CONTROL_CENTER_WIDTH: f32 = 900.0;
const DEVICE_CONTROL_CENTER_HEIGHT: f32 = 650.0;

/// A full-monitor, transparent surface used only as the stable Wayland parent
/// for the device-control-center popup.
pub struct DeviceControlCenterAnchor;

impl Render for DeviceControlCenterAnchor {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_input_region(Some(&[]));
        div().size_full()
    }
}

#[derive(Clone, Copy, Debug)]
struct StatusIcon {
    glyph: &'static str,
    visible_height: f32,
}

impl StatusIcon {
    const fn new(glyph: &'static str, visible_height: f32) -> Self {
        Self {
            glyph,
            visible_height,
        }
    }

    fn scale(self) -> f32 {
        STATUS_ICON_REFERENCE_VISIBLE_HEIGHT / self.visible_height
    }

    fn text_size(self, base_size: Pixels) -> Pixels {
        base_size * self.scale()
    }
}

fn volume_icon(status: LevelStatus) -> StatusIcon {
    if !status.available {
        StatusIcon::new("\u{f0581}", 600.0)
    } else if status.muted || status.percent == 0 {
        StatusIcon::new("\u{f075f}", 508.0)
    } else if status.percent <= 33 {
        StatusIcon::new("\u{f057f}", 928.0)
    } else if status.percent <= 66 {
        StatusIcon::new("\u{f0580}", 712.0)
    } else {
        StatusIcon::new("\u{f057e}", 584.0)
    }
}

fn network_icon(status: &ToggleStatus, route: Option<&NetworkRoute>) -> StatusIcon {
    if !status.available || route.is_none() {
        StatusIcon::new("\u{f092c}", 488.0)
    } else if route.is_some_and(|route| route.kind == NetworkKind::Wired) {
        StatusIcon::new("\u{f0200}", 572.0)
    } else if !status.enabled {
        StatusIcon::new("\u{f092c}", 488.0)
    } else {
        match status.signal_strength.unwrap_or(100) {
            0..=25 => StatusIcon::new("\u{f091f}", 476.0),
            26..=50 => StatusIcon::new("\u{f0922}", 476.0),
            51..=75 => StatusIcon::new("\u{f0925}", 476.0),
            _ => StatusIcon::new("\u{f0928}", 476.0),
        }
    }
}

fn bluetooth_icon_visible(status: &ToggleStatus) -> bool {
    status.available && status.enabled && status.connected
}

fn battery_icon(status: &BatteryStatus) -> StatusIcon {
    let percent = status.percent;
    if !status.available {
        return StatusIcon::new("\u{f0091}", 928.0);
    }
    if status.charging {
        return match percent {
            0..=10 => StatusIcon::new("\u{f0084}", 928.0),
            11..=20 => StatusIcon::new("\u{f0086}", 570.0),
            21..=30 => StatusIcon::new("\u{f0087}", 570.0),
            31..=40 => StatusIcon::new("\u{f0088}", 570.0),
            41..=50 => StatusIcon::new("\u{f0089}", 570.0),
            51..=60 => StatusIcon::new("\u{f008a}", 570.0),
            61..=70 => StatusIcon::new("\u{f008b}", 570.0),
            71..=80 => StatusIcon::new("\u{f008c}", 928.0),
            81..=90 => StatusIcon::new("\u{f008d}", 544.0),
            _ => StatusIcon::new("\u{f0085}", 570.0),
        };
    }
    match percent {
        0..=10 => StatusIcon::new("\u{f007a}", 928.0),
        11..=20 => StatusIcon::new("\u{f007b}", 928.0),
        21..=30 => StatusIcon::new("\u{f007c}", 928.0),
        31..=40 => StatusIcon::new("\u{f007d}", 928.0),
        41..=50 => StatusIcon::new("\u{f007e}", 928.0),
        51..=60 => StatusIcon::new("\u{f007f}", 928.0),
        61..=70 => StatusIcon::new("\u{f0080}", 928.0),
        71..=80 => StatusIcon::new("\u{f0081}", 928.0),
        81..=90 => StatusIcon::new("\u{f0082}", 928.0),
        _ => StatusIcon::new("\u{f0079}", 928.0),
    }
}

fn cpu_usage_tooltip(status: CpuStatus) -> String {
    if !status.available {
        return "CPU: 利用不可".to_string();
    }
    if status.core_usages.is_empty() {
        return "CPU: 計測中".to_string();
    }

    status
        .core_usages
        .iter()
        .map(|core| {
            let kind = core
                .kind
                .map(|kind| format!("({})", kind.label()))
                .unwrap_or_default();
            format!(
                "CPU{}{}: {:.1}%",
                core.index,
                kind,
                core.percent_tenths as f32 / 10.0,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn memory_usage_tooltip(status: MemoryStatus) -> String {
    if !status.available {
        return "Memory使用率: 利用不可".to_string();
    }

    const KIB_PER_GIB: f64 = 1024.0 * 1024.0;
    format!(
        "Memory使用率: {}%\n{:.1} GB/{:.1} GB",
        status.percent,
        status.used_kib as f64 / KIB_PER_GIB,
        status.total_kib as f64 / KIB_PER_GIB,
    )
}

fn volume_usage_tooltip(status: LevelStatus) -> String {
    if status.available {
        format!(
            "音量: {}%{}",
            status.percent,
            if status.muted { " (ミュート)" } else { "" }
        )
    } else {
        "音量: 利用不可".to_string()
    }
}

fn network_usage_tooltip(status: &ToggleStatus) -> String {
    let rate = |rate: Option<u64>| {
        rate.map_or_else(|| "計測中".to_string(), |rate| format!("{rate} kbps"))
    };
    format!(
        "ネットワーク\n↓ {}\n↑ {}",
        rate(status.download_kbps),
        rate(status.upload_kbps),
    )
}

fn bluetooth_usage_tooltip(status: &ToggleStatus) -> String {
    if status.available && status.connected {
        format!("Bluetooth\n{}", status.label)
    } else {
        "Bluetooth: 未接続".to_string()
    }
}

fn battery_usage_tooltip(status: &BatteryStatus) -> String {
    if status.available {
        format!(
            "バッテリー\n{}%\n状態: {}\nHealth: {}",
            status.percent, status.state, status.health,
        )
    } else {
        "バッテリー\n利用不可".to_string()
    }
}

struct StatusTooltip {
    text: String,
    theme: BarTheme,
}

impl StatusTooltip {
    fn set_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.text != text {
            let size = status_tooltip_window_size(&text);
            self.text = text;
            window.resize(size);
            cx.notify();
        }
    }
}

impl Render for StatusTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .px(px(STATUS_TOOLTIP_HORIZONTAL_PADDING))
            .py(px(STATUS_TOOLTIP_VERTICAL_PADDING))
            .rounded(self.theme.control_radius)
            .border_1()
            .border_color(self.theme.border)
            .bg(self.theme.surface(SurfaceRole::Floating))
            .text_color(self.theme.foreground)
            .font(ui_font())
            .text_size(px(STATUS_TOOLTIP_FONT_SIZE))
            .line_height(px(STATUS_TOOLTIP_LINE_HEIGHT))
            .whitespace_nowrap()
            .child(self.text.clone())
    }
}

fn status_tooltip_window_size(text: &str) -> Size<Pixels> {
    let line_count = text.lines().count().max(1) as f32;
    let max_columns = text
        .lines()
        .map(|line| {
            line.chars()
                .map(|character| if character.is_ascii() { 1.0 } else { 2.0 })
                .sum::<f32>()
        })
        .fold(0.0, f32::max);

    Size::new(
        px((max_columns * STATUS_TOOLTIP_ASCII_CHARACTER_WIDTH
            + 2.0 * (STATUS_TOOLTIP_HORIZONTAL_PADDING + STATUS_TOOLTIP_BORDER_WIDTH))
            .clamp(72.0, 240.0)),
        px(line_count * STATUS_TOOLTIP_LINE_HEIGHT
            + 2.0 * (STATUS_TOOLTIP_VERTICAL_PADDING + STATUS_TOOLTIP_BORDER_WIDTH)),
    )
}

struct JumpMenu {
    actions: Vec<JumpListAction>,
    position: Point<Pixels>,
}

struct WorkspaceMenu {
    windows: Vec<WorkspaceWindow>,
    position: Point<Pixels>,
}

impl Bar {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ipc_updates: Receiver<IpcUpdate>,
        notification_updates: Receiver<NotificationEvent>,
        notification_sender: Sender<NotificationEvent>,
        notifications: SharedNotificationStore,
        controls: ControlChannels,
        device_control_center_routes: Receiver<crate::app::DeviceControlCenterRoute>,
        device_control_center_route_server: crate::app::DeviceControlCenterRouteServer,
        device_control_center_anchor: WindowHandle<DeviceControlCenterAnchor>,
        window_switcher_commands: Receiver<crate::app::SwitcherCommand>,
        window_switcher_command_server: crate::app::WindowSwitcherCommandServer,
        clipboard: SharedClipboardHistory,
        clipboard_updates: Receiver<()>,
        clipboard_commands: Receiver<crate::app::ClipboardCommand>,
        clipboard_command_server: crate::app::ClipboardCommandServer,
        screenshot_commands: Receiver<()>,
        screenshot_command_server: crate::app::ScreenshotCommandServer,
        theme: BarTheme,
        cx: &mut Context<Self>,
    ) -> Self {
        let bar = Self {
            clock: Clock::new(),
            workspaces: None,
            notification_sender,
            notifications,
            theme,
            jump_menu: None,
            workspace_menu: None,
            jump_menu_resize_pending: false,
            status_tooltip_hover: None,
            status_tooltip_generation: 0,
            status_tooltip_popup: None,
            notification_tray: None,
            notification_tray_dismiss_target: None,
            notification_popup: None,
            network_popover: None,
            device_control_center: None,
            device_control_center_anchor,
            _device_control_center_route_server: device_control_center_route_server,
            pending_device_control_center_route: None,
            airpods_popover: None,
            window_switcher: None,
            _window_switcher_command_server: window_switcher_command_server,
            clipboard_panel: None,
            clipboard,
            clipboard_publisher: Arc::new(Mutex::new(ClipboardPublisher::default())),
            _clipboard_command_server: clipboard_command_server,
            _screenshot_command_server: screenshot_command_server,
            screenshot_active: false,
            network_icon_bounds: Rc::new(RefCell::new(Bounds::default())),
            airpods_icon_bounds: Rc::new(RefCell::new(Bounds::default())),
            bluetooth_icon_bounds: Rc::new(RefCell::new(Bounds::default())),
            controls,
            control_snapshot: ControlSnapshot::default(),
            bar_display_id: None,
        };
        Self::start_clock(cx);
        Self::start_ipc_updates(ipc_updates, cx);
        Self::start_notification_updates(notification_updates, cx);
        Self::start_control_updates(bar.controls.updates.clone(), cx);
        Self::start_wifi_events(bar.controls.wifi_events.clone(), cx);
        Self::start_bluetooth_events(bar.controls.bluetooth_events.clone(), cx);
        Self::start_network_settings_events(bar.controls.network_settings_events.clone(), cx);
        Self::start_device_control_center_routes(device_control_center_routes, cx);
        Self::start_window_switcher_commands(window_switcher_commands, cx);
        Self::start_clipboard_commands(clipboard_commands, cx);
        Self::start_clipboard_updates(clipboard_updates, cx);
        Self::start_screenshot_commands(screenshot_commands, cx);
        bar
    }

    fn start_screenshot_commands(commands: Receiver<()>, cx: &mut Context<Self>) {
        cx.spawn(async move |bar, cx| {
            while commands.recv().await.is_ok() {
                if bar.update(cx, |bar, cx| bar.start_screenshot(cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    fn start_screenshot(&mut self, cx: &mut Context<Self>) {
        if self.screenshot_active {
            info!("ignoring screenshot command while a selection is already active");
            return;
        }
        self.screenshot_active = true;
        let receiver = crate::screenshot::start_capture(self.clipboard_publisher.clone());
        cx.spawn(async move |bar, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = bar.update(cx, |bar, _| {
                bar.screenshot_active = false;
                match result {
                    crate::screenshot::ScreenshotResult::Saved {
                        path,
                        clipboard_copied,
                    } => {
                        if clipboard_copied {
                            info!(
                                "screenshot saved and copied to clipboard: {}",
                                path.display()
                            );
                        } else {
                            info!("screenshot saved: {}", path.display());
                        }
                    }
                    crate::screenshot::ScreenshotResult::Cancelled => {
                        info!("screenshot selection cancelled");
                    }
                    crate::screenshot::ScreenshotResult::Failed(error) => {
                        error!("screenshot failed: {error:#}");
                    }
                }
            });
        })
        .detach();
    }

    fn start_clipboard_commands(
        commands: Receiver<crate::app::ClipboardCommand>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |bar, cx| {
            while let Ok(command) = commands.recv().await {
                if bar
                    .update(cx, |bar, cx| bar.handle_clipboard_command(command, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    fn start_clipboard_updates(updates: Receiver<()>, cx: &mut Context<Self>) {
        cx.spawn(async move |bar, cx| {
            while updates.recv().await.is_ok() {
                if bar
                    .update(cx, |bar, cx| {
                        if let Some(panel) = bar.clipboard_panel {
                            let _ = panel.update(cx, |panel, _, cx| panel.refresh(cx));
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    fn start_window_switcher_commands(
        commands: Receiver<crate::app::SwitcherCommand>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |bar, cx| {
            while let Ok(command) = commands.recv().await {
                if bar
                    .update(cx, |bar, cx| bar.handle_switcher_command(command, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    fn start_device_control_center_routes(
        routes: Receiver<crate::app::DeviceControlCenterRoute>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |bar, cx| {
            while let Ok(route) = routes.recv().await {
                if bar
                    .update(cx, |bar, cx| {
                        bar.pending_device_control_center_route = Some(route);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    fn start_clock(cx: &mut Context<Self>) {
        cx.spawn(async move |bar, cx| {
            loop {
                if bar
                    .update(cx, |bar, cx| {
                        bar.clock.tick();
                        let expired = bar.notification_store().expire(std::time::Instant::now());
                        for id in expired {
                            emit_notification_closed(id, CloseReason::Expired);
                        }
                        if let Some(popup) = bar.notification_popup {
                            let _ = popup.update(cx, |_, _, cx| cx.notify());
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        })
        .detach();
    }

    fn start_ipc_updates(updates: Receiver<IpcUpdate>, cx: &mut Context<Self>) {
        cx.spawn(async move |bar, cx| {
            while let Ok(first_update) = updates.recv().await {
                if bar
                    .update(cx, |bar, cx| {
                        bar.apply_ipc_update(first_update);
                        while let Ok(update) = updates.try_recv() {
                            bar.apply_ipc_update(update);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    fn start_notification_updates(updates: Receiver<NotificationEvent>, cx: &mut Context<Self>) {
        cx.spawn(async move |bar, cx| {
            while let Ok(first_update) = updates.recv().await {
                if bar
                    .update(cx, |bar, cx| {
                        let mut notifications = bar.notification_store();
                        // D-Bus Notify applies to the shared store before it
                        // wakes the UI. Tray-originated close/clear events are
                        // still applied here on the GPUI thread.
                        if !matches!(first_update, NotificationEvent::Upsert(_)) {
                            notifications.apply(first_update);
                        }
                        while let Ok(update) = updates.try_recv() {
                            if !matches!(update, NotificationEvent::Upsert(_)) {
                                notifications.apply(update);
                            }
                        }
                        drop(notifications);

                        if let Some(tray) = bar.notification_tray {
                            let _ = tray.update(cx, |_, _, cx| cx.notify());
                        }
                        bar.show_notification_popup(cx);
                        if let Some(popup) = bar.notification_popup {
                            let _ = popup.update(cx, |_, _, cx| cx.notify());
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    fn start_wifi_events(
        events: Receiver<crate::modules::system_controls::WifiConnectionEvent>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |bar, cx| {
            while let Ok(event) = events.recv().await {
                if bar
                    .update(cx, |bar, cx| {
                        if let Some(popover) = bar.network_popover {
                            let _ = popover.update(cx, |popover, window, cx| {
                                popover.apply_wifi_event(event.clone(), window, cx);
                            });
                        }
                        if let Some(center) = bar.device_control_center {
                            let _ = center.update(cx, |center, _, cx| {
                                center.apply_wifi_update(event.clone(), cx);
                            });
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    fn start_bluetooth_events(
        events: Receiver<crate::modules::system_controls::BluetoothEvent>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |bar, cx| {
            while let Ok(event) = events.recv().await {
                if bar
                    .update(cx, |bar, cx| {
                        if let Some(center) = bar.device_control_center {
                            let _ = center.update(cx, |center, _, cx| {
                                center.apply_bluetooth_update(event.clone(), cx);
                            });
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    fn start_network_settings_events(
        events: Receiver<crate::modules::system_controls::NetworkSettingsEvent>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |bar, cx| {
            while let Ok(event) = events.recv().await {
                if bar
                    .update(cx, |bar, cx| {
                        if let Some(center) = bar.device_control_center {
                            let _ = center.update(cx, |center, _, cx| {
                                center.apply_network_settings_update(event.clone(), cx);
                            });
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    fn start_control_updates(updates: Receiver<ControlSnapshot>, cx: &mut Context<Self>) {
        cx.spawn(async move |bar, cx| {
            while let Ok(snapshot) = updates.recv().await {
                if bar
                    .update(cx, |bar, cx| {
                        bar.control_snapshot = snapshot.clone();
                        if let Some(tray) = bar.notification_tray {
                            let _ = tray.update(cx, |tray, _, cx| {
                                tray.set_controls(snapshot.clone(), cx);
                            });
                        }
                        if let Some(popover) = bar.network_popover {
                            let _ = popover.update(cx, |popover, window, cx| {
                                popover.set_controls(snapshot.clone(), window, cx);
                            });
                        }
                        if let Some(center) = bar.device_control_center {
                            let _ = center.update(cx, |center, _, cx| {
                                center.set_snapshot(snapshot.clone(), cx);
                            });
                        }
                        if let Some(popover) = bar.airpods_popover {
                            let _ = popover.update(cx, |popover, window, cx| {
                                popover.set_controls(snapshot.clone(), window, cx);
                            });
                        }
                        if !snapshot.airpods.connected {
                            bar.close_airpods_popover(cx);
                        }
                        bar.refresh_status_tooltip(cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    fn apply_ipc_update(&mut self, update: IpcUpdate) {
        match update {
            IpcUpdate::Workspaces(snapshot) => {
                if let Some(current) = &mut self.workspaces {
                    let workspace_changed = current.replace(snapshot, self.theme);
                    if workspace_changed
                        && (self.jump_menu.take().is_some() || self.workspace_menu.take().is_some())
                    {
                        self.jump_menu_resize_pending = true;
                    }
                } else {
                    self.workspaces = Some(Workspaces::new(snapshot));
                }
            }
            IpcUpdate::Unavailable(message) => {
                warn!("Hyprland IPC unavailable; continuing with clock only: {message}");
            }
            IpcUpdate::WorkerStopped(message) => {
                error!("Hyprland IPC worker stopped: {message}");
            }
        }
    }

    fn handle_switcher_command(
        &mut self,
        command: crate::app::SwitcherCommand,
        cx: &mut Context<Self>,
    ) {
        match command {
            crate::app::SwitcherCommand::Open => self.open_window_switcher(None, cx),
            crate::app::SwitcherCommand::Cycle => self.open_window_switcher(Some(1), cx),
            crate::app::SwitcherCommand::CycleReverse => self.open_window_switcher(Some(-1), cx),
            crate::app::SwitcherCommand::SelectPrevious => self.cycle_window_switcher(-1, cx),
            crate::app::SwitcherCommand::SelectNext => self.cycle_window_switcher(1, cx),
            crate::app::SwitcherCommand::Commit => self.commit_window_switcher(cx),
            crate::app::SwitcherCommand::Close => self.close_window_switcher(cx),
            // The regular IPC worker continuously refreshes the cache. Keeping this command
            // makes the CLI interface symmetric with Altab and useful for debugging.
            crate::app::SwitcherCommand::Refresh => cx.notify(),
        }
    }

    fn switcher_state(&self) -> Option<(SwitcherState, Option<String>)> {
        let (windows, active_address, focused_monitor) = self.workspaces.as_ref()?.switcher_state();
        Some((SwitcherState::new(windows, active_address), focused_monitor))
    }

    fn open_window_switcher(&mut self, initial_step: Option<isize>, cx: &mut Context<Self>) {
        let Some((state, focused_monitor)) = self.switcher_state() else {
            warn!("window switcher requested before Hyprland state was available");
            return;
        };
        if state.windows.is_empty() {
            return;
        }

        if let Some(switcher) = self.window_switcher {
            let _ = switcher.update(cx, |switcher, window, cx| {
                if !switcher.is_open() {
                    switcher.open(state, window, cx);
                }
                if let Some(step) = initial_step {
                    switcher.cycle(step, cx);
                }
            });
            return;
        }

        let theme = self.theme;
        // A switch belongs to the output that owned focus when the candidate
        // list was frozen. Hyprland output names and GPUI displays share the
        // deterministic UUID mapping used by the wallpaper layers.
        let display_id = focused_monitor
            .as_deref()
            .and_then(|output| crate::app::display_id_for_output(cx, output))
            .or(self.bar_display_id);
        match cx.open_window(
            WindowOptions {
                titlebar: None,
                focus: false,
                show: true,
                is_movable: false,
                is_resizable: false,
                display_id,
                app_id: Some("bah-window-switcher".to_string()),
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Client),
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: Size::new(px(1.0), px(1.0)),
                })),
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: "bah".to_string(),
                    layer: Layer::Overlay,
                    anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                    exclusive_zone: Some(px(0.0)),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |_, cx| cx.new(|_| WindowSwitcher::new(theme)),
        ) {
            Ok(switcher) => {
                let _ = switcher.update(cx, |switcher, window, cx| {
                    switcher.open(state, window, cx);
                    if let Some(step) = initial_step {
                        switcher.cycle(step, cx);
                    }
                });
                self.window_switcher = Some(switcher);
                info!("window switcher surface opened");
            }
            Err(error) => error!("failed to create window switcher surface: {error}"),
        }
    }

    fn cycle_window_switcher(&mut self, step: isize, cx: &mut Context<Self>) {
        if let Some(switcher) = self.window_switcher {
            let _ = switcher.update(cx, |switcher, _, cx| switcher.cycle(step, cx));
        }
    }

    fn close_window_switcher(&mut self, cx: &mut Context<Self>) {
        if let Some(switcher) = self.window_switcher {
            let _ = switcher.update(cx, |switcher, window, cx| switcher.close(window, cx));
        }
    }

    fn commit_window_switcher(&mut self, cx: &mut Context<Self>) {
        let address = self.window_switcher.and_then(|switcher| {
            switcher
                .read_with(cx, |switcher, _| switcher.selected_address())
                .ok()
                .flatten()
        });
        self.close_window_switcher(cx);
        if let Some(address) = address {
            focus_window(address);
        }
    }

    fn handle_clipboard_command(
        &mut self,
        command: crate::app::ClipboardCommand,
        cx: &mut Context<Self>,
    ) {
        match command {
            crate::app::ClipboardCommand::Toggle => {
                let already_open = self
                    .clipboard_panel
                    .and_then(|panel| panel.read_with(cx, |panel, _| panel.is_open()).ok())
                    .unwrap_or(false);
                if already_open {
                    self.close_clipboard_panel(cx);
                } else {
                    self.open_clipboard_panel(cx);
                }
            }
            crate::app::ClipboardCommand::Open => self.open_clipboard_panel(cx),
            crate::app::ClipboardCommand::Close => self.close_clipboard_panel(cx),
            crate::app::ClipboardCommand::Previous => {
                if let Some(panel) = self.clipboard_panel {
                    let _ = panel.update(cx, |panel, _, cx| panel.select_previous(cx));
                }
            }
            crate::app::ClipboardCommand::Next => {
                if let Some(panel) = self.clipboard_panel {
                    let _ = panel.update(cx, |panel, _, cx| panel.select_next(cx));
                }
            }
            crate::app::ClipboardCommand::Select => {
                if let Some(panel) = self.clipboard_panel {
                    let _ = panel.update(cx, |panel, window, cx| panel.choose_selected(window, cx));
                }
            }
            crate::app::ClipboardCommand::Clear => {
                let cleared = self
                    .clipboard
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .clear();
                if let Err(error) = cleared {
                    warn!("failed to clear clipboard history: {error:#}");
                }
                if let Some(panel) = self.clipboard_panel {
                    let _ = panel.update(cx, |panel, _, cx| panel.refresh(cx));
                }
            }
        }
    }

    fn open_clipboard_panel(&mut self, cx: &mut Context<Self>) {
        let (target_window, focused_monitor) = self
            .workspaces
            .as_ref()
            .map(|workspaces| {
                let (_, active, monitor) = workspaces.switcher_state();
                (active, monitor)
            })
            .unwrap_or((None, None));
        if let Some(panel) = self.clipboard_panel {
            let _ = panel.update(cx, |panel, window, cx| {
                if !panel.is_open() {
                    panel.open(target_window, window, cx);
                }
            });
            set_keybind_submap("clipboard");
            return;
        }
        let theme = self.theme;
        let history = self.clipboard.clone();
        let publisher = self.clipboard_publisher.clone();
        let display_id = focused_monitor
            .as_deref()
            .and_then(|output| crate::app::display_id_for_output(cx, output))
            .or(self.bar_display_id);
        match cx.open_window(
            WindowOptions {
                titlebar: None,
                focus: false,
                show: true,
                is_movable: false,
                is_resizable: false,
                display_id,
                app_id: Some("bah-clipboard".to_string()),
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Client),
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: Size::new(px(1.0), px(1.0)),
                })),
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: "bah-clipboard".to_string(),
                    layer: Layer::Overlay,
                    anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                    exclusive_zone: Some(px(0.0)),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |_, cx| cx.new(|cx| ClipboardPanel::new(history, publisher, theme, cx)),
        ) {
            Ok(panel) => {
                let _ = panel.update(cx, |panel, window, cx| {
                    panel.open(target_window, window, cx)
                });
                self.clipboard_panel = Some(panel);
                set_keybind_submap("clipboard");
                info!("clipboard panel surface opened");
            }
            Err(error) => error!("failed to create clipboard panel surface: {error}"),
        }
    }

    fn close_clipboard_panel(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.clipboard_panel {
            let _ = panel.update(cx, |panel, window, cx| panel.close(window, cx));
        }
    }

    fn notification_store(&self) -> MutexGuard<'_, NotificationStore> {
        self.notifications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn show_notification_popup(&mut self, cx: &mut Context<Self>) {
        if let Some(popup) = self.notification_popup {
            let _ = popup.update(cx, |_, _, cx| cx.notify());
            return;
        }
        let store = self.notification_store();
        if !store.config().enabled {
            return;
        }
        let displayed_count = store.displayed_count();
        if displayed_count == 0 {
            return;
        }
        let popup_width = store.config().popup_width;
        drop(store);
        let popup_height = NotificationPopupStack::height_for(displayed_count);
        let notifications = self.notifications.clone();
        let sender = self.notification_sender.clone();
        let theme = self.theme;
        match cx.open_window(
            WindowOptions {
                titlebar: None,
                focus: false,
                show: true,
                is_movable: false,
                is_resizable: false,
                app_id: Some("bah-notification-popup".to_string()),
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Client),
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: Size::new(px(popup_width), px(popup_height)),
                })),
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: "bah-notification-popup".to_string(),
                    layer: Layer::Overlay,
                    anchor: Anchor::TOP | Anchor::RIGHT,
                    exclusive_zone: Some(px(0.0)),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |_, cx| cx.new(|_| NotificationPopupStack::new(notifications, sender, theme)),
        ) {
            Ok(popup) => {
                self.notification_popup = Some(popup);
                info!("notification popup surface opened");
            }
            Err(error) => error!("failed to create notification popup: {error}"),
        }
    }

    fn show_notification_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tray) = self.notification_tray {
            // The tray is hidden rather than destroyed, so reopening it only
            // restores its input region and redraws the existing layer surface.
            let _ = tray.update(cx, |tray, window, cx| tray.show(window, cx));
            return;
        }

        let display = window.display(cx);
        let display_id = display.as_ref().map(|display| display.id());
        let tray_width = display.as_ref().map_or(px(576.0), |display| {
            display.bounds().size.width * TRAY_PANEL_WIDTH_RATIO
        });
        let tray_size = Size::new(tray_width, px(1.0));
        let notifications = self.notifications.clone();
        let notification_sender = self.notification_sender.clone();
        let theme = self.theme;
        let control_actions = self.controls.actions.clone();
        let control_snapshot = self.control_snapshot.clone();

        // This surface is static during the animation. It exists only so a
        // click outside the narrow tray surface can dismiss the tray.
        let dismiss_target = match cx.open_window(
            WindowOptions {
                titlebar: None,
                focus: false,
                show: true,
                is_movable: false,
                is_resizable: false,
                app_id: Some("bah-notification-tray-dismiss".to_string()),
                display_id,
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Client),
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: Size::new(px(1.0), px(1.0)),
                })),
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: "bah-notification-tray-dismiss".to_string(),
                    layer: Layer::Top,
                    anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                    exclusive_zone: Some(px(0.0)),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| cx.new(|_| NotificationTrayDismissTarget::new()),
        ) {
            Ok(handle) => handle,
            Err(error) => {
                error!("failed to create notification tray dismiss target: {error}");
                return;
            }
        };

        match cx.open_window(
            WindowOptions {
                titlebar: None,
                focus: false,
                show: true,
                is_movable: false,
                is_resizable: false,
                app_id: Some("bah-notification-tray".to_string()),
                display_id,
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Client),
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: tray_size,
                })),
                kind: WindowKind::LayerShell(LayerShellOptions {
                    // Match the bar's existing `no_anim` rule so Hyprland
                    // does not add its own layer pop-in over this animation.
                    namespace: "bah".to_string(),
                    layer: Layer::Overlay,
                    anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::RIGHT,
                    exclusive_zone: Some(px(0.0)),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |_, cx| {
                cx.new(|cx| {
                    NotificationTray::new(
                        notifications,
                        notification_sender,
                        control_snapshot,
                        control_actions,
                        dismiss_target,
                        theme,
                        cx,
                    )
                })
            },
        ) {
            Ok(handle) => {
                let _ = dismiss_target.update(cx, |target, _, _| target.set_tray(handle));
                self.notification_tray = Some(handle);
                self.notification_tray_dismiss_target = Some(dismiss_target);
                info!("notification tray opened");
            }
            Err(error) => {
                let _ = dismiss_target.update(cx, |target, window, _| target.hide(window));
                self.notification_tray_dismiss_target = Some(dismiss_target);
                error!("failed to create notification tray: {error}");
            }
        }
    }

    fn show_jump_list(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let actions = self
            .workspaces
            .as_ref()
            .map(|workspaces| workspaces.jump_list_actions().to_vec())
            .unwrap_or_default();
        if actions.is_empty() {
            return;
        }

        info!("showing jump list ({} item(s))", actions.len());
        for action in &actions {
            info!("jump-list item: {}", action.label);
        }

        self.jump_menu = Some(JumpMenu {
            actions,
            position: event.position,
        });
        self.workspace_menu = None;
        self.jump_menu_resize_pending = false;
        self.resize_for_jump_menu(window, cx);
        cx.notify();
    }

    fn launch_jump_list_action(
        &mut self,
        action: JumpListAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        launch_jump_list_action(action);
        self.jump_menu = None;
        self.jump_menu_resize_pending = false;
        self.resize_for_jump_menu(window, cx);
        cx.notify();
    }

    fn hide_jump_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.jump_menu.take().is_some() || self.workspace_menu.take().is_some() {
            self.jump_menu_resize_pending = false;
            self.resize_for_jump_menu(window, cx);
            cx.notify();
        }
    }

    fn set_status_tooltip_hovered(
        &mut self,
        id: &'static str,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !hovered {
            // Ignore a late Leave from the previous status item after the
            // pointer has already entered an adjacent item.
            if self.status_tooltip_hover == Some(id) {
                self.status_tooltip_hover = None;
                self.status_tooltip_generation = self.status_tooltip_generation.wrapping_add(1);
                self.close_status_tooltip(cx);
            }
            return;
        }

        self.status_tooltip_hover = Some(id);
        self.status_tooltip_generation = self.status_tooltip_generation.wrapping_add(1);
        let generation = self.status_tooltip_generation;
        self.close_status_tooltip(cx);

        let parent = window.window_handle();
        let anchor_position = window.mouse_position();
        let theme = self.theme;
        cx.spawn(async move |bar, cx| {
            cx.background_executor()
                .timer(STATUS_TOOLTIP_SHOW_DELAY)
                .await;
            let _ = bar.update(cx, |bar, cx| {
                if bar.status_tooltip_hover != Some(id)
                    || bar.status_tooltip_generation != generation
                {
                    return;
                }

                let text = bar.status_tooltip_text(id);
                let size = status_tooltip_window_size(&text);
                match cx.open_window(
                    WindowOptions {
                        titlebar: None,
                        focus: false,
                        app_id: Some("bah-status-tooltip".to_string()),
                        window_background: WindowBackgroundAppearance::Transparent,
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: Point::default(),
                            size,
                        })),
                        kind: WindowKind::AnchoredPopup(PopupOptions {
                            parent,
                            anchor_rect: Bounds {
                                origin: anchor_position,
                                size: Size::default(),
                            },
                            anchor: PopupAnchor::Bottom,
                            gravity: PopupGravity::BottomLeft,
                            constraint_adjustment: PopupConstraintAdjustment::SLIDE_X
                                | PopupConstraintAdjustment::FLIP_Y,
                            offset: point(px(0.0), px(6.0)),
                            grab: false,
                        }),
                        ..Default::default()
                    },
                    move |_, cx| cx.new(|_| StatusTooltip { text, theme }),
                ) {
                    Ok(popup) => bar.status_tooltip_popup = Some(popup),
                    Err(error) => warn!("failed to open status tooltip: {error}"),
                }
            });
        })
        .detach();
    }

    fn close_status_tooltip(&mut self, cx: &mut Context<Self>) {
        if let Some(popup) = self.status_tooltip_popup.take() {
            let _ = popup.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn show_network_popover(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(popover) = self.network_popover {
            if popover.update(cx, |_, _, _| {}).is_ok() {
                self.close_network_popover(cx);
                return;
            }
            self.network_popover = None;
        }
        self.close_status_tooltip(cx);
        let size = network_popover_window_size(&self.control_snapshot, &[]);
        let parent = window.window_handle();
        let measured_anchor = *self.network_icon_bounds.borrow();
        let anchor_rect = if measured_anchor.size.width > px(0.0) {
            measured_anchor
        } else {
            // This fallback is only possible before the first prepaint.
            Bounds {
                origin: point(event.position.x - px(12.0), event.position.y - px(12.0)),
                size: Size::new(px(24.0), px(24.0)),
            }
        };
        let actions = self.controls.actions.clone();
        let theme = self.theme;
        let controls = self.control_snapshot.clone();
        match cx.open_window(
            WindowOptions {
                titlebar: None,
                focus: true,
                app_id: Some("bah-network-popover".to_string()),
                window_background: WindowBackgroundAppearance::Transparent,
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size,
                })),
                kind: WindowKind::AnchoredPopup(PopupOptions {
                    parent,
                    anchor_rect,
                    anchor: PopupAnchor::Bottom,
                    gravity: PopupGravity::Bottom,
                    constraint_adjustment: PopupConstraintAdjustment::SLIDE_X,
                    offset: point(px(0.0), px(2.0)),
                    grab: true,
                }),
                ..Default::default()
            },
            move |_, cx| cx.new(|_| NetworkPopover::new(controls, actions, theme)),
        ) {
            Ok(popover) => {
                self.network_popover = Some(popover);
                let _ = self.controls.actions.try_send(
                    crate::modules::system_controls::ControlAction::SetWifiDiscovery(true),
                );
            }
            Err(error) => warn!("failed to open network popover: {error}"),
        }
    }

    fn close_network_popover(&mut self, cx: &mut Context<Self>) {
        if let Some(popover) = self.network_popover.take() {
            let _ = popover.update(cx, |_, window, _| window.remove_window());
        }
        let _ = self
            .controls
            .actions
            .try_send(crate::modules::system_controls::ControlAction::SetWifiDiscovery(false));
    }

    fn show_airpods_popover(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(popover) = self.airpods_popover {
            if popover.update(cx, |_, _, _| {}).is_ok() {
                self.close_airpods_popover(cx);
                return;
            }
            self.airpods_popover = None;
        }
        self.close_status_tooltip(cx);
        let measured_anchor = *self.airpods_icon_bounds.borrow();
        let anchor_rect = if measured_anchor.size.width > px(0.0) {
            measured_anchor
        } else {
            Bounds {
                origin: point(event.position.x - px(12.0), event.position.y - px(12.0)),
                size: Size::new(px(24.0), px(24.0)),
            }
        };
        let parent = window.window_handle();
        let actions = self.controls.actions.clone();
        let controls = self.control_snapshot.clone();
        let theme = self.theme;
        match cx.open_window(
            WindowOptions {
                titlebar: None,
                focus: true,
                app_id: Some("bah-airpods-popover".to_string()),
                window_background: WindowBackgroundAppearance::Transparent,
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: airpods_popover_window_size(),
                })),
                kind: WindowKind::AnchoredPopup(PopupOptions {
                    parent,
                    anchor_rect,
                    anchor: PopupAnchor::Bottom,
                    gravity: PopupGravity::Bottom,
                    constraint_adjustment: PopupConstraintAdjustment::SLIDE_X
                        | PopupConstraintAdjustment::FLIP_Y,
                    offset: point(px(0.0), px(2.0)),
                    grab: true,
                }),
                ..Default::default()
            },
            move |_, cx| cx.new(|_| AirPodsPopover::new(controls, actions, theme)),
        ) {
            Ok(popover) => self.airpods_popover = Some(popover),
            Err(error) => warn!("failed to open AirPods popover: {error}"),
        }
    }

    fn close_airpods_popover(&mut self, cx: &mut Context<Self>) {
        if let Some(popover) = self.airpods_popover.take() {
            let _ = popover.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn show_device_control_center(
        &mut self,
        page: crate::app::DeviceControlCenterPage,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        crate::app::request_device_control_center(crate::app::DeviceControlCenterRoute {
            page,
            ssid: None,
        });
    }

    fn open_device_control_center(
        &mut self,
        page: crate::app::DeviceControlCenterPage,
        ssid: Option<Vec<u8>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let route = crate::app::DeviceControlCenterRoute { page, ssid };
        let controls = self.controls.clone();
        let snapshot = self.control_snapshot.clone();
        let theme = self.theme;
        let parent = self.device_control_center_anchor.into();
        let display_size = window
            .display(cx)
            .map_or(window.viewport_size(), |display| display.bounds().size);
        let anchor_size = self
            .device_control_center_anchor
            .update(cx, |_, anchor_window, _| anchor_window.viewport_size())
            .unwrap_or(display_size);
        let popover_top_left = point(
            anchor_size.width / 2.0 - px(DEVICE_CONTROL_CENTER_WIDTH / 2.0),
            anchor_size.height / 2.0 - px(DEVICE_CONTROL_CENTER_HEIGHT / 2.0),
        );
        match cx.open_window(
            WindowOptions {
                titlebar: None,
                focus: true,
                app_id: Some("bah-device-control-center".to_string()),
                window_background: WindowBackgroundAppearance::Transparent,
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: Size::new(
                        px(DEVICE_CONTROL_CENTER_WIDTH),
                        px(DEVICE_CONTROL_CENTER_HEIGHT),
                    ),
                })),
                kind: WindowKind::AnchoredPopup(PopupOptions {
                    parent,
                    anchor_rect: Bounds {
                        origin: point(anchor_size.width / 2.0, anchor_size.height / 2.0),
                        size: Size::new(px(1.0), px(1.0)),
                    },
                    anchor: PopupAnchor::Center,
                    gravity: PopupGravity::Center,
                    constraint_adjustment: PopupConstraintAdjustment::SLIDE_X
                        | PopupConstraintAdjustment::SLIDE_Y,
                    offset: Point::default(),
                    grab: true,
                }),
                ..Default::default()
            },
            move |_, cx| {
                cx.new(|cx| {
                    crate::device_control_center::DeviceControlCenter::new_popover(
                        controls, snapshot, route, theme, cx,
                    )
                })
            },
        ) {
            Ok(center) => {
                info!(
                    "device control center popover opened: left_top=({:.1}, {:.1}), size=({:.1}, {:.1}), output=({:.1}, {:.1}), anchor=({:.1}, {:.1}), scale={:.2}",
                    f32::from(popover_top_left.x),
                    f32::from(popover_top_left.y),
                    DEVICE_CONTROL_CENTER_WIDTH,
                    DEVICE_CONTROL_CENTER_HEIGHT,
                    f32::from(display_size.width),
                    f32::from(display_size.height),
                    f32::from(anchor_size.width),
                    f32::from(anchor_size.height),
                    window.scale_factor(),
                );
                self.device_control_center = Some(center);
            }
            Err(error) => warn!("failed to open device control center: {error}"),
        }
    }

    fn show_device_control_center_route(
        &mut self,
        route: crate::app::DeviceControlCenterRoute,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        crate::app::request_device_control_center(route);
    }

    fn close_device_control_center(&mut self, cx: &mut Context<Self>) {
        if let Some(center) = self.device_control_center.take() {
            let _ = center.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn status_tooltip_text(&self, id: &str) -> String {
        match id {
            "cpu" => cpu_usage_tooltip(self.control_snapshot.cpu.clone()),
            "memory" => memory_usage_tooltip(self.control_snapshot.memory),
            "volume" => volume_usage_tooltip(self.control_snapshot.audio_output),
            "bluetooth" => bluetooth_usage_tooltip(&self.control_snapshot.bluetooth),
            "network" => network_usage_tooltip(&self.control_snapshot.wifi),
            "battery" => battery_usage_tooltip(&self.control_snapshot.battery),
            _ => "利用不可".to_string(),
        }
    }

    fn refresh_status_tooltip(&mut self, cx: &mut Context<Self>) {
        let (Some(id), Some(popup)) = (self.status_tooltip_hover, self.status_tooltip_popup) else {
            return;
        };
        let text = self.status_tooltip_text(id);
        let _ = popup.update(cx, |tooltip, window, cx| tooltip.set_text(text, window, cx));
    }

    fn resize_for_jump_menu(&self, window: &mut Window, cx: &App) {
        let menu_height = self.jump_menu.as_ref().map_or_else(
            || {
                self.workspace_menu.as_ref().map_or(0.0, |menu| {
                    menu.windows.len() as f32
                        * (JUMP_MENU_ROW_HEIGHT * 2.0 + JUMP_MENU_BORDER_WIDTH)
                        + JUMP_MENU_BORDER_WIDTH
                })
            },
            |menu| menu.actions.len() as f32 * JUMP_MENU_ROW_HEIGHT + 2.0 * JUMP_MENU_BORDER_WIDTH,
        );

        // A layer-shell surface only receives pointer events within its bounds.
        // While a menu is open, extend it over the output so the transparent
        // area acts as a dismissal target for clicks outside the menu.
        let height = if menu_height > 0.0 {
            window
                .display(cx)
                .map_or(self.theme.bar_height + px(menu_height), |display| {
                    display.bounds().size.height
                })
        } else {
            self.theme.bar_height
        };
        window.resize(Size::new(window.bounds().size.width, height));
    }

    fn show_workspace_menu(
        &mut self,
        workspace_id: i32,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let windows = self
            .workspaces
            .as_ref()
            .map(|workspaces| workspaces.windows_for_workspace(workspace_id))
            .unwrap_or_default();
        if windows.is_empty() {
            return;
        }

        info!(
            "showing window menu for workspace {workspace_id} ({} window(s))",
            windows.len()
        );
        self.workspace_menu = Some(WorkspaceMenu {
            windows,
            position: event.position,
        });
        self.jump_menu = None;
        self.jump_menu_resize_pending = false;
        self.resize_for_jump_menu(window, cx);
        cx.notify();
    }

    fn close_workspace_window(
        &mut self,
        address: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        close_window(address);
        self.hide_jump_list(window, cx);
    }
}

impl Render for Bar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.bar_display_id = window.display(cx).as_ref().map(|display| display.id());
        if let Some(route) = self.pending_device_control_center_route.take() {
            self.show_device_control_center_route(route, window, cx);
        }
        if self.jump_menu_resize_pending {
            self.resize_for_jump_menu(window, cx);
            self.jump_menu_resize_pending = false;
        }
        let theme = self.theme;
        let notification_count = self.notification_store().count();
        let volume_icon = volume_icon(self.control_snapshot.audio_output);
        let primary_network = self.control_snapshot.primary_network.as_ref();
        let network_icon = network_icon(&self.control_snapshot.wifi, primary_network);
        let bluetooth_icon_visible = bluetooth_icon_visible(&self.control_snapshot.bluetooth);
        let bluetooth_icon_bounds = self.bluetooth_icon_bounds.clone();
        let airpods_icon_visible = self.control_snapshot.airpods.connected;
        let airpods_icon_bounds = self.airpods_icon_bounds.clone();
        let network_icon_bounds = self.network_icon_bounds.clone();
        let battery_icon = battery_icon(&self.control_snapshot.battery);
        let cpu_usage = if self.control_snapshot.cpu.available {
            format!("{}%", self.control_snapshot.cpu.percent)
        } else {
            "—%".to_string()
        };
        let memory_usage = if self.control_snapshot.memory.available {
            format!("{}%", self.control_snapshot.memory.percent)
        } else {
            "—%".to_string()
        };
        let status_icon_size = theme.clock_font_size;
        let bluetooth_icon_size = status_icon_size + px(2.0);
        let airpods_icon_size = status_icon_size + px(3.0);
        let volume_icon_size = volume_icon.text_size(status_icon_size);
        let cpu_icon_size = status_icon_size * CPU_ICON_SCALE;
        let memory_icon_size = status_icon_size * MEMORY_ICON_SCALE;
        let network_icon_size = network_icon.text_size(status_icon_size);
        let battery_icon_size = battery_icon.text_size(status_icon_size);
        let left = self
            .workspaces
            .as_ref()
            .map(|workspaces| {
                workspaces.render(
                    theme,
                    |workspace_id| {
                        Box::new(move |_, _, _| {
                            info!("workspace {workspace_id} left-clicked");
                            switch_to_workspace(workspace_id);
                        })
                    },
                    Some(Box::new(cx.listener(|this, event, window, cx| {
                        this.show_jump_list(event, window, cx);
                        cx.stop_propagation();
                    }))),
                    |workspace_id| {
                        Box::new(cx.listener(move |this, event, window, cx| {
                            this.show_workspace_menu(workspace_id, event, window, cx);
                            cx.stop_propagation();
                        }))
                    },
                )
            })
            .unwrap_or_else(div);

        div()
            .size_full()
            .relative()
            .on_any_mouse_down(cx.listener(|this, _, window, cx| {
                this.hide_jump_list(window, cx);
                this.close_network_popover(cx);
                this.close_airpods_popover(cx);
                this.close_device_control_center(cx);
            }))
            .child(
                div()
                    .h(theme.bar_height)
                    .flex()
                    .items_center()
                    .px(theme.horizontal_padding)
                    .gap(theme.module_spacing)
                    .bg(theme.surface(SurfaceRole::Shell))
                    .border_b_1()
                    .border_color(theme.border)
                    .text_color(theme.foreground)
                    .font(ui_font())
                    .child(left)
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .id("status-cpu")
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .on_hover(cx.listener(move |this, hovered, window, cx| {
                                        this.set_status_tooltip_hovered(
                                            "cpu", *hovered, window, cx,
                                        );
                                    }))
                                    .child(
                                        div()
                                            .size(px(STATUS_ICON_FRAME_SIZE))
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_size(cpu_icon_size)
                                            .child(CPU_ICON),
                                    )
                                    .child(div().text_size(theme.clock_font_size).child(cpu_usage)),
                            )
                            .child(
                                div()
                                    .id("status-memory")
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .on_hover(cx.listener(move |this, hovered, window, cx| {
                                        this.set_status_tooltip_hovered(
                                            "memory", *hovered, window, cx,
                                        );
                                    }))
                                    .child(
                                        div()
                                            .size(px(STATUS_ICON_FRAME_SIZE))
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_size(memory_icon_size)
                                            .child(MEMORY_ICON),
                                    )
                                    .child(
                                        div().text_size(theme.clock_font_size).child(memory_usage),
                                    ),
                            )
                            .child(
                                div()
                                    .id("status-volume")
                                    .size(px(STATUS_ICON_FRAME_SIZE))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(volume_icon_size)
                                    .on_hover(cx.listener(move |this, hovered, window, cx| {
                                        this.set_status_tooltip_hovered(
                                            "volume", *hovered, window, cx,
                                        );
                                    }))
                                    .child(volume_icon.glyph),
                            )
                            .when(bluetooth_icon_visible, |status| {
                                status.child(
                                    div()
                                        .id("status-bluetooth")
                                        .size(px(STATUS_ICON_FRAME_SIZE))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(bluetooth_icon_size)
                                        .on_hover(cx.listener(move |this, hovered, window, cx| {
                                            this.set_status_tooltip_hovered(
                                                "bluetooth",
                                                *hovered,
                                                window,
                                                cx,
                                            );
                                        }))
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _event, window, cx| {
                                                this.show_device_control_center(
                                                    crate::app::DeviceControlCenterPage::Bluetooth,
                                                    window,
                                                    cx,
                                                );
                                                cx.stop_propagation();
                                            }),
                                        )
                                        .child(BLUETOOTH_ICON)
                                        .child(
                                            canvas(
                                                move |bounds, _, _| {
                                                    *bluetooth_icon_bounds.borrow_mut() = bounds;
                                                },
                                                |_, _, _, _| {},
                                            )
                                            .absolute()
                                            .inset_0(),
                                        ),
                                )
                            })
                            .when(airpods_icon_visible, |status| {
                                status.child(
                                    div()
                                        .id("status-airpods")
                                        .relative()
                                        .size(px(STATUS_ICON_FRAME_SIZE))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, event, window, cx| {
                                                this.show_airpods_popover(event, window, cx);
                                                cx.stop_propagation();
                                            }),
                                        )
                                        .child(
                                            img(std::path::PathBuf::from(env!(
                                                "CARGO_MANIFEST_DIR"
                                            ))
                                            .join("src/icon/airpods_icon.svg"))
                                            .size(airpods_icon_size),
                                        )
                                        .child(
                                            canvas(
                                                move |bounds, _, _| {
                                                    *airpods_icon_bounds.borrow_mut() = bounds;
                                                },
                                                |_, _, _, _| {},
                                            )
                                            .absolute()
                                            .inset_0(),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .id("status-network")
                                    .relative()
                                    .size(px(STATUS_ICON_FRAME_SIZE))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(network_icon_size)
                                    .on_hover(cx.listener(move |this, hovered, window, cx| {
                                        this.set_status_tooltip_hovered(
                                            "network", *hovered, window, cx,
                                        );
                                    }))
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(|this, event, window, cx| {
                                            this.show_network_popover(event, window, cx);
                                            cx.stop_propagation();
                                        }),
                                    )
                                    .child(network_icon.glyph)
                                    .child(
                                        canvas(
                                            move |bounds, _, _| {
                                                *network_icon_bounds.borrow_mut() = bounds;
                                            },
                                            |_, _, _, _| {},
                                        )
                                        .absolute()
                                        .inset_0(),
                                    ),
                            )
                            .child(
                                div()
                                    .id("status-battery")
                                    .size(px(STATUS_ICON_FRAME_SIZE))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(battery_icon_size)
                                    .on_hover(cx.listener(move |this, hovered, window, cx| {
                                        this.set_status_tooltip_hovered(
                                            "battery", *hovered, window, cx,
                                        );
                                    }))
                                    .child(battery_icon.glyph),
                            )
                            .child(
                                div()
                                    .id("notification-tray-clock")
                                    .cursor_pointer()
                                    .px(px(8.0))
                                    .py(px(2.0))
                                    .rounded(px(6.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_size(theme.clock_font_size)
                                    .text_color(theme.foreground)
                                    .hover(|style| style.bg(theme.active_background))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_notification_tray(window, cx);
                                    }))
                                    .child(self.clock.value().to_string()),
                            )
                            .child(
                                div()
                                    .id("notification-tray-button")
                                    .relative()
                                    .size(px(24.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.0))
                                    .text_color(theme.foreground)
                                    .hover(|style| style.bg(theme.active_background))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_notification_tray(window, cx);
                                    }))
                                    .child(div().text_size(px(15.0)).child(""))
                                    .when(notification_count > 0, |button| {
                                        button.child(
                                            div()
                                                .absolute()
                                                .top(px(-5.0))
                                                .right(px(-7.0))
                                                .min_w(px(15.0))
                                                .h(px(15.0))
                                                .px(px(3.0))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded(px(8.0))
                                                .bg(theme.urgent_background.alpha(1.0))
                                                .text_color(theme.foreground)
                                                .text_size(px(9.0))
                                                .font_weight(FontWeight::MEDIUM)
                                                .child(notification_badge_label(
                                                    notification_count,
                                                )),
                                        )
                                    }),
                            ),
                    ),
            )
            .when_some(self.jump_menu.as_ref(), |root, menu| {
                let left = menu.position.x;
                root.child(
                    div()
                        .absolute()
                        .top(theme.bar_height)
                        .left(left)
                        .w(px(JUMP_MENU_WIDTH))
                        .bg(theme.surface(SurfaceRole::Floating))
                        .border_1()
                        .border_color(theme.strong_border)
                        .rounded(theme.active_workspace_radius)
                        .text_color(theme.foreground)
                        .font(ui_font())
                        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                        .children(menu.actions.iter().cloned().enumerate().map(
                            |(index, action)| {
                                let label = action.label.clone();
                                div()
                                    .id(("jump-list-action", index as u32))
                                    .px(px(10.0))
                                    .h(px(JUMP_MENU_ROW_HEIGHT))
                                    .flex()
                                    .items_center()
                                    .text_color(theme.foreground)
                                    .hover(|style| style.bg(theme.active_background))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.launch_jump_list_action(action.clone(), window, cx);
                                    }))
                                    .child(label)
                            },
                        )),
                )
            })
            .when_some(self.workspace_menu.as_ref(), |root, menu| {
                let left = menu.position.x;
                root.child(
                    div()
                        .absolute()
                        .top(theme.bar_height)
                        .left(left)
                        .w(px(JUMP_MENU_WIDTH))
                        .bg(theme.surface(SurfaceRole::Floating))
                        .border_1()
                        .border_color(theme.strong_border)
                        .rounded(theme.active_workspace_radius)
                        .text_color(theme.foreground)
                        .font(ui_font())
                        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                        .children(menu.windows.iter().cloned().enumerate().flat_map(
                            |(index, workspace_window)| {
                                let app_name = workspace_window.app_name().to_string();
                                let address = workspace_window.address;
                                let icon = workspace_window.icon;
                                vec![
                                    div()
                                        .id(("workspace-window-name", index as u32))
                                        .px(px(10.0))
                                        .h(px(JUMP_MENU_ROW_HEIGHT))
                                        .flex()
                                        .items_center()
                                        .gap(theme.workspace_gap)
                                        .font_weight(FontWeight::MEDIUM)
                                        .when_some(icon, |element, icon| {
                                            element.child(
                                                img(icon)
                                                    .size(theme.active_window_icon_size)
                                                    .flex_none(),
                                            )
                                        })
                                        .child(app_name),
                                    div()
                                        .id(("workspace-window-close", index as u32))
                                        .px(px(10.0))
                                        .h(px(JUMP_MENU_ROW_HEIGHT))
                                        .flex()
                                        .items_center()
                                        .hover(|style| style.bg(theme.active_background))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.close_workspace_window(
                                                address.clone(),
                                                window,
                                                cx,
                                            );
                                        }))
                                        .child("Close Window"),
                                    div()
                                        .id(("workspace-window-separator", index as u32))
                                        .h(px(JUMP_MENU_BORDER_WIDTH))
                                        .mx(px(8.0))
                                        .bg(theme.foreground.alpha(0.35)),
                                ]
                            },
                        )),
                )
            })
    }
}

fn notification_badge_label(count: usize) -> String {
    if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        STATUS_ICON_REFERENCE_VISIBLE_HEIGHT, STATUS_ICON_UNITS_PER_EM, StatusIcon, battery_icon,
        bluetooth_icon_visible, cpu_usage_tooltip, memory_usage_tooltip, network_icon,
        notification_badge_label, volume_icon,
    };
    use crate::modules::system_controls::{
        BatteryStatus, CpuCoreKind, CpuCoreUsage, CpuStatus, LevelStatus, MemoryStatus,
        NetworkKind, NetworkRoute, ToggleStatus,
    };
    use gpui::px;

    fn network_status(signal_strength: Option<u8>) -> ToggleStatus {
        ToggleStatus {
            available: true,
            enabled: true,
            connected: false,
            wired: false,
            signal_strength,
            interface: None,
            download_kbps: None,
            upload_kbps: None,
            label: String::new(),
        }
    }

    fn battery_status(available: bool, percent: u8, charging: bool) -> BatteryStatus {
        BatteryStatus {
            available,
            percent,
            charging,
            state: String::new(),
            health: String::new(),
        }
    }

    fn assert_normalized_icon(icon: StatusIcon) {
        const BASE_SIZE: f32 = 13.0;
        let visible_height = f32::from(icon.text_size(px(BASE_SIZE))) * icon.visible_height
            / STATUS_ICON_UNITS_PER_EM;
        let reference_height =
            BASE_SIZE * STATUS_ICON_REFERENCE_VISIBLE_HEIGHT / STATUS_ICON_UNITS_PER_EM;
        assert!(
            (visible_height - reference_height).abs() < 0.001,
            "{} normalized to {} instead of {}",
            icon.glyph,
            visible_height,
            reference_height,
        );
    }

    #[test]
    fn notification_badge_caps_large_counts() {
        assert_eq!(notification_badge_label(1), "1");
        assert_eq!(notification_badge_label(99), "99");
        assert_eq!(notification_badge_label(100), "99+");
    }

    #[test]
    fn network_icon_uses_the_selected_default_route() {
        let status = network_status(Some(100));
        let ethernet = NetworkRoute {
            kind: NetworkKind::Wired,
            interface: "eth0".to_string(),
        };
        let wifi = NetworkRoute {
            kind: NetworkKind::Wifi,
            interface: "wlan0".to_string(),
        };

        assert_eq!(network_icon(&status, Some(&ethernet)).glyph, "\u{f0200}");
        assert_eq!(network_icon(&status, Some(&wifi)).glyph, "\u{f0928}");
        assert_eq!(network_icon(&status, None).glyph, "\u{f092c}");
    }

    #[test]
    fn status_icon_states_normalize_to_the_battery_reference_height() {
        for status in [
            LevelStatus {
                available: false,
                percent: 0,
                muted: false,
            },
            LevelStatus {
                available: true,
                percent: 50,
                muted: true,
            },
            LevelStatus {
                available: true,
                percent: 1,
                muted: false,
            },
            LevelStatus {
                available: true,
                percent: 34,
                muted: false,
            },
            LevelStatus {
                available: true,
                percent: 67,
                muted: false,
            },
        ] {
            assert_normalized_icon(volume_icon(status));
        }

        let wifi = NetworkRoute {
            kind: NetworkKind::Wifi,
            interface: "wlan0".to_string(),
        };
        let ethernet = NetworkRoute {
            kind: NetworkKind::Wired,
            interface: "eth0".to_string(),
        };
        let mut unavailable = network_status(None);
        unavailable.available = false;
        let mut disabled = network_status(None);
        disabled.enabled = false;
        for icon in [
            network_icon(&unavailable, Some(&wifi)),
            network_icon(&network_status(None), None),
            network_icon(&disabled, Some(&wifi)),
            network_icon(&network_status(None), Some(&ethernet)),
            network_icon(&network_status(Some(25)), Some(&wifi)),
            network_icon(&network_status(Some(50)), Some(&wifi)),
            network_icon(&network_status(Some(75)), Some(&wifi)),
            network_icon(&network_status(Some(100)), Some(&wifi)),
        ] {
            assert_normalized_icon(icon);
        }

        for percent in [0, 11, 21, 31, 41, 51, 61, 71, 81, 91] {
            assert_normalized_icon(battery_icon(&battery_status(true, percent, true)));
            assert_normalized_icon(battery_icon(&battery_status(true, percent, false)));
        }
        assert_normalized_icon(battery_icon(&battery_status(false, 0, false)));
    }

    #[test]
    fn bluetooth_icon_requires_an_active_connection() {
        let mut status = network_status(None);
        assert!(!bluetooth_icon_visible(&status));

        status.connected = true;
        assert!(bluetooth_icon_visible(&status));

        status.enabled = false;
        assert!(!bluetooth_icon_visible(&status));
    }

    #[test]
    fn memory_tooltip_includes_usage_and_total_in_gigabytes() {
        assert_eq!(
            memory_usage_tooltip(MemoryStatus {
                available: true,
                percent: 19,
                used_kib: 1_572_864,
                total_kib: 8_388_608,
            }),
            "Memory使用率: 19%\n1.5 GB/8.0 GB"
        );
    }

    #[test]
    fn cpu_tooltip_lists_each_logical_core_without_an_overall_heading() {
        assert_eq!(
            cpu_usage_tooltip(CpuStatus {
                available: true,
                percent: 53,
                core_usages: vec![
                    CpuCoreUsage {
                        index: 0,
                        kind: Some(CpuCoreKind::Performance),
                        percent_tenths: 673,
                    },
                    CpuCoreUsage {
                        index: 1,
                        kind: Some(CpuCoreKind::Efficiency),
                        percent_tenths: 500,
                    },
                ],
            }),
            "CPU0(P): 67.3%\nCPU1(E): 50.0%"
        );
    }
}
