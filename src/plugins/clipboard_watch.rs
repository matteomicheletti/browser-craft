//! Watches the system clipboard (Linux, GTK on X11 or XWayland) and turns
//! each change into a [`Capture`].
//!
//! GTK's `owner-change` signal is driven by the XFixes selection-owner
//! notification, so the watcher is woken by every new owner: other X11
//! apps, Wayland apps (XWayland mirrors the Wayland clipboard into the X11
//! CLIPBOARD selection), the browser's own web views and fields. No polling.
//! It reads the `TARGETS` first, which tells file lists
//! (`x-special/gnome-copied-files`, `text/uri-list`), images (`image/png`
//! and the other `image/*` types GdkPixbuf reads), HTML and plain text
//! apart, and spots secrets (`x-kde-passwordManagerHint`). The PRIMARY
//! selection (select-to-paste) is not recorded.

use super::clipboard_history::{Capture, CapturedImage, fnv};
use gtk::gdk;
use gtk::glib::object::ObjectExt as _;
use std::cell::Cell;
use std::rc::Rc;

/// Targets that mark a copy as a secret.
const SECRET_TARGETS: &[&str] = &["x-kde-passwordManagerHint", "application/x-nspasteboard-concealed-type"];
/// Images larger than this many pixels are not decoded (and not recorded).
const MAX_PIXELS: u64 = 64 * 1024 * 1024;

pub struct Watcher {
    clipboard: gtk::Clipboard,
    handler: Option<gtk::glib::SignalHandlerId>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if let Some(h) = self.handler.take() {
            self.clipboard.disconnect(h);
        }
    }
}

fn atom(name: &str) -> gdk::Atom {
    gdk::Atom::intern(name)
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

/// `file://` URIs from `x-special/gnome-copied-files` ("copy\nuri\nuri") or
/// `text/uri-list` (comments start with `#`).
pub fn parse_uri_list(data: &str) -> Vec<String> {
    data.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && *l != "copy" && *l != "cut")
        .filter(|l| l.contains("://"))
        .take(1000)
        .map(str::to_string)
        .collect()
}

impl Watcher {
    /// Starts watching the CLIPBOARD selection; `on_capture` runs on the GTK
    /// main thread for each change that had readable content.
    pub fn start(on_capture: impl Fn(Capture) + 'static) -> Option<Self> {
        if !gtk::is_initialized_main_thread() {
            return None;
        }
        let clipboard = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
        let on_capture: Rc<dyn Fn(Capture)> = Rc::new(on_capture);
        // Only the newest change is read to the end.
        let generation = Rc::new(Cell::new(0u64));
        let handler = clipboard.connect_local("owner-change", false, move |args| {
            let clipboard = args.first().and_then(|v| v.get::<gtk::Clipboard>().ok())?;
            let g = generation.get().wrapping_add(1);
            generation.set(g);
            let (on_capture, generation) = (on_capture.clone(), generation.clone());
            clipboard.request_contents(&atom("TARGETS"), move |cb, data| {
                if generation.get() != g {
                    return;
                }
                let targets: Vec<String> = data.targets().unwrap_or_default().iter().map(|a| a.name().to_string()).collect();
                read(cb, &targets, on_capture, generation, g);
            });
            None
        });
        Some(Watcher { clipboard, handler: Some(handler) })
    }
}

fn read(cb: &gtk::Clipboard, targets: &[String], on_capture: Rc<dyn Fn(Capture)>, generation: Rc<Cell<u64>>, g: u64) {
    let has = |t: &str| targets.iter().any(|x| x == t);
    if targets.iter().any(|t| SECRET_TARGETS.contains(&t.as_str())) {
        on_capture(Capture { secret_hint: true, ..Default::default() });
        return;
    }
    let current = move || generation.get() == g;
    let files_target = ["x-special/gnome-copied-files", "text/uri-list"].into_iter().find(|t| has(t));
    if let Some(t) = files_target {
        cb.request_contents(&atom(t), move |_, data| {
            if !current() {
                return;
            }
            let text = String::from_utf8_lossy(&data.data()).into_owned();
            let files = parse_uri_list(&text);
            if !files.is_empty() {
                on_capture(Capture { files, ..Default::default() });
            }
        });
        return;
    }
    if has("image/png") {
        cb.request_contents(&atom("image/png"), move |_, data| {
            if current()
                && let Some(img) = inspect_png(data.data())
            {
                on_capture(Capture { image: Some(img), ..Default::default() });
            }
        });
        return;
    }
    if targets.iter().any(|t| t.starts_with("image/")) {
        cb.request_image(move |_, pixbuf| {
            let png = pixbuf.and_then(|p| p.save_to_bufferv("png", &[]).ok());
            if current()
                && let Some(img) = png.and_then(inspect_png)
            {
                on_capture(Capture { image: Some(img), ..Default::default() });
            }
        });
        return;
    }
    let html = has("text/html");
    cb.request_text(move |cb, text| {
        let Some(text) = text.map(str::to_string).filter(|t| !t.is_empty()) else { return };
        if !current() {
            return;
        }
        if html {
            let current = current.clone();
            cb.request_contents(&atom("text/html"), move |_, data| {
                if !current() {
                    return;
                }
                let bytes = data.data();
                // Firefox and others offer UTF-16 HTML.
                let html = if bytes.starts_with(&[0xff, 0xfe]) {
                    let units: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
                    String::from_utf16_lossy(&units)
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                };
                on_capture(Capture { text: Some(text), html: Some(html), ..Default::default() });
            });
        } else {
            on_capture(Capture { text: Some(text), ..Default::default() });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_lists() {
        assert_eq!(parse_uri_list("copy\nfile:///a/b.txt\nfile:///c%20d.png"), ["file:///a/b.txt", "file:///c%20d.png"]);
        assert_eq!(parse_uri_list("# comment\r\nfile:///x\r\n\r\n"), ["file:///x"]);
        assert!(parse_uri_list("just text").is_empty());
    }

    #[test]
    fn png_inspection_hashes_pixels() {
        let img = image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img.clone()).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let a = inspect_png(png).unwrap();
        assert_eq!((a.width, a.height), (3, 2));
        assert_eq!(a.pixels_hash, fnv(&[img.as_raw()]));
        assert!(inspect_png(b"not a png".to_vec()).is_none());
    }
}
