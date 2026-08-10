use std::{sync::MutexGuard, time::Duration};

use async_channel::{Receiver, Sender};

use gpui::{
    App, Bounds, Context, FontWeight, InteractiveElement, MouseDownEvent, Pixels, Point, Render,
    Size, Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowHandle,
    WindowKind, WindowOptions, div, img, layer_shell::*, point, popup::*, prelude::*, px,
};
use log::{error, info, warn};

use crate::{
    hyprland::{
        IpcUpdate, JumpListAction, WorkspaceWindow, close_window, launch_jump_list_action,
        switch_to_workspace,
    },
    modules::{
        clock::Clock,
        notifications::{NotificationEvent, NotificationStore, SharedNotificationStore},
        system_controls::{
            BatteryStatus, ControlChannels, ControlSnapshot, CpuStatus, LevelStatus, MemoryStatus,
            ToggleStatus,
        },
        workspaces::Workspaces,
    },
    notification_tray::{NotificationTray, NotificationTrayDismissTarget, TRAY_PANEL_WIDTH_RATIO},
    theme::{BarTheme, ui_font},
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
    controls: ControlChannels,
    control_snapshot: ControlSnapshot,
}

const JUMP_MENU_ROW_HEIGHT: f32 = 28.0;
const JUMP_MENU_WIDTH: f32 = 220.0;
const JUMP_MENU_BORDER_WIDTH: f32 = 1.0;
const STATUS_ICON_FRAME_SIZE: f32 = 24.0;
const VOLUME_ICON_SCALE: f32 = 20.0 / 13.0;
const ETHERNET_ICON_SCALE: f32 = 20.0 / 13.0;
// Scale the MDI glyphs to the same 12.1px visible height as `md-battery`
// at the bar's standard 13px status-icon size.
const CPU_ICON_SCALE: f32 = 1.74;
const MEMORY_ICON_SCALE: f32 = 1.55;
const CPU_ICON: &str = "\u{f061a}";
const MEMORY_ICON: &str = "\u{f035b}";
const STATUS_TOOLTIP_SHOW_DELAY: Duration = Duration::from_millis(500);
const STATUS_TOOLTIP_FONT_SIZE: f32 = 11.0;
const STATUS_TOOLTIP_LINE_HEIGHT: f32 = 17.0;
const STATUS_TOOLTIP_HORIZONTAL_PADDING: f32 = 8.0;
const STATUS_TOOLTIP_VERTICAL_PADDING: f32 = 5.0;
const STATUS_TOOLTIP_BORDER_WIDTH: f32 = 1.0;
const STATUS_TOOLTIP_ASCII_CHARACTER_WIDTH: f32 = 8.0;

fn volume_icon(status: LevelStatus) -> &'static str {
    if !status.available {
        "\u{f0581}"
    } else if status.muted || status.percent == 0 {
        "\u{f075f}"
    } else if status.percent <= 33 {
        "\u{f057f}"
    } else if status.percent <= 66 {
        "\u{f0580}"
    } else {
        "\u{f057e}"
    }
}

fn network_icon(status: &ToggleStatus) -> &'static str {
    if status.wired {
        "\u{f0200}"
    } else if !status.enabled || !status.available {
        "\u{f092c}"
    } else {
        match status.signal_strength.unwrap_or(100) {
            0..=25 => "\u{f091f}",
            26..=50 => "\u{f0922}",
            51..=75 => "\u{f0925}",
            _ => "\u{f0928}",
        }
    }
}

fn battery_icon(status: &BatteryStatus) -> &'static str {
    let percent = status.percent;
    if !status.available {
        return "\u{f0091}";
    }
    if status.charging {
        return match percent {
            0..=10 => "\u{f0084}",
            11..=20 => "\u{f0086}",
            21..=30 => "\u{f0087}",
            31..=40 => "\u{f0088}",
            41..=50 => "\u{f0089}",
            51..=60 => "\u{f008a}",
            61..=70 => "\u{f008b}",
            71..=80 => "\u{f008c}",
            81..=90 => "\u{f008d}",
            _ => "\u{f0085}",
        };
    }
    match percent {
        0..=10 => "\u{f007a}",
        11..=20 => "\u{f007b}",
        21..=30 => "\u{f007c}",
        31..=40 => "\u{f007d}",
        41..=50 => "\u{f007e}",
        51..=60 => "\u{f007f}",
        61..=70 => "\u{f0080}",
        71..=80 => "\u{f0081}",
        81..=90 => "\u{f0082}",
        _ => "\u{f0079}",
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
            .rounded(px(6.0))
            .border_1()
            .border_color(self.theme.border)
            .bg(self.theme.background.alpha(1.0))
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
    pub fn new(
        ipc_updates: Receiver<IpcUpdate>,
        notification_updates: Receiver<NotificationEvent>,
        notification_sender: Sender<NotificationEvent>,
        notifications: SharedNotificationStore,
        controls: ControlChannels,
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
            controls,
            control_snapshot: ControlSnapshot::default(),
        };
        Self::start_clock(cx);
        Self::start_ipc_updates(ipc_updates, cx);
        Self::start_notification_updates(notification_updates, cx);
        Self::start_control_updates(bar.controls.updates.clone(), cx);
        bar
    }

    fn start_clock(cx: &mut Context<Self>) {
        cx.spawn(async move |bar, cx| {
            loop {
                if bar
                    .update(cx, |bar, cx| {
                        bar.clock.tick();
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
                        notifications.apply(first_update);
                        while let Ok(update) = updates.try_recv() {
                            notifications.apply(update);
                        }
                        drop(notifications);

                        if let Some(tray) = bar.notification_tray {
                            let _ = tray.update(cx, |_, _, cx| cx.notify());
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

    fn notification_store(&self) -> MutexGuard<'_, NotificationStore> {
        self.notifications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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

    fn status_tooltip_text(&self, id: &str) -> String {
        match id {
            "cpu" => cpu_usage_tooltip(self.control_snapshot.cpu.clone()),
            "memory" => memory_usage_tooltip(self.control_snapshot.memory),
            "volume" => volume_usage_tooltip(self.control_snapshot.audio_output),
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
        if self.jump_menu_resize_pending {
            self.resize_for_jump_menu(window, cx);
            self.jump_menu_resize_pending = false;
        }
        let theme = self.theme;
        let notification_count = self.notification_store().count();
        let volume_icon = volume_icon(self.control_snapshot.audio_output);
        let network_icon = network_icon(&self.control_snapshot.wifi);
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
        let volume_icon_size = status_icon_size * VOLUME_ICON_SCALE;
        let cpu_icon_size = status_icon_size * CPU_ICON_SCALE;
        let memory_icon_size = status_icon_size * MEMORY_ICON_SCALE;
        let network_icon_size = if self.control_snapshot.wifi.wired {
            status_icon_size * ETHERNET_ICON_SCALE
        } else {
            status_icon_size
        };
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
            }))
            .child(
                div()
                    .h(theme.bar_height)
                    .flex()
                    .items_center()
                    .px(theme.horizontal_padding)
                    .gap(theme.module_spacing)
                    .bg(theme.background)
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
                                    .child(volume_icon),
                            )
                            .child(
                                div()
                                    .id("status-network")
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
                                    .child(network_icon),
                            )
                            .child(
                                div()
                                    .id("status-battery")
                                    .size(px(STATUS_ICON_FRAME_SIZE))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(status_icon_size)
                                    .on_hover(cx.listener(move |this, hovered, window, cx| {
                                        this.set_status_tooltip_hovered(
                                            "battery", *hovered, window, cx,
                                        );
                                    }))
                                    .child(battery_icon),
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
                        .bg(theme.background)
                        .border_1()
                        .border_color(theme.foreground.alpha(0.7))
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
                        .bg(theme.background)
                        .border_1()
                        .border_color(theme.foreground.alpha(0.7))
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
    use super::{cpu_usage_tooltip, memory_usage_tooltip, notification_badge_label};
    use crate::modules::system_controls::{CpuCoreKind, CpuCoreUsage, CpuStatus, MemoryStatus};

    #[test]
    fn notification_badge_caps_large_counts() {
        assert_eq!(notification_badge_label(1), "1");
        assert_eq!(notification_badge_label(99), "99");
        assert_eq!(notification_badge_label(100), "99+");
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
