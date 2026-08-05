use std::path::PathBuf;

use gpui::{
    Animation, AnimationExt as _, Div, FontWeight, div, ease_out_quint, img, prelude::*, px,
};

use crate::{
    hyprland::{Workspace, WorkspaceSnapshot},
    theme::BarTheme,
};

/// Renders workspace state received from Hyprland IPC.
pub struct Workspaces {
    items: Vec<Workspace>,
    active_window_title: Option<String>,
    active_window_icon: Option<PathBuf>,
    active_workspace_id: Option<i32>,
    slide_offset: f32,
    transition_id: u32,
}

impl Workspaces {
    pub fn new(snapshot: WorkspaceSnapshot) -> Self {
        let active_workspace_id = active_workspace_id(&snapshot.workspaces);
        Self {
            items: snapshot.workspaces,
            active_window_title: snapshot.active_window_title,
            active_window_icon: snapshot.active_window_icon,
            active_workspace_id,
            slide_offset: 0.0,
            transition_id: 0,
        }
    }

    pub fn replace(&mut self, snapshot: WorkspaceSnapshot, theme: BarTheme) {
        let next_active_workspace_id = active_workspace_id(&snapshot.workspaces);
        if let (Some(previous), Some(next)) = (self.active_workspace_id, next_active_workspace_id)
            && previous != next
        {
            // A workspace to the right enters from the left, and vice versa.
            self.slide_offset = if next > previous {
                -theme.active_workspace_slide_distance
            } else {
                theme.active_workspace_slide_distance
            };
            self.transition_id = self.transition_id.wrapping_add(1);
        }
        self.items = snapshot.workspaces;
        self.active_window_title = snapshot.active_window_title;
        self.active_window_icon = snapshot.active_window_icon;
        self.active_workspace_id = next_active_workspace_id;
    }

    pub fn render(&self, theme: BarTheme) -> Div {
        let mut row = div().flex().items_center().gap(theme.workspace_gap);
        for workspace in &self.items {
            let label = workspace.name.clone();
            if workspace.active {
                let mut active_item = div()
                    .id(("workspace", workspace.id as u32))
                    .flex()
                    .items_center()
                    .gap(theme.workspace_gap)
                    .px(theme.workspace_horizontal_padding)
                    .py(theme.workspace_vertical_padding)
                    .rounded(theme.active_workspace_radius)
                    .text_size(theme.workspace_font_size)
                    .text_color(theme.foreground)
                    .bg(theme.active_background)
                    .font_weight(FontWeight::BOLD)
                    .when(workspace.urgent, |element| {
                        element.bg(theme.urgent_background)
                    });

                if let Some(icon) = &self.active_window_icon {
                    active_item = active_item.child(
                        img(icon.clone())
                            .size(theme.active_window_icon_size)
                            .flex_none(),
                    );
                } else {
                    active_item = active_item.child(label);
                }

                if let Some(title) = &self.active_window_title {
                    active_item = active_item.child(
                        div()
                            .id(("active-window-title", workspace.id as u32))
                            .max_w(theme.active_window_title_max_width)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(title.clone()),
                    );
                }
                if self.slide_offset == 0.0 {
                    row = row.child(active_item);
                } else {
                    let offset = self.slide_offset;
                    row = row.child(
                        active_item.with_animation(
                            ("active-workspace-slide", self.transition_id),
                            Animation::new(theme.active_workspace_slide_duration)
                                .with_easing(ease_out_quint()),
                            move |element, delta| {
                                element.relative().left(px(offset * (1.0 - delta)))
                            },
                        ),
                    );
                }
            } else {
                row = row.child(
                    div()
                        .id(("workspace", workspace.id as u32))
                        .px(theme.workspace_horizontal_padding)
                        .py(theme.workspace_vertical_padding)
                        .rounded(theme.inactive_workspace_radius)
                        .text_size(theme.workspace_font_size)
                        .text_color(theme.muted_foreground)
                        .when(workspace.urgent, |element| {
                            element.bg(theme.urgent_background)
                        })
                        .child(label),
                );
            }
        }
        row
    }
}

fn active_workspace_id(workspaces: &[Workspace]) -> Option<i32> {
    workspaces
        .iter()
        .find(|workspace| workspace.active)
        .map(|workspace| workspace.id)
}
