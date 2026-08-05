mod client;
pub mod events;
mod icons;
mod types;

use std::{io::BufRead, sync::mpsc::Sender, thread};

use log::{error, info};

pub use client::{HyprlandClient, SocketPaths};
pub use events::IpcUpdate;
pub use types::{Workspace, WorkspaceSnapshot};

/// Starts the blocking socket worker outside GPUI's drawing thread.
pub fn start_worker(sender: Sender<IpcUpdate>) {
    let spawn_result = thread::Builder::new()
        .name("hyprbar-hyprland-ipc".to_string())
        .spawn(move || run_worker(sender));
    if let Err(error) = spawn_result {
        error!("failed to start Hyprland IPC worker: {error}");
    }
}

fn run_worker(sender: Sender<IpcUpdate>) {
    let paths = match SocketPaths::from_environment() {
        Ok(paths) => paths,
        Err(error) => {
            let _ = sender.send(IpcUpdate::Unavailable(error.to_string()));
            return;
        }
    };
    info!("Hyprland command socket: {}", paths.command.display());
    info!("Hyprland event socket: {}", paths.events.display());

    let mut client = HyprlandClient::new(paths);
    match client.workspace_snapshot() {
        Ok(workspaces) => {
            info!("Hyprland IPC connected");
            if sender.send(IpcUpdate::Workspaces(workspaces)).is_err() {
                return;
            }
        }
        Err(error) => {
            let _ = sender.send(IpcUpdate::Unavailable(error.to_string()));
            return;
        }
    }

    let mut reader = match client.event_reader() {
        Ok(reader) => reader,
        Err(error) => {
            let _ = sender.send(IpcUpdate::WorkerStopped(error.to_string()));
            return;
        }
    };
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                let _ = sender.send(IpcUpdate::WorkerStopped(
                    "Hyprland event socket closed".to_string(),
                ));
                return;
            }
            Ok(_) if events::refreshes_workspaces(line.trim()) => match client.workspace_snapshot()
            {
                Ok(workspaces) => {
                    if sender.send(IpcUpdate::Workspaces(workspaces)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(IpcUpdate::WorkerStopped(error.to_string()));
                    return;
                }
            },
            Ok(_) => {}
            Err(error) => {
                let _ = sender.send(IpcUpdate::WorkerStopped(error.to_string()));
                return;
            }
        }
    }
}
