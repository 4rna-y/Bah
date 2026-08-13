use std::path::PathBuf;

use gpui::{
    Animation, AnimationExt as _, App, Div, FontWeight, MouseButton, MouseDownEvent, Window, div,
    ease_in_out, img, prelude::*, px,
};

use crate::{
    hyprland::{Workspace, WorkspaceSnapshot, WorkspaceWindow},
    theme::BarTheme,
};

/// Renders workspace state received from Hyprland IPC.
pub struct Workspaces {
    items: Vec<Workspace>,
    workspace_windows: Vec<WorkspaceWindow>,
    active_window_title: Option<String>,
    active_window_icon: Option<PathBuf>,
    jump_list_actions: Vec<crate::hyprland::JumpListAction>,
    active_window_address: Option<String>,
    monitors: Vec<crate::hyprland::display::Monitor>,
    active_workspace_id: Option<i32>,
    slide_offset: f32,
    transition_id: u32,
}

impl Workspaces {
    pub fn new(snapshot: WorkspaceSnapshot) -> Self {
        let active_workspace_id = active_workspace_id(&snapshot.workspaces);
        Self {
            items: snapshot.workspaces,
            workspace_windows: snapshot.workspace_windows,
            active_window_title: snapshot.active_window_title,
            active_window_icon: snapshot.active_window_icon,
            jump_list_actions: snapshot.jump_list_actions,
            active_window_address: snapshot.active_window_address,
            monitors: snapshot.monitors,
            active_workspace_id,
            slide_offset: 0.0,
            transition_id: 0,
        }
    }

    /// Replaces the IPC state and reports whether the active workspace changed.
    pub fn replace(&mut self, snapshot: WorkspaceSnapshot, theme: BarTheme) -> bool {
        let next_active_workspace_id = active_workspace_id(&snapshot.workspaces);
        let active_workspace_changed = self.active_workspace_id != next_active_workspace_id;
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
        self.workspace_windows = snapshot.workspace_windows;
        self.active_window_title = snapshot.active_window_title;
        self.active_window_icon = snapshot.active_window_icon;
        self.jump_list_actions = snapshot.jump_list_actions;
        self.active_window_address = snapshot.active_window_address;
        self.monitors = snapshot.monitors;
        self.active_workspace_id = next_active_workspace_id;
        active_workspace_changed
    }

    pub fn jump_list_actions(&self) -> &[crate::hyprland::JumpListAction] {
        &self.jump_list_actions
    }

    pub fn windows_for_workspace(&self, workspace_id: i32) -> Vec<WorkspaceWindow> {
        self.workspace_windows
            .iter()
            .filter(|window| window.workspace.id == workspace_id)
            .cloned()
            .collect()
    }

    /// Returns the immutable IPC state needed when a transient window switcher opens.
    pub fn switcher_state(&self) -> (Vec<WorkspaceWindow>, Option<String>, Option<String>) {
        let focused_monitor = self
            .monitors
            .iter()
            .find(|monitor| monitor.focused)
            .map(|monitor| monitor.name.clone());
        (
            self.workspace_windows.clone(),
            self.active_window_address.clone(),
            focused_monitor,
        )
    }

    pub fn render(
        &self,
        theme: BarTheme,
        on_workspace_mouse_down: impl Fn(i32) -> WorkspaceMouseDownHandler,
        mut on_active_workspace_right_click: Option<WorkspaceMouseDownHandler>,
        on_inactive_workspace_right_click: impl Fn(i32) -> WorkspaceMouseDownHandler,
    ) -> Div {
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
                    })
                    .on_mouse_down(MouseButton::Left, on_workspace_mouse_down(workspace.id));

                if let Some(handler) = on_active_workspace_right_click.take() {
                    active_item = active_item.on_mouse_down(MouseButton::Right, handler);
                }

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
                                .with_easing(ease_in_out),
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
                        .on_mouse_down(MouseButton::Left, on_workspace_mouse_down(workspace.id))
                        .on_mouse_down(
                            MouseButton::Right,
                            on_inactive_workspace_right_click(workspace.id),
                        )
                        .child(label),
                );
            }
        }
        row
    }
}

pub type WorkspaceMouseDownHandler = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

fn active_workspace_id(workspaces: &[Workspace]) -> Option<i32> {
    workspaces
        .iter()
        .find(|workspace| workspace.active)
        .map(|workspace| workspace.id)
}
