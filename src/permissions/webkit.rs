//! WebKitGTK's permission requests (`permission-request`): answered at once
//! from a saved decision, otherwise held until the user answers the prompt
//! bar. Requests still waiting when their tab starts another page or its
//! view is destroyed are denied.
//!
//! Also checks, on call sites (Teams, Meet, ...), whether this WebKitGTK
//! build has WebRTC at all.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use gtk::prelude::*;
use webkit2gtk::glib::translate::ToGlibPtr;
use webkit2gtk::{
    DeviceInfoPermissionRequest, GeolocationPermissionRequest, LoadEvent, NotificationPermissionRequest, PermissionRequest,
    PermissionRequestExt, UserMediaPermissionRequest, UserMediaPermissionRequestExt, WebView, WebViewExt,
    WebsiteDataAccessPermissionRequest, WebsiteDataAccessPermissionRequestExt,
};
use winit::event_loop::EventLoopProxy;

use super::{Kind, Prompt};
use crate::browser::{TabMessage, UserMessage};

struct Pending {
    prompt: Prompt,
    tab: u64,
    request: PermissionRequest,
}

thread_local! {
    static PENDING: RefCell<Vec<Pending>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    /// Tabs whose current page the user let use the camera or microphone
    /// (once, without remembering): device names follow that.
    static MEDIA_GRANTED: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
    /// Tabs between a load's start and its commit. The view's URI is then
    /// the address being loaded while the page that asks is still the old
    /// one: a request is never judged by the next site's decisions.
    static PROVISIONAL: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
}

fn set_provisional(tab: u64, on: bool) {
    PROVISIONAL.with(|p| {
        let mut p = p.borrow_mut();
        if on {
            p.insert(tab);
        } else {
            p.remove(&tab);
        }
    });
}

/// At most this many requests wait per tab; more are denied.
const MAX_PENDING_PER_TAB: usize = 8;

pub fn prompt_for(tab: u64) -> Option<Prompt> {
    PENDING.with(|p| p.borrow().iter().find(|e| e.tab == tab).map(|e| e.prompt.clone()))
}

/// Answers request `id`; returns what it asked for.
pub fn answer(id: u64, allow: bool) -> Option<Prompt> {
    let entry = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let i = p.iter().position(|e| e.prompt.id == id)?;
        Some(p.remove(i))
    })?;
    if allow {
        grant_media(entry.tab, &entry.prompt.kinds);
        entry.request.allow();
    } else {
        entry.request.deny();
    }
    // The same question asked again meanwhile (pages often ask twice) gets
    // the same answer.
    let same: Vec<Pending> = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let (same, keep) = std::mem::take(&mut *p).into_iter().partition(|e| {
            e.tab == entry.tab && e.prompt.host == entry.prompt.host && e.prompt.kinds.iter().all(|k| entry.prompt.kinds.contains(k))
        });
        *p = keep;
        same
    });
    for e in same {
        if allow {
            e.request.allow();
        } else {
            e.request.deny();
        }
    }
    Some(entry.prompt)
}

/// Answers the waiting requests that saved decisions now settle (after the
/// user saved one, or changed them in the palette or settings).
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
        if allow {
            grant_media(e.tab, &e.prompt.kinds);
            e.request.allow();
        } else {
            e.request.deny();
        }
    }
}

fn grant_media(tab: u64, kinds: &[Kind]) {
    if kinds.iter().any(|k| matches!(k, Kind::Camera | Kind::Microphone)) {
        MEDIA_GRANTED.with(|g| g.borrow_mut().insert(tab));
    }
}

/// Denies every request of `tab` (it left its page or closed). Returns
/// whether there were any.
fn cancel_tab(tab: u64) -> bool {
    MEDIA_GRANTED.with(|g| g.borrow_mut().remove(&tab));
    let gone: Vec<Pending> = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let (gone, keep) = std::mem::take(&mut *p).into_iter().partition(|e| e.tab == tab);
        *p = keep;
        gone
    });
    for e in &gone {
        e.request.deny();
    }
    !gone.is_empty()
}

/// What a request asks for, and for which site. `None`: not one the
/// browser handles (WebKit's default applies).
fn classify(view: &WebView, request: &PermissionRequest) -> Option<Classified> {
    let page_host = view.uri().and_then(|u| crate::useragent::url_host(&u));
    if let Some(r) = request.downcast_ref::<UserMediaPermissionRequest>() {
        // SAFETY: `r` is a live WebKitUserMediaPermissionRequest.
        let display = unsafe { webkit2gtk::ffi::webkit_user_media_permission_is_for_display_device(r.to_glib_none().0) } != 0;
        let mut kinds = Vec::new();
        if display {
            kinds.push(Kind::Screen);
        } else {
            if r.is_for_audio_device() {
                kinds.push(Kind::Microphone);
            }
            if r.is_for_video_device() {
                kinds.push(Kind::Camera);
            }
        }
        return Some(Classified::Ask(page_host, kinds));
    }
    if request.is::<DeviceInfoPermissionRequest>() {
        return Some(Classified::DeviceInfo(page_host));
    }
    if request.is::<GeolocationPermissionRequest>() {
        return Some(Classified::Ask(page_host, vec![Kind::Location]));
    }
    if request.is::<NotificationPermissionRequest>() {
        return Some(Classified::Ask(page_host, vec![Kind::Notifications]));
    }
    if let Some(r) = request.downcast_ref::<WebsiteDataAccessPermissionRequest>() {
        // Keyed by the embedded site that wants its own cookies.
        let host = r.requesting_domain().map(|d| d.trim_end_matches('.').to_ascii_lowercase()).filter(|d| !d.is_empty());
        return Some(Classified::Ask(host.filter(|_| page_host.is_some()), vec![Kind::Storage]));
    }
    // Newer than the bindings (WebKitGTK 2.42+): known by its type name.
    if request.type_().name() == "WebKitClipboardPermissionRequest" {
        return Some(Classified::Ask(page_host, vec![Kind::Clipboard]));
    }
    None
}

enum Classified {
    /// `None` host: not an http(s) page, denied.
    Ask(Option<String>, Vec<Kind>),
    DeviceInfo(Option<String>),
}

/// Hooks `view` (the web view of tab `tab`).
pub fn attach(view: &WebView, tab: u64, proxy: EventLoopProxy<UserMessage>) {
    let p = proxy.clone();
    view.connect_permission_request(move |view, request| {
        let Some(classified) = classify(view, request) else {
            return false;
        };
        // Asked by the page that is being left: its host is not the URI's.
        if PROVISIONAL.with(|p| p.borrow().contains(&tab)) {
            request.deny();
            return true;
        }
        let store = super::store();
        match classified {
            Classified::DeviceInfo(host) => {
                let granted = MEDIA_GRANTED.with(|g| g.borrow().contains(&tab));
                if host.is_some_and(|h| granted || store.device_info(&h)) {
                    request.allow();
                } else {
                    request.deny();
                }
            }
            Classified::Ask(None, _) => request.deny(),
            Classified::Ask(Some(host), kinds) => match store.decide(&host, &kinds) {
                Some(true) => {
                    grant_media(tab, &kinds);
                    request.allow();
                }
                Some(false) => request.deny(),
                None => {
                    let waiting = PENDING.with(|p| p.borrow().iter().filter(|e| e.tab == tab).count());
                    if waiting >= MAX_PENDING_PER_TAB {
                        request.deny();
                        return true;
                    }
                    let id = NEXT_ID.with(|n| {
                        let id = n.get();
                        n.set(id + 1);
                        id
                    });
                    PENDING.with(|q| {
                        q.borrow_mut().push(Pending { prompt: Prompt { id, host, kinds }, tab, request: request.clone() })
                    });
                    let _ = p.send_event(UserMessage::Tab(tab, TabMessage::PermissionPrompt));
                }
            },
        }
        true
    });
    let p = proxy.clone();
    view.connect_load_changed(move |view, event| match event {
        LoadEvent::Started => {
            set_provisional(tab, true);
            if cancel_tab(tab) {
                let _ = p.send_event(UserMessage::Tab(tab, TabMessage::PermissionPrompt));
            }
        }
        // Committed: the URI is the new page's. Finished without a commit
        // (a failed or stopped load): the URI is the old page's again.
        LoadEvent::Committed => set_provisional(tab, false),
        LoadEvent::Finished => {
            set_provisional(tab, false);
            check_webrtc(view, tab, &p);
        }
        _ => {}
    });
    let p = proxy;
    view.connect_destroy(move |_| {
        set_provisional(tab, false);
        if cancel_tab(tab) {
            let _ = p.send_event(UserMessage::Tab(tab, TabMessage::PermissionPrompt));
        }
    });
}

/// On a call site, finds out (once per run) whether the page has
/// `RTCPeerConnection`, and reports when it does not.
fn check_webrtc(view: &WebView, tab: u64, proxy: &EventLoopProxy<UserMessage>) {
    let Some(host) = view.uri().and_then(|u| crate::useragent::url_host(&u)) else {
        return;
    };
    if !super::is_call_host(&host) {
        return;
    }
    let report = move |proxy: &EventLoopProxy<UserMessage>| {
        let _ = proxy.send_event(UserMessage::Tab(tab, TabMessage::WebRtcMissing));
    };
    match super::webrtc_known() {
        Some(true) => {}
        Some(false) => report(proxy),
        None => {
            // In an isolated world: the page's own scripts cannot fake it.
            let proxy = proxy.clone();
            view.evaluate_javascript(
                "if (typeof RTCPeerConnection === 'undefined') throw 'bc-no-webrtc'; true",
                Some("browsecraft-webrtc-check"),
                None,
                None::<&webkit2gtk::gio::Cancellable>,
                move |result| match result {
                    Ok(_) => super::set_webrtc(true),
                    Err(e) if e.message().contains("bc-no-webrtc") => {
                        super::set_webrtc(false);
                        report(&proxy);
                    }
                    // The page went away meanwhile: ask again next time.
                    Err(_) => {}
                },
            );
        }
    }
}
