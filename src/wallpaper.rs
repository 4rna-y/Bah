use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

use async_channel::Receiver;
use gpui::{
    App, Context, ImageCacheError, ObjectFit, Render, RenderImage, Window, div, img, prelude::*,
};
use image::{Frame, RgbaImage};
use log::warn;

/// A full-output image surface. GPUI's image element also advances GIF/WebP
/// animation frames, so each decoded frame is composited by this layer.
pub struct Wallpaper {
    source: PathBuf,
    // The flock is tied to this descriptor and prevents duplicate wallpaper
    // clients for the lifetime of the Entity.
    _lock: Arc<File>,
    video_frame: Option<Arc<Mutex<Option<Arc<RenderImage>>>>>,
}

impl Wallpaper {
    pub fn new(source: PathBuf, lock: Arc<File>, cx: &mut Context<Self>) -> Self {
        let mut wallpaper = Self {
            source,
            _lock: lock,
            video_frame: None,
        };
        if is_video(&wallpaper.source) {
            wallpaper.video_frame = Some(wallpaper.start_video_decoder(cx));
        }
        wallpaper
    }

    fn start_video_decoder(&self, cx: &mut Context<Self>) -> Arc<Mutex<Option<Arc<RenderImage>>>> {
        let latest = Arc::new(Mutex::new(None));
        let (sender, receiver) = async_channel::bounded(2);
        start_ffmpeg_decoder(self.source.clone(), sender);
        Self::receive_video_frames(receiver, cx);
        latest
    }

    fn receive_video_frames(receiver: Receiver<Arc<RenderImage>>, cx: &mut Context<Self>) {
        cx.spawn(async move |wallpaper, cx| {
            while let Ok(frame) = receiver.recv().await {
                if wallpaper
                    .update(cx, |wallpaper, cx| {
                        if let Some(latest) = &wallpaper.video_frame {
                            *latest.lock().expect("video frame mutex poisoned") = Some(frame);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }
}

impl Render for Wallpaper {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let image = if let Some(latest) = self.video_frame.as_ref().map(Arc::clone) {
            img(
                move |_: &mut Window,
                      _: &mut App|
                      -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
                    latest.lock().ok().and_then(|frame| frame.clone()).map(Ok)
                },
            )
            .size_full()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
        } else {
            img(self.source.clone())
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        };
        div().size_full().overflow_hidden().child(image)
    }
}

fn is_video(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("mp4" | "webm" | "mkv" | "avi" | "mov" | "m4v")
    )
}

fn start_ffmpeg_decoder(source: PathBuf, sender: async_channel::Sender<Arc<RenderImage>>) {
    thread::spawn(move || {
        let Some((width, height)) = video_dimensions(&source) else {
            return;
        };
        let frame_len = width as usize * height as usize * 4;
        let mut child = match Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-stream_loop",
                "-1",
                "-re",
                "-i",
            ])
            .arg(&source)
            .args([
                "-an", "-vf", "fps=30", "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                warn!("failed to start ffmpeg for {}: {error}", source.display());
                return;
            }
        };
        let Some(mut output) = child.stdout.take() else {
            return;
        };
        loop {
            let mut bytes = vec![0; frame_len];
            if output.read_exact(&mut bytes).is_err() {
                break;
            }
            // GPUI's RenderImage expects BGRA, while ffmpeg outputs RGBA.
            for pixel in bytes.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            let buffer = RgbaImage::from_raw(width, height, bytes).expect("validated frame size");
            if sender
                .send_blocking(Arc::new(RenderImage::new(vec![Frame::new(buffer)])))
                .is_err()
            {
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    });
}

fn video_dimensions(source: &Path) -> Option<(u32, u32)> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0:s=x",
        ])
        .arg(source)
        .output()
        .ok()?;
    if !output.status.success() {
        warn!(
            "ffprobe could not inspect video wallpaper {}",
            source.display()
        );
        return None;
    }
    let dimensions = std::str::from_utf8(&output.stdout).ok()?.trim();
    let (width, height) = dimensions.split_once('x')?;
    let width = width.parse().ok()?;
    let height = height.parse().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}
