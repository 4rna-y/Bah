mod airpods_popover;
mod app;
mod bar;
mod config;
mod config_window;
mod device_control_center;
mod hyprland;
mod memory_usage;
mod modules;
mod network_popover;
mod notification_popup;
mod notification_tray;
mod theme;
mod tui_device_control_center;
mod wallpaper;
mod window_snapshot;
mod window_switcher;

use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use log::{LevelFilter, error, info};

/// Sends each log record to the terminal and the persistent per-user log.
struct TeeWriter {
    stdout: io::Stdout,
    file: fs::File,
}

impl Write for TeeWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stdout.write_all(buffer)?;
        self.file.write_all(buffer)?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()?;
        self.file.flush()
    }
}

fn log_file_path() -> PathBuf {
    if let Some(path) = env::var_os("BAH_LOG_FILE") {
        return PathBuf::from(path);
    }
    let state_home = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    state_home.join("bah/bah.log")
}

fn open_log_file(path: &Path) -> io::Result<fs::File> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::OpenOptions::new().create(true).append(true).open(path)
}

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
    // zbus emits one INFO entry for every NetworkManager proxy cache. Keep
    // application-level INFO logs useful without flooding the journal.
    logger.filter_module("zbus", LevelFilter::Warn);
    if options.memory_usage && std::env::var_os("RUST_LOG").is_none() {
        logger.filter_module("bah::memory_usage", log::LevelFilter::Info);
    }
    let log_path = log_file_path();
    match open_log_file(&log_path) {
        Ok(file) => {
            if matches!(options.mode, app::RunMode::DeviceControlCenterTui(_)) {
                // The TUI owns stdout for its alternate-screen renderer. Sending
                // log records there corrupts Ratatui's cell buffer, so keep them
                // in the persistent per-user log only.
                logger.target(env_logger::Target::Pipe(Box::new(file)));
            } else {
                logger.target(env_logger::Target::Pipe(Box::new(TeeWriter {
                    stdout: io::stdout(),
                    file,
                })));
            }
        }
        Err(error) => {
            eprintln!(
                "bah: could not open log file {}: {error}",
                log_path.display()
            );
        }
    }
    logger.init();
    memory_usage::start_if_enabled();

    info!("starting bah");
    info!("log file: {}", log_path.display());
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

    match app::handle_wallpaper_command(&options.mode, config.clone()) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            error!("wallpaper command failed: {error:#}");
            eprintln!("bah: {error:#}");
            std::process::exit(1);
        }
    }

    app::run(options.mode, config);
}
