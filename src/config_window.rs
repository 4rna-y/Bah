use gpui::{Context, FontWeight, Render, Window, div, prelude::*, px};

use crate::{
    app::ConfigWindowLock,
    config::Config,
    theme::{BarTheme, SurfaceRole, ui_font},
};

/// Root view for the standalone `bah window config` window.
pub struct ConfigWindow {
    config: Config,
    // Keep the process-wide lock alive for exactly as long as this root view exists.
    _lock: ConfigWindowLock,
    theme: BarTheme,
}

impl ConfigWindow {
    pub fn new(
        config: Config,
        lock: ConfigWindowLock,
        theme: BarTheme,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            config,
            _lock: lock,
            theme,
        }
    }
}

impl Render for ConfigWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;

        div()
            .size_full()
            .flex()
            .flex_col()
            .p(px(28.0))
            .gap(px(20.0))
            .bg(theme.surface(SurfaceRole::Window))
            .text_color(theme.foreground)
            .font(ui_font())
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
                        div()
                            .text_size(px(14.0))
                            .text_color(theme.muted_foreground)
                            .child(
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
                    .bg(theme.container_background)
                    .rounded(theme.panel_radius)
                    .border_1()
                    .border_color(theme.border)
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
                            .child(div().text_color(theme.muted_foreground).child("Height"))
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
                    .text_color(theme.muted_foreground)
                    .child("Only one settings window can be opened at a time."),
            )
    }
}
