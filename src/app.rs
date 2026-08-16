use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::fd::AsRawFd,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use gpui::{
    App, AppContext, Bounds, Context, DisplayId, Render, Size, Window, WindowBackgroundAppearance,
    WindowBounds, WindowDecorations, WindowKind, WindowOptions, div, layer_shell::*, point,
    prelude::*, px,
};
use gpui_platform::application;
use log::{error, info, warn};

use crate::{
    bar::Bar,
    clipboard::{ClipboardHistory, start_collector},
    config::Config,
    config_window::ConfigWindow,
    hyprland,
    modules::{
        notifications::{NotificationStore, start_server},
        system_controls,
    },
    theme::{BarTheme, VisualMode},
    wallpaper::Wallpaper,
};

const CONFIG_WINDOW_LOCK_FILE: &str = "bah/config-window.lock";
const DEVICE_CONTROL_CENTER_SOCKET_FILE: &str = "bah/device-control-center.sock";
const WINDOW_SWITCHER_SOCKET_FILE: &str = "bah/window-switcher.sock";
const CLIPBOARD_SOCKET_FILE: &str = "bah/clipboard.sock";
const SCREENSHOT_SOCKET_FILE: &str = "bah/screenshot.sock";
const WALLPAPER_LOCK_FILE: &str = "bah/wallpaper.lock";
const WALLPAPER_PID_FILE: &str = "bah/wallpaper.pid";
const USAGE: &str = "usage: bah [--memusg] [--wgpu-backend BACKENDS] [--vk-driver-files PATH] \
     [notifications COMMAND|switcher COMMAND|clipboard COMMAND|screenshot|window config|window device-control-center [network|bluetooth|display]|wallpaper [set PATH|unset]]";

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

fn decode_hex_ssid(value: &OsStr) -> Result<Vec<u8>, String> {
    let value = value
        .to_str()
        .ok_or_else(|| format!("--ssid-hex must be valid UTF-8\n{USAGE}"))?;
    if value.is_empty() || value.len() % 2 != 0 {
        return Err(format!(
            "--ssid-hex must contain complete byte pairs\n{USAGE}"
        ));
    }
    (0..value.len())
        .step_by(2)
        .map(|offset| u8::from_str_radix(&value[offset..offset + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| format!("--ssid-hex must be hexadecimal\n{USAGE}"))
}

fn encode_hex_ssid(ssid: &[u8]) -> String {
    ssid.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Opens the network page without blocking a UI render callback.
pub fn launch_device_control_center_network(ssid: Option<Vec<u8>>) {
    request_device_control_center(DeviceControlCenterRoute {
        page: DeviceControlCenterPage::Network,
        ssid,
    });
}

fn device_control_center_socket_path() -> io::Result<PathBuf> {
    runtime_lock_path(DEVICE_CONTROL_CENTER_SOCKET_FILE)
}

fn write_device_control_center_route(route: &DeviceControlCenterRoute) -> io::Result<()> {
    let mut stream = UnixStream::connect(device_control_center_socket_path()?)?;
    let payload = match route.page {
        DeviceControlCenterPage::Network => route
            .ssid
            .as_deref()
            .map(|ssid| format!("network:{}", encode_hex_ssid(ssid)))
            .unwrap_or_else(|| "network".to_string()),
        DeviceControlCenterPage::Bluetooth => "bluetooth".to_string(),
        DeviceControlCenterPage::Display => "display".to_string(),
    };
    stream.write_all(payload.as_bytes())?;
    stream.write_all(b"\n")
}

/// Requests the DCC from an in-process surface without blocking its render callback.
pub(crate) fn request_device_control_center(route: DeviceControlCenterRoute) {
    let _ = thread::Builder::new()
        .name("bah-device-control-center-request".to_string())
        .spawn(move || {
            if let Err(error) = write_device_control_center_route(&route) {
                error!("failed to request device control center: {error}");
            }
        });
}

fn start_device_control_center_route_server(
    sender: async_channel::Sender<DeviceControlCenterRoute>,
) -> io::Result<DeviceControlCenterRouteServer> {
    let path = device_control_center_socket_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // The singleton lock is held before removing this exact per-user socket,
    // so a stale endpoint from a crashed previous instance is safe to replace.
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)?;
    let worker = listener.try_clone()?;
    thread::Builder::new()
        .name("bah-device-control-center-route".to_string())
        .spawn(move || {
            for stream in worker.incoming().flatten() {
                let mut request = String::new();
                let mut stream = stream;
                if stream.read_to_string(&mut request).is_err() {
                    continue;
                }
                let route = match request.trim() {
                    "" | "network" => DeviceControlCenterRoute::default(),
                    "bluetooth" => DeviceControlCenterRoute {
                        page: DeviceControlCenterPage::Bluetooth,
                        ssid: None,
                    },
                    "display" => DeviceControlCenterRoute {
                        page: DeviceControlCenterPage::Display,
                        ssid: None,
                    },
                    value => {
                        let value = value.strip_prefix("network:").unwrap_or(value);
                        match decode_hex_ssid(OsStr::new(value)) {
                            Ok(ssid) => DeviceControlCenterRoute {
                                page: DeviceControlCenterPage::Network,
                                ssid: Some(ssid),
                            },
                            Err(error) => {
                                warn!("invalid device control center route: {error}");
                                continue;
                            }
                        }
                    }
                };
                if sender.send_blocking(route).is_err() {
                    return;
                }
            }
        })?;
    Ok(DeviceControlCenterRouteServer {
        _listener: listener,
        path,
    })
}

pub struct DeviceControlCenterRouteServer {
    _listener: UnixListener,
    path: PathBuf,
}

impl Drop for DeviceControlCenterRouteServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn window_switcher_socket_path() -> io::Result<PathBuf> {
    runtime_lock_path(WINDOW_SWITCHER_SOCKET_FILE)
}

fn run_switcher_command(command: SwitcherCommand) {
    let path = match window_switcher_socket_path() {
        Ok(path) => path,
        Err(error) => {
            error!("could not resolve the window-switcher socket path: {error}");
            return;
        }
    };
    match UnixStream::connect(path) {
        Ok(mut stream) => {
            if let Err(error) = writeln!(stream, "{}", command.as_wire()) {
                error!("failed to send switcher command: {error}");
            }
        }
        Err(error) => error!("Bah is not running; cannot control the window switcher: {error}"),
    }
}

fn start_window_switcher_server(
    sender: async_channel::Sender<SwitcherCommand>,
) -> io::Result<WindowSwitcherCommandServer> {
    let path = window_switcher_socket_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)?;
    let worker = listener.try_clone()?;
    thread::Builder::new()
        .name("bah-window-switcher-command".to_string())
        .spawn(move || {
            for stream in worker.incoming().flatten() {
                let mut request = String::new();
                let mut stream = stream;
                if stream.read_to_string(&mut request).is_err() {
                    continue;
                }
                let Some(command) = SwitcherCommand::parse(OsStr::new(request.trim())) else {
                    warn!(
                        "ignoring unknown window-switcher command: {:?}",
                        request.trim()
                    );
                    continue;
                };
                if sender.send_blocking(command).is_err() {
                    return;
                }
            }
        })?;
    Ok(WindowSwitcherCommandServer {
        _listener: listener,
        path,
    })
}

fn clipboard_socket_path() -> io::Result<PathBuf> {
    runtime_lock_path(CLIPBOARD_SOCKET_FILE)
}

fn screenshot_socket_path() -> io::Result<PathBuf> {
    runtime_lock_path(SCREENSHOT_SOCKET_FILE)
}

fn run_screenshot_command() {
    let path = match screenshot_socket_path() {
        Ok(path) => path,
        Err(error) => {
            error!("could not resolve the screenshot socket path: {error}");
            return;
        }
    };
    match UnixStream::connect(path) {
        Ok(mut stream) => {
            if let Err(error) = writeln!(stream, "capture") {
                error!("failed to send screenshot command: {error}");
            }
        }
        Err(error) => error!("Bah is not running; cannot capture a screenshot: {error}"),
    }
}

fn start_screenshot_server(
    sender: async_channel::Sender<()>,
) -> io::Result<ScreenshotCommandServer> {
    let path = screenshot_socket_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)?;
    let worker = listener.try_clone()?;
    thread::Builder::new()
        .name("bah-screenshot-command".to_string())
        .spawn(move || {
            for stream in worker.incoming().flatten() {
                let mut request = String::new();
                let mut stream = stream;
                if stream.read_to_string(&mut request).is_err() {
                    continue;
                }
                if request.trim() != "capture" {
                    warn!("ignoring unknown screenshot command: {:?}", request.trim());
                    continue;
                }
                if sender.send_blocking(()).is_err() {
                    return;
                }
            }
        })?;
    Ok(ScreenshotCommandServer {
        _listener: listener,
        path,
    })
}

pub struct ScreenshotCommandServer {
    _listener: UnixListener,
    path: PathBuf,
}

impl Drop for ScreenshotCommandServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn run_clipboard_command(command: ClipboardCommand) {
    let path = match clipboard_socket_path() {
        Ok(path) => path,
        Err(error) => {
            error!("could not resolve the clipboard socket path: {error}");
            return;
        }
    };
    match UnixStream::connect(path) {
        Ok(mut stream) => {
            if let Err(error) = writeln!(stream, "{}", command.as_wire()) {
                error!("failed to send clipboard command: {error}");
            }
        }
        Err(error) => error!("Bah is not running; cannot control clipboard history: {error}"),
    }
}

fn start_clipboard_server(
    sender: async_channel::Sender<ClipboardCommand>,
) -> io::Result<ClipboardCommandServer> {
    let path = clipboard_socket_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)?;
    let worker = listener.try_clone()?;
    thread::Builder::new()
        .name("bah-clipboard-command".to_string())
        .spawn(move || {
            for stream in worker.incoming().flatten() {
                let mut request = String::new();
                let mut stream = stream;
                if stream.read_to_string(&mut request).is_err() {
                    continue;
                }
                let Some(command) = ClipboardCommand::parse(OsStr::new(request.trim())) else {
                    warn!("ignoring unknown clipboard command: {:?}", request.trim());
                    continue;
                };
                if sender.send_blocking(command).is_err() {
                    return;
                }
            }
        })?;
    Ok(ClipboardCommandServer {
        _listener: listener,
        path,
    })
}

pub struct ClipboardCommandServer {
    _listener: UnixListener,
    path: PathBuf,
}

impl Drop for ClipboardCommandServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub struct WindowSwitcherCommandServer {
    _listener: UnixListener,
    path: PathBuf,
}

impl Drop for WindowSwitcherCommandServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Selects which surface this invocation creates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunMode {
    Bar,
    ConfigWindow,
    DeviceControlCenter(DeviceControlCenterRoute),
    Notifications(Vec<OsString>),
    Switcher(SwitcherCommand),
    Clipboard(ClipboardCommand),
    Screenshot,
    Wallpaper,
    WallpaperSet(PathBuf),
    WallpaperUnset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardCommand {
    Toggle,
    Open,
    Close,
    Previous,
    Next,
    Select,
    Clear,
}

impl ClipboardCommand {
    fn parse(value: &OsStr) -> Option<Self> {
        match value.to_str()? {
            "toggle" => Some(Self::Toggle),
            "open" => Some(Self::Open),
            "close" => Some(Self::Close),
            "previous" => Some(Self::Previous),
            "next" => Some(Self::Next),
            "select" => Some(Self::Select),
            "clear" => Some(Self::Clear),
            _ => None,
        }
    }

    fn as_wire(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Open => "open",
            Self::Close => "close",
            Self::Previous => "previous",
            Self::Next => "next",
            Self::Select => "select",
            Self::Clear => "clear",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum DeviceControlCenterPage {
    #[default]
    Network,
    Bluetooth,
    Display,
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct DeviceControlCenterRoute {
    pub page: DeviceControlCenterPage,
    pub ssid: Option<Vec<u8>>,
}

/// Commands accepted by the persistent window-switcher control socket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SwitcherCommand {
    Open,
    Cycle,
    CycleReverse,
    SelectPrevious,
    SelectNext,
    Commit,
    Close,
    Refresh,
}

impl SwitcherCommand {
    fn parse(value: &OsStr) -> Option<Self> {
        match value.to_str()? {
            "open" => Some(Self::Open),
            "cycle" => Some(Self::Cycle),
            "cycle-reverse" => Some(Self::CycleReverse),
            "select-previous" => Some(Self::SelectPrevious),
            "select-next" => Some(Self::SelectNext),
            "commit" => Some(Self::Commit),
            "close" => Some(Self::Close),
            "refresh" => Some(Self::Refresh),
            _ => None,
        }
    }

    fn as_wire(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Cycle => "cycle",
            Self::CycleReverse => "cycle-reverse",
            Self::SelectPrevious => "select-previous",
            Self::SelectNext => "select-next",
            Self::Commit => "commit",
            Self::Close => "close",
            Self::Refresh => "refresh",
        }
    }
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
            [notifications, command @ ..] if notifications == "notifications" => {
                Ok(Self::Notifications(command.to_vec()))
            }
            [switcher, command] if switcher == "switcher" => SwitcherCommand::parse(command)
                .map(Self::Switcher)
                .ok_or_else(|| USAGE.to_string()),
            [clipboard, command] if clipboard == "clipboard" => ClipboardCommand::parse(command)
                .map(Self::Clipboard)
                .ok_or_else(|| USAGE.to_string()),
            [screenshot] if screenshot == "screenshot" => Ok(Self::Screenshot),
            [window, config] if window == "window" && config == "config" => Ok(Self::ConfigWindow),
            [window, device_control_center]
                if window == "window" && device_control_center == "device-control-center" =>
            {
                Ok(Self::DeviceControlCenter(
                    DeviceControlCenterRoute::default(),
                ))
            }
            [window, device_control_center, network]
                if window == "window"
                    && device_control_center == "device-control-center"
                    && network == "network" =>
            {
                Ok(Self::DeviceControlCenter(
                    DeviceControlCenterRoute::default(),
                ))
            }
            [window, device_control_center, bluetooth]
                if window == "window"
                    && device_control_center == "device-control-center"
                    && bluetooth == "bluetooth" =>
            {
                Ok(Self::DeviceControlCenter(DeviceControlCenterRoute {
                    page: DeviceControlCenterPage::Bluetooth,
                    ssid: None,
                }))
            }
            [window, device_control_center, display]
                if window == "window"
                    && device_control_center == "device-control-center"
                    && display == "display" =>
            {
                Ok(Self::DeviceControlCenter(DeviceControlCenterRoute {
                    page: DeviceControlCenterPage::Display,
                    ssid: None,
                }))
            }
            [window, device_control_center, network, flag, ssid]
                if window == "window"
                    && device_control_center == "device-control-center"
                    && network == "network"
                    && flag == "--ssid-hex" =>
            {
                Ok(Self::DeviceControlCenter(DeviceControlCenterRoute {
                    page: DeviceControlCenterPage::Network,
                    ssid: Some(decode_hex_ssid(ssid)?),
                }))
            }
            [wallpaper] if wallpaper == "wallpaper" => Ok(Self::Wallpaper),
            [wallpaper, set, path] if wallpaper == "wallpaper" && set == "set" => {
                Ok(Self::WallpaperSet(PathBuf::from(path)))
            }
            [wallpaper, unset] if wallpaper == "wallpaper" && unset == "unset" => {
                Ok(Self::WallpaperUnset)
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
        RunMode::DeviceControlCenter(route) => run_device_control_center(route),
        RunMode::Notifications(arguments) => run_notifications(arguments),
        RunMode::Switcher(command) => run_switcher_command(command),
        RunMode::Clipboard(command) => run_clipboard_command(command),
        RunMode::Screenshot => run_screenshot_command(),
        RunMode::Wallpaper => run_wallpaper(config),
        RunMode::WallpaperSet(_) | RunMode::WallpaperUnset => {
            unreachable!("commands exit before run")
        }
    }
}

/// Applies `wallpaper set` / `unset`. `true` means that no UI should be run.
pub fn handle_wallpaper_command(mode: &RunMode, mut config: Config) -> Result<bool> {
    match mode {
        RunMode::WallpaperSet(path) => {
            let path = canonical_wallpaper_path(path)?;
            config.wallpaper = Some(path);
            config.save()?;
            replace_wallpaper_process()?;
            Ok(true)
        }
        RunMode::WallpaperUnset => {
            config.wallpaper = None;
            config.save()?;
            stop_wallpaper_process()?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn canonical_wallpaper_path(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("wallpaper file does not exist: {}", path.display()))?;
    if !path.is_file() {
        bail!("wallpaper path is not a file: {}", path.display());
    }
    Ok(path)
}

fn replace_wallpaper_process() -> Result<()> {
    stop_wallpaper_process()?;
    start_wallpaper_process()
}

fn start_wallpaper_process() -> Result<()> {
    let executable = env::current_exe().context("failed to resolve bah executable")?;
    Command::new(executable)
        .arg("wallpaper")
        .spawn()
        .context("failed to start wallpaper layer")?;
    Ok(())
}

/// Restarts the wallpaper layer after a settings-page update.
pub(crate) fn restart_wallpaper_process() -> Result<()> {
    replace_wallpaper_process()
}

fn stop_wallpaper_process() -> Result<()> {
    let path = runtime_wallpaper_pid_path()?;
    let Ok(pid) = fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(pid) = pid.trim().parse::<i32>() else {
        let _ = fs::remove_file(path);
        return Ok(());
    };
    if !is_wallpaper_process(pid) {
        // Never signal a PID which may have been reused after a crash.
        let _ = fs::remove_file(path);
        return Ok(());
    }
    // The PID is written only by a Bah wallpaper layer. SIGTERM permits the
    // layer-shell client to tear down its surface cleanly.
    let result = unsafe { kill(pid, SIGTERM) };
    if result != 0 && io::Error::last_os_error().raw_os_error() != Some(3) {
        return Err(io::Error::last_os_error().into());
    }
    for _ in 0..20 {
        if unsafe { kill(pid, 0) } != 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = fs::remove_file(path);
    Ok(())
}

fn is_wallpaper_process(pid: i32) -> bool {
    let Ok(executable) = env::current_exe().and_then(fs::canonicalize) else {
        return false;
    };
    let Ok(process_executable) = fs::canonicalize(format!("/proc/{pid}/exe")) else {
        return false;
    };
    if process_executable != executable {
        return false;
    }
    let Ok(command_line) = fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    command_line
        .split(|byte| *byte == 0)
        .any(|argument| argument == b"wallpaper")
}

fn runtime_wallpaper_pid_path() -> io::Result<PathBuf> {
    runtime_lock_path(WALLPAPER_PID_FILE)
}

struct WallpaperBootstrap;

impl Render for WallpaperBootstrap {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}

fn run_wallpaper(config: Config) {
    let fallback = config.wallpaper.clone();
    let surfaces = match hyprland::display::load_layout() {
        Ok(layout) => {
            hyprland::display::wallpaper_sources(&layout, &config.wallpapers, fallback.as_ref())
                .into_iter()
                .map(|(output, source)| (Some(output), source))
                .collect::<Vec<_>>()
        }
        Err(error) => {
            warn!("could not determine outputs for wallpapers: {error}");
            fallback.into_iter().map(|source| (None, source)).collect()
        }
    };
    if surfaces.is_empty() {
        info!("no wallpaper configured; wallpaper layer will not be created");
        return;
    }
    for (_, source) in &surfaces {
        if !source.is_file() {
            error!(
                "configured wallpaper no longer exists: {}",
                source.display()
            );
            return;
        }
    }
    let lock = match WallpaperLock::acquire() {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            info!("a wallpaper layer is already running; ignoring this invocation");
            return;
        }
        Err(error) => {
            error!("could not lock wallpaper layer: {error}");
            return;
        }
    };
    match runtime_wallpaper_pid_path().and_then(|path| {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, std::process::id().to_string())
    }) {
        Ok(()) => {}
        Err(error) => error!("could not record wallpaper process: {error}"),
    }
    let lock = std::sync::Arc::new(lock.into_file());
    application().run(move |cx: &mut App| {
        // wl_output names arrive asynchronously. Keep one transparent layer
        // surface alive until the initial Wayland output events have been
        // processed, then map the per-output wallpaper surfaces.
        let bootstrap = cx.open_window(
            WindowOptions {
                titlebar: None,
                focus: false,
                app_id: Some("bah-wallpaper-bootstrap".to_string()),
                window_background: WindowBackgroundAppearance::Transparent,
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: Size::new(px(1.0), px(1.0)),
                })),
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: "bah-wallpaper-bootstrap".to_string(),
                    layer: Layer::Bottom,
                    anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                let surfaces = surfaces.clone();
                let lock = lock.clone();
                window
                    .spawn(cx, async move |cx| {
                        for _ in 0..20 {
                            let ready = cx
                                .update(|_, cx| wallpaper_outputs_ready(cx, &surfaces))
                                .unwrap_or(false);
                            if ready {
                                break;
                            }
                            cx.background_executor()
                                .timer(Duration::from_millis(100))
                                .await;
                        }
                        let _ = cx.update(move |window, cx| {
                            open_wallpaper_surfaces(cx, &surfaces, &lock);
                            window.remove_window();
                        });
                    })
                    .detach();
                cx.new(|_| WallpaperBootstrap)
            },
        );
        if let Err(error) = bootstrap {
            error!("failed to create wallpaper bootstrap surface: {error}");
        }
    });
    if let Ok(path) = runtime_wallpaper_pid_path() {
        let _ = fs::remove_file(path);
    }
}

fn wallpaper_outputs_ready(cx: &App, surfaces: &[(Option<String>, PathBuf)]) -> bool {
    surfaces.iter().all(|(output, _)| {
        output
            .as_deref()
            .is_none_or(|output| display_id_for_output(cx, output).is_some())
    })
}

fn open_wallpaper_surfaces(
    cx: &mut App,
    surfaces: &[(Option<String>, PathBuf)],
    lock: &std::sync::Arc<File>,
) {
    for (output, source) in surfaces {
        let display_id = output
            .as_deref()
            .and_then(|output| display_id_for_output(cx, output));
        if output.is_some() && display_id.is_none() {
            warn!("could not find Wayland output for wallpaper {:?}", output);
            continue;
        }
        let source = source.clone();
        let lock = lock.clone();
        let namespace = output.as_ref().map_or_else(
            || "bah-wallpaper".to_string(),
            |output| format!("bah-wallpaper-{output}"),
        );
        let result = cx.open_window(
            WindowOptions {
                titlebar: None,
                focus: false,
                display_id,
                app_id: Some("bah-wallpaper".to_string()),
                window_background: WindowBackgroundAppearance::Opaque,
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: Size::new(px(1.0), px(1.0)),
                })),
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace,
                    layer: Layer::Background,
                    anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                    // Extend behind the bar's exclusive zone so its translucent
                    // background is composited over the wallpaper.
                    exclusive_zone: Some(px(-1.0)),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |_, cx| cx.new(|cx| Wallpaper::new(source.clone(), lock.clone(), cx)),
        );
        match result {
            Ok(_) => info!("wallpaper Layer Shell window created"),
            Err(error) => error!("failed to create wallpaper Layer Shell window: {error}"),
        }
    }
}

pub(crate) fn display_id_for_output(cx: &App, output: &str) -> Option<DisplayId> {
    let expected = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, output.as_bytes());
    cx.displays()
        .into_iter()
        .find_map(|display| (display.uuid().ok() == Some(expected)).then(|| display.id()))
}

fn run_bar(config: Config) {
    if wallpaper_is_configured(&config)
        && let Err(error) = start_wallpaper_process()
    {
        error!("could not start configured wallpaper: {error}");
    }
    application().run(move |cx: &mut App| {
        let (sender, receiver) = async_channel::unbounded();
        hyprland::start_worker(sender);
        let (notification_sender, notification_receiver) = async_channel::unbounded();
        let notifications = NotificationStore::shared(config.notifications.clone());
        start_server(notification_sender.clone(), notifications.clone());
        let controls = system_controls::start_worker();
        let (dcc_route_sender, dcc_route_receiver) = async_channel::unbounded();
        let dcc_route_server = match start_device_control_center_route_server(dcc_route_sender) {
            Ok(server) => server,
            Err(error) => {
                error!("could not start device control center route server: {error}");
                return;
            }
        };
        let (switcher_command_sender, switcher_command_receiver) = async_channel::unbounded();
        let switcher_command_server = match start_window_switcher_server(switcher_command_sender) {
            Ok(server) => server,
            Err(error) => {
                error!("could not start window switcher command server: {error}");
                return;
            }
        };
        let (clipboard_command_sender, clipboard_command_receiver) = async_channel::unbounded();
        let clipboard_command_server = match start_clipboard_server(clipboard_command_sender) {
            Ok(server) => server,
            Err(error) => {
                error!("could not start clipboard command server: {error}");
                return;
            }
        };
        let (clipboard_update_sender, clipboard_update_receiver) = async_channel::unbounded();
        let clipboard = ClipboardHistory::shared(config.clipboard.clone());
        start_collector(clipboard.clone(), clipboard_update_sender);
        let (screenshot_command_sender, screenshot_command_receiver) = async_channel::unbounded();
        let screenshot_command_server = match start_screenshot_server(screenshot_command_sender) {
            Ok(server) => server,
            Err(error) => {
                error!("could not start screenshot command server: {error}");
                return;
            }
        };

        let theme = BarTheme::from_environment(config.bar_height);
        let height = theme.bar_height;
        info!(
            "visual mode: {}",
            match theme.visual_mode {
                VisualMode::Readable => "readable",
                VisualMode::Glass => "glass",
                VisualMode::HighContrast => "high contrast",
                VisualMode::Opaque => "opaque",
            }
        );
        // Wayland popups are positioned in their parent's surface-local
        // coordinate space. Keep a full-output parent mapped for the lifetime
        // of the bar so the DCC popup can use the monitor centre directly.
        let dcc_popup_anchor = match cx.open_window(
            WindowOptions {
                titlebar: None,
                focus: false,
                show: true,
                is_movable: false,
                is_resizable: false,
                app_id: Some("bah-device-control-center-anchor".to_string()),
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Client),
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: Size::new(px(1.0), px(1.0)),
                })),
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: "bah-device-control-center-anchor".to_string(),
                    layer: Layer::Overlay,
                    anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                    exclusive_zone: Some(px(0.0)),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| cx.new(|_| crate::bar::DeviceControlCenterAnchor),
        ) {
            Ok(anchor) => anchor,
            Err(error) => {
                error!("failed to create device control center popup anchor: {error}");
                return;
            }
        };
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
                        dcc_route_receiver,
                        dcc_route_server,
                        dcc_popup_anchor,
                        switcher_command_receiver,
                        switcher_command_server,
                        clipboard,
                        clipboard_update_receiver,
                        clipboard_command_receiver,
                        clipboard_command_server,
                        screenshot_command_receiver,
                        screenshot_command_server,
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

fn wallpaper_is_configured(config: &Config) -> bool {
    config.wallpaper.is_some() || !config.wallpapers.is_empty()
}

fn run_device_control_center(route: DeviceControlCenterRoute) {
    if let Err(error) = write_device_control_center_route(&route) {
        error!("Bar is not running; cannot open the device control center popover: {error}");
    }
}

fn run_notifications(arguments: Vec<OsString>) {
    if let Err(error) = crate::modules::notifications::run_control_cli(&arguments) {
        eprintln!("bah notifications: {error}");
        std::process::exit(1);
    }
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
        let size = Size::new(px(900.0), px(650.0));
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
                window_min_size: Some(Size::new(px(480.0), px(320.0))),
                ..Default::default()
            },
            move |_, cx| {
                let theme = BarTheme::from_environment(config.bar_height);
                cx.new(|cx| ConfigWindow::new(config, lock, theme, cx))
            },
        );

        match result {
            Ok(_) => {
                hyprland::force_float_window_for_process("bah-settings", std::process::id());
                info!("configuration window created")
            }
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

/// Advisory lock that makes a wallpaper layer a singleton per user.
struct WallpaperLock {
    file: File,
}

impl WallpaperLock {
    fn acquire() -> io::Result<Option<Self>> {
        let path = runtime_lock_path(WALLPAPER_LOCK_FILE)?;
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
            Ok(Some(Self { file }))
        } else if io::Error::last_os_error().raw_os_error() == Some(11) {
            Ok(None)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn into_file(self) -> File {
        self.file
    }
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
    fn kill(pid: i32, signal: i32) -> i32;
}

const SIGTERM: i32 = 15;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        ClipboardCommand, DeviceControlCenterPage, DeviceControlCenterRoute, RunMode,
        StartupOptions, SwitcherCommand, wallpaper_is_configured,
    };
    use crate::config::Config;

    #[test]
    fn wallpaper_startup_detects_common_and_output_specific_settings() {
        let mut config = Config::default();
        assert!(!wallpaper_is_configured(&config));
        config.wallpaper = Some(PathBuf::from("wallpaper.png"));
        assert!(wallpaper_is_configured(&config));
        config.wallpaper = None;
        config
            .wallpapers
            .insert("eDP-1".into(), PathBuf::from("display.png"));
        assert!(wallpaper_is_configured(&config));
    }

    #[test]
    fn parses_supported_invocations() {
        assert_eq!(RunMode::from_args([] as [&str; 0]), Ok(RunMode::Bar));
        assert_eq!(
            RunMode::from_args(["window", "config"]),
            Ok(RunMode::ConfigWindow)
        );
        assert_eq!(
            RunMode::from_args(["window", "device-control-center"]),
            Ok(RunMode::DeviceControlCenter(
                DeviceControlCenterRoute::default(),
            ))
        );
        assert!(RunMode::from_args(["dcc", "display"]).is_err());
        assert_eq!(
            RunMode::from_args([
                "window",
                "device-control-center",
                "network",
                "--ssid-hex",
                "77696669",
            ]),
            Ok(RunMode::DeviceControlCenter(DeviceControlCenterRoute {
                page: DeviceControlCenterPage::Network,
                ssid: Some(b"wifi".to_vec()),
            }))
        );
        assert_eq!(
            RunMode::from_args(["window", "device-control-center", "bluetooth"]),
            Ok(RunMode::DeviceControlCenter(DeviceControlCenterRoute {
                page: DeviceControlCenterPage::Bluetooth,
                ssid: None,
            }))
        );
        assert_eq!(
            RunMode::from_args(["window", "device-control-center", "display"]),
            Ok(RunMode::DeviceControlCenter(DeviceControlCenterRoute {
                page: DeviceControlCenterPage::Display,
                ssid: None,
            }))
        );
        assert_eq!(RunMode::from_args(["wallpaper"]), Ok(RunMode::Wallpaper));
        assert_eq!(
            RunMode::from_args(["switcher", "cycle-reverse"]),
            Ok(RunMode::Switcher(SwitcherCommand::CycleReverse))
        );
        assert_eq!(
            RunMode::from_args(["switcher", "commit"]),
            Ok(RunMode::Switcher(SwitcherCommand::Commit))
        );
        assert_eq!(
            RunMode::from_args(["clipboard", "toggle"]),
            Ok(RunMode::Clipboard(ClipboardCommand::Toggle))
        );
        assert_eq!(
            RunMode::from_args(["clipboard", "clear"]),
            Ok(RunMode::Clipboard(ClipboardCommand::Clear))
        );
        assert_eq!(
            RunMode::from_args(["clipboard", "select"]),
            Ok(RunMode::Clipboard(ClipboardCommand::Select))
        );
        assert_eq!(RunMode::from_args(["screenshot"]), Ok(RunMode::Screenshot));
        assert_eq!(
            RunMode::from_args(["wallpaper", "set", "resrc/wallpaper.png"]),
            Ok(RunMode::WallpaperSet("resrc/wallpaper.png".into()))
        );
        assert_eq!(
            RunMode::from_args(["wallpaper", "unset"]),
            Ok(RunMode::WallpaperUnset)
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
