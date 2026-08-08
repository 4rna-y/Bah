use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io,
    os::fd::AsRawFd,
    path::PathBuf,
};

use gpui::{
    App, AppContext, Bounds, Size, WindowBackgroundAppearance, WindowBounds, WindowDecorations,
    WindowKind, WindowOptions, layer_shell::*, point, px,
};
use gpui_platform::application;
use log::{error, info};

use crate::{
    bar::Bar,
    config::Config,
    config_window::ConfigWindow,
    device_control_center::DeviceControlCenter,
    hyprland,
    modules::{
        notifications::{NotificationStore, start_server},
        system_controls,
    },
    theme::BarTheme,
};

const CONFIG_WINDOW_LOCK_FILE: &str = "bah/config-window.lock";
const DEVICE_CONTROL_CENTER_LOCK_FILE: &str = "bah/device-control-center.lock";
const USAGE: &str = "usage: bah [--memusg] [--wgpu-backend BACKENDS] [--vk-driver-files PATH] \
     [window config|window device-control-center]";

/// Options that must be applied before GPUI initializes its graphics backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupOptions {
    pub mode: RunMode,
    pub memory_usage: bool,
    pub wgpu_backend: Option<OsString>,
    pub vk_driver_files: Option<OsString>,
}

impl StartupOptions {
    pub fn from_environment() -> Result<Self, String> {
        Self::from_args(env::args_os().skip(1))
    }

    fn from_args<I, S>(arguments: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut arguments = arguments
            .into_iter()
            .map(|argument| argument.as_ref().to_os_string());
        let mut positional = Vec::new();
        let mut memory_usage = false;
        let mut wgpu_backend = None;
        let mut vk_driver_files = None;

        while let Some(argument) = arguments.next() {
            if argument == "--memusg" || argument == "--bah-memusg" || argument == "--bah_memusg" {
                if memory_usage {
                    return Err(format!("--memusg may only be specified once\n{USAGE}"));
                }
                memory_usage = true;
            } else if argument == "--wgpu-backend" || argument == "--wgpu_backend" {
                if wgpu_backend.is_some() {
                    return Err(format!(
                        "--wgpu-backend may only be specified once\n{USAGE}"
                    ));
                }
                let value = arguments
                    .next()
                    .ok_or_else(|| format!("--wgpu-backend requires a value\n{USAGE}"))?;
                validate_wgpu_backend(&value)?;
                wgpu_backend = Some(value);
            } else if argument == "--vk-driver-files" || argument == "--vk_driver_files" {
                if vk_driver_files.is_some() {
                    return Err(format!(
                        "--vk-driver-files may only be specified once\n{USAGE}"
                    ));
                }
                let value = arguments
                    .next()
                    .ok_or_else(|| format!("--vk-driver-files requires a path\n{USAGE}"))?;
                if value.is_empty() {
                    return Err(format!(
                        "--vk-driver-files requires a non-empty path\n{USAGE}"
                    ));
                }
                vk_driver_files = Some(value);
            } else {
                positional.push(argument);
            }
        }

        Ok(Self {
            mode: RunMode::from_args(positional)?,
            memory_usage,
            wgpu_backend,
            vk_driver_files,
        })
    }
}

fn validate_wgpu_backend(value: &OsStr) -> Result<(), String> {
    let value = value
        .to_str()
        .ok_or_else(|| format!("--wgpu-backend must be valid UTF-8\n{USAGE}"))?;
    let valid = !value.is_empty()
        && value.split(',').all(|backend| {
            matches!(
                backend.trim().to_ascii_lowercase().as_str(),
                "vulkan"
                    | "vk"
                    | "dx12"
                    | "d3d12"
                    | "metal"
                    | "mtl"
                    | "opengl"
                    | "gles"
                    | "gl"
                    | "webgpu"
                    | "noop"
            )
        });
    if valid {
        Ok(())
    } else {
        Err(format!(
            "--wgpu-backend contains an unknown backend: {value}\n{USAGE}"
        ))
    }
}

/// Selects which surface this invocation creates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunMode {
    Bar,
    ConfigWindow,
    DeviceControlCenter,
}

impl RunMode {
    fn from_args<I, S>(arguments: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let arguments = arguments
            .into_iter()
            .map(|argument| argument.as_ref().to_os_string())
            .collect::<Vec<_>>();
        match arguments.as_slice() {
            [] => Ok(Self::Bar),
            [window, config] if window == "window" && config == "config" => Ok(Self::ConfigWindow),
            [window, device_control_center]
                if window == "window" && device_control_center == "device-control-center" =>
            {
                Ok(Self::DeviceControlCenter)
            }
            _ => Err(USAGE.to_string()),
        }
    }
}

/// Creates the non-focusable, top-anchored layer-shell surface.
pub fn run(mode: RunMode, config: Config) {
    match mode {
        RunMode::Bar => run_bar(config),
        RunMode::ConfigWindow => run_config_window(config),
        RunMode::DeviceControlCenter => run_device_control_center(),
    }
}

fn run_bar(config: Config) {
    application().run(move |cx: &mut App| {
        let (sender, receiver) = async_channel::unbounded();
        hyprland::start_worker(sender);
        let (notification_sender, notification_receiver) = async_channel::unbounded();
        start_server(notification_sender.clone());
        let notifications = NotificationStore::shared();
        let controls = system_controls::start_worker();

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
                app_id: Some("bah".to_string()),
                window_background: WindowBackgroundAppearance::Transparent,
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    // The GPUI patch sends zero to the layer-shell protocol for
                    // opposing anchors while keeping the renderer non-zero.
                    size: Size::new(px(1.0), height),
                })),
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: "bah".to_string(),
                    layer: Layer::Top,
                    anchor: Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
                    exclusive_zone: Some(height),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |_, cx| {
                cx.new(|cx| {
                    Bar::new(
                        receiver,
                        notification_receiver,
                        notification_sender,
                        notifications,
                        controls,
                        theme,
                        cx,
                    )
                })
            },
        );

        match result {
            Ok(_) => info!("Layer Shell window created"),
            Err(error) => error!("failed to create Layer Shell window: {error}"),
        }
    });
}

fn run_device_control_center() {
    let lock = match DeviceControlCenterLock::acquire() {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            info!("a device control center window is already open; ignoring this invocation");
            return;
        }
        Err(error) => {
            error!("could not lock device control center window: {error}");
            return;
        }
    };

    application().run(move |cx: &mut App| {
        let size = Size::new(px(900.0), px(650.0));
        let result = cx.open_window(
            WindowOptions {
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("bah Device Control Center".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                focus: true,
                show: true,
                is_movable: true,
                kind: WindowKind::Normal,
                window_background: WindowBackgroundAppearance::Opaque,
                app_id: Some("bah-device-control-center".to_string()),
                window_decorations: Some(WindowDecorations::Server),
                window_bounds: Some(WindowBounds::centered(size, cx)),
                window_min_size: Some(Size::new(px(480.0), px(320.0))),
                ..Default::default()
            },
            move |_, cx| cx.new(|cx| DeviceControlCenter::new(lock, cx)),
        );

        match result {
            Ok(_) => {
                hyprland::force_float_window_for_process(
                    "bah-device-control-center",
                    std::process::id(),
                );
                info!("device control center window created")
            }
            Err(error) => error!("failed to create device control center window: {error}"),
        }
    });
}

fn run_config_window(config: Config) {
    let lock = match ConfigWindowLock::acquire() {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            info!("a configuration window is already open; ignoring this invocation");
            return;
        }
        Err(error) => {
            error!("could not lock configuration window: {error}");
            return;
        }
    };

    application().run(move |cx: &mut App| {
        let size = Size::new(px(520.0), px(360.0));
        let result = cx.open_window(
            WindowOptions {
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("bah Settings".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                focus: true,
                show: true,
                is_movable: true,
                kind: WindowKind::Normal,
                window_background: WindowBackgroundAppearance::Opaque,
                app_id: Some("bah-settings".to_string()),
                window_decorations: Some(WindowDecorations::Server),
                window_bounds: Some(WindowBounds::centered(size, cx)),
                window_min_size: Some(Size::new(px(400.0), px(280.0))),
                ..Default::default()
            },
            move |_, cx| cx.new(|cx| ConfigWindow::new(config, lock, cx)),
        );

        match result {
            Ok(_) => info!("configuration window created"),
            Err(error) => error!("failed to create configuration window: {error}"),
        }
    });
}

/// An advisory, per-user lock held for the lifetime of a configuration window.
///
/// `flock` is tied to the file descriptor, so it is released even if the process
/// terminates unexpectedly. This avoids stale PID or lock files blocking future
/// launches.
pub struct ConfigWindowLock {
    _file: File,
}

impl ConfigWindowLock {
    fn acquire() -> io::Result<Option<Self>> {
        let path = config_window_lock_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;

        // `flock(LOCK_EX | LOCK_NB)` is available on the Linux targets supported
        // by Hyprland. Error 11 is EAGAIN/EWOULDBLOCK: another window owns the lock.
        let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
        if result == 0 {
            Ok(Some(Self { _file: file }))
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(11) {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

fn config_window_lock_path() -> io::Result<PathBuf> {
    let root = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    Ok(root.join(CONFIG_WINDOW_LOCK_FILE))
}

/// Advisory lock held for the lifetime of the device-control-center window.
pub struct DeviceControlCenterLock {
    _file: File,
}

impl DeviceControlCenterLock {
    fn acquire() -> io::Result<Option<Self>> {
        let path = runtime_lock_path(DEVICE_CONTROL_CENTER_LOCK_FILE)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
        if result == 0 {
            Ok(Some(Self { _file: file }))
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(11) {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

fn runtime_lock_path(name: &str) -> io::Result<PathBuf> {
    let root = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    Ok(root.join(name))
}

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::{RunMode, StartupOptions};

    #[test]
    fn parses_supported_invocations() {
        assert_eq!(RunMode::from_args([] as [&str; 0]), Ok(RunMode::Bar));
        assert_eq!(
            RunMode::from_args(["window", "config"]),
            Ok(RunMode::ConfigWindow)
        );
        assert_eq!(
            RunMode::from_args(["window", "device-control-center"]),
            Ok(RunMode::DeviceControlCenter)
        );
        assert!(RunMode::from_args(["window"]).is_err());
        assert!(RunMode::from_args(["window", "other"]).is_err());
    }

    #[test]
    fn parses_vulkan_driver_files_before_or_after_the_run_mode() {
        let path = "/run/opengl-driver/share/vulkan/icd.d/intel_icd.x86_64.json";
        assert_eq!(
            StartupOptions::from_args(["--vk-driver-files", path]),
            Ok(StartupOptions {
                mode: RunMode::Bar,
                memory_usage: false,
                wgpu_backend: None,
                vk_driver_files: Some(path.into()),
            })
        );
        assert_eq!(
            StartupOptions::from_args(["window", "config", "--vk_driver_files", path,]),
            Ok(StartupOptions {
                mode: RunMode::ConfigWindow,
                memory_usage: false,
                wgpu_backend: None,
                vk_driver_files: Some(path.into()),
            })
        );
    }

    #[test]
    fn parses_memory_logging_and_wgpu_backend_options() {
        assert_eq!(
            StartupOptions::from_args(["--memusg", "--wgpu_backend", "vulkan,gl"]),
            Ok(StartupOptions {
                mode: RunMode::Bar,
                memory_usage: true,
                wgpu_backend: Some("vulkan,gl".into()),
                vk_driver_files: None,
            })
        );
        assert!(StartupOptions::from_args(["--memusg", "--bah_memusg"]).is_err());
        assert!(StartupOptions::from_args(["--wgpu-backend"]).is_err());
        assert!(StartupOptions::from_args(["--wgpu-backend", "unknown"]).is_err());
        assert!(StartupOptions::from_args(["--wgpu-backend", "vulkan,"]).is_err());
    }

    #[test]
    fn rejects_invalid_vulkan_driver_file_options() {
        assert!(StartupOptions::from_args(["--vk-driver-files"]).is_err());
        assert!(StartupOptions::from_args(["--vk-driver-files", ""]).is_err());
        assert!(
            StartupOptions::from_args([
                "--vk-driver-files",
                "first.json",
                "--vk_driver_files",
                "second.json",
            ])
            .is_err()
        );
    }
}
