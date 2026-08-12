use std::{
    collections::{BTreeMap, VecDeque},
    env, fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use async_channel::Sender;
use log::{info, warn};
use zbus::{blocking::connection::Builder, interface, zvariant::OwnedValue};

use crate::config::{NotificationConfig, NotificationRuleConfig};

pub const NOTIFICATION_BUS_NAME: &str = "org.freedesktop.Notifications";
pub const NOTIFICATION_OBJECT_PATH: &str = "/org/freedesktop/Notifications";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

impl Urgency {
    fn from_hint(hints: &std::collections::HashMap<String, OwnedValue>) -> Self {
        let Some(value) = hints.get("urgency") else {
            return Self::Normal;
        };
        let value = u8::try_from(value.clone()).unwrap_or(1);
        match value {
            0 => Self::Low,
            2.. => Self::Critical,
            _ => Self::Normal,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::Critical => "critical",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotificationAction {
    pub key: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub app_icon: String,
    pub category: String,
    pub desktop_entry: String,
    pub actions: Vec<NotificationAction>,
    pub urgency: Urgency,
    pub progress: Option<u8>,
    pub stack_tag: String,
    pub transient: bool,
    pub duplicate_count: u32,
    pub history_ignore: bool,
    pub skip_popup: bool,
    pub override_pause_level: u8,
    pub expires_at: Option<Instant>,
}

impl Notification {
    fn from_request(request: NotificationRequest, id: u32, config: &NotificationConfig) -> Self {
        let urgency = Urgency::from_hint(&request.hints);
        let category = string_hint(&request.hints, "category");
        let desktop_entry = string_hint(&request.hints, "desktop-entry");
        let app_icon = resolve_notification_icon(
            if request.app_icon.is_empty() {
                string_hint(&request.hints, "image-path")
            } else {
                request.app_icon.clone()
            },
            &desktop_entry,
            &request.app_name,
        );
        let mut notification = Self {
            id,
            app_name: request.app_name,
            summary: request.summary,
            body: request.body,
            app_icon,
            category,
            desktop_entry,
            actions: request
                .actions
                .chunks_exact(2)
                .filter(|pair| !is_suppress_action(&pair[0], &pair[1]))
                .map(|pair| NotificationAction {
                    key: pair[0].clone(),
                    label: pair[1].clone(),
                })
                .collect(),
            urgency,
            progress: progress_hint(&request.hints),
            stack_tag: stack_tag_hint(&request.hints),
            transient: bool_hint(&request.hints, "transient"),
            duplicate_count: 1,
            history_ignore: false,
            skip_popup: false,
            override_pause_level: default_override_pause_level(urgency),
            expires_at: timeout_from_request(request.expire_timeout, urgency, config),
        };
        apply_rules(&mut notification, &config.rules, config);
        notification
    }

    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }
}

#[derive(Clone, Debug)]
struct NotificationRequest {
    app_name: String,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: std::collections::HashMap<String, OwnedValue>,
    expire_timeout: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseReason {
    Expired = 1,
    DismissedByUser = 2,
    ClosedByClient = 3,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationEvent {
    Upsert(Notification),
    Close(u32),
    Remove(u32),
    ClearAll,
    Refresh,
}

/// Shared daemon state. Displayed notifications are transient popups; the
/// tray reads displayed, queued, and retained history from this same store.
pub struct NotificationStore {
    displayed: BTreeMap<u32, Notification>,
    waiting: VecDeque<Notification>,
    history: VecDeque<Notification>,
    config: NotificationConfig,
    pause_level: u8,
}

pub type SharedNotificationStore = Arc<Mutex<NotificationStore>>;

impl Default for NotificationStore {
    fn default() -> Self {
        Self::new(NotificationConfig::default())
    }
}

impl NotificationStore {
    pub fn new(config: NotificationConfig) -> Self {
        let pause_level = config.pause_level;
        Self {
            displayed: BTreeMap::new(),
            waiting: VecDeque::new(),
            history: VecDeque::new(),
            config,
            pause_level,
        }
    }

    pub fn shared(config: NotificationConfig) -> SharedNotificationStore {
        Arc::new(Mutex::new(Self::new(config)))
    }

    pub fn apply(&mut self, event: NotificationEvent) {
        match event {
            NotificationEvent::Upsert(notification) => self.upsert(notification),
            NotificationEvent::Close(id) => {
                self.close(id, CloseReason::DismissedByUser);
            }
            NotificationEvent::Remove(id) => {
                self.remove(id);
            }
            NotificationEvent::ClearAll => self.clear_all(),
            NotificationEvent::Refresh => {
                self.expire(Instant::now());
            }
        }
    }

    pub fn upsert(&mut self, mut notification: Notification) {
        self.expire(Instant::now());
        if let Some(existing) = self.displayed.get_mut(&notification.id) {
            notification.duplicate_count = existing.duplicate_count;
            *existing = notification;
            return;
        }
        if !notification.stack_tag.is_empty() {
            if let Some(id) = self.displayed.iter().find_map(|(id, existing)| {
                (existing.app_name == notification.app_name
                    && existing.stack_tag == notification.stack_tag)
                    .then_some(*id)
            }) {
                notification.id = id;
                self.displayed.insert(id, notification);
                return;
            }
        }
        if let Some(existing) = self.displayed.values_mut().find(|existing| {
            existing.app_name == notification.app_name
                && existing.summary == notification.summary
                && existing.body == notification.body
                && existing.app_icon == notification.app_icon
                && existing.urgency == notification.urgency
        }) {
            existing.duplicate_count = existing.duplicate_count.saturating_add(1);
            existing.expires_at = notification.expires_at;
            return;
        }
        if notification.skip_popup || notification.override_pause_level < self.pause_level {
            self.waiting.push_back(notification);
        } else if self.displayed.len() < self.config.notification_limit {
            self.displayed.insert(notification.id, notification);
        } else {
            self.waiting.push_back(notification);
        }
    }

    pub fn close(&mut self, id: u32, _reason: CloseReason) -> Option<Notification> {
        let notification = self.displayed.remove(&id).or_else(|| {
            self.waiting
                .iter()
                .position(|notification| notification.id == id)
                .and_then(|index| self.waiting.remove(index))
        })?;
        self.push_history(notification.clone());
        self.promote_waiting();
        Some(notification)
    }

    pub fn clear(&mut self) {
        let displayed = std::mem::take(&mut self.displayed);
        for notification in displayed.into_values() {
            self.push_history(notification);
        }
        self.promote_waiting();
    }

    /// Removes an entry from every presentation tier without retaining it in
    /// history. This is the tray's explicit delete action.
    pub fn remove(&mut self, id: u32) -> bool {
        if self.displayed.remove(&id).is_some() {
            self.promote_waiting();
            return true;
        }
        if let Some(index) = self
            .waiting
            .iter()
            .position(|notification| notification.id == id)
        {
            self.waiting.remove(index);
            return true;
        }
        self.remove_history(id)
    }

    /// Clears the notification centre, including already retained history.
    pub fn clear_all(&mut self) {
        self.displayed.clear();
        self.waiting.clear();
        self.history.clear();
    }

    pub fn expire(&mut self, now: Instant) -> Vec<u32> {
        let expired = self
            .displayed
            .iter()
            .filter_map(|(id, notification)| notification.is_expired(now).then_some(*id))
            .collect::<Vec<_>>();
        for id in &expired {
            let _ = self.close(*id, CloseReason::Expired);
        }
        expired
    }

    fn promote_waiting(&mut self) {
        while self.displayed.len() < self.config.notification_limit {
            let Some(index) = self.waiting.iter().position(|notification| {
                !notification.skip_popup && notification.override_pause_level >= self.pause_level
            }) else {
                break;
            };
            let notification = self
                .waiting
                .remove(index)
                .expect("index comes from waiting");
            self.displayed.insert(notification.id, notification);
        }
    }

    fn push_history(&mut self, notification: Notification) {
        if notification.history_ignore || self.config.history_length == 0 {
            return;
        }
        self.history.retain(|entry| entry.id != notification.id);
        self.history.push_front(notification);
        self.history.truncate(self.config.history_length);
    }

    pub fn set_pause_level(&mut self, level: u8) {
        self.pause_level = level.min(100);
        self.promote_waiting();
    }

    pub fn pause_level(&self) -> u8 {
        self.pause_level
    }

    pub fn count(&self) -> usize {
        self.displayed.len() + self.waiting.len() + self.history.len()
    }

    pub fn displayed_count(&self) -> usize {
        self.displayed.len()
    }

    pub fn waiting_count(&self) -> usize {
        self.waiting.len()
    }

    pub fn history_count(&self) -> usize {
        self.history.len()
    }

    pub fn snapshot(&self) -> Vec<Notification> {
        self.displayed
            .values()
            .chain(self.waiting.iter())
            .chain(self.history.iter())
            .cloned()
            .collect()
    }

    pub fn displayed_snapshot(&self) -> Vec<Notification> {
        let mut notifications = self.displayed.values().cloned().collect::<Vec<_>>();
        notifications.sort_by_key(|notification| std::cmp::Reverse(notification.urgency));
        notifications
    }

    pub fn history_snapshot(&self) -> Vec<Notification> {
        self.history.iter().cloned().collect()
    }

    pub fn rules_snapshot(&self) -> Vec<NotificationRuleConfig> {
        self.config.rules.clone()
    }

    pub fn pop_history(&mut self, id: Option<u32>) -> Option<Notification> {
        let index = match id {
            Some(id) => self
                .history
                .iter()
                .position(|notification| notification.id == id),
            None => (!self.history.is_empty()).then_some(0),
        }?;
        let mut notification = self.history.remove(index)?;
        notification.actions.clear();
        notification.expires_at = None;
        notification.skip_popup = false;
        self.upsert(notification.clone());
        Some(notification)
    }

    pub fn remove_history(&mut self, id: u32) -> bool {
        self.history
            .iter()
            .position(|notification| notification.id == id)
            .and_then(|index| self.history.remove(index))
            .is_some()
    }

    pub fn reload(&mut self, config: NotificationConfig) {
        self.pause_level = config.pause_level;
        self.config = config;
        self.promote_waiting();
    }

    pub fn config(&self) -> &NotificationConfig {
        &self.config
    }
}

fn string_hint(hints: &std::collections::HashMap<String, OwnedValue>, name: &str) -> String {
    hints
        .get(name)
        .and_then(|value| String::try_from(value.clone()).ok())
        .unwrap_or_default()
}

fn bool_hint(hints: &std::collections::HashMap<String, OwnedValue>, name: &str) -> bool {
    hints
        .get(name)
        .and_then(|value| bool::try_from(value.clone()).ok())
        .unwrap_or(false)
}

fn progress_hint(hints: &std::collections::HashMap<String, OwnedValue>) -> Option<u8> {
    ["value", "x-dunst-progress", "x-canonical-progress"]
        .iter()
        .find_map(|name| hints.get(*name))
        .and_then(|value| i32::try_from(value.clone()).ok())
        .and_then(|value| u8::try_from(value).ok())
        .map(|value| value.min(100))
}

fn stack_tag_hint(hints: &std::collections::HashMap<String, OwnedValue>) -> String {
    [
        "x-dunst-stack-tag",
        "x-canonical-private-synchronous",
        "private-synchronous",
        "synchronous",
    ]
    .iter()
    .find_map(|name| {
        hints
            .get(*name)
            .and_then(|value| String::try_from(value.clone()).ok())
    })
    .unwrap_or_default()
}

fn is_suppress_action(key: &str, label: &str) -> bool {
    let key = key.to_ascii_lowercase();
    let label = label.to_ascii_lowercase();
    label.contains("今後表示しない")
        || label.contains("don't show again")
        || label.contains("do not show again")
        || key.contains("dont-show")
        || key.contains("do-not-show")
        || key.contains("suppress")
}

/// Converts a client-provided icon name into a readable image path. Clients
/// may provide a path, a desktop-entry icon name, or no icon at all.
fn resolve_notification_icon(icon: String, desktop_entry: &str, app_name: &str) -> String {
    if Path::new(&icon).is_file() {
        return icon;
    }
    if !icon.is_empty() {
        if let Some(path) = find_icon_file(&icon) {
            return path.display().to_string();
        }
    }
    let desktop = find_desktop_entry(desktop_entry, app_name);
    let Some(desktop) = desktop else {
        return icon;
    };
    let Ok(contents) = fs::read_to_string(desktop) else {
        return icon;
    };
    let Some(desktop_icon) = contents
        .lines()
        .find_map(|line| line.strip_prefix("Icon="))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return icon;
    };
    if Path::new(desktop_icon).is_file() {
        return desktop_icon.to_string();
    }
    find_icon_file(desktop_icon)
        .map(|path| path.display().to_string())
        .unwrap_or(icon)
}

fn data_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(home));
    } else if let Some(home) = env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share"));
    }
    if let Some(dirs) = env::var_os("XDG_DATA_DIRS") {
        roots.extend(env::split_paths(&dirs));
    }
    roots.extend([
        PathBuf::from("/run/current-system/sw/share"),
        PathBuf::from("/usr/share"),
    ]);
    roots
}

fn find_desktop_entry(desktop_entry: &str, app_name: &str) -> Option<PathBuf> {
    let names = [desktop_entry, app_name]
        .into_iter()
        .filter(|name| !name.is_empty())
        .flat_map(|name| [name.to_string(), format!("{name}.desktop")])
        .collect::<Vec<_>>();
    data_roots().into_iter().find_map(|root| {
        let applications = root.join("applications");
        names
            .iter()
            .map(|name| applications.join(name))
            .find(|path| path.is_file())
    })
}

fn find_icon_file(icon_name: &str) -> Option<PathBuf> {
    let direct = Path::new(icon_name);
    if direct.is_file() {
        return Some(direct.to_path_buf());
    }
    let candidates = if Path::new(icon_name).extension().is_some() {
        vec![icon_name.to_string()]
    } else {
        ["png", "svg", "xpm", "webp"]
            .into_iter()
            .map(|extension| format!("{icon_name}.{extension}"))
            .collect()
    };
    data_roots().into_iter().find_map(|root| {
        candidates
            .iter()
            .map(|candidate| root.join("pixmaps").join(candidate))
            .find(|path| path.is_file())
            .or_else(|| {
                candidates
                    .iter()
                    .find_map(|candidate| find_file_recursively(&root.join("icons"), candidate, 5))
            })
    })
}

fn find_file_recursively(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .is_some_and(|entry_name| entry_name == name)
            && path.is_file()
        {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_file_recursively(&path, name, depth - 1) {
                return Some(found);
            }
        }
    }
    None
}

fn default_override_pause_level(urgency: Urgency) -> u8 {
    match urgency {
        Urgency::Low => 0,
        Urgency::Normal => 30,
        Urgency::Critical => 60,
    }
}

fn timeout_from_request(
    timeout: i32,
    urgency: Urgency,
    config: &NotificationConfig,
) -> Option<Instant> {
    if timeout >= 0 {
        return (timeout > 0).then(|| {
            Instant::now() + Duration::from_millis(u64::try_from(timeout).unwrap_or_default())
        });
    }
    let seconds = match urgency {
        Urgency::Low => config.low_timeout_seconds,
        Urgency::Normal => config.normal_timeout_seconds,
        Urgency::Critical => config.critical_timeout_seconds,
    };
    (seconds > 0).then(|| Instant::now() + Duration::from_secs(seconds))
}

fn rule_matches(notification: &Notification, rule: &NotificationRuleConfig) -> bool {
    rule.enabled
        && matches_optional(&rule.app_name, &notification.app_name)
        && matches_optional(&rule.summary, &notification.summary)
        && matches_optional(&rule.category, &notification.category)
        && matches_optional(&rule.desktop_entry, &notification.desktop_entry)
        && rule
            .urgency
            .as_deref()
            .is_none_or(|urgency| urgency.eq_ignore_ascii_case(notification.urgency.as_str()))
}

fn matches_optional(pattern: &Option<String>, value: &str) -> bool {
    pattern.as_deref().is_none_or(|pattern| {
        if let Some((prefix, suffix)) = pattern.split_once('*') {
            value.starts_with(prefix) && value.ends_with(suffix)
        } else {
            value == pattern
        }
    })
}

fn apply_rules(
    notification: &mut Notification,
    rules: &[NotificationRuleConfig],
    config: &NotificationConfig,
) {
    for rule in rules {
        if !rule_matches(notification, rule) {
            continue;
        }
        if let Some(timeout) = rule.timeout_seconds {
            notification.expires_at =
                (timeout > 0).then(|| Instant::now() + Duration::from_secs(timeout));
        }
        if let Some(level) = rule.override_pause_level {
            notification.override_pause_level = level.min(100);
        }
        if let Some(skip_popup) = rule.skip_popup {
            notification.skip_popup = skip_popup;
        }
        if let Some(history_ignore) = rule.history_ignore {
            notification.history_ignore = history_ignore;
        }
        if let Some(stack_tag) = &rule.stack_tag {
            notification.stack_tag = stack_tag.clone();
        }
    }
    let _ = config;
}

/// Starts the freedesktop notification endpoint in a detached thread.
pub fn start_server(sender: Sender<NotificationEvent>, store: SharedNotificationStore) {
    thread::Builder::new()
        .name("bah-notifications".to_string())
        .spawn(move || {
            let service = NotificationService {
                sender: sender.clone(),
                store: store.clone(),
                next_id: Arc::new(AtomicU32::new(1)),
            };
            let control = NotificationControl { sender, store };
            let connection = Builder::session()
                .and_then(|builder| builder.serve_at(NOTIFICATION_OBJECT_PATH, service))
                .and_then(|builder| builder.serve_at(NOTIFICATION_OBJECT_PATH, control))
                .and_then(|builder| builder.name(NOTIFICATION_BUS_NAME))
                .and_then(Builder::build);
            match connection {
                Ok(_connection) => {
                    info!("notification D-Bus service is running");
                    loop {
                        thread::park();
                    }
                }
                Err(error) => warn!(
                    "notification D-Bus service unavailable (another daemon may own it): {error}"
                ),
            }
        })
        .expect("failed to start notification D-Bus thread");
}

/// Signals are sent over a short-lived session-bus connection so UI surfaces
/// can report user interaction without sharing the server thread's connection.
pub fn emit_action_invoked(id: u32, action: &str) {
    emit_signal("ActionInvoked", &(id, action));
}

pub fn emit_notification_closed(id: u32, reason: CloseReason) {
    emit_signal("NotificationClosed", &(id, reason as u32));
}

fn emit_signal<B>(name: &str, body: &B)
where
    B: serde::ser::Serialize + zbus::zvariant::Type,
{
    let Ok(connection) = zbus::blocking::Connection::session() else {
        return;
    };
    if let Err(error) = connection.emit_signal(
        None::<&str>,
        NOTIFICATION_OBJECT_PATH,
        NOTIFICATION_BUS_NAME,
        name,
        body,
    ) {
        warn!("failed to emit notification signal {name}: {error}");
    }
}

struct NotificationService {
    sender: Sender<NotificationEvent>,
    store: SharedNotificationStore,
    next_id: Arc<AtomicU32>,
}

struct NotificationControl {
    sender: Sender<NotificationEvent>,
    store: SharedNotificationStore,
}

impl NotificationControl {
    fn with_store<T>(&self, operation: impl FnOnce(&mut NotificationStore) -> T) -> T {
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        operation(&mut store)
    }

    fn refresh(&self) {
        let _ = self.sender.try_send(NotificationEvent::Refresh);
    }
}

#[interface(name = "org.dunstproject.cmd0")]
impl NotificationControl {
    #[zbus(name = "Ping")]
    fn ping(&self) {}

    #[zbus(name = "NotificationCloseLast")]
    fn notification_close_last(&self) {
        let id = self.with_store(|store| {
            store
                .displayed_snapshot()
                .first()
                .map(|notification| notification.id)
        });
        if let Some(id) = id {
            self.with_store(|store| {
                let _ = store.close(id, CloseReason::DismissedByUser);
            });
            emit_notification_closed(id, CloseReason::DismissedByUser);
            self.refresh();
        }
    }

    #[zbus(name = "NotificationCloseAll")]
    fn notification_close_all(&self) {
        let ids = self.with_store(|store| {
            store
                .displayed_snapshot()
                .into_iter()
                .map(|notification| notification.id)
                .collect::<Vec<_>>()
        });
        self.with_store(NotificationStore::clear);
        for id in ids {
            emit_notification_closed(id, CloseReason::DismissedByUser);
        }
        self.refresh();
    }

    #[zbus(name = "NotificationClearHistory")]
    fn notification_clear_history(&self) {
        self.with_store(|store| store.history.clear());
        self.refresh();
    }

    #[zbus(name = "NotificationShow")]
    fn notification_show(&self) {
        self.with_store(|store| {
            let _ = store.pop_history(None);
        });
        self.refresh();
    }

    #[zbus(name = "NotificationPopHistory")]
    fn notification_pop_history(&self, id: u32) {
        self.with_store(|store| {
            let _ = store.pop_history(Some(id));
        });
        self.refresh();
    }

    #[zbus(name = "NotificationRemoveFromHistory")]
    fn notification_remove_from_history(&self, id: u32) {
        self.with_store(|store| {
            store.remove_history(id);
        });
        self.refresh();
    }

    #[zbus(name = "NotificationListHistory")]
    fn notification_list_history(&self) -> Vec<std::collections::HashMap<String, OwnedValue>> {
        self.with_store(|store| {
            store
                .history_snapshot()
                .iter()
                .map(notification_to_dbus_map)
                .collect()
        })
    }

    #[zbus(name = "RuleList")]
    fn rule_list(&self) -> Vec<std::collections::HashMap<String, OwnedValue>> {
        self.with_store(|store| {
            store
                .rules_snapshot()
                .iter()
                .map(rule_to_dbus_map)
                .collect()
        })
    }

    #[zbus(name = "NotificationAction")]
    fn notification_action(&self, position: u32) {
        let action = self.with_store(|store| {
            store
                .displayed_snapshot()
                .get(position as usize)
                .and_then(|notification| {
                    notification
                        .actions
                        .iter()
                        .find(|action| action.key == "default")
                        .or_else(|| {
                            (notification.actions.len() == 1).then(|| &notification.actions[0])
                        })
                        .map(|action| (notification.id, action.key.clone()))
                })
        });
        if let Some((id, action)) = action {
            emit_action_invoked(id, &action);
        }
    }

    #[zbus(name = "ContextMenuCall")]
    fn context_menu_call(&self) {
        // Bah exposes actions directly on popup cards and in the tray, so no
        // separate external menu process is necessary.
    }

    #[zbus(name = "ConfigReload")]
    fn config_reload(&self, _paths: Vec<String>) {
        match crate::config::Config::load() {
            Ok(config) => self.with_store(|store| store.reload(config.notifications)),
            Err(error) => warn!("notification configuration reload failed: {error}"),
        }
        self.refresh();
    }

    #[zbus(name = "RuleEnable")]
    fn rule_enable(&self, name: String, state: i32) {
        self.with_store(|store| {
            if let Some(rule) = store.config.rules.iter_mut().find(|rule| rule.name == name) {
                rule.enabled = match state {
                    0 => false,
                    1 => true,
                    _ => !rule.enabled,
                };
            }
        });
        self.refresh();
    }

    #[zbus(name = "displayedLength", property)]
    fn displayed_length(&self) -> u32 {
        self.with_store(|store| store.displayed_count() as u32)
    }

    #[zbus(name = "waitingLength", property)]
    fn waiting_length(&self) -> u32 {
        self.with_store(|store| store.waiting_count() as u32)
    }

    #[zbus(name = "historyLength", property)]
    fn history_length(&self) -> u32 {
        self.with_store(|store| store.history_count() as u32)
    }

    #[zbus(name = "pauseLevel", property)]
    fn pause_level(&self) -> u32 {
        self.with_store(|store| u32::from(store.pause_level()))
    }

    #[zbus(name = "pauseLevel", property)]
    fn set_pause_level(&mut self, level: u32) {
        self.with_store(|store| store.set_pause_level(level.min(100) as u8));
        self.refresh();
    }

    #[zbus(name = "paused", property)]
    fn paused(&self) -> bool {
        self.with_store(|store| store.pause_level() > 0)
    }

    #[zbus(name = "paused", property)]
    fn set_paused(&mut self, paused: bool) {
        self.with_store(|store| store.set_pause_level(if paused { 100 } else { 0 }));
        self.refresh();
    }
}

fn owned_string(value: impl Into<String>) -> OwnedValue {
    OwnedValue::from(zbus::zvariant::Str::from(value.into()))
}

fn notification_to_dbus_map(
    notification: &Notification,
) -> std::collections::HashMap<String, OwnedValue> {
    let mut map = std::collections::HashMap::new();
    map.insert("id".to_string(), OwnedValue::from(notification.id as i32));
    map.insert(
        "appname".to_string(),
        owned_string(notification.app_name.clone()),
    );
    map.insert(
        "summary".to_string(),
        owned_string(notification.summary.clone()),
    );
    map.insert("body".to_string(), owned_string(notification.body.clone()));
    map.insert(
        "category".to_string(),
        owned_string(notification.category.clone()),
    );
    map.insert(
        "icon_path".to_string(),
        owned_string(notification.app_icon.clone()),
    );
    map.insert(
        "urgency".to_string(),
        owned_string(notification.urgency.as_str()),
    );
    map.insert(
        "progress".to_string(),
        OwnedValue::from(i32::from(notification.progress.unwrap_or(255)) - 256),
    );
    map.insert(
        "stack_tag".to_string(),
        owned_string(notification.stack_tag.clone()),
    );
    map
}

fn rule_to_dbus_map(
    rule: &NotificationRuleConfig,
) -> std::collections::HashMap<String, OwnedValue> {
    let mut map = std::collections::HashMap::new();
    map.insert("name".to_string(), owned_string(rule.name.clone()));
    map.insert("enabled".to_string(), OwnedValue::from(rule.enabled));
    if let Some(app_name) = &rule.app_name {
        map.insert("appname".to_string(), owned_string(app_name.clone()));
    }
    if let Some(summary) = &rule.summary {
        map.insert("summary".to_string(), owned_string(summary.clone()));
    }
    if let Some(category) = &rule.category {
        map.insert("category".to_string(), owned_string(category.clone()));
    }
    map
}

#[interface(name = "org.freedesktop.Notifications")]
impl NotificationService {
    #[zbus(signal)]
    async fn action_invoked(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: u32,
        action: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    fn get_capabilities(&self) -> Vec<String> {
        vec![
            "actions".to_string(),
            "body".to_string(),
            "body-hyperlinks".to_string(),
            "icon-static".to_string(),
            "synchronous".to_string(),
            "private-synchronous".to_string(),
            "x-canonical-private-synchronous".to_string(),
            "x-dunst-stack-tag".to_string(),
        ]
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: std::collections::HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id == 0 {
            self.next_id.fetch_add(1, Ordering::Relaxed)
        } else {
            replaces_id
        };
        let config = self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .config()
            .clone();
        let notification = Notification::from_request(
            NotificationRequest {
                app_name,
                app_icon,
                summary,
                body,
                actions,
                hints,
                expire_timeout,
            },
            id,
            &config,
        );
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        store.upsert(notification.clone());
        drop(store);
        let _ = self
            .sender
            .try_send(NotificationEvent::Upsert(notification));
        id
    }

    fn close_notification(&self, id: u32) {
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let closed = store.close(id, CloseReason::ClosedByClient).is_some();
        drop(store);
        if closed {
            emit_notification_closed(id, CloseReason::ClosedByClient);
        }
        let _ = self.sender.try_send(NotificationEvent::Refresh);
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "bah".to_string(),
            "bah".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
            "1.2".to_string(),
        )
    }
}

/// Implements `bah notifications …` against the running daemon. Its command
/// vocabulary intentionally mirrors dunstctl, making it suitable for existing
/// Hyprland keybindings.
pub fn run_control_cli(arguments: &[std::ffi::OsString]) -> Result<(), String> {
    let arguments = arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let connection = zbus::blocking::Connection::session().map_err(|error| error.to_string())?;
    let control = zbus::blocking::Proxy::new(
        &connection,
        NOTIFICATION_BUS_NAME,
        NOTIFICATION_OBJECT_PATH,
        "org.dunstproject.cmd0",
    )
    .map_err(|error| error.to_string())?;
    let notification = zbus::blocking::Proxy::new(
        &connection,
        NOTIFICATION_BUS_NAME,
        NOTIFICATION_OBJECT_PATH,
        NOTIFICATION_BUS_NAME,
    )
    .map_err(|error| error.to_string())?;
    let command = arguments.first().map(String::as_str).unwrap_or("help");
    match command {
        "help" | "--help" | "-h" => print_control_help(),
        "close" => match arguments.get(1) {
            Some(id) => call(&notification, "CloseNotification", &(parse_id(id)?,))?,
            None => call(&control, "NotificationCloseLast", &())?,
        },
        "close-all" => call(&control, "NotificationCloseAll", &())?,
        "action" => {
            let position = arguments
                .get(1)
                .map(|value| parse_id(value))
                .transpose()?
                .unwrap_or(0);
            call(&control, "NotificationAction", &(position,))?;
        }
        "context" => call(&control, "ContextMenuCall", &())?,
        "count" => {
            let displayed: u32 = property(&control, "displayedLength")?;
            let waiting: u32 = property(&control, "waitingLength")?;
            let history: u32 = property(&control, "historyLength")?;
            match arguments.get(1).map(String::as_str) {
                None => println!(
                    "              Waiting: {waiting}\n  Currently displayed: {displayed}\n              History: {history}"
                ),
                Some("displayed") => println!("{displayed}"),
                Some("waiting") => println!("{waiting}"),
                Some("history") => println!("{history}"),
                Some(_) => return Err("count accepts displayed, waiting, or history".to_string()),
            }
        }
        "history" => print_json(&control, "NotificationListHistory")?,
        "history-clear" => call(&control, "NotificationClearHistory", &())?,
        "history-pop" => match arguments.get(1) {
            Some(id) => call(&control, "NotificationPopHistory", &(parse_id(id)?,))?,
            None => call(&control, "NotificationShow", &())?,
        },
        "history-rm" => call(
            &control,
            "NotificationRemoveFromHistory",
            &(parse_id(
                arguments.get(1).ok_or("history-rm requires an ID")?,
            )?,),
        )?,
        "is-paused" => {
            let paused: bool = property(&control, "paused")?;
            println!("{paused}");
        }
        "set-paused" => {
            let current: bool = property(&control, "paused")?;
            let value = match arguments.get(1).map(String::as_str) {
                Some("true") => true,
                Some("false") => false,
                Some("toggle") => !current,
                _ => return Err("set-paused requires true, false, or toggle".to_string()),
            };
            control
                .set_property("paused", &value)
                .map_err(|error| error.to_string())?;
        }
        "get-pause-level" => println!("{}", property::<u32>(&control, "pauseLevel")?),
        "set-pause-level" => {
            let level = parse_id(arguments.get(1).ok_or("set-pause-level requires a level")?)?;
            if level > 100 {
                return Err("pause level must be between 0 and 100".to_string());
            }
            control
                .set_property("pauseLevel", &level)
                .map_err(|error| error.to_string())?;
        }
        "rule" => {
            let name = arguments.get(1).ok_or("rule requires a name")?;
            let state = match arguments.get(2).map(String::as_str) {
                Some("disable") => 0,
                Some("enable") => 1,
                Some("toggle") => 2,
                _ => return Err("rule requires enable, disable, or toggle".to_string()),
            };
            call(&control, "RuleEnable", &(name.as_str(), state))?;
        }
        "rules" => print_json(&control, "RuleList")?,
        "reload" => call(&control, "ConfigReload", &(arguments[1..].to_vec(),))?,
        "debug" => {
            let info: (String, String, String, String) = notification
                .call("GetServerInformation", &())
                .map_err(|error| error.to_string())?;
            call(&control, "Ping", &())?;
            println!("{} version: {}", info.0, info.2);
        }
        _ => return Err(format!("unrecognized notification command: {command}")),
    }
    Ok(())
}

fn call<B: serde::ser::Serialize + zbus::zvariant::Type>(
    proxy: &zbus::blocking::Proxy<'_>,
    method: &str,
    body: &B,
) -> Result<(), String> {
    proxy
        .call::<_, _, ()>(method, body)
        .map_err(|error| error.to_string())
}

fn property<T>(proxy: &zbus::blocking::Proxy<'_>, name: &str) -> Result<T, String>
where
    T: TryFrom<OwnedValue>,
    T::Error: Into<zbus::Error>,
{
    proxy.get_property(name).map_err(|error| error.to_string())
}

fn print_json(proxy: &zbus::blocking::Proxy<'_>, method: &str) -> Result<(), String> {
    let value: Vec<std::collections::HashMap<String, OwnedValue>> =
        proxy.call(method, &()).map_err(|error| error.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn parse_id(value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| "notification ID must be a number".to_string())
}

fn print_control_help() {
    println!(
        "Usage: bah notifications <command>\n\nCommands: action, close, close-all, context, count, history, history-clear, history-pop, history-rm, is-paused, set-paused, get-pause-level, set-pause-level, rule, rules, reload, debug"
    );
}

#[cfg(test)]
mod tests {
    use super::{Notification, NotificationEvent, NotificationStore, Urgency};
    use crate::config::NotificationConfig;

    fn notification(id: u32) -> Notification {
        Notification {
            id,
            app_name: "test".into(),
            summary: format!("summary-{id}"),
            body: String::new(),
            app_icon: String::new(),
            category: String::new(),
            desktop_entry: String::new(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            progress: None,
            stack_tag: String::new(),
            transient: false,
            duplicate_count: 1,
            history_ignore: false,
            skip_popup: false,
            override_pause_level: 30,
            expires_at: None,
        }
    }

    #[test]
    fn replacements_preserve_the_notification_count() {
        let mut store = NotificationStore::default();
        store.apply(NotificationEvent::Upsert(notification(7)));
        let mut replacement = notification(7);
        replacement.summary = "new".into();
        store.apply(NotificationEvent::Upsert(replacement));
        assert_eq!(store.displayed_count(), 1);
        assert_eq!(store.snapshot()[0].summary, "new");
    }

    #[test]
    fn excess_notifications_wait_until_a_slot_opens() {
        let mut store = NotificationStore::new(NotificationConfig {
            notification_limit: 1,
            ..NotificationConfig::default()
        });
        store.upsert(notification(1));
        store.upsert(notification(2));
        assert_eq!(store.displayed_count(), 1);
        assert_eq!(store.waiting_count(), 1);
        store.close(1, super::CloseReason::DismissedByUser);
        assert_eq!(store.displayed_count(), 1);
        assert_eq!(store.waiting_count(), 0);
    }

    #[test]
    fn duplicate_notifications_are_coalesced_with_a_count() {
        let mut store = NotificationStore::default();
        let mut first = notification(1);
        first.summary = "same".into();
        let mut second = notification(2);
        second.summary = "same".into();
        store.upsert(first);
        store.upsert(second);
        assert_eq!(store.displayed_count(), 1);
        assert_eq!(store.displayed_snapshot()[0].duplicate_count, 2);
    }

    #[test]
    fn pause_level_queues_then_releases_eligible_notifications() {
        let mut store = NotificationStore::default();
        store.set_pause_level(100);
        store.upsert(notification(1));
        assert_eq!(store.waiting_count(), 1);
        store.set_pause_level(0);
        assert_eq!(store.displayed_count(), 1);
        assert_eq!(store.waiting_count(), 0);
    }

    #[test]
    fn remove_and_clear_all_delete_retained_history() {
        let mut store = NotificationStore::default();
        store.upsert(notification(1));
        store.close(1, super::CloseReason::DismissedByUser);
        assert_eq!(store.history_count(), 1);
        assert!(store.remove(1));
        assert_eq!(store.history_count(), 0);
        store.upsert(notification(2));
        store.close(2, super::CloseReason::DismissedByUser);
        store.clear_all();
        assert_eq!(store.count(), 0);
    }

    #[test]
    fn suppression_actions_are_not_presented() {
        assert!(super::is_suppress_action(
            "dont-show-again",
            "今後表示しない"
        ));
        assert!(super::is_suppress_action("", "Don't show again"));
        assert!(!super::is_suppress_action("open", "Open"));
    }
}
