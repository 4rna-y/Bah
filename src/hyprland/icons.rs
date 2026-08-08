use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};

/// Resolves Hyprland app IDs to icon files without invoking an external command.
///
/// Resolution happens in the IPC worker, never on GPUI's drawing thread.
#[derive(Clone, Debug)]
pub struct AppIconResolver {
    data_dirs: Vec<PathBuf>,
    desktop_icons: HashMap<String, String>,
    desktop_names: HashMap<String, String>,
    cache: HashMap<String, Option<PathBuf>>,
}

impl AppIconResolver {
    pub fn new() -> Self {
        let data_dirs = data_directories();
        let desktop_icons = desktop_icon_index(&data_dirs);
        let desktop_names = desktop_name_index(&data_dirs);
        Self {
            data_dirs,
            desktop_icons,
            desktop_names,
            cache: HashMap::new(),
        }
    }

    pub fn resolve(&mut self, app_id: &str, initial_app_id: &str) -> Option<PathBuf> {
        let app_id = app_id
            .trim()
            .split_once('\0')
            .map_or(app_id.trim(), |(value, _)| value);
        let initial_app_id = initial_app_id.trim();
        let cache_key = format!(
            "{}\u{1f}{}",
            app_id.to_lowercase(),
            initial_app_id.to_lowercase()
        );
        if let Some(icon) = self.cache.get(&cache_key) {
            return icon.clone();
        }

        let icon_name = [app_id, initial_app_id]
            .into_iter()
            .filter(|value| !value.is_empty())
            .find_map(|value| self.desktop_icons.get(&value.to_lowercase()))
            .cloned()
            // Some Wayland apps, including Ghostty, use an app ID that is
            // already the icon name even when their desktop file is absent.
            .or_else(|| {
                [app_id, initial_app_id]
                    .into_iter()
                    .find(|value| !value.is_empty())
                    .map(str::to_string)
            });
        let icon = icon_name.and_then(|name| self.find_icon(&name));
        self.cache.insert(cache_key, icon.clone());
        icon
    }

    /// Returns the user-facing application name from its Desktop Entry.
    pub fn display_name(&self, app_id: &str, initial_app_id: &str) -> String {
        let app_id = app_id
            .trim()
            .split_once('\0')
            .map_or(app_id.trim(), |(value, _)| value);
        let initial_app_id = initial_app_id.trim();
        [app_id, initial_app_id]
            .into_iter()
            .filter(|value| !value.is_empty())
            .find_map(|value| self.desktop_names.get(&value.to_lowercase()))
            .cloned()
            .or_else(|| {
                [app_id, initial_app_id]
                    .into_iter()
                    .find(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "Unknown application".to_string())
    }

    fn find_icon(&self, icon_name: &str) -> Option<PathBuf> {
        let direct_path = Path::new(icon_name);
        if direct_path.is_absolute() && direct_path.is_file() {
            return Some(direct_path.to_path_buf());
        }

        self.data_dirs
            .iter()
            .find_map(|data_dir| find_icon_in_data_dir(data_dir, icon_name))
    }
}

pub(crate) fn data_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(data_home) = env::var_os("XDG_DATA_HOME") {
        directories.push(PathBuf::from(data_home));
    } else if let Some(home) = env::var_os("HOME") {
        directories.push(PathBuf::from(home).join(".local/share"));
    }

    let data_dirs =
        env::var_os("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    directories.extend(env::split_paths(&data_dirs));
    // NixOS system packages expose desktop entries and icons through this profile.
    directories.push(PathBuf::from("/run/current-system/sw/share"));
    let mut deduplicated = Vec::with_capacity(directories.len());
    for directory in directories {
        if !deduplicated.contains(&directory) {
            deduplicated.push(directory);
        }
    }
    deduplicated
}

fn desktop_icon_index(data_dirs: &[PathBuf]) -> HashMap<String, String> {
    let mut icons = HashMap::new();
    for data_dir in data_dirs {
        let applications = data_dir.join("applications");
        let Ok(entries) = fs::read_dir(applications) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("desktop") {
                continue;
            }
            let Some((icon, startup_wm_class)) = desktop_entry_icon(&path) else {
                continue;
            };
            if let Some(file_stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                icons
                    .entry(file_stem.to_lowercase())
                    .or_insert_with(|| icon.clone());
            }
            if let Some(startup_wm_class) = startup_wm_class {
                icons.entry(startup_wm_class.to_lowercase()).or_insert(icon);
            }
        }
    }
    icons
}

fn desktop_name_index(data_dirs: &[PathBuf]) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for data_dir in data_dirs {
        let applications = data_dir.join("applications");
        let Ok(entries) = fs::read_dir(applications) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("desktop") {
                continue;
            }
            let Some((name, startup_wm_class)) = desktop_entry_name(&path) else {
                continue;
            };
            if let Some(file_stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                names
                    .entry(file_stem.to_lowercase())
                    .or_insert_with(|| name.clone());
            }
            if let Some(startup_wm_class) = startup_wm_class {
                names.entry(startup_wm_class.to_lowercase()).or_insert(name);
            }
        }
    }
    names
}

fn desktop_entry_icon(path: &Path) -> Option<(String, Option<String>)> {
    let contents = fs::read_to_string(path).ok()?;
    let mut in_desktop_entry = false;
    let mut icon = None;
    let mut startup_wm_class = None;

    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Icon" => icon = Some(value.to_string()),
            "StartupWMClass" => startup_wm_class = Some(value.to_string()),
            _ => {}
        }
    }

    icon.map(|icon| (icon, startup_wm_class))
}

fn desktop_entry_name(path: &Path) -> Option<(String, Option<String>)> {
    let contents = fs::read_to_string(path).ok()?;
    let mut in_desktop_entry = false;
    let mut name = None;
    let mut startup_wm_class = None;

    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Name" => name = Some(value.to_string()),
            "StartupWMClass" => startup_wm_class = Some(value.to_string()),
            _ => {}
        }
    }

    name.map(|name| (name, startup_wm_class))
}

fn find_icon_in_data_dir(data_dir: &Path, icon_name: &str) -> Option<PathBuf> {
    find_icon_in_directory(&data_dir.join("pixmaps"), icon_name).or_else(|| {
        let icons_root = data_dir.join("icons");
        let entries = fs::read_dir(&icons_root).ok()?;
        entries
            .flatten()
            .find_map(|theme| find_icon_in_theme(&theme.path(), icon_name))
    })
}

fn find_icon_in_theme(theme: &Path, icon_name: &str) -> Option<PathBuf> {
    const PREFERRED_SIZES: [&str; 10] = [
        "32x32",
        "24x24",
        "16x16",
        "48x48",
        "64x64",
        "128x128",
        "256x256",
        "512x512",
        "512x512@2",
        "scalable",
    ];

    for size in PREFERRED_SIZES {
        if let Some(icon) = find_icon_in_directory(&theme.join(size).join("apps"), icon_name) {
            return Some(icon);
        }
    }
    None
}

fn find_icon_in_directory(directory: &Path, icon_name: &str) -> Option<PathBuf> {
    let filenames = if has_image_extension(icon_name) {
        vec![icon_name.to_string()]
    } else {
        ["png", "svg", "xpm"]
            .into_iter()
            .map(|extension| format!("{icon_name}.{extension}"))
            .collect()
    };
    filenames
        .into_iter()
        .map(|filename| directory.join(filename))
        .find(|path| path.is_file())
}

fn has_image_extension(icon_name: &str) -> bool {
    matches!(
        Path::new(icon_name)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "svg" | "xpm" | "jpg" | "jpeg" | "webp")
    )
}

#[cfg(test)]
mod tests {
    use super::has_image_extension;

    #[test]
    fn reverse_dns_app_ids_are_not_image_extensions() {
        assert!(!has_image_extension("com.mitchellh.ghostty"));
        assert!(has_image_extension("com.mitchellh.ghostty.png"));
    }
}
