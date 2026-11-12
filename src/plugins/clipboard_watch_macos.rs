//! Watches the system clipboard (macOS) and turns each change into a
//! [`Capture`], like `clipboard_watch.rs` does with GTK on Linux.
//!
//! AppKit has no change notification for the general pasteboard: a thread
//! reads its `changeCount` a few times a second and reads the content only
//! when it moved. File lists (`public.file-url`, one per pasteboard item),
//! PNG images, HTML with its plain text, and plain text are told apart;
//! copies marked by password managers (`org.nspasteboard.ConcealedType`)
//! are only reported as secrets, and transient ones are not reported.

use super::clipboard_history::{Capture, CapturedImage, fnv};
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSPasteboard;
use objc2_foundation::NSString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Images larger than this many pixels are not decoded (and not recorded).
const MAX_PIXELS: u64 = 64 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(400);
/// <http://nspasteboard.org>: the copy is a secret / must not be kept.
const CONCEALED: &str = "org.nspasteboard.ConcealedType";
const TRANSIENT: &str = "org.nspasteboard.TransientType";

pub struct Watcher {
    stop: Arc<AtomicBool>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Decodes a PNG once: size and a hash of its pixels.
pub fn inspect_png(png: Vec<u8>) -> Option<CapturedImage> {
    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).ok()?;
    if u64::from(img.width()) * u64::from(img.height()) > MAX_PIXELS {
        return None;
    }
    let rgba = img.to_rgba8();
    Some(CapturedImage { width: rgba.width(), height: rgba.height(), pixels_hash: fnv(&[rgba.as_raw()]), png })
}

impl Watcher {
    /// Starts watching the general pasteboard; `on_capture` runs on the
    /// watcher's thread for each change that had readable content.
    pub fn start(on_capture: impl Fn(Capture) + Send + 'static) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::Builder::new()
            .name("clipboard-watch".into())
            .spawn(move || {
                // What is on the clipboard at start is not a new copy.
                let mut seen = autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount());
                while !stopped.load(Ordering::SeqCst) {
                    std::thread::sleep(POLL);
                    let capture = autoreleasepool(|_| {
                        let pb = NSPasteboard::generalPasteboard();
                        let count = pb.changeCount();
                        if count == seen {
                            return None;
                        }
                        seen = count;
                        read(&pb)
                    });
                    if let Some(c) = capture
                        && !stopped.load(Ordering::SeqCst)
                    {
                        on_capture(c);
                    }
                }
            })
            .ok()?;
        Some(Watcher { stop })
    }
}

fn read(pb: &NSPasteboard) -> Option<Capture> {
    let types: Vec<String> = pb.types().map(|t| t.iter().map(|s| s.to_string()).collect()).unwrap_or_default();
    let has = |t: &str| types.iter().any(|x| x == t);
    if has(TRANSIENT) {
        return None;
    }
    if has(CONCEALED) {
        return Some(Capture { secret_hint: true, ..Default::default() });
    }
    let ty = |s: &str| NSString::from_str(s);
    if has("public.file-url") {
        let files: Vec<String> = pb
            .pasteboardItems()
            .map(|items| items.iter().filter_map(|i| i.stringForType(&ty("public.file-url"))).map(|s| s.to_string()).take(1000).collect())
            .unwrap_or_default();
        if !files.is_empty() {
            return Some(Capture { files, ..Default::default() });
        }
    }
    if has("public.png") {
        let png = pb.dataForType(&ty("public.png"))?.to_vec();
        return inspect_png(png).map(|img| Capture { image: Some(img), ..Default::default() });
    }
    let text = pb.stringForType(&ty("public.utf8-plain-text")).map(|s| s.to_string()).filter(|t| !t.is_empty())?;
    let html = if has("public.html") { pb.stringForType(&ty("public.html")).map(|s| s.to_string()) } else { None };
    Some(Capture { text: Some(text), html, ..Default::default() })
}
