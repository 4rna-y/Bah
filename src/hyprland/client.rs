use std::{
    env, fs,
    io::{BufReader, Read, Write},
    net::Shutdown,
    os::unix::{fs::FileTypeExt, net::UnixStream},
    path::PathBuf,
};

use anyhow::{Context, Result, bail};
use log::debug;

use super::{
    icons::AppIconResolver,
    jump_list::JumpListAction,
    types::{ActiveWindow, ActiveWorkspace, Workspace, WorkspaceSnapshot, WorkspaceWindow},
};

/// Paths for Hyprland's command and event Unix sockets.
#[derive(Clone, Debug)]
pub struct SocketPaths {
    pub command: PathBuf,
    pub events: PathBuf,
}

impl SocketPaths {
    pub fn from_environment() -> Result<Self> {
        let runtime = env::var_os("XDG_RUNTIME_DIR")
            .context("XDG_RUNTIME_DIR is not set; start bah inside Hyprland")?;
        let signature = env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .context("HYPRLAND_INSTANCE_SIGNATURE is not set; start bah inside Hyprland")?;
        let base = PathBuf::from(runtime).join("hypr").join(signature);
        let paths = Self {
            command: base.join(".socket.sock"),
            events: base.join(".socket2.sock"),
        };
        for path in [&paths.command, &paths.events] {
            if !path.exists() {
                bail!("Hyprland socket does not exist: {}", path.display());
            }
            if !fs::metadata(path)?.file_type().is_socket() {
                bail!(
                    "Hyprland socket path is not a Unix socket: {}",
                    path.display()
                );
            }
        }
        Ok(paths)
    }
}

/// Thin direct client for Hyprland's command socket.
#[derive(Clone, Debug)]
pub struct HyprlandClient {
    paths: SocketPaths,
    icon_resolver: AppIconResolver,
}

impl HyprlandClient {
    pub fn new(paths: SocketPaths) -> Self {
        Self {
            paths,
            icon_resolver: AppIconResolver::new(),
        }
    }

    pub fn workspace_snapshot(&mut self) -> Result<WorkspaceSnapshot> {
        let workspaces: Vec<Workspace> = serde_json::from_str(&self.command("j/workspaces")?)
            .context("Hyprland returned invalid workspace JSON")?;
        let mut workspace_windows: Vec<WorkspaceWindow> =
            serde_json::from_str(&self.command("j/clients")?)
                .context("Hyprland returned invalid client JSON")?;
        let active: ActiveWorkspace = serde_json::from_str(&self.command("j/activeworkspace")?)
            .context("Hyprland returned invalid active-workspace JSON")?;
        let active_window: ActiveWindow = serde_json::from_str(&self.command("j/activewindow")?)
            .context("Hyprland returned invalid active-window JSON")?;
        let active_window_title =
            (!active_window.title.trim().is_empty()).then_some(active_window.title);
        let active_window_icon = self
            .icon_resolver
            .resolve(&active_window.app_id, &active_window.initial_app_id);
        let jump_list_actions =
            JumpListAction::resolve(&active_window.app_id, &active_window.initial_app_id);
        for workspace_window in &mut workspace_windows {
            workspace_window.display_name = self
                .icon_resolver
                .display_name(&workspace_window.app_id, &workspace_window.initial_app_id);
            workspace_window.icon = self
                .icon_resolver
                .resolve(&workspace_window.app_id, &workspace_window.initial_app_id);
            if workspace_window.display_name == "Unknown application"
                && !workspace_window.title.trim().is_empty()
            {
                workspace_window.display_name = workspace_window.title.clone();
            }
        }
        debug!(
            "active window icon: app_id={:?}, resolved={}",
            active_window.app_id,
            active_window_icon
                .as_ref()
                .map_or_else(|| "none".to_string(), |path| path.display().to_string())
        );

        Ok(WorkspaceSnapshot {
            workspaces: Workspace::display_set(workspaces, active.id),
            workspace_windows,
            active_window_title,
            active_window_icon,
            jump_list_actions,
        })
    }

    pub fn event_reader(&self) -> Result<BufReader<UnixStream>> {
        let stream = UnixStream::connect(&self.paths.events).with_context(|| {
            format!(
                "failed to connect event socket {}",
                self.paths.events.display()
            )
        })?;
        Ok(BufReader::new(stream))
    }

    fn command(&self, command: &str) -> Result<String> {
        let mut stream = UnixStream::connect(&self.paths.command).with_context(|| {
            format!(
                "failed to connect command socket {}",
                self.paths.command.display()
            )
        })?;
        stream
            .write_all(command.as_bytes())
            .with_context(|| format!("failed to write Hyprland command {command}"))?;
        stream.shutdown(Shutdown::Write)?;
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .with_context(|| format!("failed to read Hyprland response for {command}"))?;
        Ok(response)
    }

    /// Sends a Hyprland dispatcher command through the command socket.
    pub fn dispatch(&self, dispatcher: &str) -> Result<()> {
        // Hyprland 0.55 evaluates command-socket dispatchers as Lua. The
        // argument is consequently a dispatcher expression such as
        // `hl.dsp.focus({ workspace = 3 })`.
        let response = self.command(&format!("dispatch {dispatcher}"))?;
        if response.trim() != "ok" {
            bail!(
                "Hyprland rejected dispatcher {dispatcher:?}: {}",
                response.trim()
            );
        }
        Ok(())
    }
}
