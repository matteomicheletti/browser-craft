//! The few X11 calls the window code needs that neither winit nor wry
//! offer: moving a tab's web view (wry's X11 container window) into another
//! browser window, and keeping the hover card above the web views.
//!
//! wry embeds each WebKitGTK view in an X11 container window it creates as
//! a child of the browser window (`XCreateSimpleWindow`), wrapped by GDK as
//! a foreign window. Reparenting that container with `XReparentWindow`
//! moves the live page, with its state, to another top-level window. The
//! calls go through GDK's own X connection (the one wry uses), via x11-dl
//! (loaded at run time, as wry does).

use std::os::raw::c_ulong;

use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use winit::window::Window;
use wry::WebView;
use x11_dl::xlib::{Display, Xlib};

#[link(name = "gdk-3")]
unsafe extern "C" {
    fn gdk_x11_window_get_xid(window: *mut gtk::gdk::ffi::GdkWindow) -> c_ulong;
    fn gdk_x11_display_get_xdisplay(display: *mut gtk::gdk::ffi::GdkDisplay) -> *mut Display;
}

/// GDK's X display and the Xlib functions.
fn xlib() -> Option<(Xlib, *mut Display)> {
    use gtk::glib::object::ObjectExt as _;
    use gtk::glib::translate::ToGlibPtr as _;
    let display = gtk::gdk::Display::default()?;
    // Only an X11 display has an X connection (GDK_BACKEND=x11 is forced
    // in main; anything else means no WebKit views to move).
    if !display.type_().name().contains("X11") {
        return None;
    }
    let xlib = Xlib::open().ok()?;
    // SAFETY: `display` is a live GdkX11Display (checked above).
    let dpy = unsafe { gdk_x11_display_get_xdisplay(display.to_glib_none().0) };
    (!dpy.is_null()).then_some((xlib, dpy))
}

/// The X11 id of a winit window.
pub fn window_xid(window: &Window) -> Option<c_ulong> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(h) => Some(h.window),
        RawWindowHandle::Xcb(h) => Some(c_ulong::from(h.window.get())),
        _ => None,
    }
}

/// The X11 id of the container window wry made for `wv`.
pub fn webview_xid(wv: &WebView) -> Option<c_ulong> {
    use gtk::glib::translate::ToGlibPtr as _;
    use gtk::prelude::WidgetExt as _;
    use wry::WebViewExtUnix as _;
    let toplevel = wv.webview().toplevel()?;
    let gdk_window = toplevel.window()?;
    // SAFETY: a realized GTK window of an X11 display has an X11 GdkWindow.
    let xid = unsafe { gdk_x11_window_get_xid(gdk_window.to_glib_none().0) };
    (xid != 0).then_some(xid)
}

/// Moves `wv`'s container window into `parent`. The page keeps running
/// (nothing is reloaded). Returns false when it could not be done.
pub fn reparent_webview(wv: &WebView, parent: &Window) -> bool {
    let (Some(child), Some(parent)) = (webview_xid(wv), window_xid(parent)) else {
        return false;
    };
    let Some((xlib, dpy)) = xlib() else {
        return false;
    };
    // SAFETY: both windows exist on this display; Xlib reports errors
    // asynchronously (to the error handler), never by crashing here.
    unsafe {
        (xlib.XReparentWindow)(dpy, child, parent, 0, 0);
        (xlib.XFlush)(dpy);
    }
    true
}

/// Puts `window` above its sibling windows (the web views' containers).
pub fn raise(window: &Window) {
    let Some(xid) = window_xid(window) else { return };
    let Some((xlib, dpy)) = xlib() else { return };
    // SAFETY: as above.
    unsafe {
        (xlib.XRaiseWindow)(dpy, xid);
        (xlib.XFlush)(dpy);
    }
}
