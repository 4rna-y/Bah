mod client;
pub mod events;
mod icons;
mod jump_list;
mod types;

use std::{io::BufRead, thread};

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
