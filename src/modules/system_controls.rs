use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};
use log::{debug, error, warn};
use zbus::{
    blocking::{Connection, Proxy},
    fdo::ManagedObjects,
    zvariant::{OwnedObjectPath, OwnedValue},
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const WORKER_TICK: Duration = Duration::from_millis(50);
const CPU_STAT_PATH: &str = "/proc/stat";
const CPU_INFO_PATH: &str = "/proc/cpuinfo";
const CPU_SYSFS_PATH: &str = "/sys/devices/system/cpu";
const MEMORY_INFO_PATH: &str = "/proc/meminfo";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioEndpoint {
    Output,
    Input,
}

impl AudioEndpoint {
    fn wpctl_id(self) -> &'static str {
        match self {
            Self::Output => "@DEFAULT_AUDIO_SINK@",
            Self::Input => "@DEFAULT_AUDIO_SOURCE@",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlAction {
    ToggleWifi,
    ToggleBluetooth,
    ToggleMute(AudioEndpoint),
    SetVolume(AudioEndpoint, u8),
    SetBrightness(u8),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToggleStatus {
    pub available: bool,
    pub enabled: bool,
    pub wired: bool,
    pub signal_strength: Option<u8>,
    pub interface: Option<String>,
    pub download_kbps: Option<u64>,
    pub upload_kbps: Option<u64>,
    pub label: String,
}

impl ToggleStatus {
    fn unavailable() -> Self {
        Self {
            available: false,
            enabled: false,
            wired: false,
            signal_strength: None,
            interface: None,
            download_kbps: None,
            upload_kbps: None,
            label: "利用不可".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LevelStatus {
    pub available: bool,
    pub percent: u8,
    pub muted: bool,
}

impl LevelStatus {
    fn unavailable() -> Self {
        Self {
            available: false,
            percent: 0,
            muted: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuStatus {
    pub available: bool,
    pub percent: u8,
    pub core_usages: Vec<CpuCoreUsage>,
}

impl CpuStatus {
    fn unavailable() -> Self {
        Self {
            available: false,
            percent: 0,
            core_usages: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuCoreUsage {
    pub index: usize,
    pub kind: Option<CpuCoreKind>,
    pub percent_tenths: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuCoreKind {
    Performance,
    Efficiency,
}

impl CpuCoreKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Performance => "P",
            Self::Efficiency => "E",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryStatus {
    pub available: bool,
    pub percent: u8,
    pub used_kib: u64,
    pub total_kib: u64,
}

impl MemoryStatus {
    fn unavailable() -> Self {
        Self {
            available: false,
            percent: 0,
            used_kib: 0,
            total_kib: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatteryStatus {
    pub available: bool,
    pub percent: u8,
    pub charging: bool,
    pub state: String,
    pub health: String,
}

impl BatteryStatus {
    fn unavailable() -> Self {
        Self {
            available: false,
            percent: 0,
            charging: false,
            state: "不明".to_string(),
            health: "不明".to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlSnapshot {
    pub wifi: ToggleStatus,
    pub bluetooth: ToggleStatus,
    pub cpu: CpuStatus,
    pub memory: MemoryStatus,
    pub audio_output: LevelStatus,
    pub audio_input: LevelStatus,
    pub brightness: LevelStatus,
    pub battery: BatteryStatus,
}

#[derive(Clone)]
pub struct ControlChannels {
    pub actions: Sender<ControlAction>,
    pub updates: Receiver<ControlSnapshot>,
}

impl Default for ControlSnapshot {
    fn default() -> Self {
        Self {
            wifi: ToggleStatus::unavailable(),
            bluetooth: ToggleStatus::unavailable(),
            cpu: CpuStatus::unavailable(),
            memory: MemoryStatus::unavailable(),
            audio_output: LevelStatus::unavailable(),
            audio_input: LevelStatus::unavailable(),
            brightness: LevelStatus::unavailable(),
            battery: BatteryStatus::unavailable(),
        }
    }
}

/// Starts the system-control worker. All D-Bus, process and sysfs I/O stays off
/// GPUI's render thread; the tray only exchanges snapshots and small actions.
pub fn start_worker() -> ControlChannels {
    let (action_sender, action_receiver) = async_channel::unbounded();
    let (snapshot_sender, snapshot_receiver) = async_channel::unbounded();

    if let Err(error) = thread::Builder::new()
        .name("bah-system-controls".to_string())
        .spawn(move || run_worker(action_receiver, snapshot_sender))
    {
        error!("failed to start system-controls worker: {error}");
    }

    ControlChannels {
        actions: action_sender,
        updates: snapshot_receiver,
    }
}

fn run_worker(actions: Receiver<ControlAction>, snapshots: Sender<ControlSnapshot>) {
    let mut backend = SystemBackend::new();
    let mut current = backend.snapshot();
    if snapshots.send_blocking(current.clone()).is_err() {
        return;
    }
    let mut refreshed_at = Instant::now();

    loop {
        let mut handled_action = false;
        while let Ok(action) = actions.try_recv() {
            handled_action = true;
            match backend.apply(&current, action.clone()) {
                Ok(()) => match action {
                    ControlAction::ToggleWifi => current.wifi.enabled = !current.wifi.enabled,
                    ControlAction::ToggleBluetooth => {
                        current.bluetooth.enabled = !current.bluetooth.enabled
                    }
                    ControlAction::ToggleMute(AudioEndpoint::Output) => {
                        current.audio_output.muted = !current.audio_output.muted
                    }
                    ControlAction::ToggleMute(AudioEndpoint::Input) => {
                        current.audio_input.muted = !current.audio_input.muted
                    }
                    ControlAction::SetVolume(AudioEndpoint::Output, percent) => {
                        current.audio_output.percent = percent.min(100)
                    }
                    ControlAction::SetVolume(AudioEndpoint::Input, percent) => {
                        current.audio_input.percent = percent.min(100)
                    }
                    ControlAction::SetBrightness(percent) => {
                        current.brightness.percent = percent.min(100)
                    }
                },
                Err(error) => warn!("system control action failed: {error}"),
            }
        }

        if handled_action || refreshed_at.elapsed() >= REFRESH_INTERVAL {
            current = backend.snapshot();
            refreshed_at = Instant::now();
            if snapshots.send_blocking(current.clone()).is_err() {
                return;
            }
        }
        thread::sleep(WORKER_TICK);
    }
}

struct SystemBackend {
    system_bus: Option<Connection>,
    backlight: Option<BacklightDevice>,
    battery: Option<BatteryDevice>,
    network_traffic: NetworkTrafficSampler,
    cpu_usage: CpuUsageSampler,
}

impl SystemBackend {
    fn new() -> Self {
        let system_bus = Connection::system()
            .map_err(|error| warn!("system D-Bus unavailable: {error}"))
            .ok();
        let backlight = select_backlight(Path::new("/sys/class/backlight"));
        let battery = select_battery(Path::new("/sys/class/power_supply"));
        Self {
            system_bus,
            backlight,
            battery,
            network_traffic: NetworkTrafficSampler::default(),
            cpu_usage: CpuUsageSampler::new(),
        }
    }

    fn snapshot(&mut self) -> ControlSnapshot {
        let mut wifi = self.wifi_status().unwrap_or_else(|error| {
            debug!("could not read NetworkManager state: {error}");
            ToggleStatus::unavailable()
        });
        let (download_kbps, upload_kbps) = self.network_traffic.sample(wifi.interface.as_deref());
        wifi.download_kbps = download_kbps;
        wifi.upload_kbps = upload_kbps;

        ControlSnapshot {
            wifi,
            bluetooth: self.bluetooth_status().unwrap_or_else(|error| {
                debug!("could not read BlueZ state: {error}");
                ToggleStatus::unavailable()
            }),
            cpu: self.cpu_usage.sample(),
            memory: memory_usage(),
            audio_output: read_wpctl(AudioEndpoint::Output).unwrap_or_else(|error| {
                debug!("could not read output volume: {error}");
                LevelStatus::unavailable()
            }),
            audio_input: read_wpctl(AudioEndpoint::Input).unwrap_or_else(|error| {
                debug!("could not read input volume: {error}");
                LevelStatus::unavailable()
            }),
            brightness: self
                .backlight
                .as_ref()
                .and_then(BacklightDevice::status)
                .unwrap_or_else(LevelStatus::unavailable),
            battery: self
                .battery
                .as_ref()
                .and_then(BatteryDevice::status)
                .unwrap_or_else(BatteryStatus::unavailable),
        }
    }

    fn apply(&self, current: &ControlSnapshot, action: ControlAction) -> Result<(), String> {
        match action {
            ControlAction::ToggleWifi => self.set_wifi(!current.wifi.enabled),
            ControlAction::ToggleBluetooth => self.set_bluetooth(!current.bluetooth.enabled),
            ControlAction::ToggleMute(endpoint) => {
                run_wpctl(&["set-mute", endpoint.wpctl_id(), "toggle"])
            }
            ControlAction::SetVolume(endpoint, percent) => {
                let value = format!("{}%", percent.min(100));
                run_wpctl(&["set-volume", "-l", "1.0", endpoint.wpctl_id(), &value])
            }
            ControlAction::SetBrightness(percent) => self.set_brightness(percent.min(100)),
        }
    }

    fn wifi_status(&self) -> Result<ToggleStatus, String> {
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        let manager = Proxy::new(
            connection,
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
        )
        .map_err(|error| error.to_string())?;
        let enabled: bool = manager
            .get_property("WirelessEnabled")
            .map_err(|error| error.to_string())?;
        let devices: Vec<OwnedObjectPath> = manager
            .call("GetDevices", &())
            .map_err(|error| error.to_string())?;

        let mut wifi_label = None;
        let mut wifi_signal_strength = None;
        let mut active_interface = None;
        let mut ethernet_active = false;
        for path in devices {
            let device = Proxy::new(
                connection,
                "org.freedesktop.NetworkManager",
                path.as_str(),
                "org.freedesktop.NetworkManager.Device",
            )
            .map_err(|error| error.to_string())?;
            let kind: u32 = device.get_property("DeviceType").unwrap_or_default();
            let state: u32 = device.get_property("State").unwrap_or_default();
            if state != 100 {
                continue;
            }
            let interface: String = device.get_property("Interface").unwrap_or_default();
            if kind == 1 {
                ethernet_active = true;
                if active_interface.is_none() && !interface.is_empty() {
                    active_interface = Some(interface);
                }
            } else if kind == 2 {
                let wireless = Proxy::new(
                    connection,
                    "org.freedesktop.NetworkManager",
                    path.as_str(),
                    "org.freedesktop.NetworkManager.Device.Wireless",
                )
                .map_err(|error| error.to_string())?;
                let access_point: OwnedObjectPath = wireless
                    .get_property("ActiveAccessPoint")
                    .map_err(|error| error.to_string())?;
                if access_point.as_str() != "/" {
                    let access_point = Proxy::new(
                        connection,
                        "org.freedesktop.NetworkManager",
                        access_point.as_str(),
                        "org.freedesktop.NetworkManager.AccessPoint",
                    )
                    .map_err(|error| error.to_string())?;
                    let ssid: Vec<u8> = access_point.get_property("Ssid").unwrap_or_default();
                    if !ssid.is_empty() {
                        wifi_label = Some(String::from_utf8_lossy(&ssid).into_owned());
                        wifi_signal_strength = access_point.get_property("Strength").ok();
                        if !interface.is_empty() {
                            active_interface = Some(interface);
                        }
                    }
                }
            }
        }

        let wired = enabled && wifi_label.is_none() && ethernet_active;
        let label = if !enabled {
            "Off".to_string()
        } else if let Some(ssid) = wifi_label {
            ssid
        } else if wired {
            "有線接続".to_string()
        } else {
            "未接続".to_string()
        };
        Ok(ToggleStatus {
            available: true,
            enabled,
            wired,
            signal_strength: wifi_signal_strength,
            interface: active_interface,
            download_kbps: None,
            upload_kbps: None,
            label,
        })
    }

    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        Proxy::new(
            connection,
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
        )
        .map_err(|error| error.to_string())?
        .set_property("WirelessEnabled", enabled)
        .map_err(|error| error.to_string())
    }

    fn bluetooth_objects(&self) -> Result<ManagedObjects, String> {
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        Proxy::new(
            connection,
            "org.bluez",
            "/",
            "org.freedesktop.DBus.ObjectManager",
        )
        .map_err(|error| error.to_string())?
        .call("GetManagedObjects", &())
        .map_err(|error| error.to_string())
    }

    fn bluetooth_status(&self) -> Result<ToggleStatus, String> {
        let objects = self.bluetooth_objects()?;
        let mut adapters = objects
            .iter()
            .filter_map(|(path, interfaces)| {
                property_map(interfaces, "org.bluez.Adapter1").map(|properties| (path, properties))
            })
            .collect::<Vec<_>>();
        adapters.sort_by_key(|(path, _)| path.as_str());
        let (_, adapter) = adapters.first().ok_or("no Bluetooth adapter")?;
        let enabled = owned_bool(adapter.get("Powered")).unwrap_or(false);

        let mut connected_names = objects
            .values()
            .filter_map(|interfaces| property_map(interfaces, "org.bluez.Device1"))
            .filter(|properties| owned_bool(properties.get("Connected")) == Some(true))
            .map(|properties| {
                owned_string(properties.get("Alias"))
                    .or_else(|| owned_string(properties.get("Name")))
                    .unwrap_or_else(|| "Bluetooth device".to_string())
            })
            .collect::<Vec<_>>();
        connected_names.sort();

        let label = bluetooth_label(enabled, &connected_names);
        Ok(ToggleStatus {
            available: true,
            enabled,
            wired: false,
            signal_strength: None,
            interface: None,
            download_kbps: None,
            upload_kbps: None,
            label,
        })
    }

    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        let objects = self.bluetooth_objects()?;
        let mut adapter_paths = objects
            .iter()
            .filter(|(_, interfaces)| property_map(interfaces, "org.bluez.Adapter1").is_some())
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>();
        adapter_paths.sort_unstable();
        let path = adapter_paths.first().ok_or("no Bluetooth adapter")?;
        Proxy::new(connection, "org.bluez", *path, "org.bluez.Adapter1")
            .map_err(|error| error.to_string())?
            .set_property("Powered", enabled)
            .map_err(|error| error.to_string())
    }

    fn set_brightness(&self, percent: u8) -> Result<(), String> {
        let device = self.backlight.as_ref().ok_or("no backlight device")?;
        let value = ((u64::from(device.max) * u64::from(percent)) / 100) as u32;

        if let Some(connection) = &self.system_bus {
            let result = Proxy::new(
                connection,
                "org.freedesktop.login1",
                "/org/freedesktop/login1/session/auto",
                "org.freedesktop.login1.Session",
            )
            .and_then(|proxy| {
                proxy.call::<_, _, ()>("SetBrightness", &("backlight", device.name.as_str(), value))
            });
            if result.is_ok() {
                return Ok(());
            }
        }

        fs::write(device.path.join("brightness"), value.to_string())
            .map_err(|error| error.to_string())
    }
}

fn property_map<'a>(
    interfaces: &'a std::collections::HashMap<
        zbus::names::OwnedInterfaceName,
        std::collections::HashMap<String, OwnedValue>,
    >,
    wanted: &str,
) -> Option<&'a std::collections::HashMap<String, OwnedValue>> {
    interfaces
        .iter()
        .find(|(name, _)| name.as_str() == wanted)
        .map(|(_, properties)| properties)
}

fn owned_bool(value: Option<&OwnedValue>) -> Option<bool> {
    value.and_then(|value| bool::try_from(value.clone()).ok())
}

fn owned_string(value: Option<&OwnedValue>) -> Option<String> {
    value.and_then(|value| String::try_from(value.clone()).ok())
}

fn bluetooth_label(enabled: bool, connected_names: &[String]) -> String {
    if !enabled {
        "Off".to_string()
    } else {
        match connected_names {
            [] => "未接続".to_string(),
            [name] => name.clone(),
            names => format!("{}台接続", names.len()),
        }
    }
}

fn read_wpctl(endpoint: AudioEndpoint) -> Result<LevelStatus, String> {
    let output = Command::new("wpctl")
        .args(["get-volume", endpoint.wpctl_id()])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    parse_wpctl_volume(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| "unrecognized wpctl volume output".to_string())
}

fn parse_wpctl_volume(output: &str) -> Option<LevelStatus> {
    let volume = output
        .split_whitespace()
        .find_map(|part| part.parse::<f32>().ok())?;
    Some(LevelStatus {
        available: true,
        percent: (volume * 100.0).round().clamp(0.0, 100.0) as u8,
        muted: output.contains("[MUTED]"),
    })
}

fn run_wpctl(arguments: &[&str]) -> Result<(), String> {
    let output = Command::new("wpctl")
        .args(arguments)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[derive(Clone, Debug)]
struct BacklightDevice {
    name: String,
    path: PathBuf,
    max: u32,
}

impl BacklightDevice {
    fn status(&self) -> Option<LevelStatus> {
        let value = read_u32(&self.path.join("actual_brightness"))
            .or_else(|| read_u32(&self.path.join("brightness")))?;
        Some(LevelStatus {
            available: true,
            percent: ((u64::from(value) * 100) / u64::from(self.max.max(1))).min(100) as u8,
            muted: false,
        })
    }
}

fn select_backlight(root: &Path) -> Option<BacklightDevice> {
    let mut devices = fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let max = read_u32(&path.join("max_brightness"))?;
            let kind = fs::read_to_string(path.join("type")).unwrap_or_default();
            let priority = match kind.trim() {
                "raw" => 0,
                "platform" => 1,
                "firmware" => 2,
                _ => 3,
            };
            Some((
                priority,
                entry.file_name().to_string_lossy().into_owned(),
                path,
                max,
            ))
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| (left.0, &left.1).cmp(&(right.0, &right.1)));
    devices
        .into_iter()
        .next()
        .map(|(_, name, path, max)| BacklightDevice { name, path, max })
}

#[derive(Clone, Debug)]
struct BatteryDevice {
    path: PathBuf,
}

impl BatteryDevice {
    fn status(&self) -> Option<BatteryStatus> {
        let percent = read_u32(&self.path.join("capacity"))?.min(100) as u8;
        let state = fs::read_to_string(self.path.join("status"))
            .unwrap_or_else(|_| "Unknown".to_string())
            .trim()
            .to_string();
        let health = fs::read_to_string(self.path.join("health"))
            .unwrap_or_else(|_| "Unknown".to_string())
            .trim()
            .to_string();
        Some(BatteryStatus {
            available: true,
            percent,
            charging: state == "Charging",
            state,
            health,
        })
    }
}

#[derive(Default)]
struct NetworkTrafficSampler {
    previous: HashMap<String, (u64, u64, Instant)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CpuTimes {
    total: u64,
    idle: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CpuSnapshot {
    total: CpuTimes,
    cores: Vec<(usize, CpuTimes)>,
}

#[derive(Default)]
struct CpuUsageSampler {
    previous: Option<CpuSnapshot>,
    core_kinds: HashMap<usize, CpuCoreKind>,
}

impl CpuUsageSampler {
    fn new() -> Self {
        Self {
            previous: None,
            core_kinds: detect_cpu_core_kinds(),
        }
    }

    fn sample(&mut self) -> CpuStatus {
        let status = fs::read_to_string(CPU_STAT_PATH)
            .ok()
            .and_then(|stat| parse_cpu_snapshot(&stat))
            .map(|snapshot| self.sample_snapshot(snapshot));
        status.unwrap_or_else(CpuStatus::unavailable)
    }

    fn sample_snapshot(&mut self, current: CpuSnapshot) -> CpuStatus {
        let previous = self.previous.replace(current.clone());
        let Some(previous) = previous else {
            return CpuStatus {
                available: true,
                percent: 0,
                core_usages: Vec::new(),
            };
        };

        CpuStatus {
            available: true,
            percent: cpu_usage_percent(current.total, previous.total),
            core_usages: current
                .cores
                .iter()
                .filter_map(|(index, times)| {
                    previous
                        .cores
                        .iter()
                        .find(|(previous_index, _)| previous_index == index)
                        .map(|(_, previous_times)| CpuCoreUsage {
                            index: *index,
                            kind: self.core_kinds.get(index).copied(),
                            percent_tenths: cpu_usage_percent_tenths(*times, *previous_times),
                        })
                })
                .collect(),
        }
    }
}

fn parse_cpu_times(stat: &str) -> Option<CpuTimes> {
    parse_cpu_time_values(stat.lines().find_map(|line| line.strip_prefix("cpu "))?)
}

fn parse_cpu_snapshot(stat: &str) -> Option<CpuSnapshot> {
    let total = parse_cpu_times(stat)?;
    let cores = stat
        .lines()
        .filter_map(|line| {
            let (name, values) = line.split_once(char::is_whitespace)?;
            let index = name.strip_prefix("cpu")?.parse::<usize>().ok()?;
            Some((index, parse_cpu_time_values(values)?))
        })
        .collect();
    Some(CpuSnapshot { total, cores })
}

fn parse_cpu_time_values(values: &str) -> Option<CpuTimes> {
    let values = values
        .split_whitespace()
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() < 4 {
        return None;
    }

    let total = values
        .iter()
        .take(8)
        .try_fold(0_u64, |sum, value| sum.checked_add(*value))?;
    let idle = values[3].checked_add(*values.get(4).unwrap_or(&0))?;
    Some(CpuTimes { total, idle })
}

fn cpu_usage_percent(current: CpuTimes, previous: CpuTimes) -> u8 {
    let total_delta = current.total.saturating_sub(previous.total);
    if total_delta == 0 {
        return 0;
    }
    let idle_delta = current.idle.saturating_sub(previous.idle);
    let busy_delta = total_delta.saturating_sub(idle_delta);
    ((busy_delta * 100 + total_delta / 2) / total_delta).min(100) as u8
}

fn cpu_usage_percent_tenths(current: CpuTimes, previous: CpuTimes) -> u16 {
    let total_delta = current.total.saturating_sub(previous.total);
    if total_delta == 0 {
        return 0;
    }
    let idle_delta = current.idle.saturating_sub(previous.idle);
    let busy_delta = total_delta.saturating_sub(idle_delta);
    ((busy_delta * 1_000 + total_delta / 2) / total_delta).min(1_000) as u16
}

fn detect_cpu_core_kinds() -> HashMap<usize, CpuCoreKind> {
    if !is_intel_cpu() {
        return HashMap::new();
    }

    let thread_counts = fs::read_dir(CPU_SYSFS_PATH)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let index = name.to_str()?.strip_prefix("cpu")?.parse::<usize>().ok()?;
            let count = fs::read_to_string(entry.path().join("topology/thread_siblings_list"))
                .ok()
                .and_then(|cpus| cpu_list_count(&cpus))?;
            Some((index, count))
        })
        .collect::<Vec<_>>();

    classify_cpu_core_kinds(&thread_counts)
}

fn is_intel_cpu() -> bool {
    fs::read_to_string(CPU_INFO_PATH).is_ok_and(|cpuinfo| {
        cpuinfo.lines().any(|line| {
            let Some((name, value)) = line.split_once(':') else {
                return false;
            };
            name.trim() == "vendor_id" && value.trim() == "GenuineIntel"
        })
    })
}

fn classify_cpu_core_kinds(thread_counts: &[(usize, usize)]) -> HashMap<usize, CpuCoreKind> {
    let has_smt_core = thread_counts.iter().any(|(_, count)| *count > 1);
    let has_single_thread_core = thread_counts.iter().any(|(_, count)| *count == 1);
    if !has_smt_core || !has_single_thread_core {
        return HashMap::new();
    }

    thread_counts
        .iter()
        .filter_map(|(index, count)| match count {
            1 => Some((*index, CpuCoreKind::Efficiency)),
            _ if *count > 1 => Some((*index, CpuCoreKind::Performance)),
            _ => None,
        })
        .collect()
}

fn cpu_list_count(cpus: &str) -> Option<usize> {
    cpus.trim().split(',').try_fold(0_usize, |count, item| {
        let (start, end) = match item.split_once('-') {
            Some((start, end)) => (start.parse::<usize>().ok()?, end.parse::<usize>().ok()?),
            None => {
                let index = item.parse::<usize>().ok()?;
                (index, index)
            }
        };
        end.checked_sub(start)?.checked_add(1)?.checked_add(count)
    })
}

fn memory_usage() -> MemoryStatus {
    fs::read_to_string(MEMORY_INFO_PATH)
        .ok()
        .and_then(|meminfo| parse_memory_usage(&meminfo))
        .unwrap_or_else(MemoryStatus::unavailable)
}

fn parse_memory_usage(meminfo: &str) -> Option<MemoryStatus> {
    let mut total_kib = None;
    let mut available_kib = None;
    for line in meminfo.lines() {
        let (name, value) = line.split_once(':')?;
        let value_kib = value.split_whitespace().next()?.parse::<u64>().ok()?;
        match name {
            "MemTotal" => total_kib = Some(value_kib),
            "MemAvailable" => available_kib = Some(value_kib),
            _ => {}
        }
    }

    let total_kib = total_kib?;
    let available_kib = available_kib?;
    if total_kib == 0 {
        return None;
    }
    let used_kib = total_kib.saturating_sub(available_kib);
    Some(MemoryStatus {
        available: true,
        percent: ((used_kib * 100 + total_kib / 2) / total_kib).min(100) as u8,
        used_kib,
        total_kib,
    })
}

impl NetworkTrafficSampler {
    fn sample(&mut self, interface: Option<&str>) -> (Option<u64>, Option<u64>) {
        let Some(interface) = interface else {
            self.previous.clear();
            return (None, None);
        };
        let root = Path::new("/sys/class/net")
            .join(interface)
            .join("statistics");
        let Some(download_bytes) = read_u64(&root.join("rx_bytes")) else {
            return (None, None);
        };
        let Some(upload_bytes) = read_u64(&root.join("tx_bytes")) else {
            return (None, None);
        };
        let now = Instant::now();
        let previous = self
            .previous
            .insert(interface.to_string(), (download_bytes, upload_bytes, now));
        let Some((previous_download, previous_upload, previous_at)) = previous else {
            return (None, None);
        };
        let seconds = now.duration_since(previous_at).as_secs_f64();
        if seconds == 0.0 || download_bytes < previous_download || upload_bytes < previous_upload {
            return (None, None);
        }
        let to_kbps = |bytes| ((bytes as f64 * 8.0) / seconds / 1_000.0).round() as u64;
        (
            Some(to_kbps(download_bytes - previous_download)),
            Some(to_kbps(upload_bytes - previous_upload)),
        )
    }
}

fn select_battery(root: &Path) -> Option<BatteryDevice> {
    let mut batteries = fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (fs::read_to_string(path.join("type")).ok()?.trim() == "Battery")
                .then_some((entry.file_name().to_string_lossy().into_owned(), path))
        })
        .collect::<Vec<_>>();
    batteries.sort_by(|left, right| left.0.cmp(&right.0));
    batteries
        .into_iter()
        .next()
        .map(|(_, path)| BatteryDevice { path })
}

fn read_u32(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        AudioEndpoint, CpuCoreKind, CpuCoreUsage, CpuStatus, CpuTimes, CpuUsageSampler,
        LevelStatus, MemoryStatus, bluetooth_label, classify_cpu_core_kinds, cpu_list_count,
        parse_cpu_snapshot, parse_cpu_times, parse_memory_usage, parse_wpctl_volume,
    };

    #[test]
    fn calculates_cpu_usage_from_consecutive_proc_stat_samples() {
        let mut sampler = CpuUsageSampler::default();
        assert_eq!(
            sampler.sample_snapshot(
                parse_cpu_snapshot(
                    "cpu  100 0 0 700 0 0 0 0\n\
                     cpu0 50 0 0 300 0 0 0 0\n\
                     cpu1 50 0 0 400 0 0 0 0\n",
                )
                .unwrap(),
            ),
            CpuStatus {
                available: true,
                percent: 0,
                core_usages: vec![],
            }
        );
        assert_eq!(
            sampler.sample_snapshot(
                parse_cpu_snapshot(
                    "cpu  160 0 0 740 0 0 0 0\n\
                     cpu0 90 0 0 320 0 0 0 0\n\
                     cpu1 70 0 0 420 0 0 0 0\n",
                )
                .unwrap(),
            ),
            CpuStatus {
                available: true,
                percent: 60,
                core_usages: vec![
                    CpuCoreUsage {
                        index: 0,
                        kind: None,
                        percent_tenths: 667,
                    },
                    CpuCoreUsage {
                        index: 1,
                        kind: None,
                        percent_tenths: 500,
                    },
                ],
            }
        );
    }

    #[test]
    fn parses_cpu_time_fields_without_counting_guest_time_twice() {
        assert_eq!(
            parse_cpu_times("cpu  10 20 30 40 50 60 70 80 90 100\n"),
            Some(CpuTimes {
                total: 360,
                idle: 90,
            })
        );
    }

    #[test]
    fn classifies_mixed_intel_thread_topology_as_p_and_e_cores() {
        assert_eq!(
            classify_cpu_core_kinds(&[(0, 2), (1, 2), (2, 1), (3, 1)]),
            HashMap::from([
                (0, CpuCoreKind::Performance),
                (1, CpuCoreKind::Performance),
                (2, CpuCoreKind::Efficiency),
                (3, CpuCoreKind::Efficiency),
            ])
        );
        assert!(classify_cpu_core_kinds(&[(0, 2), (1, 2)]).is_empty());
    }

    #[test]
    fn counts_ranges_in_cpu_sibling_lists() {
        assert_eq!(cpu_list_count("0-3,8,10-11\n"), Some(7));
    }

    #[test]
    fn calculates_memory_usage_from_mem_available() {
        assert_eq!(
            parse_memory_usage("MemTotal:       1000 kB\nMemAvailable:    365 kB\n"),
            Some(MemoryStatus {
                available: true,
                percent: 64,
                used_kib: 635,
                total_kib: 1000,
            })
        );
    }

    #[test]
    fn parses_wpctl_volume_and_mute() {
        assert_eq!(
            parse_wpctl_volume("Volume: 0.42 [MUTED]\n"),
            Some(LevelStatus {
                available: true,
                percent: 42,
                muted: true,
            })
        );
        assert_eq!(
            parse_wpctl_volume("Volume: 1.00\n"),
            Some(LevelStatus {
                available: true,
                percent: 100,
                muted: false,
            })
        );
    }

    #[test]
    fn endpoint_maps_to_wireplumber_defaults() {
        assert_eq!(AudioEndpoint::Output.wpctl_id(), "@DEFAULT_AUDIO_SINK@");
        assert_eq!(AudioEndpoint::Input.wpctl_id(), "@DEFAULT_AUDIO_SOURCE@");
    }

    #[test]
    fn bluetooth_label_uses_name_or_connected_count() {
        assert_eq!(bluetooth_label(false, &[]), "Off");
        assert_eq!(bluetooth_label(true, &[]), "未接続");
        assert_eq!(bluetooth_label(true, &["Headset".to_string()]), "Headset");
        assert_eq!(
            bluetooth_label(true, &["Headset".to_string(), "Keyboard".to_string()]),
            "2台接続"
        );
    }
}
