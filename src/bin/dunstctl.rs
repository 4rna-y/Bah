//! Compatibility entry point for existing dunstctl keybindings.
//!
//! The implementation lives in `bah notifications`; keeping this tiny wrapper
//! makes the migration explicit while preserving the familiar command name.

use std::os::unix::process::CommandExt;

fn main() {
    let executable = std::env::var_os("BAH_BIN").unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.parent()
                    .map(|parent| parent.join("bah").into_os_string())
            })
            .unwrap_or_else(|| "bah".into())
    });
    let error = std::process::Command::new(executable)
        .arg("notifications")
        .args(std::env::args_os().skip(1))
        .exec();
    eprintln!("dunstctl: failed to launch bah notifications: {error}");
    std::process::exit(127);
}
