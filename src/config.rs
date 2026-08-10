use std::{env, fs, path::PathBuf};

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bar_height: 36.0,
            wallpaper: None,
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

fn config_path() -> Result<PathBuf> {
    let root = match env::var_os("XDG_CONFIG_HOME") {
        Some(path) => PathBuf::from(path),
        None => env::current_dir()
            .context("XDG_CONFIG_HOME is unset and the current directory is unavailable")?,
    };
    Ok(root.join("bah").join("config.toml"))
}
