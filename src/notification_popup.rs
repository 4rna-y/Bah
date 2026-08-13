use std::{path::Path, sync::MutexGuard};

use async_channel::Sender;
use gpui::{Context, FontWeight, Render, Window, div, img, prelude::*, px};

use crate::{
    modules::notifications::{
        CloseReason, Notification, NotificationEvent, NotificationStore, SharedNotificationStore,
        emit_action_invoked, emit_notification_closed,
    },
    theme::{BarTheme, SurfaceRole, ui_font},
};

/// Right-aligned transient notification surface. It intentionally stays alive
/// after its last card disappears; recreating layer-shell windows rapidly can
/// race Wayland frame callbacks.
pub struct NotificationPopupStack {
    notifications: SharedNotificationStore,
    updates: Sender<NotificationEvent>,
    theme: BarTheme,
}

impl NotificationPopupStack {
    pub fn new(
        notifications: SharedNotificationStore,
        updates: Sender<NotificationEvent>,
        theme: BarTheme,
    ) -> Self {
        Self {
            notifications,
            updates,
            theme,
        }
    }

    pub fn height_for(notification_count: usize) -> f32 {
        // Keep the surface inside a typical output even when the daemon's
        // notification limit is large. The list scrolls beyond this height.
        (notification_count.min(6) as f32 * 118.0 + 16.0).clamp(1.0, 724.0)
    }

    fn store(&self) -> MutexGuard<'_, NotificationStore> {
        self.notifications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn dismiss(&mut self, id: u32, cx: &mut Context<Self>) {
        emit_notification_closed(id, CloseReason::DismissedByUser);
        let _ = self.updates.try_send(NotificationEvent::Close(id));
        cx.notify();
    }

    fn invoke(&mut self, id: u32, action: String, cx: &mut Context<Self>) {
        emit_action_invoked(id, &action);
        let _ = self.updates.try_send(NotificationEvent::Close(id));
        cx.notify();
    }

    fn card(&self, notification: Notification, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let id = notification.id;
        let urgency_color = match notification.urgency {
            crate::modules::notifications::Urgency::Low => theme.muted_foreground,
            crate::modules::notifications::Urgency::Normal => theme.foreground,
            crate::modules::notifications::Urgency::Critical => theme.error,
        };
        let body = notification.body.clone();
        let summary = notification.summary.clone();
        let actions = notification.actions.clone();
        let icon = if Path::new(&notification.app_icon).is_file() {
            img(std::path::PathBuf::from(&notification.app_icon))
                .size(px(24.0))
                .into_any_element()
        } else {
            div()
                .size(px(24.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.0))
                .bg(theme.active_background)
                .text_size(px(13.0))
                .child(if notification.app_icon.is_empty() {
                    ""
                } else {
                    "󰂚"
                })
                .into_any_element()
        };
        div()
            .id(("notification-popup", id))
            .m(px(6.0))
            .p(px(10.0))
            .rounded(theme.panel_radius)
            .bg(theme.surface(SurfaceRole::Dialog))
            .border_1()
            .border_color(theme.border)
            .text_color(theme.foreground)
            .child(
                div().flex().items_start().gap(px(8.0)).child(icon).child(
                    div()
                        .flex_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_size(px(12.0))
                                        .text_color(urgency_color)
                                        .child(summary),
                                )
                                .when(notification.duplicate_count > 1, |row| {
                                    row.child(
                                        div()
                                            .text_size(px(10.0))
                                            .text_color(theme.muted_foreground)
                                            .child(format!("×{}", notification.duplicate_count)),
                                    )
                                })
                                .child(
                                    div()
                                        .id(("dismiss-popup-notification", id))
                                        .px(px(4.0))
                                        .cursor_pointer()
                                        .text_color(theme.muted_foreground)
                                        .hover(|style| style.text_color(theme.foreground))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.dismiss(id, cx);
                                        }))
                                        .child("×"),
                                ),
                        )
                        .when(!notification.app_name.is_empty(), |card| {
                            card.child(
                                div()
                                    .mt(px(3.0))
                                    .text_size(px(10.0))
                                    .text_color(theme.muted_foreground)
                                    .child(notification.app_name),
                            )
                        })
                        .when(!body.is_empty(), |card| {
                            card.child(div().mt(px(5.0)).text_size(px(11.0)).child(body))
                        })
                        .when(notification.progress.is_some(), |card| {
                            let progress =
                                f32::from(notification.progress.unwrap_or_default()) / 100.0;
                            card.child(
                                div()
                                    .mt(px(8.0))
                                    .h(px(4.0))
                                    .rounded(px(2.0))
                                    .bg(theme.border)
                                    .child(
                                        div()
                                            .h_full()
                                            .w(gpui::relative(progress))
                                            .rounded(px(2.0))
                                            .bg(urgency_color),
                                    ),
                            )
                        })
                        .when(!actions.is_empty(), |card| {
                            card.child(div().mt(px(8.0)).flex().flex_wrap().gap(px(5.0)).children(
                                actions.into_iter().map(|action| {
                                    let action_key = action.key.clone();
                                    div()
                                        .id(format!("notification-action-{id}-{action_key}"))
                                        .px(px(7.0))
                                        .py(px(4.0))
                                        .rounded(px(5.0))
                                        .cursor_pointer()
                                        .text_size(px(10.0))
                                        .bg(theme.active_background)
                                        .hover(|style| style.bg(theme.border))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.invoke(id, action_key.clone(), cx);
                                        }))
                                        .child(action.label)
                                }),
                            ))
                        }),
                ),
            )
            .into_any_element()
    }
}

impl Render for NotificationPopupStack {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let notifications = self.store().displayed_snapshot();
        window.resize(gpui::Size::new(
            window.bounds().size.width,
            px(Self::height_for(notifications.len())),
        ));
        if notifications.is_empty() {
            window.set_input_region(Some(&[]));
            return div().size_full().into_any_element();
        }
        window.set_input_region(None);
        div()
            .size_full()
            .overflow_hidden()
            .font(ui_font())
            .children(
                notifications
                    .into_iter()
                    .map(|notification| self.card(notification, cx)),
            )
            .into_any_element()
    }
}
