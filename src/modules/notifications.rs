use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    thread,
};

use async_channel::Sender;
use log::{info, warn};
use zbus::{blocking::connection::Builder, interface, zvariant::OwnedValue};

/// A notification retained by the tray. Notifications are ordered by their
/// D-Bus identifier, which is monotonically assigned by the server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationEvent {
    Upsert(Notification),
    Close(u32),
    Clear,
}

/// Shared state read by both the bar and the separate notification-tray
/// surface. The bar applies D-Bus events; the tray may dismiss entries itself.
#[derive(Default)]
pub struct NotificationStore {
    notifications: BTreeMap<u32, Notification>,
}

pub type SharedNotificationStore = Arc<Mutex<NotificationStore>>;

impl NotificationStore {
    pub fn shared() -> SharedNotificationStore {
        Arc::new(Mutex::new(Self::default()))
    }

    pub fn apply(&mut self, event: NotificationEvent) {
        match event {
            NotificationEvent::Upsert(notification) => {
                self.notifications.insert(notification.id, notification);
            }
            NotificationEvent::Close(id) => {
                self.notifications.remove(&id);
            }
            NotificationEvent::Clear => self.clear(),
        }
    }

    pub fn clear(&mut self) {
        self.notifications.clear();
    }

    pub fn count(&self) -> usize {
        self.notifications.len()
    }

    pub fn snapshot(&self) -> Vec<Notification> {
        self.notifications.values().rev().cloned().collect()
    }
}

/// Starts the standard freedesktop.org notification endpoint in a detached
/// thread. Its methods only enqueue events, leaving all GPUI state on the UI
/// thread.
pub fn start_server(sender: Sender<NotificationEvent>) {
    thread::Builder::new()
        .name("bah-notifications".to_string())
        .spawn(move || {
            let service = NotificationService {
                sender,
                next_id: Arc::new(AtomicU32::new(1)),
            };
            let connection = Builder::session()
                .and_then(|builder| builder.serve_at("/org/freedesktop/Notifications", service))
                .and_then(|builder| builder.name("org.freedesktop.Notifications"))
                .and_then(Builder::build);

            match connection {
                Ok(_connection) => {
                    info!("notification D-Bus service is running");
                    // zbus services method calls on its internal executor. Keep the
                    // connection alive for the lifetime of the application.
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

struct NotificationService {
    sender: Sender<NotificationEvent>,
    next_id: Arc<AtomicU32>,
}

#[interface(name = "org.freedesktop.Notifications")]
impl NotificationService {
    fn get_capabilities(&self) -> Vec<String> {
        vec!["body".to_string()]
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        _actions: Vec<String>,
        _hints: HashMap<String, OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id == 0 {
            self.next_id.fetch_add(1, Ordering::Relaxed)
        } else {
            replaces_id
        };
        let _ = self
            .sender
            .send_blocking(NotificationEvent::Upsert(Notification {
                id,
                app_name,
                summary,
                body,
            }));
        id
    }

    fn close_notification(&self, id: u32) {
        let _ = self.sender.send_blocking(NotificationEvent::Close(id));
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

#[cfg(test)]
mod tests {
    use super::{Notification, NotificationEvent, NotificationStore};

    #[test]
    fn replacements_preserve_the_notification_count() {
        let mut store = NotificationStore::default();
        store.apply(NotificationEvent::Upsert(Notification {
            id: 7,
            app_name: "first".into(),
            summary: "old".into(),
            body: String::new(),
        }));
        store.apply(NotificationEvent::Upsert(Notification {
            id: 7,
            app_name: "first".into(),
            summary: "new".into(),
            body: String::new(),
        }));

        assert_eq!(store.count(), 1);
        assert_eq!(store.snapshot()[0].summary, "new");
    }
}
