use super::types::WorkspaceSnapshot;

/// Updates sent from the socket worker to the GPUI entity.
#[derive(Debug)]
pub enum IpcUpdate {
    Workspaces(WorkspaceSnapshot),
    Unavailable(String),
    WorkerStopped(String),
}

/// Socket2 events that require refreshing workspace state from the command socket.
pub fn refreshes_workspaces(line: &str) -> bool {
    matches!(
        line.split_once(">>").map(|(event, _)| event),
        Some("workspace")
            | Some("workspacev2")
            | Some("focusedmon")
            | Some("focusedmonv2")
            | Some("createworkspace")
            | Some("createworkspacev2")
            | Some("destroyworkspace")
            | Some("destroyworkspacev2")
            | Some("renameworkspace")
            | Some("activewindow")
            | Some("activewindowv2")
            | Some("openwindow")
            | Some("closewindow")
            | Some("movewindow")
            | Some("movewindowv2")
            | Some("changefloatingmode")
            | Some("fullscreen")
            | Some("windowtitle")
            | Some("windowtitlev2")
    )
}
