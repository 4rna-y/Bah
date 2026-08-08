mod app;
mod bar;
mod config;
mod config_window;
mod hyprland;
mod memory_usage;
mod modules;
mod notification_tray;
mod theme;

use log::{error, info};

fn main() {
    env_logger::init();
    memory_usage::start_if_enabled();

    let mode = match app::RunMode::from_environment() {
        Ok(mode) => mode,
        Err(message) => {
            error!("{message}");
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    info!("starting bah");
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

    app::run(mode, config);
}
