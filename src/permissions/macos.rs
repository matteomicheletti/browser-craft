//! WKWebView's camera, microphone and screen requests: answered at once
//! from a saved decision, otherwise held until the user answers the prompt
//! bar, like `webkit.rs` does on Linux.
//!
//! wry's web view delegate grants every camera and microphone request and
//! does not answer screen sharing at all. This replaces its camera and
//! microphone method and adds the screen one (WebKit's private
//! `_webView:requestDisplayCapturePermissionForOrigin:…`), on wry's delegate
//! class, once. The web views are told apart by their address: each tab's
//! web view is registered with `attach`.
//!
//! Requests are keyed by the asking frame's origin (an embedded call frame
//! is asked about as itself). Requests still waiting when the tab gets a
//! new web view are denied.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use block2::{Block, RcBlock};
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2_foundation::NSString;
use winit::event_loop::EventLoopProxy;

use super::{Kind, Prompt};
use crate::browser::{TabMessage, UserMessage};

/// `WKPermissionDecision`.
const GRANT: isize = 1;
const DENY: isize = 2;
/// `WKDisplayCapturePermissionDecision`.
const SCREEN_DENY: isize = 0;
const SCREEN_PROMPT: isize = 1;
/// At most this many requests wait per tab; more are denied.
const MAX_PENDING_PER_TAB: usize = 8;

enum Handler {
    Media(RcBlock<dyn Fn(isize)>),
    Screen(RcBlock<dyn Fn(isize)>),
}

impl Handler {
    fn decide(&self, allow: bool) {
        match self {
            Handler::Media(h) => h.call((if allow { GRANT } else { DENY },)),
            Handler::Screen(h) => h.call((if allow { SCREEN_PROMPT } else { SCREEN_DENY },)),
        }
    }
}

struct Pending {
    prompt: Prompt,
    tab: u64,
    handler: Handler,
}

thread_local! {
    static PENDING: RefCell<Vec<Pending>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    /// Web view address → its tab.
    static TABS: RefCell<HashMap<usize, u64>> = RefCell::new(HashMap::new());
    static PROXY: RefCell<Option<EventLoopProxy<UserMessage>>> = const { RefCell::new(None) };
}

/// Registers tab `tab`'s web view (and installs the delegate methods once).
pub fn attach(wv: &wry::WebView, tab: u64, proxy: EventLoopProxy<UserMessage>) {
    use wry::WebViewExtMacOS as _;
    install(&wv.webview());
    // A new web view for the tab: the old one is gone, and so are its requests.
    detach(tab);
    let key = std::ptr::from_ref(&*wv.webview()) as usize;
    TABS.with(|t| t.borrow_mut().insert(key, tab));
    PROXY.with(|p| *p.borrow_mut() = Some(proxy));
}

/// Forgets tab `tab`'s web view and denies what it was waiting for.
fn detach(tab: u64) {
    TABS.with(|t| t.borrow_mut().retain(|_, v| *v != tab));
    let gone: Vec<Pending> = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let (gone, keep) = std::mem::take(&mut *p).into_iter().partition(|e| e.tab == tab);
        *p = keep;
        gone
    });
    for e in &gone {
        e.handler.decide(false);
    }
    if !gone.is_empty() {
        notify(tab);
    }
}

pub fn prompt_for(tab: u64) -> Option<Prompt> {
    PENDING.with(|p| p.borrow().iter().find(|e| e.tab == tab).map(|e| e.prompt.clone()))
}

/// Answers request `id` (and the same question asked again meanwhile);
/// returns what it asked for.
pub fn answer(id: u64, allow: bool) -> Option<Prompt> {
    let entry = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let i = p.iter().position(|e| e.prompt.id == id)?;
        Some(p.remove(i))
    })?;
    entry.handler.decide(allow);
    let same: Vec<Pending> = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let (same, keep) = std::mem::take(&mut *p).into_iter().partition(|e| {
            e.tab == entry.tab && e.prompt.host == entry.prompt.host && e.prompt.kinds.iter().all(|k| entry.prompt.kinds.contains(k))
        });
        *p = keep;
        same
    });
    for e in same {
        e.handler.decide(allow);
    }
    Some(entry.prompt)
}

/// Answers the waiting requests that saved decisions now settle.
pub fn settle_saved() {
    let store = super::store();
    let settled: Vec<(Pending, bool)> = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let mut out = Vec::new();
        let mut keep = Vec::new();
        for e in std::mem::take(&mut *p) {
            match store.decide(&e.prompt.host, &e.prompt.kinds) {
                Some(allow) => out.push((e, allow)),
                None => keep.push(e),
            }
        }
        *p = keep;
        out
    });
    for (e, allow) in settled {
        e.handler.decide(allow);
    }
}

fn notify(tab: u64) {
    PROXY.with(|p| {
        if let Some(proxy) = &*p.borrow() {
            let _ = proxy.send_event(UserMessage::Tab(tab, TabMessage::PermissionPrompt));
        }
    });
}

/// The host of a `WKSecurityOrigin` (empty for `file:` and the like).
fn origin_host(origin: *mut AnyObject) -> Option<String> {
    // SAFETY: WebKit passes a live WKSecurityOrigin; `protocol` and `host`
    // return NSStrings (or nil).
    let origin = unsafe { origin.as_ref() }?;
    let protocol: Option<objc2::rc::Retained<NSString>> = unsafe { msg_send![origin, protocol] };
    let host: Option<objc2::rc::Retained<NSString>> = unsafe { msg_send![origin, host] };
    let protocol = protocol?.to_string();
    let host = host?.to_string().trim_end_matches('.').to_ascii_lowercase();
    // Only web pages may ask (and localhost, for tests and local apps).
    (matches!(protocol.as_str(), "https" | "http") && !host.is_empty()).then_some(host)
}

/// The request of `web_view` for `kinds`: answered now, or held.
fn request(web_view: *mut AnyObject, origin: *mut AnyObject, kinds: Vec<Kind>, handler: Handler) {
    let tab = TABS.with(|t| t.borrow().get(&(web_view as usize)).copied());
    if std::env::var_os("BROWSECRAFT_DEBUG_PERMISSIONS").is_some() {
        let known = TABS.with(|t| t.borrow().len());
        eprintln!("browsecraft: request from tab {tab:?} ({known} web views known), host {:?}, {kinds:?}", origin_host(origin));
    }
    let (Some(tab), Some(host)) = (tab, origin_host(origin)) else {
        handler.decide(false);
        return;
    };
    match super::store().decide(&host, &kinds) {
        Some(allow) => handler.decide(allow),
        None => {
            let waiting = PENDING.with(|p| p.borrow().iter().filter(|e| e.tab == tab).count());
            if waiting >= MAX_PENDING_PER_TAB {
                handler.decide(false);
                return;
            }
            let id = NEXT_ID.with(|n| {
                let id = n.get();
                n.set(id + 1);
                id
            });
            PENDING.with(|q| q.borrow_mut().push(Pending { prompt: Prompt { id, host, kinds }, tab, handler }));
            notify(tab);
        }
    }
}

unsafe extern "C-unwind" fn media(
    _this: *mut AnyObject,
    _cmd: Sel,
    web_view: *mut AnyObject,
    origin: *mut AnyObject,
    _frame: *mut AnyObject,
    capture_type: isize,
    handler: *mut Block<dyn Fn(isize)>,
) {
    // SAFETY: WebKit passes a live decision block; copying keeps it.
    let Some(handler) = (unsafe { RcBlock::copy(handler) }) else { return };
    if std::env::var_os("BROWSECRAFT_DEBUG_PERMISSIONS").is_some() {
        eprintln!("browsecraft: media capture request, type {capture_type}");
    }
    // WKMediaCaptureType: camera, microphone, camera and microphone.
    let kinds = match capture_type {
        0 => vec![Kind::Camera],
        1 => vec![Kind::Microphone],
        _ => vec![Kind::Microphone, Kind::Camera],
    };
    request(web_view, origin, kinds, Handler::Media(handler));
}

unsafe extern "C-unwind" fn screen(
    _this: *mut AnyObject,
    _cmd: Sel,
    web_view: *mut AnyObject,
    origin: *mut AnyObject,
    _frame: *mut AnyObject,
    _system_audio: Bool,
    handler: *mut Block<dyn Fn(isize)>,
) {
    // SAFETY: as above.
    let Some(handler) = (unsafe { RcBlock::copy(handler) }) else { return };
    request(web_view, origin, vec![Kind::Screen], Handler::Screen(handler));
}

/// Puts `media` and `screen` on the class of `web_view`'s UI delegate
/// (wry's, whose registered name objc2 makes unique), once.
fn install(web_view: &objc2_web_kit::WKWebView) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: `UIDelegate` returns the web view's delegate (or nil).
        let delegate: Option<objc2::rc::Retained<AnyObject>> = unsafe { msg_send![web_view, UIDelegate] };
        let Some(delegate) = delegate else {
            eprintln!("browsecraft: the web view has no delegate; camera and microphone requests are not asked");
            return;
        };
        let class: &AnyClass = delegate.class();
        let class = std::ptr::from_ref(class).cast_mut();
        let bool_type = if cfg!(target_arch = "aarch64") { "B" } else { "c" };
        let media_types = std::ffi::CString::new("v@:@@@q@?").unwrap_or_default();
        let screen_types = std::ffi::CString::new(format!("v@:@@@{bool_type}@?")).unwrap_or_default();
        type MediaFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject, *mut AnyObject, isize, *mut Block<dyn Fn(isize)>);
        type ScreenFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject, *mut AnyObject, Bool, *mut Block<dyn Fn(isize)>);
        // SAFETY: the functions have the methods' C signatures (self, _cmd,
        // then the five arguments). Replacing wry's camera method is the
        // point: it granted everything.
        if std::env::var_os("BROWSECRAFT_DEBUG_PERMISSIONS").is_some() {
            eprintln!("browsecraft: permission methods installed on {}", delegate.class().name().to_string_lossy());
        }
        unsafe {
            let media_imp: Imp = std::mem::transmute(media as MediaFn);
            let screen_imp: Imp = std::mem::transmute(screen as ScreenFn);
            objc2::ffi::class_replaceMethod(
                class,
                objc2::sel!(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:),
                media_imp,
                media_types.as_ptr(),
            );
            objc2::ffi::class_replaceMethod(
                class,
                objc2::sel!(_webView:requestDisplayCapturePermissionForOrigin:initiatedByFrame:withSystemAudio:decisionHandler:),
                screen_imp,
                screen_types.as_ptr(),
            );
        }
    });
}
