use std::{
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
    pub label: String,
}

impl ToggleStatus {
    fn unavailable() -> Self {
        Self {
            available: false,
            enabled: false,
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
pub struct ControlSnapshot {
    pub wifi: ToggleStatus,
    pub bluetooth: ToggleStatus,
    pub audio_output: LevelStatus,
    pub audio_input: LevelStatus,
    pub brightness: LevelStatus,
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
            audio_output: LevelStatus::unavailable(),
            audio_input: LevelStatus::unavailable(),
            brightness: LevelStatus::unavailable(),
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
    let backend = SystemBackend::new();
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
}

impl SystemBackend {
    fn new() -> Self {
        let system_bus = Connection::system()
            .map_err(|error| warn!("system D-Bus unavailable: {error}"))
            .ok();
        let backlight = select_backlight(Path::new("/sys/class/backlight"));
        Self {
            system_bus,
            backlight,
        }
    }

    fn snapshot(&self) -> ControlSnapshot {
        ControlSnapshot {
            wifi: self.wifi_status().unwrap_or_else(|error| {
                debug!("could not read NetworkManager state: {error}");
                ToggleStatus::unavailable()
            }),
            bluetooth: self.bluetooth_status().unwrap_or_else(|error| {
                debug!("could not read BlueZ state: {error}");
                ToggleStatus::unavailable()
            }),
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
            if kind == 1 {
                ethernet_active = true;
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
                    }
                }
            }
        }

        let label = if !enabled {
            "Off".to_string()
        } else if let Some(ssid) = wifi_label {
            ssid
        } else if ethernet_active {
            "有線".to_string()
        } else {
            "未接続".to_string()
        };
        Ok(ToggleStatus {
            available: true,
            enabled,
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

fn read_u32(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{AudioEndpoint, LevelStatus, bluetooth_label, parse_wpctl_volume};

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
