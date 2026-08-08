use gpui::{Context, FontWeight, Render, Window, div, prelude::*, px, rgb};

use crate::{app::ConfigWindowLock, config::Config};

/// Root view for the standalone `bah window config` window.
pub struct ConfigWindow {
    config: Config,
    // Keep the process-wide lock alive for exactly as long as this root view exists.
    _lock: ConfigWindowLock,
}

impl ConfigWindow {
    pub fn new(config: Config, lock: ConfigWindowLock, _cx: &mut Context<Self>) -> Self {
        Self {
            config,
            _lock: lock,
        }
    }
}

impl Render for ConfigWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let panel = rgb(0x202128);
        let muted = rgb(0xaeb1bd);

        div()
            .size_full()
            .flex()
            .flex_col()
            .p(px(28.0))
            .gap(px(20.0))
            .bg(rgb(0x17181e))
            .text_color(rgb(0xf5f5f7))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(px(24.0))
                            .font_weight(FontWeight::BOLD)
                            .child("bah Settings"),
                    )
                    .child(
                        div().text_size(px(14.0)).text_color(muted).child(
                            "Configuration is loaded from $XDG_CONFIG_HOME/bah/config.toml.",
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .p(px(18.0))
                    .bg(panel)
                    .rounded(px(8.0))
                    .child(
                        div()
                            .text_size(px(16.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Bar"),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(div().text_color(muted).child("Height"))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(format!("{:.0} px", self.config.bar_height)),
                            ),
                    ),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(muted)
                    .child("Only one settings window can be opened at a time."),
            )
    }
}
