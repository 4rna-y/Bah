use std::{sync::MutexGuard, time::Instant};

use async_channel::Sender;
use chrono::{Datelike, Local, NaiveDate};
use gpui::{
    Context, FontWeight, Render, Window, WindowHandle, div, ease_in_out, prelude::*, px, relative,
};

use crate::{
    modules::notifications::{NotificationEvent, NotificationStore, SharedNotificationStore},
    theme::{BarTheme, ui_font},
};

const CALENDAR_HEIGHT_RATIO: f32 = 0.35;
const CALENDAR_COLUMNS: usize = 7;
const CALENDAR_ROWS: usize = 6;
const WEEKDAY_LABELS: [&str; CALENDAR_COLUMNS] = ["日", "月", "火", "水", "木", "金", "土"];
pub(crate) const TRAY_PANEL_WIDTH_RATIO: f32 = 0.30;

/// A transparent, full-output surface that only receives clicks outside the
/// tray panel. Keeping this separate lets the animated tray use a narrow GPU
/// surface instead of repainting the entire output on every frame.
pub struct NotificationTrayDismissTarget {
    tray: Option<WindowHandle<NotificationTray>>,
}

impl NotificationTrayDismissTarget {
    pub fn new() -> Self {
        Self { tray: None }
    }

    pub fn set_tray(&mut self, tray: WindowHandle<NotificationTray>) {
        self.tray = Some(tray);
    }

    pub fn show(&mut self, window: &mut Window) {
        window.set_input_region(None);
    }

    pub fn hide(&mut self, window: &mut Window) {
        window.set_input_region(Some(&[]));
    }
}

impl Render for NotificationTrayDismissTarget {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .on_any_mouse_down(cx.listener(|this, _, window, cx| {
                // Disable this surface first so repeated clicks cannot enqueue
                // another hide while the panel is sliding out.
                this.hide(window);
                if let Some(tray) = this.tray {
                    let _ = tray.update(cx, |tray, window, cx| {
                        tray.hide_from_dismiss_target(window, cx);
                    });
                }
            }))
    }
}

/// Content view for the right-anchored Layer Shell notification tray.
pub struct NotificationTray {
    notifications: SharedNotificationStore,
    updates: Sender<NotificationEvent>,
    dismiss_target: WindowHandle<NotificationTrayDismissTarget>,
    theme: BarTheme,
    visible: bool,
    hiding: bool,
    slide_started_at: Option<Instant>,
    start_show_on_first_render: bool,
}

impl NotificationTray {
    pub fn new(
        notifications: SharedNotificationStore,
        updates: Sender<NotificationEvent>,
        dismiss_target: WindowHandle<NotificationTrayDismissTarget>,
        theme: BarTheme,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            notifications,
            updates,
            dismiss_target,
            theme,
            visible: true,
            hiding: false,
            slide_started_at: None,
            // A layer-shell window can wait for its initial configure longer
            // than the animation duration. Start only once it can render.
            start_show_on_first_render: !cx.reduce_motion(),
        }
    }

    pub(crate) fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = true;
        self.hiding = false;
        self.start_show_on_first_render = false;
        self.slide_started_at = (!cx.reduce_motion()).then(Instant::now);
        window.set_input_region(None);
        let _ = self
            .dismiss_target
            .update(cx, |target, window, _| target.show(window));
        cx.notify();
    }

    fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = self
            .dismiss_target
            .update(cx, |target, window, _| target.hide(window));
        self.begin_hide(window, cx);
    }

    fn hide_from_dismiss_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_hide(window, cx);
    }

    fn begin_hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible || self.hiding {
            return;
        }

        if cx.reduce_motion() {
            self.visible = false;
            window.set_input_region(Some(&[]));
            cx.notify();
            return;
        }

        self.hiding = true;
        self.slide_started_at = Some(Instant::now());
        // Prevent further interaction while the still-visible tray slides out.
        window.set_input_region(Some(&[]));
        cx.notify();
    }

    fn slide_offset(&mut self) -> f32 {
        let Some(started_at) = self.slide_started_at else {
            return 0.0;
        };
        let progress = (started_at.elapsed().as_secs_f32()
            / self.theme.notification_tray_slide_duration.as_secs_f32())
        .min(1.0);
        let eased_progress = ease_in_out(progress);
        let offset = if self.hiding {
            eased_progress
        } else {
            1.0 - eased_progress
        };

        if progress == 1.0 {
            self.slide_started_at = None;
            if self.hiding {
                self.visible = false;
                self.hiding = false;
                // Keep the layer surface alive, but make it visually absent.
                // Destroying a GPUI layer-shell window can race post-destroy
                // Wayland frame callbacks, which otherwise log "window not found".
            }
        }

        offset
    }

    fn store(&self) -> MutexGuard<'_, NotificationStore> {
        self.notifications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        let _ = self.updates.try_send(NotificationEvent::Clear);
        cx.notify();
    }

    fn dismiss(&mut self, id: u32, cx: &mut Context<Self>) {
        let _ = self.updates.try_send(NotificationEvent::Close(id));
        cx.notify();
    }
}

impl Render for NotificationTray {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().size_full().into_any_element();
        }

        if self.start_show_on_first_render {
            self.start_show_on_first_render = false;
            self.slide_started_at = Some(Instant::now());
        }
        let slide_offset = self.slide_offset();
        if !self.visible {
            return div().size_full().into_any_element();
        }
        if self.slide_started_at.is_some() && !cx.reduce_motion() {
            window.request_animation_frame();
        }

        let theme = self.theme;
        let notifications = self.store().snapshot();
        let count = notifications.len();
        let today = Local::now().date_naive();
        let calendar_days = calendar_days_for_month(today.year(), today.month());
        let slide_distance = f32::from(window.bounds().size.width);

        div()
            .size_full()
            .overflow_hidden()
            .child(
                div().size_full().child(
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .bg(theme.background)
                        .border_l_1()
                        .border_color(theme.border.alpha(0.9))
                        .text_color(theme.foreground)
                        .font(ui_font())
                        .child(
                            div()
                                .h(theme.bar_height)
                                .px(px(14.0))
                                .flex()
                                .items_center()
                                .justify_between()
                                .border_b_1()
                                .border_color(theme.border)
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_size(px(14.0))
                                        .child(format!("Notifications ({count})")),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .child(
                                            div()
                                                .id("clear-notifications")
                                                .px(px(7.0))
                                                .py(px(4.0))
                                                .rounded(px(5.0))
                                                .text_size(px(11.0))
                                                .text_color(theme.muted_foreground)
                                                .hover(|style| style.bg(theme.active_background))
                                                .on_click(
                                                    cx.listener(|this, _, _, cx| this.clear(cx)),
                                                )
                                                .child("Clear"),
                                        )
                                        .child(
                                            div()
                                                .id("close-notification-tray")
                                                .size(px(22.0))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded(px(5.0))
                                                .text_color(theme.muted_foreground)
                                                .hover(|style| style.bg(theme.active_background))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.hide(window, cx);
                                                }))
                                                .child("×"),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .id("notification-list")
                                .flex_1()
                                .overflow_y_scroll()
                                .when(notifications.is_empty(), |list| {
                                    list.child(
                                        div()
                                            .p(px(16.0))
                                            .text_size(px(12.0))
                                            .text_color(theme.muted_foreground)
                                            .child("No notifications"),
                                    )
                                })
                                .children(notifications.into_iter().map(|notification| {
                                    let id = notification.id;
                                    div()
                                        .id(("notification", id))
                                        .m(px(8.0))
                                        .p(px(10.0))
                                        .rounded(px(7.0))
                                        .bg(theme.active_background)
                                        .child(
                                            div()
                                                .flex()
                                                .justify_between()
                                                .gap(px(8.0))
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_size(px(12.0))
                                                        .child(notification.summary),
                                                )
                                                .child(
                                                    div()
                                                        .id(("dismiss-notification", id))
                                                        .px(px(4.0))
                                                        .text_color(theme.muted_foreground)
                                                        .hover(|style| {
                                                            style.text_color(theme.foreground)
                                                        })
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.dismiss(id, cx);
                                                            },
                                                        ))
                                                        .child("×"),
                                                ),
                                        )
                                        .when(!notification.app_name.is_empty(), |item| {
                                            item.child(
                                                div()
                                                    .mt(px(4.0))
                                                    .text_size(px(10.0))
                                                    .text_color(theme.muted_foreground)
                                                    .child(notification.app_name),
                                            )
                                        })
                                        .when(!notification.body.is_empty(), |item| {
                                            item.child(
                                                div()
                                                    .mt(px(6.0))
                                                    .text_size(px(11.0))
                                                    .text_color(theme.foreground)
                                                    .child(notification.body),
                                            )
                                        })
                                })),
                        )
                        .child(
                            div()
                                .id("notification-calendar")
                                .h(relative(CALENDAR_HEIGHT_RATIO))
                                .flex_none()
                                .p(px(14.0))
                                .flex()
                                .flex_col()
                                .border_t_1()
                                .border_color(theme.border)
                                .child(
                                    div()
                                        .mb(px(8.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_size(px(13.0))
                                        .child(format!("{}年{}月", today.year(), today.month())),
                                )
                                .child(div().mb(px(4.0)).flex().children(
                                    WEEKDAY_LABELS.iter().enumerate().map(|(index, label)| {
                                        div()
                                            .id(("calendar-weekday", index as u32))
                                            .flex_1()
                                            .text_center()
                                            .text_size(px(10.0))
                                            .text_color(
                                                if index == 0 || index == CALENDAR_COLUMNS - 1 {
                                                    theme.muted_foreground
                                                } else {
                                                    theme.foreground
                                                },
                                            )
                                            .child(*label)
                                    }),
                                ))
                                .children((0..CALENDAR_ROWS).map(|week| {
                                    div()
                                        .id(("calendar-week", week as u32))
                                        .flex()
                                        .flex_1()
                                        .children((0..CALENDAR_COLUMNS).map(|weekday| {
                                            let index = week * CALENDAR_COLUMNS + weekday;
                                            let day = calendar_days[index];
                                            let is_today = day == Some(today.day());

                                            div()
                                                .id(("calendar-day", index as u32))
                                                .flex_1()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .text_size(px(11.0))
                                                .when(is_today, |cell| {
                                                    cell.mx(px(2.0))
                                                        .rounded(px(6.0))
                                                        .bg(theme.active_background)
                                                        .font_weight(FontWeight::MEDIUM)
                                                })
                                                .when_some(day, |cell, day| {
                                                    cell.child(day.to_string())
                                                })
                                        }))
                                })),
                        )
                        .relative()
                        .left(px(slide_distance * slide_offset)),
                ),
            )
            .into_any_element()
    }
}

fn calendar_days_for_month(year: i32, month: u32) -> Vec<Option<u32>> {
    let first_day = NaiveDate::from_ymd_opt(year, month, 1).expect("valid calendar month");
    let next_month = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1).expect("valid next calendar month")
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1).expect("valid next calendar month")
    };
    let days_in_month = next_month
        .pred_opt()
        .expect("next calendar month has a previous day")
        .day();
    let first_weekday = first_day.weekday().num_days_from_sunday() as usize;
    let mut days = vec![None; CALENDAR_ROWS * CALENDAR_COLUMNS];

    for day in 1..=days_in_month {
        days[first_weekday + day as usize - 1] = Some(day);
    }

    days
}

#[cfg(test)]
mod tests {
    use super::calendar_days_for_month;

    #[test]
    fn calendar_starts_on_the_first_weekday_and_includes_every_day() {
        let days = calendar_days_for_month(2026, 8);

        assert_eq!(days.len(), 42);
        assert_eq!(days[6], Some(1));
        assert_eq!(days[36], Some(31));
        assert_eq!(days.iter().flatten().count(), 31);
    }

    #[test]
    fn calendar_handles_february_in_a_leap_year() {
        let days = calendar_days_for_month(2024, 2);

        assert_eq!(days.iter().flatten().count(), 29);
        assert_eq!(days.iter().flatten().copied().max(), Some(29));
    }
}
