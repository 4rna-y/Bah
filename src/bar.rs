use std::{sync::MutexGuard, time::Duration};

use async_channel::{Receiver, Sender};

use gpui::{
    App, Bounds, Context, FontWeight, MouseDownEvent, Pixels, Point, Render, Size, Window,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowHandle, WindowKind,
    WindowOptions, div, img, layer_shell::*, point, prelude::*, px,
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
        system_controls::ControlChannels,
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
    notification_tray: Option<WindowHandle<NotificationTray>>,
    notification_tray_dismiss_target: Option<WindowHandle<NotificationTrayDismissTarget>>,
    controls: ControlChannels,
}

const JUMP_MENU_ROW_HEIGHT: f32 = 28.0;
const JUMP_MENU_WIDTH: f32 = 220.0;
const JUMP_MENU_BORDER_WIDTH: f32 = 1.0;

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
            notification_tray: None,
            notification_tray_dismiss_target: None,
            controls,
        };
        Self::start_clock(cx);
        Self::start_ipc_updates(ipc_updates, cx);
        Self::start_notification_updates(notification_updates, cx);
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
        let control_updates = self.controls.updates.clone();
        let control_actions = self.controls.actions.clone();

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
                        control_updates,
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
    use super::notification_badge_label;

    #[test]
    fn notification_badge_caps_large_counts() {
        assert_eq!(notification_badge_label(1), "1");
        assert_eq!(notification_badge_label(99), "99");
        assert_eq!(notification_badge_label(100), "99+");
    }
}
