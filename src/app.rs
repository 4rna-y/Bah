use std::{
    env,
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
    hyprland,
    modules::notifications::{NotificationStore, start_server},
    theme::BarTheme,
};

const CONFIG_WINDOW_LOCK_FILE: &str = "bah/config-window.lock";

/// Selects which surface this invocation creates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunMode {
    Bar,
    ConfigWindow,
}

impl RunMode {
    pub fn from_environment() -> Result<Self, String> {
        Self::from_args(env::args_os().skip(1))
    }

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
            _ => Err("usage: bah [window config]".to_string()),
        }
    }
}

/// Creates the non-focusable, top-anchored layer-shell surface.
pub fn run(mode: RunMode, config: Config) {
    match mode {
        RunMode::Bar => run_bar(config),
        RunMode::ConfigWindow => run_config_window(config),
    }
}

fn run_bar(config: Config) {
    application().run(move |cx: &mut App| {
        let (sender, receiver) = async_channel::unbounded();
        hyprland::start_worker(sender);
        let (notification_sender, notification_receiver) = async_channel::unbounded();
        start_server(notification_sender.clone());
        let notifications = NotificationStore::shared();

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

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::RunMode;

    #[test]
    fn parses_supported_invocations() {
        assert_eq!(RunMode::from_args([] as [&str; 0]), Ok(RunMode::Bar));
        assert_eq!(
            RunMode::from_args(["window", "config"]),
            Ok(RunMode::ConfigWindow)
        );
        assert!(RunMode::from_args(["window"]).is_err());
        assert!(RunMode::from_args(["window", "other"]).is_err());
    }
}
