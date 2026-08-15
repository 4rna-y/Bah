//! Frozen rectangular screenshots for a Hyprland Wayland session.
//!
//! `grim` captures the whole desktop before `slurp` begins the interactive
//! selection. The final crop therefore represents the moment the shortcut was
//! pressed, not the point at which the mouse button is released.

use std::{
    env, fs,
    fs::OpenOptions,
    io::{Cursor, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Context, Result, anyhow, bail};
use image::{DynamicImage, ImageFormat, RgbaImage, imageops};

use crate::{
    clipboard::ClipboardPublisher,
    hyprland::{HyprlandClient, SocketPaths, display::DisplayLayout},
};

#[derive(Debug)]
pub enum ScreenshotResult {
    Saved {
        path: PathBuf,
        clipboard_copied: bool,
    },
    Cancelled,
    Failed(anyhow::Error),
}

/// Starts one complete capture session. The receiver produces exactly one
/// result, allowing the bar to reject overlapping commands without blocking
/// GPUI's render thread.
pub fn start_capture(
    publisher: Arc<Mutex<ClipboardPublisher>>,
) -> async_channel::Receiver<ScreenshotResult> {
    let (sender, receiver) = async_channel::bounded(1);
    let _ = thread::Builder::new()
        .name("bah-screenshot".to_string())
        .spawn(move || {
            let result = capture_and_save(publisher);
            let _ = sender.send_blocking(result);
        });
    receiver
}

fn capture_and_save(publisher: Arc<Mutex<ClipboardPublisher>>) -> ScreenshotResult {
    match capture_and_save_inner(publisher) {
        Ok((path, clipboard_copied)) => ScreenshotResult::Saved {
            path,
            clipboard_copied,
        },
        Err(error) if error.to_string() == "selection cancelled" => ScreenshotResult::Cancelled,
        Err(error) => ScreenshotResult::Failed(error),
    }
}

fn capture_and_save_inner(publisher: Arc<Mutex<ClipboardPublisher>>) -> Result<(PathBuf, bool)> {
    // The monitor layout is frozen alongside the pixels: geometry returned by
    // slurp is expressed in this layout's logical coordinate system.
    let layout = SocketPaths::from_environment()
        .and_then(|paths| HyprlandClient::new(paths).display_layout())
        .context("failed to read Hyprland monitor layout")?;
    if layout.monitors.is_empty() {
        bail!("no enabled monitor is available for screenshot capture");
    }

    let captured = capture_desktop()?;
    let geometry = select_geometry()?;
    let crop = crop_frozen_desktop(&captured, &layout, geometry)?;
    let png = encode_png(&crop)?;
    let path = save_png(&png)?;

    let clipboard_copied = match publisher
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .publish_bytes("image/png", &png)
    {
        Ok(()) => true,
        Err(error) => {
            log::warn!("screenshot was saved but could not be copied to the clipboard: {error:#}");
            false
        }
    };
    Ok((path, clipboard_copied))
}

fn capture_desktop() -> Result<RgbaImage> {
    let output = Command::new("grim")
        .arg("-")
        .output()
        .context("failed to start grim; install grim to enable screenshots")?;
    if !output.status.success() {
        bail!(
            "grim failed to capture the desktop: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    image::load_from_memory(&output.stdout)
        .context("grim returned an invalid image")
        .map(|image| image.to_rgba8())
}

fn select_geometry() -> Result<Selection> {
    let output = Command::new("slurp")
        .args(["-f", "%x,%y %wx%h"])
        .output()
        .context("failed to start slurp; install slurp to enable rectangular screenshots")?;
    if !output.status.success() {
        bail!("selection cancelled");
    }
    let selection =
        String::from_utf8(output.stdout).context("slurp returned non-UTF-8 geometry")?;
    parse_selection(selection.trim())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Selection {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

fn parse_selection(value: &str) -> Result<Selection> {
    let (origin, size) = value
        .split_once(' ')
        .context("slurp returned an invalid selection geometry")?;
    let (x, y) = origin
        .split_once(',')
        .context("slurp returned an invalid selection origin")?;
    let (width, height) = size
        .split_once('x')
        .context("slurp returned an invalid selection size")?;
    let selection = Selection {
        x: x.parse().context("slurp returned an invalid selection x")?,
        y: y.parse().context("slurp returned an invalid selection y")?,
        width: width
            .parse()
            .context("slurp returned an invalid selection width")?,
        height: height
            .parse()
            .context("slurp returned an invalid selection height")?,
    };
    if selection.width <= 0 || selection.height <= 0 {
        bail!("selection cancelled");
    }
    Ok(selection)
}

fn crop_frozen_desktop(
    captured: &RgbaImage,
    layout: &DisplayLayout,
    selection: Selection,
) -> Result<RgbaImage> {
    let (left, top, right, bottom) = layout_bounds(layout)?;
    let logical_width = (right - left) as f32;
    let logical_height = (bottom - top) as f32;
    let scale_x = captured.width() as f32 / logical_width;
    let scale_y = captured.height() as f32 / logical_height;
    let selection_right = selection.x.saturating_add(selection.width);
    let selection_bottom = selection.y.saturating_add(selection.height);

    let pixel_left = ((selection.x - left) as f32 * scale_x).floor().max(0.0) as u32;
    let pixel_top = ((selection.y - top) as f32 * scale_y).floor().max(0.0) as u32;
    let pixel_right = ((selection_right - left) as f32 * scale_x)
        .ceil()
        .min(captured.width() as f32) as u32;
    let pixel_bottom = ((selection_bottom - top) as f32 * scale_y)
        .ceil()
        .min(captured.height() as f32) as u32;
    if pixel_right <= pixel_left || pixel_bottom <= pixel_top {
        bail!("selected area is outside the captured desktop");
    }
    Ok(imageops::crop_imm(
        captured,
        pixel_left,
        pixel_top,
        pixel_right - pixel_left,
        pixel_bottom - pixel_top,
    )
    .to_image())
}

fn layout_bounds(layout: &DisplayLayout) -> Result<(i32, i32, i32, i32)> {
    let mut monitors = layout.monitors.iter();
    let first = monitors
        .next()
        .context("desktop layout contains no monitors")?;
    let (first_width, first_height) = first.logical_size();
    let mut left = first.x;
    let mut top = first.y;
    let mut right = first.x.saturating_add(first_width);
    let mut bottom = first.y.saturating_add(first_height);
    for monitor in monitors {
        let (width, height) = monitor.logical_size();
        left = left.min(monitor.x);
        top = top.min(monitor.y);
        right = right.max(monitor.x.saturating_add(width));
        bottom = bottom.max(monitor.y.saturating_add(height));
    }
    if right <= left || bottom <= top {
        bail!("desktop layout has invalid dimensions");
    }
    Ok((left, top, right, bottom))
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .context("failed to encode screenshot PNG")?;
    Ok(bytes)
}

fn save_png(bytes: &[u8]) -> Result<PathBuf> {
    let directory = screenshots_directory();
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "failed to create screenshot directory {}",
            directory.display()
        )
    })?;
    let timestamp = chrono::Local::now().format("Screenshot_%Y-%m-%d_%H-%M-%S");
    for suffix in 0u32..10_000 {
        let name = if suffix == 0 {
            format!("{timestamp}.png")
        } else {
            format!("{timestamp}_{suffix}.png")
        };
        let path = directory.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(bytes)
                    .with_context(|| format!("failed to write screenshot {}", path.display()))?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create screenshot {}", path.display()));
            }
        }
    }
    Err(anyhow!("could not allocate a unique screenshot file name"))
}

fn screenshots_directory() -> PathBuf {
    pictures_directory().join("Screenshots")
}

fn pictures_directory() -> PathBuf {
    if let Some(path) = env::var_os("XDG_PICTURES_DIR").filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    let home = env::var_os("HOME").map(PathBuf::from);
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|path| path.join(".config")));
    if let (Some(home), Some(config_home)) = (home.as_ref(), config_home) {
        if let Ok(contents) = fs::read_to_string(config_home.join("user-dirs.dirs"))
            && let Some(path) = parse_pictures_directory(&contents, home)
        {
            return path;
        }
        return home.join("Pictures");
    }
    PathBuf::from("Pictures")
}

fn parse_pictures_directory(contents: &str, home: &Path) -> Option<PathBuf> {
    let value = contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("XDG_PICTURES_DIR=").map(str::trim))?;
    let value = value.strip_prefix('"')?.strip_suffix('"')?;
    if value == "$HOME" {
        Some(home.to_path_buf())
    } else if let Some(relative) = value.strip_prefix("$HOME/") {
        Some(home.join(relative))
    } else {
        Some(PathBuf::from(value))
    }
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::{
        DisplayLayout, Selection, crop_frozen_desktop, parse_pictures_directory, parse_selection,
    };
    use crate::hyprland::display::Monitor;

    fn monitor(name: &str, x: i32, y: i32, width: i32, height: i32, scale: f32) -> Monitor {
        Monitor {
            name: name.into(),
            description: String::new(),
            width,
            height,
            refresh_rate: 60.0,
            x,
            y,
            scale,
            transform: 0,
            focused: false,
            disabled: false,
        }
    }

    #[test]
    fn parses_slurp_geometry() {
        assert_eq!(
            parse_selection("-120,10 400x300").unwrap(),
            Selection {
                x: -120,
                y: 10,
                width: 400,
                height: 300
            }
        );
    }

    #[test]
    fn crops_using_desktop_origin_not_zero() {
        let layout = DisplayLayout::from_monitors(vec![monitor("left", -100, 0, 100, 100, 1.0)]);
        let mut image = RgbaImage::new(100, 100);
        image.put_pixel(0, 0, Rgba([1, 2, 3, 255]));
        let crop = crop_frozen_desktop(
            &image,
            &layout,
            Selection {
                x: -100,
                y: 0,
                width: 10,
                height: 10,
            },
        )
        .unwrap();
        assert_eq!(crop.dimensions(), (10, 10));
        assert_eq!(crop.get_pixel(0, 0), &Rgba([1, 2, 3, 255]));
    }

    #[test]
    fn pictures_directory_expands_home() {
        assert_eq!(
            parse_pictures_directory(
                "XDG_PICTURES_DIR=\"$HOME/Images\"",
                std::path::Path::new("/tmp/home")
            ),
            Some(std::path::PathBuf::from("/tmp/home/Images"))
        );
    }
}
