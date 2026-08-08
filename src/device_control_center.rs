use gpui::{Context, Render, Window, div, prelude::*};

use crate::app::DeviceControlCenterLock;

/// Empty root view reserved for the standalone device control center.
pub struct DeviceControlCenter {
    _lock: DeviceControlCenterLock,
}

impl DeviceControlCenter {
    pub fn new(lock: DeviceControlCenterLock, _cx: &mut Context<Self>) -> Self {
        Self { _lock: lock }
    }
}

impl Render for DeviceControlCenter {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}
