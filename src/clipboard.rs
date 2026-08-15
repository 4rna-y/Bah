//! Persistent Wayland clipboard history backed by `wl-clipboard`.
//!
//! Clipboard data is copied into Bah's private data directory as soon as it is
//! observed. This is important on Wayland: the application that originally
//! owned a selection can exit at any time.

use std::{
    collections::HashSet,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use async_channel::Sender;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::ClipboardConfig;

const MANIFEST: &str = "history.json";
const ENTRY_DIRECTORY: &str = "entries";
const POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ClipboardPayload {
    pub mime_type: String,
    pub file_name: String,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ClipboardEntry {
    pub id: String,
    pub created_at: i64,
    pub payloads: Vec<ClipboardPayload>,
    pub total_size: u64,
}

impl ClipboardEntry {
    pub fn display_mime_type(&self) -> &str {
        self.payloads
            .iter()
            .find(|payload| payload.mime_type.starts_with("image/"))
            .or_else(|| {
                self.payloads
                    .iter()
                    .find(|payload| is_text(&payload.mime_type))
            })
            .or_else(|| self.payloads.first())
            .map(|payload| payload.mime_type.as_str())
            .unwrap_or("application/octet-stream")
    }

    pub fn image_payload(&self) -> Option<&ClipboardPayload> {
        self.payloads
            .iter()
            .find(|payload| payload.mime_type.starts_with("image/"))
    }

    pub fn text_payload(&self) -> Option<&ClipboardPayload> {
        self.payloads
            .iter()
            .find(|payload| is_text(&payload.mime_type))
    }

    pub fn preferred_payload(&self) -> Option<&ClipboardPayload> {
        self.payloads
            .iter()
            .find(|payload| payload.mime_type.starts_with("image/"))
            .or_else(|| {
                self.payloads
                    .iter()
                    .find(|payload| payload.mime_type == "text/html")
            })
            .or_else(|| {
                self.payloads
                    .iter()
                    .find(|payload| is_text(&payload.mime_type))
            })
            .or_else(|| self.payloads.first())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ClipboardManifest {
    entries: Vec<ClipboardEntry>,
}

/// Thread-safe history state used by the collector and the GPUI UI.
#[derive(Debug)]
pub struct ClipboardHistory {
    root: PathBuf,
    config: ClipboardConfig,
    entries: Vec<ClipboardEntry>,
}

pub type SharedClipboardHistory = Arc<Mutex<ClipboardHistory>>;

impl ClipboardHistory {
    pub fn shared(config: ClipboardConfig) -> SharedClipboardHistory {
        let root = data_path();
        let entries = load_entries(&root);
        Arc::new(Mutex::new(Self {
            root,
            config,
            entries,
        }))
    }

    pub fn entries(&self) -> &[ClipboardEntry] {
        &self.entries
    }

    pub fn bytes(&self, entry: &ClipboardEntry, payload: &ClipboardPayload) -> Result<Vec<u8>> {
        let path = self.entry_path(entry).join(&payload.file_name);
        fs::read(&path)
            .with_context(|| format!("failed to read clipboard payload {}", path.display()))
    }

    pub fn add(&mut self, payloads: Vec<(String, Vec<u8>)>) -> Result<bool> {
        let total_size = payloads
            .iter()
            .map(|(_, data)| data.len() as u64)
            .sum::<u64>();
        if payloads.is_empty() || total_size > self.config.max_entry_bytes {
            return Ok(false);
        }
        let id = content_id(&payloads);
        if self.entries.first().is_some_and(|entry| entry.id == id) {
            return Ok(false);
        }
        if let Some(index) = self.entries.iter().position(|entry| entry.id == id) {
            let entry = self.entries.remove(index);
            self.entries.insert(0, entry);
            self.save()?;
            return Ok(true);
        }

        let directory = self.entry_path_for(&id);
        let temporary = self.root.join(format!(".{id}.tmp"));
        let entries_directory = self.root.join(ENTRY_DIRECTORY);
        fs::create_dir_all(&entries_directory)?;
        set_private_directory(&self.root)?;
        set_private_directory(&entries_directory)?;
        fs::create_dir_all(&temporary)?;
        set_private_directory(&temporary)?;
        let mut stored_payloads = Vec::with_capacity(payloads.len());
        for (index, (mime_type, data)) in payloads.into_iter().enumerate() {
            let file_name = format!("{index:03}.bin");
            let file_path = temporary.join(&file_name);
            fs::write(&file_path, &data)?;
            set_private_file(&file_path)?;
            stored_payloads.push(ClipboardPayload {
                mime_type,
                file_name,
                size: data.len() as u64,
            });
        }
        if directory.exists() {
            let _ = fs::remove_dir_all(&directory);
        }
        fs::rename(&temporary, &directory)?;
        self.entries.insert(
            0,
            ClipboardEntry {
                id,
                created_at: chrono::Utc::now().timestamp(),
                payloads: stored_payloads,
                total_size,
            },
        );
        self.prune()?;
        self.save()?;
        Ok(true)
    }

    pub fn clear(&mut self) -> Result<()> {
        let root = self.root.clone();
        for entry in self.entries.drain(..) {
            let _ = fs::remove_dir_all(root.join(ENTRY_DIRECTORY).join(entry.id));
        }
        self.save()
    }

    fn prune(&mut self) -> Result<()> {
        let mut total = self
            .entries
            .iter()
            .map(|entry| entry.total_size)
            .sum::<u64>();
        while self.entries.len() > self.config.max_entries || total > self.config.max_total_bytes {
            let Some(entry) = self.entries.pop() else {
                break;
            };
            total = total.saturating_sub(entry.total_size);
            fs::remove_dir_all(self.entry_path(&entry))
                .with_context(|| "failed to prune clipboard entry")?;
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        fs::create_dir_all(self.root.join(ENTRY_DIRECTORY))?;
        set_private_directory(&self.root)?;
        set_private_directory(&self.root.join(ENTRY_DIRECTORY))?;
        let temporary = self.root.join(format!(".{MANIFEST}.tmp"));
        let contents = serde_json::to_vec_pretty(&ClipboardManifest {
            entries: self.entries.clone(),
        })?;
        fs::write(&temporary, contents)?;
        set_private_file(&temporary)?;
        fs::rename(temporary, self.root.join(MANIFEST))?;
        Ok(())
    }

    fn entry_path(&self, entry: &ClipboardEntry) -> PathBuf {
        self.entry_path_for(&entry.id)
    }

    fn entry_path_for(&self, id: &str) -> PathBuf {
        self.root.join(ENTRY_DIRECTORY).join(id)
    }
}

/// Starts the polling collector. `wl-paste` exposes the MIME offer even when
/// the source application does not cooperate with a direct protocol client.
pub fn start_collector(history: SharedClipboardHistory, updates: Sender<()>) {
    let _ = thread::Builder::new()
        .name("bah-clipboard-history".to_string())
        .spawn(move || {
            loop {
                let max_entry_bytes = {
                    history
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .config
                        .max_entry_bytes
                };
                match read_system_clipboard(max_entry_bytes) {
                    Ok(Some(payloads)) => {
                        let changed = match history
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner())
                            .add(payloads)
                        {
                            Ok(changed) => changed,
                            Err(error) => {
                                log::warn!("failed to persist clipboard history: {error:#}");
                                false
                            }
                        };
                        if changed {
                            let _ = updates.send_blocking(());
                        }
                    }
                    Ok(None) => {}
                    Err(error) => log::debug!("clipboard collection skipped: {error:#}"),
                }
                thread::sleep(POLL_INTERVAL);
            }
        });
}

/// Keeps wl-copy alive for as long as Bah owns the selection.
#[derive(Default)]
pub struct ClipboardPublisher {
    process: Option<Child>,
}

impl ClipboardPublisher {
    pub fn publish(&mut self, history: &ClipboardHistory, entry: &ClipboardEntry) -> Result<()> {
        let payload = entry
            .preferred_payload()
            .context("clipboard entry has no payload")?;
        let data = history.bytes(entry, payload)?;
        self.publish_bytes(&payload.mime_type, &data)
    }

    /// Publishes bytes that do not originate from a history entry. This is
    /// used by screenshot capture before the collector observes the new image.
    pub fn publish_bytes(&mut self, mime_type: &str, data: &[u8]) -> Result<()> {
        if let Some(mut previous) = self.process.take() {
            // `--foreground` keeps the old owner alive. Explicitly stop it
            // before replacing the selection so it cannot keep serving stale
            // data after another history item is chosen.
            let _ = previous.kill();
            let _ = previous.wait();
        }
        let mut child = Command::new("wl-copy")
            .arg("--foreground")
            .arg("--type")
            .arg(mime_type)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start wl-copy")?;
        let mut stdin = child.stdin.take().context("wl-copy did not expose stdin")?;
        stdin.write_all(data)?;
        drop(stdin);
        self.process = Some(child);
        Ok(())
    }
}

fn read_system_clipboard(max_entry_bytes: u64) -> Result<Option<Vec<(String, Vec<u8>)>>> {
    let types = Command::new("wl-paste")
        .arg("--list-types")
        .output()
        .context("wl-paste is unavailable")?;
    if !types.status.success() {
        return Ok(None);
    }
    let mime_types = String::from_utf8_lossy(&types.stdout)
        .lines()
        .map(str::trim)
        .filter(|mime| !mime.is_empty() && !is_transient_mime_type(mime))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let mime_types = normalized_mime_types(mime_types);
    if mime_types.is_empty() {
        return Ok(None);
    }
    let mut payloads = Vec::new();
    let mut total = 0u64;
    for mime_type in mime_types {
        let output = Command::new("wl-paste")
            .arg("--no-newline")
            .arg("--type")
            .arg(&mime_type)
            .output()
            .with_context(|| format!("failed to read clipboard MIME {mime_type}"))?;
        if !output.status.success() {
            continue;
        }
        total = total.saturating_add(output.stdout.len() as u64);
        if total > max_entry_bytes {
            return Ok(None);
        }
        payloads.push((mime_type, output.stdout));
    }
    Ok((!payloads.is_empty()).then_some(payloads))
}

fn load_entries(root: &Path) -> Vec<ClipboardEntry> {
    let manifest = root.join(MANIFEST);
    let Ok(contents) = fs::read(manifest) else {
        return Vec::new();
    };
    let Ok(mut stored) = serde_json::from_slice::<ClipboardManifest>(&contents) else {
        log::warn!("clipboard history manifest is invalid; starting with an empty history");
        return Vec::new();
    };
    stored.entries.retain(|entry| {
        !entry.payloads.is_empty()
            && entry.payloads.iter().all(|payload| {
                root.join(ENTRY_DIRECTORY)
                    .join(&entry.id)
                    .join(&payload.file_name)
                    .is_file()
            })
    });
    // Older Bah releases considered text/plain, UTF8_STRING, STRING, TEXT,
    // and wl-copy's transient pid/* target to be separate representations.
    // The offered targets can vary after restarting an application, which
    // created duplicate visible history items for identical text.
    let mut seen_plain_text = HashSet::new();
    stored.entries.retain(|entry| {
        let Some(payload) = entry
            .payloads
            .iter()
            .find(|payload| is_plain_text_alias(&payload.mime_type))
        else {
            return true;
        };
        if !entry.payloads.iter().all(|payload| {
            is_plain_text_alias(&payload.mime_type) || is_transient_mime_type(&payload.mime_type)
        }) {
            return true;
        }
        fs::read(
            root.join(ENTRY_DIRECTORY)
                .join(&entry.id)
                .join(&payload.file_name),
        )
        .map(|bytes| seen_plain_text.insert(bytes))
        .unwrap_or(false)
    });
    stored.entries
}

fn normalized_mime_types(mut mime_types: Vec<String>) -> Vec<String> {
    mime_types.retain(|mime| !is_transient_mime_type(mime));
    mime_types.sort();
    mime_types.dedup();
    // Cap the number of representations; applications sometimes advertise a
    // large generated target list. Keep one canonical plain-text target: the
    // remaining X11 aliases always contain the same text, not distinct data.
    mime_types.sort_by_key(|mime| mime_priority(mime));
    let mut kept_plain_text = false;
    mime_types.retain(|mime| {
        if is_plain_text_alias(mime) {
            if kept_plain_text {
                false
            } else {
                kept_plain_text = true;
                true
            }
        } else {
            true
        }
    });
    mime_types.truncate(32);
    mime_types
}

fn content_id(payloads: &[(String, Vec<u8>)]) -> String {
    let mut digest = Sha256::new();
    for (mime, bytes) in payloads {
        digest.update(mime.as_bytes());
        digest.update([0]);
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

fn mime_priority(mime: &str) -> u8 {
    if mime == "text/plain;charset=utf-8" {
        0
    } else if mime == "text/plain" {
        1
    } else if mime.starts_with("image/png") {
        2
    } else if mime.starts_with("image/") {
        3
    } else if mime == "text/uri-list" {
        4
    } else {
        10
    }
}

fn is_text(mime: &str) -> bool {
    mime.starts_with("text/") || matches!(mime, "UTF8_STRING" | "STRING" | "TEXT")
}

fn is_plain_text_alias(mime: &str) -> bool {
    matches!(
        mime,
        "text/plain" | "text/plain;charset=utf-8" | "UTF8_STRING" | "STRING" | "TEXT"
    )
}

fn is_transient_mime_type(mime: &str) -> bool {
    mime.starts_with("application/x-bah") || mime.starts_with("pid/")
}

fn data_path() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("bah/clipboard")
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ClipboardEntry, ClipboardPayload, content_id, is_text, mime_priority, normalized_mime_types,
    };

    #[test]
    fn content_identity_includes_mime_and_bytes() {
        assert_ne!(
            content_id(&[("text/plain".into(), b"same".to_vec())]),
            content_id(&[("text/html".into(), b"same".to_vec())])
        );
    }

    #[test]
    fn image_is_preferred_for_paste() {
        let entry = ClipboardEntry {
            id: "x".into(),
            created_at: 0,
            total_size: 2,
            payloads: vec![
                ClipboardPayload {
                    mime_type: "image/png".into(),
                    file_name: "0".into(),
                    size: 1,
                },
                ClipboardPayload {
                    mime_type: "text/plain".into(),
                    file_name: "1".into(),
                    size: 1,
                },
            ],
        };
        assert_eq!(entry.preferred_payload().unwrap().mime_type, "image/png");
        assert!(is_text("text/plain;charset=utf-8"));
        assert!(mime_priority("text/plain") < mime_priority("image/png"));
    }

    #[test]
    fn normalizes_equivalent_text_targets_and_discards_pid_targets() {
        assert_eq!(
            normalized_mime_types(vec![
                "pid/2413".into(),
                "UTF8_STRING".into(),
                "TEXT".into(),
                "text/plain".into(),
                "text/plain;charset=utf-8".into(),
                "image/png".into(),
            ]),
            vec!["text/plain;charset=utf-8", "image/png"]
        );
    }
}
