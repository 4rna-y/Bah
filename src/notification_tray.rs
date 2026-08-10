use std::{
    process::Command,
    sync::MutexGuard,
    thread,
    time::{Duration, Instant},
};

use async_channel::Sender;
use chrono::{Datelike, Local, NaiveDate};
use gpui::{
    Context, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, Window,
    WindowHandle, div, ease_in_out, prelude::*, px, relative,
};
use log::warn;

use crate::{
    modules::notifications::{NotificationEvent, NotificationStore, SharedNotificationStore},
    modules::system_controls::{
        AudioEndpoint, ControlAction, ControlSnapshot, LevelStatus, NetworkKind, ToggleStatus,
    },
    theme::{BarTheme, ui_font},
};

const CALENDAR_HEIGHT_RATIO: f32 = 0.35;
const CALENDAR_COLUMNS: usize = 7;
const CALENDAR_ROWS: usize = 6;
const WEEKDAY_LABELS: [&str; CALENDAR_COLUMNS] = ["日", "月", "火", "水", "木", "金", "土"];
pub(crate) const TRAY_PANEL_WIDTH_RATIO: f32 = 0.40;
const CONTROL_PADDING: f32 = 14.0;
const CONTROL_GAP: f32 = 8.0;
const CONTROL_ICON_BUTTON_WIDTH: f32 = 32.0;
const SLIDER_SEND_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SliderKind {
    AudioOutput,
    AudioInput,
    Brightness,
}

#[derive(Clone, Copy, Debug)]
struct SliderDrag {
    kind: SliderKind,
    left: f32,
    width: f32,
    last_sent_at: Option<Instant>,
}

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
    control_actions: Sender<ControlAction>,
    dismiss_target: WindowHandle<NotificationTrayDismissTarget>,
    theme: BarTheme,
    visible: bool,
    hiding: bool,
    slide_started_at: Option<Instant>,
    start_show_on_first_render: bool,
    controls: ControlSnapshot,
    slider_drag: Option<SliderDrag>,
    calendar_month: NaiveDate,
}

impl NotificationTray {
    pub fn new(
        notifications: SharedNotificationStore,
        updates: Sender<NotificationEvent>,
        controls: ControlSnapshot,
        control_actions: Sender<ControlAction>,
        dismiss_target: WindowHandle<NotificationTrayDismissTarget>,
        theme: BarTheme,
        cx: &mut Context<Self>,
    ) -> Self {
        let today = Local::now().date_naive();
        Self {
            notifications,
            updates,
            control_actions,
            dismiss_target,
            theme,
            visible: true,
            hiding: false,
            slide_started_at: None,
            // A layer-shell window can wait for its initial configure longer
            // than the animation duration. Start only once it can render.
            start_show_on_first_render: !cx.reduce_motion(),
            controls,
            slider_drag: None,
            calendar_month: NaiveDate::from_ymd_opt(today.year(), today.month(), 1)
                .expect("current date has a valid month"),
        }
    }

    pub(crate) fn set_controls(&mut self, controls: ControlSnapshot, cx: &mut Context<Self>) {
        let dragging = self
            .slider_drag
            .map(|drag| (drag.kind, self.slider_percent(drag.kind)));
        self.controls = controls;
        if let Some((kind, percent)) = dragging {
            self.set_slider_percent(kind, percent);
        }
        cx.notify();
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

    fn launch_device_control_center(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide(window, cx);
        if let Err(error) = thread::Builder::new()
            .name("bah-device-control-center-launch".to_string())
            .spawn(|| {
                let executable = match std::env::current_exe() {
                    Ok(executable) => executable,
                    Err(error) => {
                        warn!("could not resolve current executable: {error}");
                        return;
                    }
                };
                if let Err(error) = Command::new(executable)
                    .args(["window", "device-control-center"])
                    .spawn()
                {
                    warn!("failed to launch device control center: {error}");
                }
            })
        {
            warn!("failed to start device control center launcher: {error}");
        }
    }

    fn launch_config_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide(window, cx);
        if let Err(error) = thread::Builder::new()
            .name("bah-config-window-launch".to_string())
            .spawn(|| {
                let executable = match std::env::current_exe() {
                    Ok(executable) => executable,
                    Err(error) => {
                        warn!("could not resolve current executable: {error}");
                        return;
                    }
                };
                if let Err(error) = Command::new(executable).args(["window", "config"]).spawn() {
                    warn!("failed to launch config window: {error}");
                }
            })
        {
            warn!("failed to start config window launcher: {error}");
        }
    }

    fn toggle_wifi(&mut self, cx: &mut Context<Self>) {
        if self.controls.wifi.available {
            self.controls.wifi.enabled = !self.controls.wifi.enabled;
            self.controls.wifi.label = if self.controls.wifi.enabled {
                "未接続".to_string()
            } else {
                "Off".to_string()
            };
            let _ = self.control_actions.try_send(ControlAction::ToggleWifi);
            cx.notify();
        }
    }

    fn toggle_bluetooth(&mut self, cx: &mut Context<Self>) {
        if self.controls.bluetooth.available {
            self.controls.bluetooth.enabled = !self.controls.bluetooth.enabled;
            self.controls.bluetooth.label = if self.controls.bluetooth.enabled {
                "未接続".to_string()
            } else {
                "Off".to_string()
            };
            let _ = self
                .control_actions
                .try_send(ControlAction::ToggleBluetooth);
            cx.notify();
        }
    }

    fn toggle_mute(&mut self, endpoint: AudioEndpoint, cx: &mut Context<Self>) {
        let status = match endpoint {
            AudioEndpoint::Output => &mut self.controls.audio_output,
            AudioEndpoint::Input => &mut self.controls.audio_input,
        };
        if status.available {
            status.muted = !status.muted;
            let _ = self
                .control_actions
                .try_send(ControlAction::ToggleMute(endpoint));
            cx.notify();
        }
    }

    fn slider_percent(&self, kind: SliderKind) -> u8 {
        match kind {
            SliderKind::AudioOutput => self.controls.audio_output.percent,
            SliderKind::AudioInput => self.controls.audio_input.percent,
            SliderKind::Brightness => self.controls.brightness.percent,
        }
    }

    fn slider_available(&self, kind: SliderKind) -> bool {
        match kind {
            SliderKind::AudioOutput => self.controls.audio_output.available,
            SliderKind::AudioInput => self.controls.audio_input.available,
            SliderKind::Brightness => self.controls.brightness.available,
        }
    }

    fn set_slider_percent(&mut self, kind: SliderKind, percent: u8) {
        match kind {
            SliderKind::AudioOutput => self.controls.audio_output.percent = percent,
            SliderKind::AudioInput => self.controls.audio_input.percent = percent,
            SliderKind::Brightness => self.controls.brightness.percent = percent,
        }
    }

    fn slider_action(kind: SliderKind, percent: u8) -> ControlAction {
        match kind {
            SliderKind::AudioOutput => ControlAction::SetVolume(AudioEndpoint::Output, percent),
            SliderKind::AudioInput => ControlAction::SetVolume(AudioEndpoint::Input, percent),
            SliderKind::Brightness => ControlAction::SetBrightness(percent),
        }
    }

    fn begin_slider_drag(
        &mut self,
        kind: SliderKind,
        left: f32,
        width: f32,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        if !self.slider_available(kind) {
            return;
        }
        self.slider_drag = Some(SliderDrag {
            kind,
            left,
            width,
            last_sent_at: None,
        });
        self.update_slider(event.position.x.into(), true, cx);
    }

    fn update_slider(&mut self, pointer_x: f32, force_send: bool, cx: &mut Context<Self>) {
        let Some(mut drag) = self.slider_drag else {
            return;
        };
        let percent = slider_percent_from_pointer(pointer_x, drag.left, drag.width);
        self.set_slider_percent(drag.kind, percent);
        let should_send = force_send
            || drag
                .last_sent_at
                .is_none_or(|last| last.elapsed() >= SLIDER_SEND_INTERVAL);
        if should_send {
            let _ = self
                .control_actions
                .try_send(Self::slider_action(drag.kind, percent));
            drag.last_sent_at = Some(Instant::now());
            self.slider_drag = Some(drag);
        }
        cx.notify();
    }

    fn move_slider(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button == Some(MouseButton::Left) {
            self.update_slider(event.position.x.into(), false, cx);
        }
    }

    fn end_slider_drag(&mut self, pointer_x: f32, cx: &mut Context<Self>) {
        if self.slider_drag.is_some() {
            self.update_slider(pointer_x, true, cx);
            self.slider_drag = None;
            cx.notify();
        }
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

    fn show_previous_calendar_month(&mut self, cx: &mut Context<Self>) {
        self.calendar_month = adjacent_calendar_month(self.calendar_month, -1);
        cx.notify();
    }

    fn show_next_calendar_month(&mut self, cx: &mut Context<Self>) {
        self.calendar_month = adjacent_calendar_month(self.calendar_month, 1);
        cx.notify();
    }

    fn render_slider(
        kind: SliderKind,
        status: LevelStatus,
        left: f32,
        width: f32,
        theme: BarTheme,
        dragging: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let percent = status.percent.min(100);
        let fraction = f32::from(percent) / 100.0;
        div()
            .id(match kind {
                SliderKind::AudioOutput => "audio-output-slider",
                SliderKind::AudioInput => "audio-input-slider",
                SliderKind::Brightness => "brightness-slider",
            })
            .relative()
            .h(px(30.0))
            .flex_1()
            .flex()
            .items_center()
            .cursor_pointer()
            .when(!status.available, |slider| slider.opacity(0.4))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, _, cx| {
                    this.begin_slider_drag(kind, left, width, event, cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(4.0))
                    .rounded(px(2.0))
                    .bg(theme.border)
                    .child(
                        div()
                            .absolute()
                            .left(px(0.0))
                            .top(px(0.0))
                            .h_full()
                            .w(relative(fraction))
                            .rounded(px(2.0))
                            .bg(theme.foreground),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(relative(fraction))
                            .top(px(-4.0))
                            .ml(px(-6.0))
                            .size(px(12.0))
                            .rounded(px(6.0))
                            .bg(theme.foreground)
                            .border_1()
                            .border_color(theme.background.alpha(1.0))
                            .when(dragging, |knob| {
                                knob.child(
                                    div()
                                        .absolute()
                                        .bottom(px(18.0))
                                        .left(px(-14.0))
                                        .min_w(px(40.0))
                                        .px(px(5.0))
                                        .py(px(3.0))
                                        .rounded(px(5.0))
                                        .bg(theme.background.alpha(1.0))
                                        .border_1()
                                        .border_color(theme.border)
                                        .text_center()
                                        .text_size(px(10.0))
                                        .text_color(theme.foreground)
                                        .child(format!("{percent}%")),
                                )
                            }),
                    ),
            )
            .when(!status.available, |slider| {
                slider.child(
                    div()
                        .absolute()
                        .left(relative(0.5))
                        .ml(px(-28.0))
                        .px(px(4.0))
                        .bg(theme.background.alpha(0.9))
                        .text_size(px(9.0))
                        .text_color(theme.muted_foreground)
                        .child("利用不可"),
                )
            })
            .into_any_element()
    }

    fn render_audio_row(
        endpoint: AudioEndpoint,
        status: LevelStatus,
        slider_left: f32,
        slider_width: f32,
        theme: BarTheme,
        dragging: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (row_id, mute_id, device_id, kind, unmuted_icon) = match endpoint {
            AudioEndpoint::Output => (
                "audio-output-row",
                "audio-output-mute",
                "audio-output-device",
                SliderKind::AudioOutput,
                "",
            ),
            AudioEndpoint::Input => (
                "audio-input-row",
                "audio-input-mute",
                "audio-input-device",
                SliderKind::AudioInput,
                "",
            ),
        };
        let mute_icon = match (endpoint, status.muted) {
            (AudioEndpoint::Output, true) => "",
            (AudioEndpoint::Input, true) => "",
            (_, false) => unmuted_icon,
        };
        let slider =
            Self::render_slider(kind, status, slider_left, slider_width, theme, dragging, cx);

        div()
            .id(row_id)
            .h(px(38.0))
            .flex()
            .items_center()
            .gap(px(CONTROL_GAP))
            .child(
                div()
                    .id(mute_id)
                    .w(px(CONTROL_ICON_BUTTON_WIDTH))
                    .h(px(30.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .text_size(px(14.0))
                    .cursor_pointer()
                    .when(!status.available, |button| button.opacity(0.4))
                    .hover(|style| style.bg(theme.active_background))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_mute(endpoint, cx);
                    }))
                    .child(mute_icon),
            )
            .child(slider)
            .child(
                div()
                    .id(device_id)
                    .w(px(CONTROL_ICON_BUTTON_WIDTH))
                    .h(px(30.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .text_size(px(14.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.active_background))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.launch_device_control_center(window, cx);
                    }))
                    .child(""),
            )
            .into_any_element()
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
        let calendar_month = self.calendar_month;
        let calendar_days = calendar_days_for_month(calendar_month.year(), calendar_month.month());
        let slide_distance = f32::from(window.bounds().size.width);
        let control_width = (slide_distance - CONTROL_PADDING * 2.0).max(1.0);
        let audio_slider_left = CONTROL_PADDING + CONTROL_ICON_BUTTON_WIDTH + CONTROL_GAP;
        let audio_slider_width =
            (control_width - CONTROL_ICON_BUTTON_WIDTH * 2.0 - CONTROL_GAP * 2.0).max(1.0);
        let wifi = self.controls.wifi.clone();
        let bluetooth = self.controls.bluetooth.clone();
        let audio_output = self.controls.audio_output;
        let audio_input = self.controls.audio_input;
        let brightness = self.controls.brightness;
        let output_dragging = self
            .slider_drag
            .is_some_and(|drag| drag.kind == SliderKind::AudioOutput);
        let input_dragging = self
            .slider_drag
            .is_some_and(|drag| drag.kind == SliderKind::AudioInput);
        let brightness_dragging = self
            .slider_drag
            .is_some_and(|drag| drag.kind == SliderKind::Brightness);

        let output_row = Self::render_audio_row(
            AudioEndpoint::Output,
            audio_output,
            audio_slider_left,
            audio_slider_width,
            theme,
            output_dragging,
            cx,
        );
        let input_row = Self::render_audio_row(
            AudioEndpoint::Input,
            audio_input,
            audio_slider_left,
            audio_slider_width,
            theme,
            input_dragging,
            cx,
        );
        let brightness_slider = Self::render_slider(
            SliderKind::Brightness,
            brightness,
            audio_slider_left,
            audio_slider_width,
            theme,
            brightness_dragging,
            cx,
        );
        let brightness_row = div()
            .id("brightness-row")
            .h(px(38.0))
            .flex()
            .items_center()
            .gap(px(CONTROL_GAP))
            .child(
                div()
                    .id("brightness-icon")
                    .w(px(CONTROL_ICON_BUTTON_WIDTH))
                    .h(px(30.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(18.0))
                    .child("\u{f0599}"),
            )
            .child(brightness_slider)
            .child(
                div()
                    .w(px(CONTROL_ICON_BUTTON_WIDTH))
                    .h(px(30.0))
                    .flex_none(),
            );

        let (network_icon, network_title) = if self
            .controls
            .primary_network
            .as_ref()
            .is_some_and(|route| route.kind == NetworkKind::Wired)
        {
            ("󰈀", "有線接続")
        } else {
            ("", "Wi-Fi")
        };
        let wifi_button =
            control_toggle_button("wifi-control", network_icon, network_title, &wifi, theme)
                .flex_grow(1.0)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.toggle_wifi(cx);
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _, window, cx| {
                        this.launch_device_control_center(window, cx);
                        cx.stop_propagation();
                    }),
                );
        let bluetooth_button =
            control_toggle_button("bluetooth-control", "", "Bluetooth", &bluetooth, theme)
                .flex_grow(1.0)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.toggle_bluetooth(cx);
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _, window, cx| {
                        this.launch_device_control_center(window, cx);
                        cx.stop_propagation();
                    }),
                );

        let controls_panel = div()
            .id("system-controls")
            .px(px(CONTROL_PADDING))
            .py(px(12.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .h(px(56.0))
                    .flex()
                    .gap(px(CONTROL_GAP))
                    .child(wifi_button)
                    .child(bluetooth_button),
            )
            .child(output_row)
            .child(input_row)
            .child(brightness_row);

        div()
            .size_full()
            .overflow_hidden()
            .on_mouse_move(cx.listener(|this, event, _, cx| {
                this.move_slider(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    this.end_slider_drag(event.position.x.into(), cx);
                }),
            )
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
                                .border_b_1()
                                .border_color(theme.border)
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .child(
                                            div()
                                                .id("notification-tray-settings")
                                                .size(px(22.0))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded(px(5.0))
                                                .text_color(theme.muted_foreground)
                                                .hover(|style| style.bg(theme.active_background))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.launch_config_window(window, cx);
                                                }))
                                                .child(""),
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
                        .child(controls_panel)
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
                                    div().flex().items_center().gap(px(6.0)).child(
                                        div()
                                            .id("clear-notifications")
                                            .px(px(7.0))
                                            .py(px(4.0))
                                            .rounded(px(5.0))
                                            .text_size(px(11.0))
                                            .text_color(theme.muted_foreground)
                                            .hover(|style| style.bg(theme.active_background))
                                            .on_click(cx.listener(|this, _, _, cx| this.clear(cx)))
                                            .child("Clear"),
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
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .child(
                                                    div()
                                                        .id("calendar-previous-month")
                                                        .size(px(22.0))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .rounded(px(5.0))
                                                        .cursor_pointer()
                                                        .text_color(theme.muted_foreground)
                                                        .hover(|style| {
                                                            style.bg(theme.active_background)
                                                        })
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.show_previous_calendar_month(cx);
                                                        }))
                                                        .child(""),
                                                )
                                                .child(
                                                    div()
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_size(px(13.0))
                                                        .child(format!(
                                                            "{}年{}月",
                                                            calendar_month.year(),
                                                            calendar_month.month()
                                                        )),
                                                )
                                                .child(
                                                    div()
                                                        .id("calendar-next-month")
                                                        .size(px(22.0))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .rounded(px(5.0))
                                                        .cursor_pointer()
                                                        .text_color(theme.muted_foreground)
                                                        .hover(|style| {
                                                            style.bg(theme.active_background)
                                                        })
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.show_next_calendar_month(cx);
                                                        }))
                                                        .child(""),
                                                ),
                                        ),
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
                                            let is_today = calendar_month.year() == today.year()
                                                && calendar_month.month() == today.month()
                                                && day == Some(today.day());

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

fn control_toggle_button(
    id: &'static str,
    icon: &'static str,
    title: &'static str,
    status: &ToggleStatus,
    theme: BarTheme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .min_w(px(0.0))
        .h_full()
        .px(px(12.0))
        .flex_basis(px(0.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .when(status.enabled, |button| button.bg(theme.active_background))
        .when(!status.available, |button| button.opacity(0.4))
        .hover(|style| style.bg(theme.active_background))
        .child(div().flex_none().text_size(px(18.0)).child(icon))
        .child(
            div()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(FontWeight::MEDIUM)
                        .child(title),
                )
                .when(!status.wired, |content| {
                    content.child(
                        div()
                            .overflow_hidden()
                            .text_size(px(10.0))
                            .text_color(theme.muted_foreground)
                            .child(status.label.clone()),
                    )
                }),
        )
}

fn slider_percent_from_pointer(pointer_x: f32, left: f32, width: f32) -> u8 {
    (((pointer_x - left) / width.max(1.0)) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u8
}

fn adjacent_calendar_month(month: NaiveDate, direction: i32) -> NaiveDate {
    debug_assert!(matches!(direction, -1 | 1));

    let (year, month_number) = match (month.month(), direction) {
        (1, -1) => (month.year() - 1, 12),
        (12, 1) => (month.year() + 1, 1),
        (month_number, -1) => (month.year(), month_number - 1),
        (month_number, 1) => (month.year(), month_number + 1),
        _ => unreachable!("calendar direction must be one month"),
    };

    NaiveDate::from_ymd_opt(year, month_number, 1)
        .expect("adjacent calendar month is within Chrono's date range")
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
    use chrono::NaiveDate;

    use super::{adjacent_calendar_month, calendar_days_for_month, slider_percent_from_pointer};

    #[test]
    fn calendar_navigation_crosses_year_boundaries() {
        assert_eq!(
            adjacent_calendar_month(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), -1),
            NaiveDate::from_ymd_opt(2025, 12, 1).unwrap()
        );
        assert_eq!(
            adjacent_calendar_month(NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(), 1),
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()
        );
    }

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

    #[test]
    fn slider_percentage_tracks_and_clamps_the_pointer() {
        assert_eq!(slider_percent_from_pointer(10.0, 10.0, 200.0), 0);
        assert_eq!(slider_percent_from_pointer(110.0, 10.0, 200.0), 50);
        assert_eq!(slider_percent_from_pointer(210.0, 10.0, 200.0), 100);
        assert_eq!(slider_percent_from_pointer(-50.0, 10.0, 200.0), 0);
        assert_eq!(slider_percent_from_pointer(500.0, 10.0, 200.0), 100);
    }
}
