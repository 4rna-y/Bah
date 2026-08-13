use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

use super::{HyprlandClient, SocketPaths};

const BAH_REQUIRE: &str = "-- bah: display-layout\nrequire(\"bah_displays\")\n";
const BAH_DISPLAYS_FILE: &str = "bah_displays.lua";
const SNAP_DISTANCE: i32 = 24;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Monitor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub width: i32,
    pub height: i32,
    #[serde(default, rename = "refreshRate")]
    pub refresh_rate: f32,
    pub x: i32,
    pub y: i32,
    #[serde(default = "one")]
    pub scale: f32,
    #[serde(default)]
    pub transform: i32,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub disabled: bool,
}

fn one() -> f32 {
    1.0
}

impl Monitor {
    pub fn logical_size(&self) -> (i32, i32) {
        let (width, height) = if self.transform % 2 != 0 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };
        (
            ((width as f32 / self.scale.max(0.01)).round() as i32).max(1),
            ((height as f32 / self.scale.max(0.01)).round() as i32).max(1),
        )
    }

    pub fn logical_position(&self) -> (i32, i32) {
        (
            (self.x as f32 / self.scale.max(0.01)).round() as i32,
            (self.y as f32 / self.scale.max(0.01)).round() as i32,
        )
    }

    fn mode(&self) -> String {
        format!("{}x{}@{:.2}", self.width, self.height, self.refresh_rate)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayLayout {
    pub monitors: Vec<Monitor>,
    pub main: String,
}

impl DisplayLayout {
    pub fn from_monitors(mut monitors: Vec<Monitor>) -> Self {
        monitors.retain(|monitor| !monitor.disabled);
        monitors.sort_by(|left, right| left.name.cmp(&right.name));
        let main = monitors
            .iter()
            .find(|monitor| monitor.x == 0 && monitor.y == 0)
            .or_else(|| monitors.iter().find(|monitor| monitor.focused))
            .or_else(|| monitors.first())
            .map(|monitor| monitor.name.clone())
            .unwrap_or_default();
        Self { monitors, main }
    }

    pub fn monitor(&self, name: &str) -> Option<&Monitor> {
        self.monitors.iter().find(|monitor| monitor.name == name)
    }

    pub fn monitor_mut(&mut self, name: &str) -> Option<&mut Monitor> {
        self.monitors
            .iter_mut()
            .find(|monitor| monitor.name == name)
    }

    pub fn normalize_main(&mut self, name: &str) -> bool {
        let Some(main) = self.monitor(name).cloned() else {
            return false;
        };
        for monitor in &mut self.monitors {
            monitor.x -= main.x;
            monitor.y -= main.y;
        }
        self.main = name.to_string();
        true
    }

    pub fn move_monitor(&mut self, name: &str, x: i32, y: i32) -> bool {
        if name == self.main {
            return false;
        }
        let Some(monitor) = self.monitor_mut(name) else {
            return false;
        };
        monitor.x = x;
        monitor.y = y;
        let (snapped_x, snapped_y) = snapped_position(self, name, x, y, SNAP_DISTANCE);
        let monitor = self.monitor_mut(name).expect("monitor was checked above");
        monitor.x = snapped_x;
        monitor.y = snapped_y;
        true
    }

    pub fn overlaps(&self) -> bool {
        self.monitors.iter().enumerate().any(|(index, monitor)| {
            self.monitors[index + 1..]
                .iter()
                .any(|other| rectangles_overlap(monitor, other))
        })
    }
}

pub fn snapped_position(
    layout: &DisplayLayout,
    moving_name: &str,
    x: i32,
    y: i32,
    distance: i32,
) -> (i32, i32) {
    let Some(moving) = layout.monitor(moving_name) else {
        return (x, y);
    };
    let (width, height) = moving.logical_size();
    let mut best_x = None;
    let mut best_y = None;
    for other in &layout.monitors {
        if other.name == moving_name {
            continue;
        }
        let (other_width, other_height) = other.logical_size();
        for candidate in [other.x - width, other.x + other_width] {
            let delta = (candidate - x).abs();
            if delta <= distance && best_x.is_none_or(|(_, current)| delta < current) {
                best_x = Some((candidate, delta));
            }
        }
        for candidate in [other.y - height, other.y + other_height] {
            let delta = (candidate - y).abs();
            if delta <= distance && best_y.is_none_or(|(_, current)| delta < current) {
                best_y = Some((candidate, delta));
            }
        }
    }
    (
        best_x.map_or(x, |(value, _)| value),
        best_y.map_or(y, |(value, _)| value),
    )
}

fn rectangles_overlap(left: &Monitor, right: &Monitor) -> bool {
    let (left_width, left_height) = left.logical_size();
    let (right_width, right_height) = right.logical_size();
    left.x < right.x + right_width
        && left.x + left_width > right.x
        && left.y < right.y + right_height
        && left.y + left_height > right.y
}

pub fn load_layout() -> Result<DisplayLayout> {
    let paths = SocketPaths::from_environment()?;
    HyprlandClient::new(paths).display_layout()
}

pub fn apply_layout(layout: &DisplayLayout) -> Result<()> {
    if layout.monitors.is_empty() {
        bail!("no active monitors are available");
    }
    if layout.main.is_empty() || layout.monitor(&layout.main).is_none() {
        bail!("a main monitor must be selected");
    }
    if layout.overlaps() {
        bail!("monitor panels overlap; separate them before applying");
    }

    let config_path = hyprland_config_path()?;
    let source = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    if fs::symlink_metadata(&config_path)?.file_type().is_symlink() {
        bail!("Hyprland configuration is a symbolic link and cannot be edited safely");
    }
    if !source.contains("require(\"bah_displays\")") {
        let mut updated = source.clone();
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push_str(BAH_REQUIRE);
        atomic_write(&config_path, updated.as_bytes())?;
    }

    let managed_path = config_path
        .parent()
        .context("Hyprland config has no parent directory")?
        .join(BAH_DISPLAYS_FILE);
    let old_managed = fs::read(&managed_path).ok();
    if let Err(error) = atomic_write(&managed_path, render_layout_lua(layout).as_bytes()) {
        if source != fs::read_to_string(&config_path).unwrap_or_default() {
            let _ = atomic_write(&config_path, source.as_bytes());
        }
        return Err(error);
    }

    let client = HyprlandClient::new(SocketPaths::from_environment()?);
    let applied = client.reload().and_then(|()| client.display_layout());
    match applied {
        Ok(actual) if positions_match(layout, &actual) => {
            client.dispatch(&format!(
                "hl.dsp.workspace.move({{ workspace = \"1\", monitor = {} }})",
                lua_string(&layout.main)
            ))?;
            Ok(())
        }
        Ok(_) => rollback_layout(
            &client,
            &config_path,
            &source,
            &managed_path,
            old_managed,
            "Hyprland reported a different monitor layout",
        ),
        Err(error) => rollback_layout(
            &client,
            &config_path,
            &source,
            &managed_path,
            old_managed,
            &error.to_string(),
        ),
    }
}

fn rollback_layout(
    client: &HyprlandClient,
    config_path: &Path,
    source: &str,
    managed_path: &Path,
    old_managed: Option<Vec<u8>>,
    reason: &str,
) -> Result<()> {
    atomic_write(config_path, source.as_bytes())?;
    match old_managed {
        Some(old) => atomic_write(managed_path, &old)?,
        None if managed_path.exists() => fs::remove_file(managed_path)
            .with_context(|| format!("failed to remove {}", managed_path.display()))?,
        None => {}
    }
    let _ = client.reload();
    bail!("could not apply display layout: {reason}");
}

fn positions_match(expected: &DisplayLayout, actual: &DisplayLayout) -> bool {
    expected.monitors.iter().all(|monitor| {
        actual
            .monitor(&monitor.name)
            .is_some_and(|current| current.x == monitor.x && current.y == monitor.y)
    })
}

pub fn render_layout_lua(layout: &DisplayLayout) -> String {
    let mut result = String::from("-- Generated by Bah. Changes will be replaced.\n\n");
    for monitor in &layout.monitors {
        result.push_str(&format!(
            "hl.monitor({{ output = {}, mode = {}, position = {}, scale = {:.4}, transform = {} }})\n",
            lua_string(&monitor.name),
            lua_string(&monitor.mode()),
            lua_string(&format!("{}x{}", monitor.x, monitor.y)),
            monitor.scale,
            monitor.transform,
        ));
    }
    result.push_str(&format!(
        "\nhl.workspace_rule({{ workspace = \"1\", monitor = {}, default = true }})\n",
        lua_string(&layout.main)
    ));
    result
}

fn lua_string(value: &str) -> String {
    serde_json::to_string(value).expect("strings always serialize")
}

fn hyprland_config_path() -> Result<PathBuf> {
    let root = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .context("XDG_CONFIG_HOME and HOME are unset")?;
    Ok(root.join("hypr").join("hyprland.lua"))
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().context("path has no parent directory")?;
    let temp = parent.join(format!(
        ".{}.bah-new",
        path.file_name().unwrap().to_string_lossy()
    ));
    fs::write(&temp, contents).with_context(|| format!("failed to write {}", temp.display()))?;
    fs::rename(&temp, path).with_context(|| format!("failed to replace {}", path.display()))
}

pub fn wallpaper_sources(
    layout: &DisplayLayout,
    wallpapers: &BTreeMap<String, PathBuf>,
    fallback: Option<&PathBuf>,
) -> Vec<(String, PathBuf)> {
    layout
        .monitors
        .iter()
        .filter_map(|monitor| {
            wallpapers
                .get(&monitor.name)
                .or(fallback)
                .filter(|path| path.is_file())
                .map(|path| (monitor.name.clone(), path.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(name: &str, x: i32, y: i32, width: i32, height: i32) -> Monitor {
        Monitor {
            name: name.into(),
            description: String::new(),
            width,
            height,
            refresh_rate: 60.0,
            x,
            y,
            scale: 1.0,
            transform: 0,
            focused: false,
            disabled: false,
        }
    }

    #[test]
    fn changing_main_keeps_relative_positions() {
        let mut layout = DisplayLayout::from_monitors(vec![
            monitor("A", 0, 0, 100, 100),
            monitor("B", 100, 20, 100, 100),
        ]);
        assert!(layout.normalize_main("B"));
        assert_eq!(
            (
                layout.monitor("B").unwrap().x,
                layout.monitor("B").unwrap().y
            ),
            (0, 0)
        );
        assert_eq!(
            (
                layout.monitor("A").unwrap().x,
                layout.monitor("A").unwrap().y
            ),
            (-100, -20)
        );
    }

    #[test]
    fn snapping_places_an_edge_against_another_monitor() {
        let mut layout = DisplayLayout::from_monitors(vec![
            monitor("A", 0, 0, 100, 100),
            monitor("B", 250, 0, 100, 100),
        ]);
        layout.move_monitor("B", 108, 0);
        assert_eq!(layout.monitor("B").unwrap().x, 100);
    }

    #[test]
    fn main_monitor_remains_at_the_origin() {
        let mut layout = DisplayLayout::from_monitors(vec![
            monitor("A", 0, 0, 100, 100),
            monitor("B", 100, 0, 100, 100),
        ]);
        assert!(!layout.move_monitor("A", 80, 40));
        assert_eq!(
            (
                layout.monitor("A").unwrap().x,
                layout.monitor("A").unwrap().y
            ),
            (0, 0)
        );
    }

    #[test]
    fn overlapping_monitors_are_rejected() {
        let layout = DisplayLayout::from_monitors(vec![
            monitor("A", 0, 0, 100, 100),
            monitor("B", 50, 0, 100, 100),
        ]);
        assert!(layout.overlaps());
    }

    #[test]
    fn generated_lua_contains_positions_and_default_workspace() {
        let layout = DisplayLayout {
            monitors: vec![monitor("DP-1", 0, 0, 1920, 1080)],
            main: "DP-1".into(),
        };
        let lua = render_layout_lua(&layout);
        assert!(lua.contains("position = \"0x0\""));
        assert!(lua.contains("workspace = \"1\""));
    }

    #[test]
    fn output_wallpaper_takes_precedence_over_the_shared_wallpaper() {
        let layout = DisplayLayout::from_monitors(vec![monitor("A", 0, 0, 100, 100)]);
        let mut wallpapers = BTreeMap::new();
        let individual = std::env::temp_dir().join("bah-display-wallpaper-test.png");
        let shared = std::env::temp_dir().join("bah-display-shared-wallpaper-test.png");
        fs::write(&individual, []).unwrap();
        fs::write(&shared, []).unwrap();
        wallpapers.insert("A".into(), individual.clone());
        assert_eq!(
            wallpaper_sources(&layout, &wallpapers, Some(&shared)),
            vec![("A".into(), individual.clone())]
        );
        let _ = fs::remove_file(individual);
        let _ = fs::remove_file(shared);
    }

    #[test]
    fn monitor_reads_hyprland_camel_case_refresh_rate() {
        let monitor: Monitor = serde_json::from_str(
            r#"{"name":"DP-1","width":1920,"height":1080,"refreshRate":143.99,"x":0,"y":0,"scale":1.0}"#,
        )
        .unwrap();
        assert_eq!(monitor.refresh_rate, 143.99);
    }
}
