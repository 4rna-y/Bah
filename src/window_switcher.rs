use std::{collections::HashMap, path::PathBuf, sync::Arc};

use gpui::{
    Context, FontWeight, MouseButton, Render, RenderImage, ScrollHandle,
    StatefulInteractiveElement, Window, div, img, prelude::*, px,
};
use image::Frame;

use crate::{
    hyprland::WorkspaceWindow,
    theme::{BarTheme, SurfaceRole, ui_font},
    window_snapshot::capture_snapshots,
};

pub const CARD_WIDTH: f32 = 252.0;
pub const CARD_HEIGHT: f32 = 202.0;
const CARD_PREVIEW_HEIGHT: f32 = 132.0;
const CARD_GAP: f32 = 12.0;
const PANEL_PADDING: f32 = 18.0;
const PANEL_MAX_WIDTH: f32 = 1100.0;

/// A frozen candidate set for one Alt-Tab interaction.
#[derive(Clone, Debug)]
pub struct SwitcherState {
    pub windows: Vec<WorkspaceWindow>,
    pub active_address: Option<String>,
}

impl SwitcherState {
    pub fn new(mut windows: Vec<WorkspaceWindow>, active_address: Option<String>) -> Self {
        windows.retain(WorkspaceWindow::is_switcher_candidate);
        windows.sort_by(|left, right| {
            left.focus_history_id
                .cmp(&right.focus_history_id)
                .then_with(|| left.address.cmp(&right.address))
        });
        Self {
            windows,
            active_address,
        }
    }

    fn active_index(&self) -> usize {
        self.active_address
            .as_ref()
            .and_then(|address| {
                self.windows
                    .iter()
                    .position(|window| &window.address == address)
            })
            .unwrap_or(0)
    }
}

/// Full-output Overlay layer content for the Windows-style window switcher.
pub struct WindowSwitcher {
    theme: BarTheme,
    open: bool,
    windows: Vec<WorkspaceWindow>,
    selected_index: usize,
    snapshots: HashMap<String, Arc<RenderImage>>,
    scroll_handle: ScrollHandle,
    snapshot_generation: u64,
}

impl WindowSwitcher {
    pub fn new(theme: BarTheme) -> Self {
        Self {
            theme,
            open: false,
            windows: Vec::new(),
            selected_index: 0,
            snapshots: HashMap::new(),
            scroll_handle: ScrollHandle::new(),
            snapshot_generation: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self, state: SwitcherState, window: &mut Window, cx: &mut Context<Self>) {
        let selected_index = state.active_index();
        self.windows = state.windows;
        self.selected_index = selected_index;
        self.snapshots.clear();
        self.open = true;
        self.snapshot_generation = self.snapshot_generation.wrapping_add(1);
        window.set_input_region(None);
        self.reveal_selected();
        let generation = self.snapshot_generation;
        let snapshots = capture_snapshots(self.windows.clone());
        cx.spawn(async move |switcher, cx| {
            while let Ok(snapshot) = snapshots.recv().await {
                let image = Arc::new(RenderImage::new(vec![Frame::new(snapshot.image)]));
                if switcher
                    .update(cx, |switcher, cx| {
                        if switcher.snapshot_generation == generation {
                            switcher.set_snapshot(snapshot.address, image, cx);
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        self.windows.clear();
        self.snapshots.clear();
        self.snapshot_generation = self.snapshot_generation.wrapping_add(1);
        window.set_input_region(Some(&[]));
        cx.notify();
    }

    pub fn cycle(&mut self, step: isize, cx: &mut Context<Self>) {
        if !self.open || self.windows.is_empty() {
            return;
        }
        let count = self.windows.len() as isize;
        self.selected_index = (self.selected_index as isize + step).rem_euclid(count) as usize;
        self.reveal_selected();
        cx.notify();
    }

    pub fn selected_address(&self) -> Option<String> {
        self.open
            .then(|| self.windows.get(self.selected_index))
            .flatten()
            .map(|window| window.address.clone())
    }

    /// Snapshot frames are supplied asynchronously by the compositor-capture worker.
    pub fn set_snapshot(
        &mut self,
        address: String,
        image: Arc<RenderImage>,
        cx: &mut Context<Self>,
    ) {
        if self.open && self.windows.iter().any(|window| window.address == address) {
            self.snapshots.insert(address, image);
            cx.notify();
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.open && index < self.windows.len() {
            self.selected_index = index;
            self.reveal_selected();
            cx.notify();
        }
    }

    fn reveal_selected(&self) {
        self.scroll_handle.scroll_to_item(self.selected_index);
    }
}

impl Render for WindowSwitcher {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().size_full().into_any_element();
        }

        let theme = self.theme;
        let panel_width = panel_width_for(self.windows.len());
        let selected_index = self.selected_index;
        let snapshots = self.snapshots.clone();
        let cards = self.windows.iter().enumerate().map(|(index, candidate)| {
            let selected = index == selected_index;
            switcher_card(
                candidate,
                snapshots.get(&candidate.address).cloned(),
                selected,
                theme,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.select(index, cx);
                }),
            )
        });

        div()
            .size_full()
            .p(px(32.0))
            .flex()
            .items_center()
            .justify_center()
            .font(ui_font())
            .child(
                div()
                    .id("window-switcher-panel")
                    .w(px(panel_width))
                    .max_w(px(PANEL_MAX_WIDTH))
                    .p(px(PANEL_PADDING))
                    .rounded(theme.panel_radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface(SurfaceRole::Floating))
                    .child(
                        div()
                            .id("window-switcher-scroll")
                            .w_full()
                            .overflow_x_scroll()
                            .track_scroll(&self.scroll_handle)
                            .flex()
                            .gap(px(CARD_GAP))
                            .children(cards),
                    ),
            )
            .into_any_element()
    }
}

/// Width needed to display the frozen card set without empty panel space.
/// A maximum retains the horizontal-scroll behavior for large window sets.
fn panel_width_for(window_count: usize) -> f32 {
    let cards = CARD_WIDTH * window_count as f32;
    let gaps = CARD_GAP * window_count.saturating_sub(1) as f32;
    (cards + gaps + PANEL_PADDING * 2.0).min(PANEL_MAX_WIDTH)
}

fn switcher_card(
    candidate: &WorkspaceWindow,
    snapshot: Option<Arc<RenderImage>>,
    selected: bool,
    theme: BarTheme,
) -> gpui::Stateful<gpui::Div> {
    let title = non_empty(candidate.title_or_initial(), "Untitled").to_string();
    let app_name = non_empty(candidate.app_name(), "Application").to_string();
    let icon = candidate.icon.clone();
    let address = candidate.address.clone();
    let card_id = stable_card_id(&address);
    let mut card = div()
        .id(("window-switcher-card", card_id))
        .w(px(CARD_WIDTH))
        .h(px(CARD_HEIGHT))
        .flex_none()
        .overflow_hidden()
        .rounded(theme.panel_radius)
        .border_1()
        .border_color(if selected {
            theme.strong_border
        } else {
            theme.border
        })
        .bg(if selected {
            theme.active_background
        } else {
            theme.container_background
        })
        .flex()
        .flex_col()
        .p(px(12.0))
        .gap(px(10.0));

    let preview = div()
        .w_full()
        .h(px(CARD_PREVIEW_HEIGHT))
        .flex_none()
        .overflow_hidden()
        .rounded(theme.control_radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface(SurfaceRole::Dialog))
        .flex()
        .items_center()
        .justify_center()
        .child(match snapshot {
            Some(snapshot) => img(snapshot)
                .size_full()
                .object_fit(gpui::ObjectFit::Contain)
                .into_any_element(),
            None => preview_fallback(icon.clone(), &app_name, theme),
        });
    card = card.child(preview);

    let identity = div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(app_icon(icon, &app_name, theme))
        .child(
            div()
                .min_w(px(0.0))
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .w_full()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(title),
                )
                .child(
                    div()
                        .w_full()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(px(11.0))
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{} · Workspace {}",
                            app_name, candidate.workspace.id
                        )),
                ),
        );
    card.child(identity)
}

fn preview_fallback(icon: Option<PathBuf>, app_name: &str, theme: BarTheme) -> gpui::AnyElement {
    match icon {
        Some(path) => img(path).size(px(64.0)).into_any_element(),
        None => div()
            .size(px(64.0))
            .rounded_full()
            .bg(theme.hover_background)
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.foreground)
            .text_size(px(28.0))
            .child(initial(app_name))
            .into_any_element(),
    }
}

fn app_icon(icon: Option<PathBuf>, app_name: &str, theme: BarTheme) -> gpui::AnyElement {
    match icon {
        Some(path) => img(path).size(px(32.0)).flex_none().into_any_element(),
        None => div()
            .size(px(32.0))
            .flex_none()
            .rounded_full()
            .bg(theme.hover_background)
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.foreground)
            .child(initial(app_name))
            .into_any_element(),
    }
}

fn initial(value: &str) -> String {
    value
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string()
}

fn non_empty<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    (!value.trim().is_empty())
        .then_some(value)
        .unwrap_or(fallback)
}

fn stable_card_id(address: &str) -> u64 {
    let text = address.trim_start_matches("0x");
    u64::from_str_radix(text, 16).unwrap_or_else(|_| {
        text.bytes().fold(0u64, |hash, byte| {
            hash.wrapping_mul(1_099_511_628_211)
                .wrapping_add(u64::from(byte))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{PANEL_MAX_WIDTH, SwitcherState, panel_width_for};
    use crate::hyprland::{WindowWorkspace, WorkspaceWindow};

    fn window(address: &str, history: i64) -> WorkspaceWindow {
        WorkspaceWindow {
            address: address.to_string(),
            app_id: String::new(),
            initial_app_id: String::new(),
            title: String::new(),
            initial_title: String::new(),
            mapped: true,
            hidden: false,
            pinned: false,
            focus_history_id: history,
            workspace: WindowWorkspace { id: 1 },
            display_name: String::new(),
            icon: None,
        }
    }

    #[test]
    fn candidates_are_mru_sorted_and_start_at_the_active_window() {
        let state = SwitcherState::new(
            vec![window("0x3", 2), window("0x2", 1), window("0x1", 1)],
            Some("0x3".to_string()),
        );
        assert_eq!(
            state
                .windows
                .iter()
                .map(|window| window.address.as_str())
                .collect::<Vec<_>>(),
            vec!["0x1", "0x2", "0x3"]
        );
        assert_eq!(state.active_index(), 2);
    }

    #[test]
    fn hidden_pinned_and_special_windows_are_excluded() {
        let mut unmapped = window("0x0", 0);
        unmapped.mapped = false;
        let mut hidden = window("0x1", 1);
        hidden.hidden = true;
        let mut pinned = window("0x2", 2);
        pinned.pinned = true;
        let mut special = window("0x3", 3);
        special.workspace.id = -99;
        assert!(
            SwitcherState::new(vec![unmapped, hidden, pinned, special], None)
                .windows
                .is_empty()
        );
    }

    #[test]
    fn panel_width_tracks_card_count_until_the_scroll_limit() {
        assert_eq!(panel_width_for(1), 288.0);
        assert_eq!(panel_width_for(4), 1080.0);
        assert_eq!(panel_width_for(5), PANEL_MAX_WIDTH);
    }
}
