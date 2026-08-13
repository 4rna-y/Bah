use std::path::PathBuf;

use serde::Deserialize;

use super::jump_list::JumpListAction;

/// Workspace data used by the UI. Individual IDs remain stable for future click dispatchers.
#[derive(Clone, Debug, Deserialize)]
pub struct Workspace {
    pub id: i32,
    pub name: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub monitor: Option<String>,
    #[serde(default)]
    pub windows: u32,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub urgent: bool,
}

impl Workspace {
    /// Keeps workspaces that contain windows and the active workspace itself.
    ///
    /// Hyprland creates an empty workspace when it is selected. Keeping the
    /// active one ensures it appears immediately, without requiring a window.
    pub fn display_set(mut workspaces: Vec<Self>, active_id: i32) -> Vec<Self> {
        for workspace in &mut workspaces {
            workspace.active = active_id == workspace.id;
            workspace.focused = workspace.active;
        }
        workspaces.retain(|workspace| workspace.windows > 0 || workspace.active);
        workspaces.sort_by_key(|workspace| workspace.id);
        workspaces
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ActiveWorkspace {
    pub id: i32,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ActiveWindow {
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "class")]
    pub app_id: String,
    #[serde(default, rename = "initialClass")]
    pub initial_app_id: String,
}

/// A single IPC refresh used by the workspace module.
#[derive(Clone, Debug)]
pub struct WorkspaceSnapshot {
    pub workspaces: Vec<Workspace>,
    pub workspace_windows: Vec<WorkspaceWindow>,
    pub monitors: Vec<super::display::Monitor>,
    pub active_window_address: Option<String>,
    pub active_window_title: Option<String>,
    pub active_window_icon: Option<PathBuf>,
    pub jump_list_actions: Vec<JumpListAction>,
}

/// A closable window, grouped by the workspace it belongs to.
#[derive(Clone, Debug, Deserialize)]
pub struct WorkspaceWindow {
    pub address: String,
    #[serde(default, rename = "class")]
    pub app_id: String,
    #[serde(default, rename = "initialClass")]
    pub initial_app_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "initialTitle")]
    pub initial_title: String,
    #[serde(default)]
    pub mapped: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, rename = "focusHistoryID")]
    pub focus_history_id: i64,
    pub workspace: WindowWorkspace,
    #[serde(skip)]
    pub display_name: String,
    #[serde(skip)]
    pub icon: Option<PathBuf>,
}

impl WorkspaceWindow {
    pub fn app_name(&self) -> &str {
        &self.display_name
    }

    /// Mirrors Altab's eligibility rule for normal, user-switchable windows.
    pub fn is_switcher_candidate(&self) -> bool {
        self.mapped && !self.hidden && !self.pinned && (1..=100).contains(&self.workspace.id)
    }

    pub fn title_or_initial(&self) -> &str {
        if self.title.trim().is_empty() {
            &self.initial_title
        } else {
            &self.title
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct WindowWorkspace {
    pub id: i32,
}

#[cfg(test)]
mod tests {
    use super::Workspace;

    fn workspace(id: i32, windows: u32) -> Workspace {
        Workspace {
            id,
            name: id.to_string(),
            monitor: None,
            windows,
            active: false,
            focused: false,
            urgent: false,
        }
    }

    #[test]
    fn display_set_keeps_populated_and_active_workspaces() {
        let displayed =
            Workspace::display_set(vec![workspace(1, 1), workspace(2, 0), workspace(3, 2)], 2);

        assert_eq!(
            displayed
                .iter()
                .map(|workspace| workspace.id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(displayed[1].active);
    }

    #[test]
    fn display_set_hides_empty_inactive_workspaces() {
        let displayed =
            Workspace::display_set(vec![workspace(1, 1), workspace(2, 0), workspace(3, 1)], 1);

        assert_eq!(
            displayed
                .iter()
                .map(|workspace| workspace.id)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
    }
}
