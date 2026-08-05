use std::{sync::mpsc::Receiver, time::Duration};

use gpui::{Context, FontWeight, Render, Window, div, prelude::*};
use log::{error, warn};

use crate::{
    hyprland::IpcUpdate,
    modules::{clock::Clock, workspaces::Workspaces},
    theme::BarTheme,
};

const FONT_FAMILY: &str = "JetBrainsMono Nerd Font Mono";

/// GPUI entity that owns bar-local state and redraw scheduling.
pub struct Bar {
    clock: Clock,
    workspaces: Option<Workspaces>,
    ipc_updates: Receiver<IpcUpdate>,
    theme: BarTheme,
}

impl Bar {
    pub fn new(ipc_updates: Receiver<IpcUpdate>, theme: BarTheme, cx: &mut Context<Self>) -> Self {
        let bar = Self {
            clock: Clock::new(),
            workspaces: None,
            ipc_updates,
            theme,
        };
        Self::start_clock(cx);
        Self::start_ipc_updates(cx);
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

    fn start_ipc_updates(cx: &mut Context<Self>) {
        cx.spawn(async move |bar, cx| {
            loop {
                if bar
                    .update(cx, |bar, cx| {
                        if bar.apply_ipc_updates() {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
            }
        })
        .detach();
    }

    fn apply_ipc_updates(&mut self) -> bool {
        let mut changed = false;
        while let Ok(update) = self.ipc_updates.try_recv() {
            match update {
                IpcUpdate::Workspaces(snapshot) => {
                    if let Some(current) = &mut self.workspaces {
                        current.replace(snapshot, self.theme);
                    } else {
                        self.workspaces = Some(Workspaces::new(snapshot));
                    }
                    changed = true;
                }
                IpcUpdate::Unavailable(message) => {
                    warn!("Hyprland IPC unavailable; continuing with clock only: {message}");
                }
                IpcUpdate::WorkerStopped(message) => {
                    error!("Hyprland IPC worker stopped: {message}");
                }
            }
        }
        changed
    }
}

impl Render for Bar {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let left = self
            .workspaces
            .as_ref()
            .map(|workspaces| workspaces.render(theme))
            .unwrap_or_else(div);

        div()
            .size_full()
            .flex()
            .items_center()
            .px(theme.horizontal_padding)
            .gap(theme.module_spacing)
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border)
            .text_color(theme.foreground)
            .font_family(FONT_FAMILY)
            .child(left)
            .child(div().flex_1())
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(theme.clock_font_size)
                    .text_color(theme.foreground)
                    .child(self.clock.value().to_string()),
            )
    }
}
