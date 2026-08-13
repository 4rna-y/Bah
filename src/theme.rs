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

/// The visual density selected for the shell surfaces.
///
/// Readable is the default because it remains useful on bright wallpapers
/// without compositor blur. Glass is intentionally opt-in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VisualMode {
    Readable,
    Glass,
    HighContrast,
    Opaque,
}

/// Semantic background roles shared by every Bah surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceRole {
    /// Persistent, edge-attached UI: the bar and notification tray.
    Shell,
    /// Short-lived contextual UI: popovers, tooltips, and menus.
    Floating,
    /// Focused, information-dense UI: DCC, notification cards, and dialogs.
    Dialog,
    /// Ordinary decorated application windows such as Settings.
    Window,
}

/// All visual values used throughout Bah.
///
/// Colors use GPUI's `0xRRGGBBAA` representation. Backgrounds are defined by
/// semantic role rather than by individual window so new surfaces inherit the
/// same contrast and hierarchy automatically.
#[derive(Clone, Copy, Debug)]
pub struct BahTheme {
    /// Kept as the compatibility name for the persistent shell background.
    pub background: Rgba,
    pub floating_background: Rgba,
    pub dialog_background: Rgba,
    pub window_background: Rgba,
    pub foreground: Rgba,
    pub muted_foreground: Rgba,
    pub active_background: Rgba,
    pub hover_background: Rgba,
    pub pressed_background: Rgba,
    pub container_background: Rgba,
    pub urgent_background: Rgba,
    pub border: Rgba,
    pub strong_border: Rgba,
    pub success: Rgba,
    pub focus: Rgba,
    pub error: Rgba,
    pub bar_height: Pixels,
    pub horizontal_padding: Pixels,
    pub module_spacing: Pixels,
    pub workspace_horizontal_padding: Pixels,
    pub workspace_vertical_padding: Pixels,
    pub workspace_gap: Pixels,
    pub active_workspace_radius: Pixels,
    pub inactive_workspace_radius: Pixels,
    pub control_radius: Pixels,
    pub panel_radius: Pixels,
    pub workspace_font_size: Pixels,
    pub clock_font_size: Pixels,
    pub active_window_icon_size: Pixels,
    pub active_window_title_max_width: Pixels,
    pub active_workspace_slide_distance: f32,
    pub active_workspace_slide_duration: Duration,
    pub notification_tray_slide_duration: Duration,
    pub visual_mode: VisualMode,
}

/// Backwards-compatible name used by the existing Bar-oriented modules.
pub type BarTheme = BahTheme;

impl BahTheme {
    pub fn from_environment(bar_height: f32) -> Self {
        let high_contrast = environment_flag("BAH_HIGH_CONTRAST");
        let transparency_disabled = environment_flag("BAH_DISABLE_TRANSPARENCY");
        let glass = environment_flag("BAH_GLASS");

        Self::new(bar_height, high_contrast, transparency_disabled, glass)
    }

    fn new(bar_height: f32, high_contrast: bool, transparency_disabled: bool, glass: bool) -> Self {
        let visual_mode = if high_contrast {
            VisualMode::HighContrast
        } else if transparency_disabled {
            VisualMode::Opaque
        } else if glass {
            VisualMode::Glass
        } else {
            VisualMode::Readable
        };

        let (
            background,
            floating_background,
            dialog_background,
            window_background,
            foreground,
            muted_foreground,
            active_background,
            hover_background,
            pressed_background,
            container_background,
            urgent_background,
            border,
            strong_border,
        ) = match visual_mode {
            VisualMode::HighContrast => (
                rgba(0x101014F5),
                rgba(0x101014FB),
                rgba(0x101014FF),
                rgba(0x101014FF),
                rgba(0xFAFAFCFF),
                rgba(0xD8D8DEFF),
                rgba(0xFAFAFC42),
                rgba(0xFAFAFC52),
                rgba(0xFAFAFC66),
                rgba(0xFAFAFC1F),
                rgba(0xF2A0A066),
                rgba(0xFAFAFC52),
                rgba(0xFAFAFC70),
            ),
            VisualMode::Opaque => (
                rgba(0x121216FF),
                rgba(0x121216FF),
                rgba(0x121216FF),
                rgba(0x121216FF),
                rgba(0xF5F5F7FF),
                rgba(0xCACAD2FF),
                rgba(0xF5F5F72E),
                rgba(0xF5F5F71F),
                rgba(0xF5F5F73D),
                rgba(0xF5F5F70F),
                rgba(0xF2A0A052),
                rgba(0xF5F5F71F),
                rgba(0xF5F5F738),
            ),
            VisualMode::Glass => (
                rgba(0x12121680),
                rgba(0x121216E0),
                rgba(0x121216F0),
                rgba(0x121216FF),
                rgba(0xF5F5F7FF),
                rgba(0xCACAD2FF),
                rgba(0xF5F5F72E),
                rgba(0xF5F5F71F),
                rgba(0xF5F5F73D),
                rgba(0xF5F5F70F),
                rgba(0xF2A0A052),
                rgba(0xF5F5F71F),
                rgba(0xF5F5F738),
            ),
            VisualMode::Readable => (
                rgba(0x121216B8),
                rgba(0x121216E0),
                rgba(0x121216F0),
                rgba(0x121216FF),
                rgba(0xF5F5F7FF),
                rgba(0xCACAD2FF),
                rgba(0xF5F5F72E),
                rgba(0xF5F5F71F),
                rgba(0xF5F5F73D),
                rgba(0xF5F5F70F),
                rgba(0xF2A0A052),
                rgba(0xF5F5F71F),
                rgba(0xF5F5F738),
            ),
        };

        let background = transparency_disabled
            .then(|| background.alpha(1.0))
            .unwrap_or(background);
        let floating_background = transparency_disabled
            .then(|| floating_background.alpha(1.0))
            .unwrap_or(floating_background);
        let dialog_background = transparency_disabled
            .then(|| dialog_background.alpha(1.0))
            .unwrap_or(dialog_background);
        let window_background = transparency_disabled
            .then(|| window_background.alpha(1.0))
            .unwrap_or(window_background);

        Self {
            background,
            floating_background,
            dialog_background,
            window_background,
            foreground,
            muted_foreground,
            active_background,
            hover_background,
            pressed_background,
            container_background,
            urgent_background,
            border,
            strong_border,
            success: rgba(0x63D297FF),
            focus: rgba(0x7DA7FFFF),
            error: rgba(0xF2A0A0FF),
            bar_height: px(bar_height),
            horizontal_padding: px(10.0),
            module_spacing: px(6.0),
            workspace_horizontal_padding: px(7.0),
            workspace_vertical_padding: px(3.0),
            workspace_gap: px(4.0),
            active_workspace_radius: px(8.0),
            inactive_workspace_radius: px(6.0),
            control_radius: px(6.0),
            panel_radius: px(8.0),
            workspace_font_size: px(12.0),
            clock_font_size: px(13.0),
            active_window_icon_size: px(14.0),
            active_window_title_max_width: px(240.0),
            active_workspace_slide_distance: 24.0,
            active_workspace_slide_duration: Duration::from_millis(240),
            notification_tray_slide_duration: Duration::from_millis(220),
            visual_mode,
        }
    }

    pub fn surface(self, role: SurfaceRole) -> Rgba {
        match role {
            SurfaceRole::Shell => self.background,
            SurfaceRole::Floating => self.floating_background,
            SurfaceRole::Dialog => self.dialog_background,
            SurfaceRole::Window => self.window_background,
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
    use super::{BahTheme, SurfaceRole, VisualMode, environment_flag_value};

    #[test]
    fn invalid_environment_values_fall_back_to_disabled() {
        assert!(environment_flag_value(Some("1")));
        assert!(environment_flag_value(Some(" true ")));
        assert!(!environment_flag_value(Some("sometimes")));
        assert!(!environment_flag_value(Some("0")));
        assert!(!environment_flag_value(None));
    }

    #[test]
    fn visual_mode_precedence_preserves_readability() {
        assert_eq!(
            BahTheme::new(36.0, false, false, false).visual_mode,
            VisualMode::Readable
        );
        assert_eq!(
            BahTheme::new(36.0, false, false, true).visual_mode,
            VisualMode::Glass
        );
        assert_eq!(
            BahTheme::new(36.0, true, false, true).visual_mode,
            VisualMode::HighContrast
        );
        assert_eq!(
            BahTheme::new(36.0, true, true, true).visual_mode,
            VisualMode::HighContrast
        );
        assert_eq!(BahTheme::new(36.0, true, true, true).background.a, 1.0);
    }

    #[test]
    fn surface_roles_preserve_their_density_hierarchy() {
        let theme = BahTheme::new(36.0, false, false, false);
        assert!(theme.surface(SurfaceRole::Floating).a > theme.surface(SurfaceRole::Shell).a);
        assert!(theme.surface(SurfaceRole::Dialog).a > theme.surface(SurfaceRole::Floating).a);
        assert_eq!(theme.surface(SurfaceRole::Window).a, 1.0);
    }
}
