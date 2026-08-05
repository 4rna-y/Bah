use std::sync::mpsc;

use gpui::{
    App, AppContext, Bounds, Size, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, layer_shell::*, point, px,
};
use gpui_platform::application;
use log::{error, info};

use crate::{bar::Bar, config::Config, hyprland, theme::BarTheme};

/// Creates the non-focusable, top-anchored layer-shell surface.
pub fn run(config: Config) {
    application().run(move |cx: &mut App| {
        let (sender, receiver) = mpsc::channel();
        hyprland::start_worker(sender);

        let theme = BarTheme::from_environment(config.bar_height);
        let height = theme.bar_height;
        info!(
            "visual mode: {}{}",
            if theme.high_contrast {
                "high contrast"
            } else {
                "standard"
            },
            if theme.transparency_disabled {
                ", transparency disabled"
            } else {
                ""
            }
        );
        let result = cx.open_window(
            WindowOptions {
                titlebar: None,
                focus: false,
                app_id: Some("hyprbar".to_string()),
                window_background: WindowBackgroundAppearance::Transparent,
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    // The GPUI patch sends zero to the layer-shell protocol for
                    // opposing anchors while keeping the renderer non-zero.
                    size: Size::new(px(1.0), height),
                })),
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: "hyprbar".to_string(),
                    layer: Layer::Top,
                    anchor: Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
                    exclusive_zone: Some(height),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |_, cx| cx.new(|cx| Bar::new(receiver, theme, cx)),
        );

        match result {
            Ok(_) => info!("Layer Shell window created"),
            Err(error) => error!("failed to create Layer Shell window: {error}"),
        }
    });
}
