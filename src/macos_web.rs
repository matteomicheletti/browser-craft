//! macOS: pictures of pages (WKWebView snapshots) and the hover card's
//! native view, the pieces Linux gets from WebKitGTK and X11.

use std::ffi::c_void;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly as _};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSView, NSWindowOrderingMode};
use objc2_foundation::{NSDictionary, NSError, NSPoint, NSRect, NSSize};
use objc2_web_kit::WKSnapshotConfiguration;
use winit::event_loop::EventLoopProxy;
use winit::raw_window_handle::{
    AppKitWindowHandle, DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle, WindowHandle,
};

use crate::browser::{TabMessage, UserMessage};

/// Asks WebKit for a picture of the page (`TabMessage::Thumbnail`), scaled
/// to at most `thumbnails::MAX_WIDTH` wide and cropped to the top of the page.
pub fn request_thumbnail(wv: &wry::WebView, proxy: EventLoopProxy<UserMessage>, tab: u64) {
    use wry::WebViewExtMacOS as _;
    let Some(mtm) = MainThreadMarker::new() else { return };
    let view = wv.webview();
    // SAFETY: a default configuration (the visible page), on the main thread.
    let config = unsafe { WKSnapshotConfiguration::new(mtm) };
    let block = RcBlock::new(move |image: *mut NSImage, _error: *mut NSError| {
        // SAFETY: WebKit hands a valid image (or null) for the call.
        let Some(image) = (unsafe { image.as_ref() }) else { return };
        if let Some((w, h, rgba)) = thumbnail_rgba(image) {
            let _ = proxy.send_event(UserMessage::Tab(tab, TabMessage::Thumbnail { w, h, rgba, save: true }));
        }
    });
    // SAFETY: a configuration and a block with the signature WebKit expects.
    unsafe { view.takeSnapshotWithConfiguration_completionHandler(Some(&config), &block) };
}

/// Asks WebKit for a full-size picture of the visible page
/// (`TabMessage::Snapshot`), drawn in its place while a menu covers it. An
/// empty picture is sent when WebKit has none, so the caller stops waiting.
pub fn request_snapshot(wv: &wry::WebView, proxy: EventLoopProxy<UserMessage>, tab: u64) {
    use wry::WebViewExtMacOS as _;
    let Some(mtm) = MainThreadMarker::new() else { return };
    let view = wv.webview();
    // SAFETY: a default configuration (the visible page), on the main thread.
    let config = unsafe { WKSnapshotConfiguration::new(mtm) };
    let block = RcBlock::new(move |image: *mut NSImage, _error: *mut NSError| {
        // SAFETY: WebKit hands a valid image (or null) for the call.
        let (w, h, rgba) = unsafe { image.as_ref() }.and_then(image_rgba).unwrap_or((0, 0, Vec::new()));
        let _ = proxy.send_event(UserMessage::Tab(tab, TabMessage::Snapshot { w, h, rgba }));
    });
    // SAFETY: a configuration and a block with the signature WebKit expects.
    unsafe { view.takeSnapshotWithConfiguration_completionHandler(Some(&config), &block) };
}

/// An image's pixels, through PNG (the `image` crate reads it).
fn image_rgba(image: &NSImage) -> Option<(usize, usize, Vec<u8>)> {
    let tiff = image.TIFFRepresentation()?;
    let rep = NSBitmapImageRep::imageRepWithData(&tiff)?;
    // SAFETY: an empty property dictionary.
    let png = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
    let img = image::load_from_memory_with_format(&png.to_vec(), image::ImageFormat::Png).ok()?.to_rgba8();
    Some((img.width() as usize, img.height() as usize, img.into_raw()))
}

/// The snapshot as a thumbnail: through PNG (the `image` crate reads it),
/// cropped and scaled with a good filter.
fn thumbnail_rgba(image: &NSImage) -> Option<(usize, usize, Vec<u8>)> {
    let (w, h, rgba) = image_rgba(image)?;
    let img = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
    let (tw, th, crop_h) = crate::thumbnails::target_size(w, h);
    if tw == 0 || crop_h == 0 {
        return None;
    }
    let top = image::imageops::crop_imm(&img, 0, 0, w as u32, crop_h as u32).to_image();
    let small = image::imageops::resize(&top, tw as u32, th as u32, image::imageops::FilterType::Triangle);
    Some((tw, th, small.into_raw()))
}

/// The browser's own title bar on macOS: a normal titled window whose title
/// bar is transparent, untitled and without the traffic lights, with the
/// content drawn under it. A borderless window (winit's "no decorations")
/// is not a real window to macOS: window managers such as Magnet and the
/// system's own zoom left the height of a title bar empty above it, and a
/// click did not bring it to the front. `custom` false restores the
/// system's title bar.
pub fn set_custom_titlebar(window: &winit::window::Window, custom: bool) {
    use objc2_app_kit::{NSWindowButton, NSWindowStyleMask, NSWindowTitleVisibility};
    if MainThreadMarker::new().is_none() {
        return;
    }
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
    // SAFETY: winit's handle points at its live NSView.
    let Some(view) = (unsafe { Retained::retain(h.ns_view.as_ptr().cast::<NSView>()) }) else { return };
    let Some(ns_window) = view.window() else { return };
    let mut mask = ns_window.styleMask();
    if custom {
        mask |= NSWindowStyleMask::FullSizeContentView;
    } else {
        mask &= !NSWindowStyleMask::FullSizeContentView;
    }
    ns_window.setStyleMask(mask);
    ns_window.setTitlebarAppearsTransparent(custom);
    ns_window.setTitleVisibility(if custom { NSWindowTitleVisibility::Hidden } else { NSWindowTitleVisibility::Visible });
    for b in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
        if let Some(button) = ns_window.standardWindowButton(b) {
            button.setHidden(custom);
        }
    }
}

/// A native view as a softbuffer target.
#[derive(Clone)]
pub struct ViewHandle(NonNull<c_void>);

impl HasWindowHandle for ViewHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = RawWindowHandle::AppKit(AppKitWindowHandle::new(self.0));
        // SAFETY: the view lives as long as the card that owns this handle.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

impl HasDisplayHandle for ViewHandle {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(DisplayHandle::appkit())
    }
}

/// The hover card's view: a layer-backed subview of the browser window's
/// view, kept above the web views (which are its siblings).
pub struct CardView {
    view: Retained<NSView>,
    parent: Retained<NSView>,
}

impl CardView {
    /// A hidden view inside `parent` (winit's view of the browser window).
    pub fn new(parent: &winit::window::Window) -> Option<(Self, ViewHandle)> {
        let mtm = MainThreadMarker::new()?;
        let RawWindowHandle::AppKit(h) = parent.window_handle().ok()?.as_raw() else { return None };
        // SAFETY: winit's handle points at its live NSView.
        let parent = unsafe { Retained::retain(h.ns_view.as_ptr().cast::<NSView>()) }?;
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0));
        let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
        view.setHidden(true);
        parent.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Above, None);
        let handle = ViewHandle(NonNull::from(&*view).cast());
        Some((CardView { view, parent }, handle))
    }

    /// Places the view at `pos` (`w × h`), in the parent's pixels.
    pub fn place(&self, pos: (i32, i32), w: u32, h: u32, scale: f32) {
        let s = f64::from(scale.max(0.5));
        let (x, y, w, h) = (f64::from(pos.0) / s, f64::from(pos.1) / s, f64::from(w) / s, f64::from(h) / s);
        let y = if self.parent.isFlipped() { y } else { self.parent.bounds().size.height - y - h };
        self.view.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)));
    }

    pub fn set_visible(&self, visible: bool) {
        self.view.setHidden(!visible);
    }

    /// Above every sibling again (a web view shown later covers it).
    pub fn raise(&self) {
        self.parent.addSubview_positioned_relativeTo(&self.view, NSWindowOrderingMode::Above, None);
    }
}

impl Drop for CardView {
    fn drop(&mut self) {
        self.view.removeFromSuperview();
    }
}
