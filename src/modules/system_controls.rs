use std::{
    collections::HashMap,
    fs,
    net::{Ipv4Addr, Ipv6Addr},
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
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const WORKER_TICK: Duration = Duration::from_millis(50);
const CPU_STAT_PATH: &str = "/proc/stat";
const CPU_INFO_PATH: &str = "/proc/cpuinfo";
const CPU_SYSFS_PATH: &str = "/sys/devices/system/cpu";
const MEMORY_INFO_PATH: &str = "/proc/meminfo";
const NM_802_11_AP_FLAGS_PRIVACY: u32 = 0x1;
const NM_802_11_AP_SEC_KEY_MGMT_PSK: u32 = 0x100;
const NM_802_11_AP_SEC_KEY_MGMT_802_1X: u32 = 0x200;
const NM_802_11_AP_SEC_KEY_MGMT_SAE: u32 = 0x400;

type NmConnectionSettings = HashMap<String, HashMap<String, OwnedValue>>;

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
    SetWifiDiscovery(bool),
    ConnectWifi {
        ssid: Vec<u8>,
        password: Option<String>,
    },
    UpdateNetworkSettings {
        connection_uuid: String,
        settings: NetworkSettings,
    },
    ToggleBluetooth,
    ToggleMute(AudioEndpoint),
    SetVolume(AudioEndpoint, u8),
    SetBrightness(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WifiSecurity {
    Open,
    Personal,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WifiNetwork {
    /// Raw SSID is retained for NetworkManager calls; `label` is only for display.
    pub ssid: Vec<u8>,
    pub label: String,
    pub strength: u8,
    pub connected: bool,
    pub saved: bool,
    pub security: WifiSecurity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkKind {
    Wired,
    Wifi,
}

/// The interface NetworkManager selected for general internet traffic.
/// IPv4 takes precedence when both protocol families use different routes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkRoute {
    pub kind: NetworkKind,
    pub interface: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpSettings {
    pub automatic: bool,
    pub address: String,
    pub subnet_or_prefix: String,
    pub gateway: String,
    pub primary_dns: String,
    pub secondary_dns: String,
}

impl IpSettings {
    fn automatic() -> Self {
        Self {
            automatic: true,
            address: String::new(),
            subnet_or_prefix: String::new(),
            gateway: String::new(),
            primary_dns: String::new(),
            secondary_dns: String::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkSettings {
    pub ipv4: IpSettings,
    pub ipv6: IpSettings,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            ipv4: IpSettings::automatic(),
            ipv6: IpSettings::automatic(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveNetwork {
    pub label: String,
    pub kind: NetworkKind,
    pub interface: String,
    pub default_ipv4: bool,
    pub default_ipv6: bool,
    pub connection_uuid: Option<String>,
    pub speed_mbps: Option<u64>,
    pub settings: NetworkSettings,
}

#[derive(Default)]
struct ActiveConnectionInfo {
    connection_uuid: Option<String>,
    profile_settings: Option<NmConnectionSettings>,
    default_ipv4: bool,
    default_ipv6: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WifiConnectionEvent {
    Connecting { ssid: Vec<u8> },
    Succeeded { ssid: Vec<u8> },
    Failed { ssid: Vec<u8>, message: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkSettingsEvent {
    Applied {
        connection_uuid: String,
    },
    Failed {
        connection_uuid: String,
        message: String,
    },
}

fn wifi_security(flags: u32, wpa_flags: u32, rsn_flags: u32) -> WifiSecurity {
    if flags & NM_802_11_AP_FLAGS_PRIVACY == 0 {
        return WifiSecurity::Open;
    }
    let key_management = wpa_flags | rsn_flags;
    if key_management & (NM_802_11_AP_SEC_KEY_MGMT_PSK | NM_802_11_AP_SEC_KEY_MGMT_SAE) != 0 {
        WifiSecurity::Personal
    } else if key_management & NM_802_11_AP_SEC_KEY_MGMT_802_1X != 0 {
        WifiSecurity::Unsupported
    } else {
        // WEP and uncommon legacy modes require a workflow other than the
        // password form that this control center deliberately exposes.
        WifiSecurity::Unsupported
    }
}

fn owned_value<'a, T>(value: T) -> Result<OwnedValue, String>
where
    Value<'a>: From<T>,
{
    OwnedValue::try_from(Value::from(value)).map_err(|error| error.to_string())
}

fn sort_wifi_networks(networks: &mut [WifiNetwork]) {
    networks.sort_by(|left, right| {
        right
            .connected
            .cmp(&left.connected)
            .then_with(|| right.strength.cmp(&left.strength))
            .then_with(|| left.label.cmp(&right.label))
    });
}

fn ipv4_prefix_from_mask(mask: &str) -> Result<u8, String> {
    let mask = mask
        .parse::<Ipv4Addr>()
        .map_err(|_| "IPv4サブネットマスクが正しくありません".to_string())?;
    let bits = u32::from(mask);
    let prefix = bits.leading_ones() as u8;
    if bits != (!0_u32 << (32 - prefix)) {
        return Err("IPv4サブネットマスクが正しくありません".to_string());
    }
    Ok(prefix)
}

fn ipv4_mask_from_prefix(prefix: u8) -> String {
    Ipv4Addr::from(if prefix == 0 {
        0
    } else {
        !0_u32 << (32 - prefix)
    })
    .to_string()
}

fn dns_values(settings: &IpSettings) -> Vec<String> {
    [&settings.primary_dns, &settings.secondary_dns]
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .collect()
}

fn validate_ip_settings(settings: &IpSettings, ipv6: bool) -> Result<Option<u8>, String> {
    if settings.automatic {
        return Ok(None);
    }
    if ipv6 {
        settings
            .address
            .parse::<Ipv6Addr>()
            .map_err(|_| "IPv6アドレスが正しくありません".to_string())?;
        let prefix = settings
            .subnet_or_prefix
            .parse::<u8>()
            .map_err(|_| "IPv6サブネットプレフィックスの長さが正しくありません".to_string())?;
        if prefix > 128 {
            return Err("IPv6サブネットプレフィックスの長さは0〜128で入力してください".to_string());
        }
        if !settings.gateway.trim().is_empty() {
            settings
                .gateway
                .parse::<Ipv6Addr>()
                .map_err(|_| "IPv6ゲートウェイが正しくありません".to_string())?;
        }
        for dns in dns_values(settings) {
            dns.parse::<Ipv6Addr>()
                .map_err(|_| "IPv6 DNSが正しくありません".to_string())?;
        }
        Ok(Some(prefix))
    } else {
        settings
            .address
            .parse::<Ipv4Addr>()
            .map_err(|_| "IPv4アドレスが正しくありません".to_string())?;
        let prefix = ipv4_prefix_from_mask(&settings.subnet_or_prefix)?;
        if !settings.gateway.trim().is_empty() {
            settings
                .gateway
                .parse::<Ipv4Addr>()
                .map_err(|_| "IPv4ゲートウェイが正しくありません".to_string())?;
        }
        for dns in dns_values(settings) {
            dns.parse::<Ipv4Addr>()
                .map_err(|_| "IPv4 DNSが正しくありません".to_string())?;
        }
        Ok(Some(prefix))
    }
}

fn profile_string(section: Option<&HashMap<String, OwnedValue>>, key: &str) -> String {
    section
        .and_then(|section| section.get(key))
        .and_then(|value| String::try_from(value.clone()).ok())
        .unwrap_or_default()
}

fn profile_dns(section: Option<&HashMap<String, OwnedValue>>) -> Vec<String> {
    section
        .and_then(|section| section.get("dns-data"))
        .and_then(|value| Vec::<String>::try_from(value.clone()).ok())
        .unwrap_or_default()
}

fn profile_address(section: Option<&HashMap<String, OwnedValue>>) -> (String, Option<u8>) {
    let Some(value) = section.and_then(|section| section.get("address-data")) else {
        return (String::new(), None);
    };
    let Ok(addresses) = Vec::<HashMap<String, OwnedValue>>::try_from(value.clone()) else {
        return (String::new(), None);
    };
    let Some(address) = addresses.first() else {
        return (String::new(), None);
    };
    (
        address
            .get("address")
            .and_then(|value| String::try_from(value.clone()).ok())
            .unwrap_or_default(),
        address
            .get("prefix")
            .and_then(|value| u32::try_from(value.clone()).ok())
            .and_then(|value| u8::try_from(value).ok()),
    )
}

fn ip_settings_from_profile(
    section: Option<&HashMap<String, OwnedValue>>,
    ipv6: bool,
) -> IpSettings {
    let automatic = profile_string(section, "method") != "manual";
    let (address, prefix) = profile_address(section);
    let dns = profile_dns(section);
    IpSettings {
        automatic,
        address,
        subnet_or_prefix: prefix
            .map(|prefix| {
                if ipv6 {
                    prefix.to_string()
                } else {
                    ipv4_mask_from_prefix(prefix)
                }
            })
            .unwrap_or_default(),
        gateway: profile_string(section, "gateway"),
        primary_dns: dns.first().cloned().unwrap_or_default(),
        secondary_dns: dns.get(1).cloned().unwrap_or_default(),
    }
}

fn network_settings_from_profile(settings: &NmConnectionSettings) -> NetworkSettings {
    NetworkSettings {
        ipv4: ip_settings_from_profile(settings.get("ipv4"), false),
        ipv6: ip_settings_from_profile(settings.get("ipv6"), true),
    }
}

fn ip_settings_from_runtime(
    addresses: &[HashMap<String, OwnedValue>],
    gateway: String,
    nameservers: &[HashMap<String, OwnedValue>],
    ipv6: bool,
) -> IpSettings {
    let address = addresses.first();
    let prefix = address
        .and_then(|address| address.get("prefix"))
        .and_then(|value| u32::try_from(value.clone()).ok())
        .and_then(|value| u8::try_from(value).ok());
    let dns: Vec<String> = nameservers
        .iter()
        .filter_map(|nameserver| {
            nameserver
                .get("address")
                .and_then(|value| String::try_from(value.clone()).ok())
        })
        .collect();
    IpSettings {
        automatic: true,
        address: address
            .and_then(|address| address.get("address"))
            .and_then(|value| String::try_from(value.clone()).ok())
            .unwrap_or_default(),
        subnet_or_prefix: prefix
            .map(|prefix| {
                if ipv6 {
                    prefix.to_string()
                } else {
                    ipv4_mask_from_prefix(prefix)
                }
            })
            .unwrap_or_default(),
        gateway,
        primary_dns: dns.first().cloned().unwrap_or_default(),
        secondary_dns: dns.get(1).cloned().unwrap_or_default(),
    }
}

fn update_ip_section(
    section: &mut HashMap<String, OwnedValue>,
    settings: &IpSettings,
    prefix: Option<u8>,
    ipv6: bool,
) -> Result<(), String> {
    section.insert(
        "method".to_string(),
        owned_value(if settings.automatic { "auto" } else { "manual" })?,
    );
    section.remove("address-data");
    section.remove("gateway");
    section.remove("dns-data");
    section.remove("ignore-auto-dns");
    if let Some(prefix) = prefix {
        let mut address = HashMap::new();
        address.insert(
            "address".to_string(),
            owned_value(settings.address.clone())?,
        );
        address.insert("prefix".to_string(), owned_value(u32::from(prefix))?);
        section.insert("address-data".to_string(), owned_value(vec![address])?);
        if !settings.gateway.trim().is_empty() {
            section.insert(
                "gateway".to_string(),
                owned_value(settings.gateway.trim().to_string())?,
            );
        }
        let dns = dns_values(settings);
        if !dns.is_empty() {
            section.insert("dns-data".to_string(), owned_value(dns)?);
            section.insert("ignore-auto-dns".to_string(), owned_value(true)?);
        }
    }
    if ipv6 && settings.automatic {
        // NetworkManager uses "auto" for SLAAC/DHCPv6; removing static data
        // above also restores automatic DNS and routes.
    }
    Ok(())
}

fn nmcli_error(output: &std::process::Output, fallback: &str) -> String {
    let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if message.is_empty() {
        fallback.to_string()
    } else {
        message
    }
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
    pub wired_active: bool,
    pub wireless_interface: Option<String>,
    pub wifi_networks: Vec<WifiNetwork>,
    pub active_networks: Vec<ActiveNetwork>,
    pub primary_network: Option<NetworkRoute>,
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
    pub wifi_events: Receiver<WifiConnectionEvent>,
    pub network_settings_events: Receiver<NetworkSettingsEvent>,
}

impl Default for ControlSnapshot {
    fn default() -> Self {
        Self {
            wifi: ToggleStatus::unavailable(),
            wired_active: false,
            wireless_interface: None,
            wifi_networks: Vec::new(),
            active_networks: Vec::new(),
            primary_network: None,
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
    let (wifi_event_sender, wifi_event_receiver) = async_channel::unbounded();
    let (network_settings_event_sender, network_settings_event_receiver) =
        async_channel::unbounded();

    if let Err(error) = thread::Builder::new()
        .name("bah-system-controls".to_string())
        .spawn(move || {
            run_worker(
                action_receiver,
                snapshot_sender,
                wifi_event_sender,
                network_settings_event_sender,
            )
        })
    {
        error!("failed to start system-controls worker: {error}");
    }

    ControlChannels {
        actions: action_sender,
        updates: snapshot_receiver,
        wifi_events: wifi_event_receiver,
        network_settings_events: network_settings_event_receiver,
    }
}

fn run_worker(
    actions: Receiver<ControlAction>,
    snapshots: Sender<ControlSnapshot>,
    wifi_events: Sender<WifiConnectionEvent>,
    network_settings_events: Sender<NetworkSettingsEvent>,
) {
    let mut backend = SystemBackend::new();
    let mut discovery_enabled = false;
    let mut current = backend.snapshot(discovery_enabled);
    if snapshots.send_blocking(current.clone()).is_err() {
        return;
    }
    let mut refreshed_at = Instant::now();

    loop {
        let mut handled_action = false;
        while let Ok(action) = actions.try_recv() {
            handled_action = true;
            match action {
                ControlAction::SetWifiDiscovery(enabled) => {
                    discovery_enabled = enabled;
                    if enabled {
                        backend.request_wifi_scan();
                    }
                }
                ControlAction::ConnectWifi { ssid, password } => {
                    let _ = wifi_events
                        .send_blocking(WifiConnectionEvent::Connecting { ssid: ssid.clone() });
                    match backend.connect_wifi(&ssid, password.as_deref()) {
                        Ok(()) => {
                            let _ =
                                wifi_events.send_blocking(WifiConnectionEvent::Succeeded { ssid });
                        }
                        Err(message) => {
                            let _ = wifi_events
                                .send_blocking(WifiConnectionEvent::Failed { ssid, message });
                        }
                    }
                }
                ControlAction::UpdateNetworkSettings {
                    connection_uuid,
                    settings,
                } => match backend.update_network_settings(&connection_uuid, &settings) {
                    Ok(()) => {
                        let _ = network_settings_events
                            .send_blocking(NetworkSettingsEvent::Applied { connection_uuid });
                    }
                    Err(message) => {
                        let _ =
                            network_settings_events.send_blocking(NetworkSettingsEvent::Failed {
                                connection_uuid,
                                message,
                            });
                    }
                },
                action => match backend.apply(&current, action.clone()) {
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
                        ControlAction::SetWifiDiscovery(_)
                        | ControlAction::ConnectWifi { .. }
                        | ControlAction::UpdateNetworkSettings { .. } => {}
                    },
                    Err(error) => warn!("system control action failed: {error}"),
                },
            }
        }

        if handled_action || refreshed_at.elapsed() >= REFRESH_INTERVAL {
            current = backend.snapshot(discovery_enabled);
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

    fn snapshot(&mut self, include_wifi_networks: bool) -> ControlSnapshot {
        let mut wifi = self.wifi_status().unwrap_or_else(|error| {
            debug!("could not read NetworkManager state: {error}");
            ToggleStatus::unavailable()
        });
        let (wired_active, wireless_interface, wifi_networks) =
            if include_wifi_networks && wifi.available && wifi.enabled {
                self.wifi_networks().unwrap_or_else(|error| {
                    debug!("could not read Wi-Fi access points: {error}");
                    (false, None, Vec::new())
                })
            } else {
                (
                    self.wired_active().unwrap_or(false),
                    self.wireless_interface().ok().flatten(),
                    Vec::new(),
                )
            };
        let active_networks = self.active_networks().unwrap_or_else(|error| {
            debug!("could not read active NetworkManager connections: {error}");
            Vec::new()
        });
        let primary_network = preferred_network_route(&active_networks);
        let (download_kbps, upload_kbps) = self.network_traffic.sample(
            primary_network
                .as_ref()
                .map(|route| route.interface.as_str()),
        );
        wifi.download_kbps = download_kbps;
        wifi.upload_kbps = upload_kbps;
        wifi.wired = primary_network
            .as_ref()
            .is_some_and(|route| route.kind == NetworkKind::Wired);
        wifi.interface = primary_network
            .as_ref()
            .map(|route| route.interface.clone());

        ControlSnapshot {
            wifi,
            wired_active,
            wireless_interface,
            wifi_networks,
            active_networks,
            primary_network,
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
            ControlAction::SetWifiDiscovery(_)
            | ControlAction::ConnectWifi { .. }
            | ControlAction::UpdateNetworkSettings { .. } => Ok(()),
        }
    }

    fn active_networks(&self) -> Result<Vec<ActiveNetwork>, String> {
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        let devices: Vec<OwnedObjectPath> = self
            .manager()?
            .call("GetDevices", &())
            .map_err(|error| error.to_string())?;
        let mut networks = Vec::new();

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
            if state != 100 || !matches!(kind, 1 | 2) {
                continue;
            }
            let interface: String = device.get_property("Interface").unwrap_or_default();
            if interface.is_empty() {
                continue;
            }
            let active_path: OwnedObjectPath = device
                .get_property("ActiveConnection")
                .unwrap_or_else(|_| OwnedObjectPath::try_from("/").expect("root object path"));
            let active_connection = self
                .active_connection_settings(&active_path)
                .unwrap_or_default();
            let mut settings = active_connection
                .profile_settings
                .as_ref()
                .map(network_settings_from_profile)
                .unwrap_or_default();
            self.apply_runtime_automatic_settings(&active_path, &mut settings);
            let (label, speed_mbps) = if kind == 1 {
                let wired = Proxy::new(
                    connection,
                    "org.freedesktop.NetworkManager",
                    path.as_str(),
                    "org.freedesktop.NetworkManager.Device.Wired",
                )
                .map_err(|error| error.to_string())?;
                (
                    "有線接続".to_string(),
                    wired.get_property::<u32>("Speed").ok().map(u64::from),
                )
            } else {
                let wireless = Proxy::new(
                    connection,
                    "org.freedesktop.NetworkManager",
                    path.as_str(),
                    "org.freedesktop.NetworkManager.Device.Wireless",
                )
                .map_err(|error| error.to_string())?;
                let label = wireless
                    .get_property::<OwnedObjectPath>("ActiveAccessPoint")
                    .ok()
                    .filter(|access_point| access_point.as_str() != "/")
                    .and_then(|access_point| {
                        Proxy::new(
                            connection,
                            "org.freedesktop.NetworkManager",
                            access_point.as_str(),
                            "org.freedesktop.NetworkManager.AccessPoint",
                        )
                        .ok()?
                        .get_property::<Vec<u8>>("Ssid")
                        .ok()
                    })
                    .filter(|ssid| !ssid.is_empty())
                    .map(|ssid| String::from_utf8_lossy(&ssid).into_owned())
                    .unwrap_or_else(|| "Wi-Fi".to_string());
                (
                    label,
                    wireless
                        .get_property::<u32>("Bitrate")
                        .ok()
                        .map(|speed| u64::from(speed) / 1000),
                )
            };
            networks.push(ActiveNetwork {
                label,
                kind: if kind == 1 {
                    NetworkKind::Wired
                } else {
                    NetworkKind::Wifi
                },
                interface,
                default_ipv4: active_connection.default_ipv4,
                default_ipv6: active_connection.default_ipv6,
                connection_uuid: active_connection.connection_uuid,
                speed_mbps,
                settings,
            });
        }
        networks.sort_by_key(|network| match network.kind {
            NetworkKind::Wired => 0,
            NetworkKind::Wifi => 1,
        });
        Ok(networks)
    }

    fn active_connection_settings(
        &self,
        active_path: &OwnedObjectPath,
    ) -> Result<ActiveConnectionInfo, String> {
        if active_path.as_str() == "/" {
            return Ok(ActiveConnectionInfo::default());
        }
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        let active = Proxy::new(
            connection,
            "org.freedesktop.NetworkManager",
            active_path.as_str(),
            "org.freedesktop.NetworkManager.Connection.Active",
        )
        .map_err(|error| error.to_string())?;
        let profile_path: OwnedObjectPath = active
            .get_property("Connection")
            .map_err(|error| error.to_string())?;
        let profile = Proxy::new(
            connection,
            "org.freedesktop.NetworkManager",
            profile_path.as_str(),
            "org.freedesktop.NetworkManager.Settings.Connection",
        )
        .map_err(|error| error.to_string())?;
        let settings: NmConnectionSettings = profile
            .call("GetSettings", &())
            .map_err(|error| error.to_string())?;
        let uuid = settings
            .get("connection")
            .and_then(|connection| connection.get("uuid"))
            .and_then(|value| String::try_from(value.clone()).ok());
        let default_ipv4 = active.get_property("Default").unwrap_or(false);
        let default_ipv6 = active.get_property("Default6").unwrap_or(false);
        Ok(ActiveConnectionInfo {
            connection_uuid: uuid,
            profile_settings: Some(settings),
            default_ipv4,
            default_ipv6,
        })
    }

    fn apply_runtime_automatic_settings(
        &self,
        active_path: &OwnedObjectPath,
        settings: &mut NetworkSettings,
    ) {
        if active_path.as_str() == "/" {
            return;
        }
        let Some(connection) = self.system_bus.as_ref() else {
            return;
        };
        let Ok(active) = Proxy::new(
            connection,
            "org.freedesktop.NetworkManager",
            active_path.as_str(),
            "org.freedesktop.NetworkManager.Connection.Active",
        ) else {
            return;
        };
        if settings.ipv4.automatic {
            let path = active.get_property::<OwnedObjectPath>("Ip4Config").ok();
            if let Some(runtime) = path.and_then(|path| self.runtime_ip_settings(&path, false)) {
                settings.ipv4 = runtime;
            }
        }
        if settings.ipv6.automatic {
            let path = active.get_property::<OwnedObjectPath>("Ip6Config").ok();
            if let Some(runtime) = path.and_then(|path| self.runtime_ip_settings(&path, true)) {
                settings.ipv6 = runtime;
            }
        }
    }

    fn runtime_ip_settings(&self, path: &OwnedObjectPath, ipv6: bool) -> Option<IpSettings> {
        if path.as_str() == "/" {
            return None;
        }
        let connection = self.system_bus.as_ref()?;
        let interface = if ipv6 {
            "org.freedesktop.NetworkManager.IP6Config"
        } else {
            "org.freedesktop.NetworkManager.IP4Config"
        };
        let config = Proxy::new(
            connection,
            "org.freedesktop.NetworkManager",
            path.as_str(),
            interface,
        )
        .ok()?;
        let addresses = config
            .get_property::<Vec<HashMap<String, OwnedValue>>>("AddressData")
            .unwrap_or_default();
        let gateway = config.get_property::<String>("Gateway").unwrap_or_default();
        let nameservers = config
            .get_property::<Vec<HashMap<String, OwnedValue>>>("NameserverData")
            .unwrap_or_default();
        Some(ip_settings_from_runtime(
            &addresses,
            gateway,
            &nameservers,
            ipv6,
        ))
    }

    fn update_network_settings(
        &self,
        connection_uuid: &str,
        network_settings: &NetworkSettings,
    ) -> Result<(), String> {
        let ipv4_prefix = validate_ip_settings(&network_settings.ipv4, false)?;
        let ipv6_prefix = validate_ip_settings(&network_settings.ipv6, true)?;
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        let settings_service = Proxy::new(
            connection,
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager/Settings",
            "org.freedesktop.NetworkManager.Settings",
        )
        .map_err(|error| error.to_string())?;
        let profiles: Vec<OwnedObjectPath> = settings_service
            .call("ListConnections", &())
            .map_err(|error| error.to_string())?;
        let mut target: Option<(OwnedObjectPath, NmConnectionSettings)> = None;
        for path in profiles {
            let settings: NmConnectionSettings = {
                let profile = Proxy::new(
                    connection,
                    "org.freedesktop.NetworkManager",
                    path.as_str(),
                    "org.freedesktop.NetworkManager.Settings.Connection",
                )
                .map_err(|error| error.to_string())?;
                profile
                    .call("GetSettings", &())
                    .map_err(|error| error.to_string())?
            };
            let uuid = settings
                .get("connection")
                .and_then(|section| section.get("uuid"))
                .and_then(|value| String::try_from(value.clone()).ok());
            if uuid.as_deref() == Some(connection_uuid) {
                target = Some((path, settings));
                break;
            }
        }
        let Some((profile_path, mut settings)) = target else {
            return Err("接続プロファイルが見つかりません".to_string());
        };
        let profile = Proxy::new(
            connection,
            "org.freedesktop.NetworkManager",
            profile_path.as_str(),
            "org.freedesktop.NetworkManager.Settings.Connection",
        )
        .map_err(|error| error.to_string())?;
        update_ip_section(
            settings.entry("ipv4".to_string()).or_default(),
            &network_settings.ipv4,
            ipv4_prefix,
            false,
        )?;
        update_ip_section(
            settings.entry("ipv6".to_string()).or_default(),
            &network_settings.ipv6,
            ipv6_prefix,
            true,
        )?;
        profile
            .call::<_, _, ()>("Update", &(settings,))
            .map_err(|error| error.to_string())?;

        let down = Command::new("nmcli")
            .args(["connection", "down", "uuid", connection_uuid])
            .output()
            .map_err(|error| format!("NetworkManagerコマンドを起動できません: {error}"))?;
        if !down.status.success() {
            return Err(nmcli_error(&down, "接続を切断できませんでした"));
        }
        let up = Command::new("nmcli")
            .args(["connection", "up", "uuid", connection_uuid])
            .output()
            .map_err(|error| format!("NetworkManagerコマンドを起動できません: {error}"))?;
        if up.status.success() {
            Ok(())
        } else {
            Err(nmcli_error(&up, "接続を再確立できませんでした"))
        }
    }

    fn manager(&self) -> Result<Proxy<'_>, String> {
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        Proxy::new(
            connection,
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
        )
        .map_err(|error| error.to_string())
    }

    fn wireless_devices(&self) -> Result<Vec<(OwnedObjectPath, String, u32)>, String> {
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        let devices: Vec<OwnedObjectPath> = self
            .manager()?
            .call("GetDevices", &())
            .map_err(|error| error.to_string())?;
        Ok(devices
            .into_iter()
            .filter_map(|path| {
                let (kind, interface, state) = {
                    let device = Proxy::new(
                        connection,
                        "org.freedesktop.NetworkManager",
                        path.as_str(),
                        "org.freedesktop.NetworkManager.Device",
                    )
                    .ok()?;
                    (
                        device.get_property::<u32>("DeviceType").ok()?,
                        device.get_property("Interface").unwrap_or_default(),
                        device.get_property("State").unwrap_or_default(),
                    )
                };
                (kind == 2).then_some((path, interface, state))
            })
            .collect())
    }

    fn wireless_interface(&self) -> Result<Option<String>, String> {
        Ok(self
            .wireless_devices()?
            .into_iter()
            .map(|(_, interface, _)| interface)
            .find(|interface| !interface.is_empty()))
    }

    fn wired_active(&self) -> Result<bool, String> {
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        let devices: Vec<OwnedObjectPath> = self
            .manager()?
            .call("GetDevices", &())
            .map_err(|error| error.to_string())?;
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
            if kind == 1 && state == 100 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn request_wifi_scan(&self) {
        let Ok(connection) = self.system_bus.as_ref().ok_or("system bus unavailable") else {
            return;
        };
        for (path, _, _) in self.wireless_devices().unwrap_or_default() {
            let result = Proxy::new(
                connection,
                "org.freedesktop.NetworkManager",
                path.as_str(),
                "org.freedesktop.NetworkManager.Device.Wireless",
            )
            .and_then(|wireless| {
                wireless.call::<_, _, ()>("RequestScan", &(HashMap::<String, OwnedValue>::new()))
            });
            if let Err(error) = result {
                debug!("could not request Wi-Fi scan: {error}");
            }
        }
    }

    fn wifi_networks(&self) -> Result<(bool, Option<String>, Vec<WifiNetwork>), String> {
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        let saved_ssids = self.saved_wifi_ssids()?;
        let wired_active = self.wired_active()?;
        let mut networks: HashMap<Vec<u8>, WifiNetwork> = HashMap::new();
        let mut wireless_interface = None;

        for (device_path, interface, _) in self.wireless_devices()? {
            if wireless_interface.is_none() && !interface.is_empty() {
                wireless_interface = Some(interface);
            }
            let wireless = Proxy::new(
                connection,
                "org.freedesktop.NetworkManager",
                device_path.as_str(),
                "org.freedesktop.NetworkManager.Device.Wireless",
            )
            .map_err(|error| error.to_string())?;
            let active_path: OwnedObjectPath = wireless
                .get_property("ActiveAccessPoint")
                .unwrap_or_else(|_| OwnedObjectPath::try_from("/").expect("root object path"));
            let access_points: Vec<OwnedObjectPath> = wireless
                .call("GetAllAccessPoints", &())
                .map_err(|error| error.to_string())?;

            for access_point_path in access_points {
                let access_point = Proxy::new(
                    connection,
                    "org.freedesktop.NetworkManager",
                    access_point_path.as_str(),
                    "org.freedesktop.NetworkManager.AccessPoint",
                )
                .map_err(|error| error.to_string())?;
                let ssid: Vec<u8> = access_point.get_property("Ssid").unwrap_or_default();
                if ssid.is_empty() {
                    continue;
                }
                let strength: u8 = access_point.get_property("Strength").unwrap_or_default();
                let flags: u32 = access_point.get_property("Flags").unwrap_or_default();
                let wpa_flags: u32 = access_point.get_property("WpaFlags").unwrap_or_default();
                let rsn_flags: u32 = access_point.get_property("RsnFlags").unwrap_or_default();
                let security = wifi_security(flags, wpa_flags, rsn_flags);
                let candidate = WifiNetwork {
                    label: String::from_utf8_lossy(&ssid).into_owned(),
                    saved: saved_ssids.contains(&ssid),
                    connected: access_point_path == active_path,
                    ssid: ssid.clone(),
                    strength,
                    security,
                };
                match networks.get_mut(&ssid) {
                    Some(existing) => {
                        existing.connected |= candidate.connected;
                        existing.saved |= candidate.saved;
                        if candidate.strength > existing.strength {
                            *existing = candidate;
                        }
                    }
                    None => {
                        networks.insert(ssid, candidate);
                    }
                }
            }
        }

        let mut networks: Vec<_> = networks.into_values().collect();
        sort_wifi_networks(&mut networks);
        Ok((wired_active, wireless_interface, networks))
    }

    fn saved_wifi_ssids(&self) -> Result<Vec<Vec<u8>>, String> {
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        let settings = Proxy::new(
            connection,
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager/Settings",
            "org.freedesktop.NetworkManager.Settings",
        )
        .map_err(|error| error.to_string())?;
        let paths: Vec<OwnedObjectPath> = settings
            .call("ListConnections", &())
            .map_err(|error| error.to_string())?;
        let mut ssids = Vec::new();
        for path in paths {
            let Ok(profile) = Proxy::new(
                connection,
                "org.freedesktop.NetworkManager",
                path.as_str(),
                "org.freedesktop.NetworkManager.Settings.Connection",
            ) else {
                continue;
            };
            let Ok(settings) = profile
                .call::<_, _, HashMap<String, HashMap<String, OwnedValue>>>("GetSettings", &())
            else {
                continue;
            };
            let Some(wireless) = settings.get("802-11-wireless") else {
                continue;
            };
            if let Some(ssid) = wireless
                .get("ssid")
                .and_then(|value| Vec::<u8>::try_from(value.clone()).ok())
            {
                ssids.push(ssid);
            }
        }
        Ok(ssids)
    }

    fn connect_wifi(&self, ssid: &[u8], password: Option<&str>) -> Result<(), String> {
        if let Some(password) = password {
            return self.connect_wifi_with_password(ssid, password);
        }
        let interface = self
            .wireless_interface()?
            .ok_or("Wi-Fiアダプターが見つかりません")?;
        let label = String::from_utf8(ssid.to_vec())
            .map_err(|_| "このSSIDは接続に対応していない文字を含みます".to_string())?;
        let mut command = Command::new("nmcli");
        command.args(["device", "wifi", "connect", &label, "ifname", &interface]);
        if let Some(password) = password {
            command.args(["password", password]);
        }
        let output = command
            .output()
            .map_err(|error| format!("NetworkManagerコマンドを起動できません: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(if message.is_empty() {
                "接続できませんでした".to_string()
            } else {
                message
            })
        }
    }

    fn connect_wifi_with_password(&self, ssid: &[u8], password: &str) -> Result<(), String> {
        let _ = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        let (device_path, access_point_path, sae_only) = self.best_access_point(ssid)?;
        let label = String::from_utf8_lossy(ssid).into_owned();
        let mut connection_settings = HashMap::new();
        connection_settings.insert("id".to_string(), owned_value(&label)?);
        connection_settings.insert("type".to_string(), owned_value("802-11-wireless")?);
        connection_settings.insert("autoconnect".to_string(), owned_value(true)?);

        let mut wireless_settings = HashMap::new();
        wireless_settings.insert("ssid".to_string(), owned_value(ssid.to_vec())?);
        wireless_settings.insert("mode".to_string(), owned_value("infrastructure")?);

        let mut security_settings = HashMap::new();
        security_settings.insert(
            "key-mgmt".to_string(),
            owned_value(if sae_only { "sae" } else { "wpa-psk" })?,
        );
        security_settings.insert("psk".to_string(), owned_value(password)?);

        let mut settings = HashMap::new();
        settings.insert("connection".to_string(), connection_settings);
        settings.insert("802-11-wireless".to_string(), wireless_settings);
        settings.insert("802-11-wireless-security".to_string(), security_settings);

        let manager = self.manager()?;
        manager
            .call::<_, _, (OwnedObjectPath, OwnedObjectPath)>(
                "AddAndActivateConnection",
                &(settings, device_path, access_point_path),
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn best_access_point(
        &self,
        wanted_ssid: &[u8],
    ) -> Result<(OwnedObjectPath, OwnedObjectPath, bool), String> {
        let connection = self.system_bus.as_ref().ok_or("system bus unavailable")?;
        let mut best: Option<(u8, OwnedObjectPath, OwnedObjectPath, bool)> = None;
        for (device_path, _, _) in self.wireless_devices()? {
            let wireless = Proxy::new(
                connection,
                "org.freedesktop.NetworkManager",
                device_path.as_str(),
                "org.freedesktop.NetworkManager.Device.Wireless",
            )
            .map_err(|error| error.to_string())?;
            let access_points: Vec<OwnedObjectPath> = wireless
                .call("GetAllAccessPoints", &())
                .map_err(|error| error.to_string())?;
            for access_point_path in access_points {
                let (ssid, strength, wpa_flags, rsn_flags): (Vec<u8>, u8, u32, u32) = {
                    let access_point = Proxy::new(
                        connection,
                        "org.freedesktop.NetworkManager",
                        access_point_path.as_str(),
                        "org.freedesktop.NetworkManager.AccessPoint",
                    )
                    .map_err(|error| error.to_string())?;
                    (
                        access_point.get_property("Ssid").unwrap_or_default(),
                        access_point.get_property("Strength").unwrap_or_default(),
                        access_point.get_property("WpaFlags").unwrap_or_default(),
                        access_point.get_property("RsnFlags").unwrap_or_default(),
                    )
                };
                if ssid == wanted_ssid
                    && best
                        .as_ref()
                        .is_none_or(|(current_strength, _, _, _)| strength > *current_strength)
                {
                    let key_management = wpa_flags | rsn_flags;
                    let sae_only = key_management & NM_802_11_AP_SEC_KEY_MGMT_SAE != 0
                        && key_management & NM_802_11_AP_SEC_KEY_MGMT_PSK == 0;
                    best = Some((strength, device_path.clone(), access_point_path, sae_only));
                }
            }
        }
        best.map(|(_, device, access_point, sae_only)| (device, access_point, sae_only))
            .ok_or("選択したSSIDは見つかりません".to_string())
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

fn preferred_network_route(networks: &[ActiveNetwork]) -> Option<NetworkRoute> {
    networks
        .iter()
        .find(|network| network.default_ipv4)
        .or_else(|| networks.iter().find(|network| network.default_ipv6))
        .map(|network| NetworkRoute {
            kind: network.kind,
            interface: network.interface.clone(),
        })
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
        ActiveNetwork, AudioEndpoint, CpuCoreKind, CpuCoreUsage, CpuStatus, CpuTimes,
        CpuUsageSampler, IpSettings, LevelStatus, MemoryStatus, NetworkKind, NetworkRoute,
        NetworkSettings, WifiNetwork, WifiSecurity, bluetooth_label, classify_cpu_core_kinds,
        cpu_list_count, parse_cpu_snapshot, parse_cpu_times, parse_memory_usage,
        parse_wpctl_volume, preferred_network_route, sort_wifi_networks, validate_ip_settings,
        wifi_security,
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

    fn active_network(
        kind: NetworkKind,
        interface: &str,
        default_ipv4: bool,
        default_ipv6: bool,
    ) -> ActiveNetwork {
        ActiveNetwork {
            label: interface.to_string(),
            kind,
            interface: interface.to_string(),
            default_ipv4,
            default_ipv6,
            connection_uuid: None,
            speed_mbps: None,
            settings: NetworkSettings::default(),
        }
    }

    #[test]
    fn primary_network_prefers_ipv4_default_route() {
        let networks = vec![
            active_network(NetworkKind::Wired, "eth0", true, false),
            active_network(NetworkKind::Wifi, "wlan0", false, true),
        ];

        assert_eq!(
            preferred_network_route(&networks),
            Some(NetworkRoute {
                kind: NetworkKind::Wired,
                interface: "eth0".to_string(),
            })
        );
    }

    #[test]
    fn primary_network_falls_back_to_ipv6_and_requires_a_default_route() {
        let ipv6_only = vec![active_network(NetworkKind::Wifi, "wlan0", false, true)];
        assert_eq!(
            preferred_network_route(&ipv6_only),
            Some(NetworkRoute {
                kind: NetworkKind::Wifi,
                interface: "wlan0".to_string(),
            })
        );

        let connected_without_route =
            vec![active_network(NetworkKind::Wired, "eth0", false, false)];
        assert_eq!(preferred_network_route(&connected_without_route), None);
    }

    #[test]
    fn wifi_sorting_keeps_connected_network_first_then_strength() {
        let mut networks = vec![
            WifiNetwork {
                ssid: b"weak".to_vec(),
                label: "weak".to_string(),
                strength: 12,
                connected: false,
                saved: false,
                security: WifiSecurity::Open,
            },
            WifiNetwork {
                ssid: b"connected".to_vec(),
                label: "connected".to_string(),
                strength: 4,
                connected: true,
                saved: true,
                security: WifiSecurity::Personal,
            },
            WifiNetwork {
                ssid: b"strong".to_vec(),
                label: "strong".to_string(),
                strength: 88,
                connected: false,
                saved: false,
                security: WifiSecurity::Open,
            },
        ];
        sort_wifi_networks(&mut networks);
        assert_eq!(
            networks
                .into_iter()
                .map(|network| network.label)
                .collect::<Vec<_>>(),
            ["connected", "strong", "weak"]
        );
    }

    #[test]
    fn wifi_security_distinguishes_open_personal_and_enterprise() {
        assert_eq!(wifi_security(0, 0, 0), WifiSecurity::Open);
        assert_eq!(wifi_security(1, 0x100, 0), WifiSecurity::Personal);
        assert_eq!(wifi_security(1, 0, 0x200), WifiSecurity::Unsupported);
    }

    #[test]
    fn validates_ipv4_masks_and_manual_addresses() {
        let settings = IpSettings {
            automatic: false,
            address: "192.168.10.5".to_string(),
            subnet_or_prefix: "255.255.255.0".to_string(),
            gateway: "192.168.10.1".to_string(),
            primary_dns: "1.1.1.1".to_string(),
            secondary_dns: "8.8.8.8".to_string(),
        };
        assert_eq!(validate_ip_settings(&settings, false), Ok(Some(24)));

        let invalid_mask = IpSettings {
            subnet_or_prefix: "255.0.255.0".to_string(),
            ..settings
        };
        assert!(validate_ip_settings(&invalid_mask, false).is_err());
    }

    #[test]
    fn validates_ipv6_prefix_limits() {
        let settings = IpSettings {
            automatic: false,
            address: "2001:db8::10".to_string(),
            subnet_or_prefix: "64".to_string(),
            gateway: "2001:db8::1".to_string(),
            primary_dns: "2001:4860:4860::8888".to_string(),
            secondary_dns: String::new(),
        };
        assert_eq!(validate_ip_settings(&settings, true), Ok(Some(64)));
        let invalid_prefix = IpSettings {
            subnet_or_prefix: "129".to_string(),
            ..settings
        };
        assert!(validate_ip_settings(&invalid_prefix, true).is_err());
    }
}
