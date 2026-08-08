mod app;
mod bar;
mod config;
mod config_window;
mod device_control_center;
mod hyprland;
mod memory_usage;
mod modules;
mod notification_tray;
mod theme;

use log::{error, info};

fn main() {
    let options = match app::StartupOptions::from_environment() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    // SAFETY: startup parsing does not create threads, and this runs before the logger, GPUI, or
    // any worker is initialized. No other thread can concurrently access the environment.
    unsafe {
        if options.memory_usage {
            std::env::set_var("BAH_MEMUSG", "1");
        }
        if let Some(wgpu_backend) = &options.wgpu_backend {
            std::env::set_var("WGPU_BACKEND", wgpu_backend);
        }
        if let Some(vk_driver_files) = &options.vk_driver_files {
            std::env::set_var("VK_DRIVER_FILES", vk_driver_files);
        }
    }

    let mut logger = env_logger::Builder::from_default_env();
    if options.memory_usage && std::env::var_os("RUST_LOG").is_none() {
        logger.filter_module("bah::memory_usage", log::LevelFilter::Info);
    }
    logger.init();
    memory_usage::start_if_enabled();

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

    app::run(options.mode, config);
}
