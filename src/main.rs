mod app;
mod bar;
mod config;
mod hyprland;
mod modules;
mod theme;

use log::{error, info};

fn main() {
    env_logger::init();
    info!("starting hyprbar");
    info!(
        "Wayland display: {}",
        std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "<unset>".to_string())
    );

    let config = match config::Config::load() {
        Ok(config) => config,
        Err(error) => {
            error!("configuration could not be loaded: {error}; using defaults");
            config::Config::default()
        }
    };

    app::run(config);
}
