//! Frozen per-toplevel previews for the switcher.
//!
//! Hyprland exposes this through the `hyprland-toplevel-export-v1` protocol.
//! This module intentionally captures one window at a time: the switcher needs
//! a stable preview, not a continuously streaming mirror of every application.

use std::{
    collections::{HashMap, VecDeque},
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::fd::{AsFd, FromRawFd},
    thread,
};

use image::{Rgba, RgbaImage, imageops};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop,
    protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool},
};
use wayland_protocols_hyprland::{
    toplevel_export::v1::client::{
        hyprland_toplevel_export_frame_v1::{self, HyprlandToplevelExportFrameV1},
        hyprland_toplevel_export_manager_v1::{self, HyprlandToplevelExportManagerV1},
    },
    toplevel_mapping::v1::client::{
        hyprland_toplevel_mapping_manager_v1::HyprlandToplevelMappingManagerV1,
        hyprland_toplevel_window_mapping_handle_v1::{self, HyprlandToplevelWindowMappingHandleV1},
    },
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

use crate::hyprland::WorkspaceWindow;

const PREVIEW_MAX_WIDTH: u32 = 504;
const PREVIEW_MAX_HEIGHT: u32 = 264;

pub struct SnapshotResult {
    pub address: String,
    pub image: RgbaImage,
}

/// Starts a best-effort worker. A missing protocol or one failed window never
/// prevents the switcher from opening; callers retain their icon fallback.
pub fn capture_snapshots(windows: Vec<WorkspaceWindow>) -> async_channel::Receiver<SnapshotResult> {
    let (sender, receiver) = async_channel::bounded(4);
    let addresses = windows.into_iter().map(|window| window.address).collect();
    thread::spawn(move || {
        if let Err(error) = capture_worker(addresses, sender) {
            log::debug!("window snapshot capture unavailable: {error:#}");
        }
    });
    receiver
}

fn capture_worker(
    addresses: Vec<String>,
    sender: async_channel::Sender<SnapshotResult>,
) -> anyhow::Result<()> {
    if addresses.is_empty() {
        return Ok(());
    }

    let connection = Connection::connect_to_env()?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());

    let mut state = CaptureState::new(addresses, sender);
    queue.roundtrip(&mut state)?;
    state.begin_next(&handle);
    while !state.finished {
        queue.blocking_dispatch(&mut state)?;
    }
    Ok(())
}

struct CaptureState {
    shm: Option<wl_shm::WlShm>,
    export_manager: Option<HyprlandToplevelExportManagerV1>,
    mapping_manager: Option<HyprlandToplevelMappingManagerV1>,
    pending: VecDeque<String>,
    mapped_toplevels: HashMap<String, ZwlrForeignToplevelHandleV1>,
    active_address: Option<String>,
    buffers: HashMap<u32, CaptureBuffer>,
    sender: async_channel::Sender<SnapshotResult>,
    finished: bool,
}

impl CaptureState {
    fn new(addresses: Vec<String>, sender: async_channel::Sender<SnapshotResult>) -> Self {
        Self {
            shm: None,
            export_manager: None,
            mapping_manager: None,
            pending: addresses.into_iter().collect(),
            mapped_toplevels: HashMap::new(),
            active_address: None,
            buffers: HashMap::new(),
            sender,
            finished: false,
        }
    }

    fn begin_next(&mut self, qh: &QueueHandle<Self>) {
        if self.active_address.is_some() || self.finished {
            return;
        }
        let Some(address) = self.pending.pop_front() else {
            self.finished = true;
            return;
        };
        let Some(manager) = self.export_manager.as_ref() else {
            self.finished = true;
            return;
        };
        let Some(toplevel) = self.mapped_toplevels.get(&address) else {
            self.begin_next(qh);
            return;
        };
        self.active_address = Some(address);
        manager.capture_toplevel_with_wlr_toplevel_handle(0, toplevel, qh, ());
    }

    fn create_buffer(
        &mut self,
        frame: &HyprlandToplevelExportFrameV1,
        format: wl_shm::Format,
        width: u32,
        height: u32,
        stride: u32,
        qh: &QueueHandle<Self>,
    ) -> anyhow::Result<()> {
        let Some(shm) = self.shm.as_ref() else {
            return Ok(());
        };
        let size = (stride as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| anyhow::anyhow!("snapshot buffer is too large"))?;
        let file = anonymous_file(size)?;
        let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            format,
            qh,
            (),
        );
        self.buffers.insert(
            frame.id().protocol_id(),
            CaptureBuffer {
                file,
                width,
                height,
                stride,
                format,
                buffer,
                _pool: pool,
                y_inverted: false,
            },
        );
        frame.copy(&self.buffers[&frame.id().protocol_id()].buffer, 1);
        Ok(())
    }

    fn complete_frame(&mut self, frame: &HyprlandToplevelExportFrameV1, qh: &QueueHandle<Self>) {
        let frame_id = frame.id().protocol_id();
        if let (Some(address), Some(buffer)) =
            (self.active_address.take(), self.buffers.remove(&frame_id))
        {
            if let Ok(image) = buffer.read_image() {
                let _ = self.sender.send_blocking(SnapshotResult { address, image });
            }
        }
        frame.destroy();
        self.begin_next(qh);
    }
}

struct CaptureBuffer {
    file: File,
    width: u32,
    height: u32,
    stride: u32,
    format: wl_shm::Format,
    // These proxies retain the Wayland buffer and pool until `ready` arrives.
    buffer: wl_buffer::WlBuffer,
    _pool: wl_shm_pool::WlShmPool,
    y_inverted: bool,
}

impl CaptureBuffer {
    fn read_image(mut self) -> anyhow::Result<RgbaImage> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut bytes = vec![0; (self.stride as usize) * (self.height as usize)];
        self.file.read_exact(&mut bytes)?;
        let mut pixels = RgbaImage::new(self.width, self.height);
        for y in 0..self.height {
            let source_y = if self.y_inverted {
                self.height - 1 - y
            } else {
                y
            };
            let row = source_y as usize * self.stride as usize;
            for x in 0..self.width {
                let offset = row + x as usize * 4;
                if offset + 3 >= bytes.len() {
                    continue;
                }
                let (blue, green, red, alpha) = (
                    bytes[offset],
                    bytes[offset + 1],
                    bytes[offset + 2],
                    bytes[offset + 3],
                );
                let alpha = if self.format == wl_shm::Format::Xrgb8888 {
                    255
                } else {
                    alpha
                };
                pixels.put_pixel(x, y, Rgba([red, green, blue, alpha]));
            }
        }
        Ok(imageops::thumbnail(
            &pixels,
            PREVIEW_MAX_WIDTH,
            PREVIEW_MAX_HEIGHT,
        ))
    }
}

fn anonymous_file(size: usize) -> anyhow::Result<File> {
    let name = b"bah-window-preview\0";
    // `memfd` keeps a private, anonymous wl_shm backing file without leaving a
    // temporary path on disk.
    let fd = unsafe { libc::memfd_create(name.as_ptr().cast(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let truncate = unsafe { libc::ftruncate(fd, size as libc::off_t) };
    if truncate != 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(error.into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

impl Dispatch<wl_registry::WlRegistry, ()> for CaptureState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "zwlr_foreign_toplevel_manager_v1" => {
                registry.bind::<ZwlrForeignToplevelManagerV1, _, _>(name, version.min(3), qh, ());
            }
            "hyprland_toplevel_mapping_manager_v1" => {
                state.mapping_manager = Some(registry.bind(name, 1, qh, ()));
            }
            "hyprland_toplevel_export_manager_v1" if version >= 2 => {
                state.export_manager = Some(registry.bind(name, 2, qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            if let Some(manager) = state.mapping_manager.as_ref() {
                manager.get_window_for_toplevel_wlr(&toplevel, qh, toplevel.clone());
            }
        }
    }
}

impl Dispatch<HyprlandToplevelWindowMappingHandleV1, ZwlrForeignToplevelHandleV1> for CaptureState {
    fn event(
        state: &mut Self,
        mapping: &HyprlandToplevelWindowMappingHandleV1,
        event: hyprland_toplevel_window_mapping_handle_v1::Event,
        toplevel: &ZwlrForeignToplevelHandleV1,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let hyprland_toplevel_window_mapping_handle_v1::Event::WindowAddress {
            address_hi,
            address,
        } = event
        {
            let address = format!("0x{:x}", ((address_hi as u64) << 32) | address as u64);
            state.mapped_toplevels.insert(address, toplevel.clone());
        }
        mapping.destroy();
    }
}

impl Dispatch<HyprlandToplevelExportManagerV1, ()> for CaptureState {
    fn event(
        _: &mut Self,
        _: &HyprlandToplevelExportManagerV1,
        _: hyprland_toplevel_export_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<HyprlandToplevelMappingManagerV1, ()> for CaptureState {
    fn event(
        _: &mut Self,
        _: &HyprlandToplevelMappingManagerV1,
        _: wayland_protocols_hyprland::toplevel_mapping::v1::client::hyprland_toplevel_mapping_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<HyprlandToplevelExportFrameV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        frame: &HyprlandToplevelExportFrameV1,
        event: hyprland_toplevel_export_frame_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            hyprland_toplevel_export_frame_v1::Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => {
                if let Err(error) = state.create_buffer(frame, format, width, height, stride, qh) {
                    log::debug!("could not allocate snapshot buffer: {error:#}");
                }
            }
            hyprland_toplevel_export_frame_v1::Event::Flags { flags } => {
                if let Some(buffer) = state.buffers.get_mut(&frame.id().protocol_id()) {
                    buffer.y_inverted = matches!(
                        flags,
                        WEnum::Value(hyprland_toplevel_export_frame_v1::Flags::YInvert)
                    );
                }
            }
            hyprland_toplevel_export_frame_v1::Event::Ready { .. }
            | hyprland_toplevel_export_frame_v1::Event::Failed => state.complete_frame(frame, qh),
            _ => {}
        }
    }
}

delegate_noop!(CaptureState: ignore ZwlrForeignToplevelHandleV1);
delegate_noop!(CaptureState: ignore wl_shm::WlShm);
delegate_noop!(CaptureState: ignore wl_shm_pool::WlShmPool);
delegate_noop!(CaptureState: ignore wl_buffer::WlBuffer);
