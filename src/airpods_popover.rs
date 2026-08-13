use std::{f32::consts::TAU, path::PathBuf};

use async_channel::Sender;
use gpui::{Context, Render, Size, Window, canvas, div, img, point, prelude::*, px};

use crate::{
    modules::{
        airpods::AirPodsListeningMode,
        system_controls::{ControlAction, ControlSnapshot},
    },
    theme::{BarTheme, SurfaceRole, ui_font},
};

const POPOVER_SIZE: f32 = 280.0;
const RING_SIZE: f32 = 142.0;
const TRANSPARENCY_ICON: &str = "\u{f07c5}";
const ADAPTIVE_ICON: &str = "\u{f2a2}";
const NOISE_CANCELLATION_ICON: &str = "\u{f0a45}";

#[derive(Clone, Copy)]
struct PendingModeChange {
    requested: AirPodsListeningMode,
    previous: Option<AirPodsListeningMode>,
}

pub fn window_size() -> Size<gpui::Pixels> {
    Size::new(px(POPOVER_SIZE), px(286.0))
}

pub struct AirPodsPopover {
    controls: ControlSnapshot,
    actions: Sender<ControlAction>,
    theme: BarTheme,
    pending_mode_change: Option<PendingModeChange>,
}

impl AirPodsPopover {
    pub fn new(controls: ControlSnapshot, actions: Sender<ControlAction>, theme: BarTheme) -> Self {
        Self {
            controls,
            actions,
            theme,
            pending_mode_change: None,
        }
    }

    pub fn set_controls(
        &mut self,
        mut controls: ControlSnapshot,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pending) = self.pending_mode_change {
            if controls.airpods.listening_mode == Some(pending.requested) {
                self.pending_mode_change = None;
            } else if controls.airpods.message.is_some() {
                controls.airpods.listening_mode = pending.previous;
                self.pending_mode_change = None;
            } else {
                // The control worker can publish one stale snapshot before the
                // AirPods worker handles the command. Keep the optimistic
                // selection until the worker confirms it or reports failure.
                controls.airpods.listening_mode = Some(pending.requested);
            }
        }
        self.controls = controls;
        cx.notify();
    }

    fn set_mode(&mut self, mode: AirPodsListeningMode, cx: &mut Context<Self>) {
        if self.controls.airpods.ready {
            let previous = self.controls.airpods.listening_mode;
            self.controls.airpods.listening_mode = Some(mode);
            self.controls.airpods.message = None;
            if self
                .actions
                .try_send(ControlAction::SetAirPodsListeningMode(mode))
                .is_err()
            {
                self.controls.airpods.listening_mode = previous;
                self.controls.airpods.message =
                    Some("AirPodsへの操作要求を送信できませんでした".to_string());
                self.pending_mode_change = None;
            } else {
                self.pending_mode_change = Some(PendingModeChange {
                    requested: mode,
                    previous,
                });
            }
        }
        cx.notify();
    }
}

impl Render for AirPodsPopover {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let average = self.controls.airpods.average_percent;
        let ready = self.controls.airpods.ready;
        let mode = self.controls.airpods.listening_mode;
        let left_battery = self
            .controls
            .airpods
            .left_percent
            .map_or_else(|| "—".to_string(), |percent| percent.to_string());
        let right_battery = self
            .controls
            .airpods
            .right_percent
            .map_or_else(|| "—".to_string(), |percent| percent.to_string());
        let image_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/icon/airpods_icon.svg");

        div()
            .size_full()
            .p(px(14.0))
            .rounded(theme.panel_radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface(SurfaceRole::Floating))
            .text_color(theme.foreground)
            .font(ui_font())
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .relative()
                    .size(px(RING_SIZE))
                    .child(progress_ring(average, theme))
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(img(image_path).size(px(76.0))),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .h(px(16.0))
                    .text_size(px(12.0))
                    .text_color(theme.muted_foreground)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(format!("{left_battery}% / {right_battery}%")),
            )
            .child(
                div()
                    .w_full()
                    .h(px(54.0))
                    .flex()
                    .gap(px(4.0))
                    .child(mode_button(
                        "transparency",
                        "外音取り込み",
                        TRANSPARENCY_ICON,
                        AirPodsListeningMode::Transparency,
                        mode,
                        ready,
                        theme,
                        cx,
                    ))
                    .child(mode_button(
                        "adaptive",
                        "アダプティブ",
                        ADAPTIVE_ICON,
                        AirPodsListeningMode::Adaptive,
                        mode,
                        ready,
                        theme,
                        cx,
                    ))
                    .child(mode_button(
                        "noise-cancellation",
                        "ノイズキャンセリング",
                        NOISE_CANCELLATION_ICON,
                        AirPodsListeningMode::NoiseCancellation,
                        mode,
                        ready,
                        theme,
                        cx,
                    )),
            )
            .when_some(self.controls.airpods.message.clone(), |root, message| {
                root.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.error)
                        .child(message),
                )
            })
            .when(!ready && self.controls.airpods.message.is_none(), |root| {
                root.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.muted_foreground)
                        .child("AirPodsを準備中…"),
                )
            })
    }
}

fn mode_button(
    id: &'static str,
    label: &'static str,
    icon: &'static str,
    candidate: AirPodsListeningMode,
    selected: Option<AirPodsListeningMode>,
    enabled: bool,
    theme: BarTheme,
    cx: &mut Context<AirPodsPopover>,
) -> impl IntoElement {
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
                .id(format!("airpods-mode-{id}"))
                .w_full()
                .h(px(34.0))
                .rounded(theme.control_radius)
                .border_1()
                .border_color(theme.border)
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(22.0))
                .text_color(if is_selected {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .bg(if is_selected {
                    theme.active_background
                } else {
                    theme.container_background
                })
                .opacity(if enabled { 1.0 } else { 0.45 })
                .cursor_pointer()
                .hover(|style| style.bg(theme.active_background))
                .on_click(cx.listener(move |this, _, _, cx| this.set_mode(candidate, cx)))
                .child(icon),
        )
        .child(
            div()
                .h(px(16.0))
                .text_size(px(8.0))
                .text_color(if is_selected {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .child(label),
        )
}

fn progress_ring(percent: Option<u8>, theme: BarTheme) -> impl IntoElement {
    canvas(
        move |bounds, _, _| bounds,
        move |bounds, _, window, _| {
            let center = point(
                bounds.origin.x + bounds.size.width / 2.0,
                bounds.origin.y + bounds.size.height / 2.0,
            );
            let radius = bounds.size.width.min(bounds.size.height) / 2.0 - px(4.0);
            let mut background = gpui::PathBuilder::stroke(px(5.0));
            let steps = 64;
            for index in 0..=steps {
                let angle = -std::f32::consts::FRAC_PI_2 + TAU * index as f32 / steps as f32;
                let point = point(
                    center.x + radius * angle.cos(),
                    center.y + radius * angle.sin(),
                );
                if index == 0 {
                    background.move_to(point);
                } else {
                    background.line_to(point);
                }
            }
            if let Ok(path) = background.build() {
                window.paint_path(path, theme.border);
            }
            if let Some(percent) = percent {
                let count =
                    ((steps as f32 * percent.min(100) as f32 / 100.0).round() as usize).max(1);
                let mut foreground = gpui::PathBuilder::stroke(px(5.0));
                for index in 0..=count {
                    let angle = -std::f32::consts::FRAC_PI_2 + TAU * index as f32 / steps as f32;
                    let point = point(
                        center.x + radius * angle.cos(),
                        center.y + radius * angle.sin(),
                    );
                    if index == 0 {
                        foreground.move_to(point);
                    } else {
                        foreground.line_to(point);
                    }
                }
                if let Ok(path) = foreground.build() {
                    window.paint_path(path, theme.success);
                }
            }
        },
    )
    .absolute()
    .inset_0()
}
