use std::sync::{Arc, Mutex};

use gpui::{
    Context, MouseButton, Render, RenderImage, StatefulInteractiveElement, Window, div, img,
    prelude::*, px,
};
use image::{Frame, GenericImageView};

use crate::{
    clipboard::{ClipboardEntry, ClipboardPublisher, SharedClipboardHistory},
    hyprland::{paste_into_window, set_keybind_submap},
    theme::{BarTheme, SurfaceRole, ui_font},
};

const PANEL_WIDTH_RATIO: f32 = 0.35;
const PANEL_HEIGHT_RATIO: f32 = 0.50;

/// Full-output overlay containing the transient clipboard history panel.
pub struct ClipboardPanel {
    history: SharedClipboardHistory,
    publisher: Arc<Mutex<ClipboardPublisher>>,
    theme: BarTheme,
    open: bool,
    selected_index: usize,
    target_window: Option<String>,
}

impl ClipboardPanel {
    pub fn new(
        history: SharedClipboardHistory,
        publisher: Arc<Mutex<ClipboardPublisher>>,
        theme: BarTheme,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            history,
            publisher,
            theme,
            open: false,
            selected_index: 0,
            target_window: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(
        &mut self,
        target_window: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open = true;
        self.selected_index = 0;
        self.target_window = target_window;
        window.set_input_region(None);
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The panel deliberately has no keyboard focus. Restore the normal
        // Hyprland keymap when it closes via a mouse click or a CLI command.
        set_keybind_submap("reset");
        self.open = false;
        self.target_window = None;
        window.set_input_region(Some(&[]));
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.selected_index = self
            .selected_index
            .min(self.entries().len().saturating_sub(1));
        cx.notify();
    }

    pub fn select_previous(&mut self, cx: &mut Context<Self>) {
        let len = self.entries().len();
        if self.open && len > 0 {
            self.selected_index = (self.selected_index + len - 1) % len;
            cx.notify();
        }
    }

    pub fn select_next(&mut self, cx: &mut Context<Self>) {
        let len = self.entries().len();
        if self.open && len > 0 {
            self.selected_index = (self.selected_index + 1) % len;
            cx.notify();
        }
    }

    pub fn choose_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.choose(self.selected_index, window, cx);
        }
    }

    fn entries(&self) -> Vec<ClipboardEntry> {
        self.history
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entries()
            .to_vec()
    }

    fn choose(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entries().get(index).cloned() else {
            return;
        };
        let published = {
            let history = self
                .history
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            self.publisher
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .publish(&history, &entry)
        };
        match published {
            Ok(()) => {
                if let Some(address) = self.target_window.clone() {
                    paste_into_window(address);
                }
                self.close(window, cx);
            }
            Err(error) => log::warn!("failed to set clipboard selection: {error:#}"),
        }
    }
}

impl Render for ClipboardPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().size_full().into_any_element();
        }
        let size = window.viewport_size();
        let panel_width = size.width * PANEL_WIDTH_RATIO;
        let panel_height = size.height * PANEL_HEIGHT_RATIO;
        let entries = self.entries();
        let theme = self.theme;
        let selected = self.selected_index;
        let rows = entries.iter().enumerate().map(|(index, entry)| {
            clipboard_row(entry, index == selected, theme, &self.history, panel_width)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| this.choose(index, window, cx)),
                )
        });
        div()
            .size_full()
            .font(ui_font())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close(window, cx)),
            )
            .child(
                div()
                    .id("clipboard-panel-position")
                    .w_full()
                    .pt(theme.bar_height)
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .id("clipboard-panel")
                            .w(panel_width)
                            .h(panel_height)
                            .overflow_hidden()
                            .rounded(theme.panel_radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.surface(SurfaceRole::Floating))
                            .flex()
                            .flex_col()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_, _, _, cx| cx.stop_propagation()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .px(px(16.0))
                                    .py(px(12.0))
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .text_color(theme.foreground)
                                    .child("Clipboard"),
                            )
                            .child(
                                div()
                                    .id("clipboard-list")
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .overflow_y_scroll()
                                    .p(px(10.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(6.0))
                                    .when(entries.is_empty(), |list| {
                                        list.child(
                                            div()
                                                .p(px(12.0))
                                                .text_color(theme.muted_foreground)
                                                .child("Clipboard history is empty"),
                                        )
                                    })
                                    .children(rows),
                            ),
                    ),
            )
            .into_any_element()
    }
}

fn clipboard_row(
    entry: &ClipboardEntry,
    selected: bool,
    theme: BarTheme,
    history: &SharedClipboardHistory,
    panel_width: gpui::Pixels,
) -> gpui::Stateful<gpui::Div> {
    let mut row = div()
        .id(("clipboard-row", stable_id(&entry.id)))
        .w_full()
        .min_h(px(44.0))
        .px(px(12.0))
        .py(px(10.0))
        .rounded(theme.control_radius)
        .bg(if selected {
            theme.active_background
        } else {
            theme.container_background
        })
        .text_color(if selected {
            theme.foreground
        } else {
            theme.muted_foreground
        })
        .overflow_hidden();
    if let Some(text) = entry.text_payload() {
        let preview = history
            .lock()
            .ok()
            .and_then(|history| history.bytes(entry, text).ok())
            .map(|bytes| text_preview(&bytes))
            .unwrap_or_else(|| "<unable to read text>".into());
        row = row.child(
            div()
                .w_full()
                .overflow_hidden()
                .text_ellipsis()
                .child(preview),
        );
    } else if let Some(image_payload) = entry.image_payload() {
        let image = history
            .lock()
            .ok()
            .and_then(|history| history.bytes(entry, image_payload).ok())
            .and_then(|bytes| image_preview(&bytes));
        if let Some((render_image, width, height)) = image {
            let content_height = image_display_height(panel_width, width, height);
            row = row.child(
                img(render_image)
                    .w_full()
                    .h(content_height)
                    .object_fit(gpui::ObjectFit::Contain),
            );
        } else {
            row = row.child(format!(
                "{}  {} bytes",
                image_payload.mime_type, image_payload.size
            ));
        }
    } else {
        row = row.child(format!(
            "{}  {} bytes",
            entry.display_mime_type(),
            entry.total_size
        ));
    }
    row
}

fn image_display_height(panel_width: gpui::Pixels, width: u32, height: u32) -> gpui::Pixels {
    (panel_width - px(44.0)).max(px(1.0)) * height as f32 / width.max(1) as f32
}

fn text_preview(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn image_preview(bytes: &[u8]) -> Option<(Arc<RenderImage>, u32, u32)> {
    let image = image::load_from_memory(bytes).ok()?;
    let (width, height) = image.dimensions();
    Some((
        Arc::new(RenderImage::new(vec![Frame::new(image.to_rgba8())])),
        width,
        height,
    ))
}

fn stable_id(value: &str) -> u64 {
    u64::from_str_radix(&value[..value.len().min(16)], 16).unwrap_or_else(|_| {
        value.bytes().fold(0, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(byte as u64)
        })
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn panel_ratios_match_the_product_specification() {
        assert_eq!(super::PANEL_WIDTH_RATIO, 0.35);
        assert_eq!(super::PANEL_HEIGHT_RATIO, 0.50);
    }

    #[test]
    fn image_height_preserves_aspect_ratio_at_list_width() {
        assert_eq!(
            f32::from(super::image_display_height(gpui::px(400.0), 200, 100)),
            178.0
        );
    }
}
