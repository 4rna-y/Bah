mod client;
pub mod events;
mod icons;
mod jump_list;
mod types;

use std::{io::BufRead, thread, time::Duration};

use async_channel::Sender;

use log::{error, info, warn};

pub use client::{HyprlandClient, SocketPaths};
pub use events::IpcUpdate;
pub use jump_list::JumpListAction;
pub use types::{Workspace, WorkspaceSnapshot, WorkspaceWindow};

/// Requests a workspace change without blocking GPUI's drawing thread.
pub fn switch_to_workspace(id: i32) {
    let _ = thread::Builder::new()
        .name("bah-workspace-switch".to_string())
        .spawn(move || {
            let dispatcher = format!("hl.dsp.focus({{ workspace = {id} }})");
            match SocketPaths::from_environment()
                .and_then(|paths| HyprlandClient::new(paths).dispatch(&dispatcher))
            {
                Ok(()) => info!("switched to workspace {id}"),
                Err(error) => warn!("failed to switch to workspace {id}: {error}"),
            }
        });
}

/// Closes a window by its Hyprland address without blocking GPUI's drawing thread.
pub fn close_window(address: String) {
    let _ = thread::Builder::new()
        .name("bah-window-close".to_string())
        .spawn(move || {
            let window_selector = format!("address:{address}");
            let dispatcher = format!("hl.dsp.window.close({{ window = {window_selector:?} }})");
            match SocketPaths::from_environment()
                .and_then(|paths| HyprlandClient::new(paths).dispatch(&dispatcher))
            {
                Ok(()) => info!("closed window {address}"),
                Err(error) => warn!("failed to close window {address}: {error}"),
            }
        });
}

/// Floats the newly mapped window belonging to this process without relying on
/// a Hyprland window rule. The compositor remains authoritative; this only
/// uses Hyprland's own dispatcher once the uniquely identified client exists.
pub fn force_float_window_for_process(app_id: &'static str, pid: u32) {
    let _ = thread::Builder::new()
        .name("bah-device-control-center-float".to_string())
        .spawn(move || {
            let paths = match SocketPaths::from_environment() {
                Ok(paths) => paths,
                Err(error) => {
                    warn!("cannot float {app_id}: {error}");
                    return;
                }
            };
            let client = HyprlandClient::new(paths);
            for _ in 0..40 {
                match client.client_address_for(pid, app_id) {
                    Ok(Some(address)) => {
                        let selector = format!("address:{address}");
                        let dispatcher = format!(
                            "hl.dsp.window.float({{ action = \"set\", window = {selector:?} }})"
                        );
                        match client.dispatch(&dispatcher).and_then(|()| {
                            let resize = format!(
                                "hl.dsp.window.resize({{ x = 900, y = 650, window = {selector:?} }})"
                            );
                            client.dispatch(&resize)
                        }).and_then(|()| {
                            let center =
                                format!("hl.dsp.window.center({{ window = {selector:?} }})");
                            client.dispatch(&center)
                        }) {
                            Ok(()) => {
                                info!("floated, resized and centered {app_id} window {address}")
                            }
                            Err(error) => {
                                warn!(
                                    "failed to float, resize and center {app_id} window {address}: {error}"
                                )
                            }
                        }
                        return;
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(50)),
                    Err(error) => {
                        warn!("failed to locate {app_id} window: {error}");
                        return;
                    }
                }
            }
            warn!("timed out waiting for {app_id} window for pid {pid}");
        });
}

/// Launches an action declared by the active app's Desktop Entry.
pub fn launch_jump_list_action(action: JumpListAction) {
    jump_list::launch(action);
}

/// Starts the blocking socket worker outside GPUI's drawing thread.
pub fn start_worker(sender: Sender<IpcUpdate>) {
    let spawn_result = thread::Builder::new()
        .name("bah-hyprland-ipc".to_string())
        .spawn(move || run_worker(sender));
    if let Err(error) = spawn_result {
        error!("failed to start Hyprland IPC worker: {error}");
    }
}

fn run_worker(sender: Sender<IpcUpdate>) {
    let paths = match SocketPaths::from_environment() {
        Ok(paths) => paths,
        Err(error) => {
            let _ = sender.send_blocking(IpcUpdate::Unavailable(error.to_string()));
            return;
        }
    };
    info!("Hyprland command socket: {}", paths.command.display());
    info!("Hyprland event socket: {}", paths.events.display());

    let mut client = HyprlandClient::new(paths);
    match client.workspace_snapshot() {
        Ok(workspaces) => {
            info!("Hyprland IPC connected");
            if sender
                .send_blocking(IpcUpdate::Workspaces(workspaces))
                .is_err()
            {
                return;
            }
        }
        Err(error) => {
            let _ = sender.send_blocking(IpcUpdate::Unavailable(error.to_string()));
            return;
        }
    }

    let mut reader = match client.event_reader() {
        Ok(reader) => reader,
        Err(error) => {
            let _ = sender.send_blocking(IpcUpdate::WorkerStopped(error.to_string()));
            return;
        }
    };
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                let _ = sender.send_blocking(IpcUpdate::WorkerStopped(
                    "Hyprland event socket closed".to_string(),
                ));
                return;
            }
            Ok(_) if events::refreshes_workspaces(line.trim()) => match client.workspace_snapshot()
            {
                Ok(workspaces) => {
                    if sender
                        .send_blocking(IpcUpdate::Workspaces(workspaces))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send_blocking(IpcUpdate::WorkerStopped(error.to_string()));
                    return;
                }
            },
            Ok(_) => {}
            Err(error) => {
                let _ = sender.send_blocking(IpcUpdate::WorkerStopped(error.to_string()));
                return;
            }
        }
    }
}
