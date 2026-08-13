use std::{collections::BTreeMap, env, fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Minimal, file-backed settings for the layer surface.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    /// Logical height reserved for the bar.
    pub bar_height: f32,
    /// Local image (or an animated image) drawn by the wallpaper layer.
    pub wallpaper: Option<PathBuf>,
    /// Per-output wallpaper overrides. Keys are Hyprland output names.
    pub wallpapers: BTreeMap<String, PathBuf>,
    /// Notification daemon and popup behaviour.
    pub notifications: NotificationConfig,
    /// Terminal launch command for the device control centre. The executable is
    /// the first item; remaining items are passed as literal arguments.
    pub device_control_center: DeviceControlCenterConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct DeviceControlCenterConfig {
    pub terminal_command: Vec<String>,
}

/// Native notification settings. These intentionally cover Bah's behaviour
/// rather than attempting to parse dunst's INI configuration language.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct NotificationConfig {
    pub enabled: bool,
    pub popup_width: f32,
    pub notification_limit: usize,
    pub history_length: usize,
    pub low_timeout_seconds: u64,
    pub normal_timeout_seconds: u64,
    /// Zero means that critical notifications remain visible until dismissed.
    pub critical_timeout_seconds: u64,
    pub pause_level: u8,
    pub rules: Vec<NotificationRuleConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct NotificationRuleConfig {
    pub name: String,
    pub enabled: bool,
    pub app_name: Option<String>,
    pub summary: Option<String>,
    pub category: Option<String>,
    pub desktop_entry: Option<String>,
    pub urgency: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub override_pause_level: Option<u8>,
    pub skip_popup: Option<bool>,
    pub history_ignore: Option<bool>,
    pub stack_tag: Option<String>,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            popup_width: 360.0,
            notification_limit: 20,
            history_length: 20,
            low_timeout_seconds: 10,
            normal_timeout_seconds: 10,
            critical_timeout_seconds: 0,
            pause_level: 0,
            rules: Vec::new(),
        }
    }
}

impl Default for NotificationRuleConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            app_name: None,
            summary: None,
            category: None,
            desktop_entry: None,
            urgency: None,
            timeout_seconds: None,
            override_pause_level: None,
            skip_popup: None,
            history_ignore: None,
            stack_tag: None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bar_height: 36.0,
            wallpaper: None,
            wallpapers: BTreeMap::new(),
            notifications: NotificationConfig::default(),
            device_control_center: DeviceControlCenterConfig::default(),
        }
    }
}

impl Config {
    /// Loads `$XDG_CONFIG_HOME/bah/config.toml` when it exists.
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read configuration file {}", path.display()))?;
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse configuration file {}", path.display()))?;
        if !config.bar_height.is_finite() || config.bar_height <= 0.0 {
            bail!("configuration bar_height must be a positive finite number");
        }
        if !config.notifications.popup_width.is_finite() || config.notifications.popup_width <= 0.0
        {
            bail!("configuration notifications.popup_width must be a positive finite number");
        }
        if config.notifications.pause_level > 100 {
            bail!("configuration notifications.pause_level must be between 0 and 100");
        }
        Ok(config)
    }

    /// Writes the complete configuration, creating its XDG directory when needed.
    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        let parent = path
            .parent()
            .context("configuration path has no parent directory")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create configuration directory {}",
                parent.display()
            )
        })?;
        let contents = toml::to_string_pretty(self).context("failed to serialize configuration")?;
        fs::write(&path, contents)
            .with_context(|| format!("failed to write configuration file {}", path.display()))
    }
}

pub fn config_path() -> Result<PathBuf> {
    let root = match env::var_os("XDG_CONFIG_HOME") {
        Some(path) => PathBuf::from(path),
        None => env::current_dir()
            .context("XDG_CONFIG_HOME is unset and the current directory is unavailable")?,
    };
    Ok(root.join("bah").join("config.toml"))
}
