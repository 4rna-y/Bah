use std::{env, time::Duration};

use gpui::{Font, FontFallbacks, Pixels, Rgba, font, px, rgba};

const UI_FONT_FAMILY: &str = "JetBrainsMono Nerd Font Mono";
const JAPANESE_FONT_FAMILY: &str = "Noto Sans Mono CJK JP";

pub(crate) fn ui_font() -> Font {
    let mut font = font(UI_FONT_FAMILY);
    font.fallbacks = Some(FontFallbacks::from_fonts(vec![
        JAPANESE_FONT_FAMILY.to_string(),
    ]));
    font
}

/// All visual values used by the layer-shell bar.
///
/// Colors use GPUI's `0xRRGGBBAA` representation. The dark root background,
/// rather than compositor blur alone, is the primary readability safeguard.
#[derive(Clone, Copy, Debug)]
pub struct BarTheme {
    pub background: Rgba,
    pub foreground: Rgba,
    pub muted_foreground: Rgba,
    pub active_background: Rgba,
    pub urgent_background: Rgba,
    pub border: Rgba,
    pub bar_height: Pixels,
    pub horizontal_padding: Pixels,
    pub module_spacing: Pixels,
    pub workspace_horizontal_padding: Pixels,
    pub workspace_vertical_padding: Pixels,
    pub workspace_gap: Pixels,
    pub active_workspace_radius: Pixels,
    pub inactive_workspace_radius: Pixels,
    pub workspace_font_size: Pixels,
    pub clock_font_size: Pixels,
    pub active_window_icon_size: Pixels,
    pub active_window_title_max_width: Pixels,
    pub active_workspace_slide_distance: f32,
    pub active_workspace_slide_duration: Duration,
    pub notification_tray_slide_duration: Duration,
    pub high_contrast: bool,
    pub transparency_disabled: bool,
}

impl BarTheme {
    pub fn from_environment(bar_height: f32) -> Self {
        let high_contrast = environment_flag("BAH_HIGH_CONTRAST");
        let transparency_disabled = environment_flag("BAH_DISABLE_TRANSPARENCY");

        Self::new(bar_height, high_contrast, transparency_disabled)
    }

    fn new(bar_height: f32, high_contrast: bool, transparency_disabled: bool) -> Self {
        let (
            background,
            foreground,
            muted_foreground,
            active_background,
            urgent_background,
            border,
        ) = if high_contrast {
            (
                rgba(0x101014F5),
                rgba(0xFAFAFCFF),
                rgba(0xD8D8DEFF),
                rgba(0xFAFAFC42),
                rgba(0xF2A0A066),
                rgba(0xFAFAFC52),
            )
        } else {
            (
                rgba(0x121216B8),
                rgba(0xF5F5F7FF),
                rgba(0xCACAD2FF),
                rgba(0xF5F5F72E),
                rgba(0xF2A0A052),
                rgba(0xF5F5F71F),
            )
        };

        Self {
            // An explicit opaque fallback keeps the bar legible when compositor
            // blur is disabled or transparency is intentionally turned off.
            background: if transparency_disabled {
                background.alpha(1.0)
            } else {
                background
            },
            foreground,
            muted_foreground,
            active_background,
            urgent_background,
            border,
            bar_height: px(bar_height),
            horizontal_padding: px(10.0),
            module_spacing: px(6.0),
            workspace_horizontal_padding: px(7.0),
            workspace_vertical_padding: px(3.0),
            workspace_gap: px(4.0),
            active_workspace_radius: px(8.0),
            inactive_workspace_radius: px(6.0),
            workspace_font_size: px(12.0),
            clock_font_size: px(13.0),
            active_window_icon_size: px(14.0),
            active_window_title_max_width: px(240.0),
            active_workspace_slide_distance: 24.0,
            active_workspace_slide_duration: Duration::from_millis(240),
            notification_tray_slide_duration: Duration::from_millis(240),
            high_contrast,
            transparency_disabled,
        }
    }
}

fn environment_flag(name: &str) -> bool {
    environment_flag_value(env::var(name).ok().as_deref())
}

fn environment_flag_value(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim),
        Some("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

#[cfg(test)]
mod tests {
    use super::{BarTheme, environment_flag_value};

    #[test]
    fn invalid_environment_values_fall_back_to_disabled() {
        assert!(environment_flag_value(Some("1")));
        assert!(environment_flag_value(Some(" true ")));
        assert!(!environment_flag_value(Some("sometimes")));
        assert!(!environment_flag_value(Some("0")));
        assert!(!environment_flag_value(None));
    }

    #[test]
    fn accessibility_modes_adjust_background_alpha() {
        assert!(BarTheme::new(36.0, true, false).background.a > 0.9);
        assert_eq!(BarTheme::new(36.0, false, true).background.a, 1.0);
    }
}
