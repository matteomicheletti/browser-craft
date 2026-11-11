//! Applies [`super::resolve_url`] to WebKitGTK views.
//!
//! WebKitGTK has one User-Agent per view (its `WebKitSettings`), so it is
//! switched as the view moves from site to site, in the navigation policy
//! decision, before the request leaves: WebKit builds a main-frame request's
//! header after that decision, so the first request to the new host carries
//! the new string, and `navigator.userAgent` follows (both read the same
//! setting).
//!
//! The decision does not say whether it is for the main frame or an
//! iframe, and an iframe must not change its page's string. So the switch
//! happens at the decision only for navigations that are the main frame's
//! (the view's address is already the target: typed addresses, reloads,
//! server redirects) or most likely are (link clicks, form submissions,
//! back/forward), and it is undone if the response turns out to be an
//! iframe's. A main-frame response that still came with the wrong string
//! (a script navigation, `location.href = …`) switches it and, for a GET,
//! loads the page again so the site sees one browser from its first page on.

use std::cell::RefCell;
use std::rc::Rc;

use webkit2gtk::glib::prelude::*;
use webkit2gtk::{
    NavigationPolicyDecision, NavigationPolicyDecisionExt, NavigationType, PolicyDecisionExt, PolicyDecisionType, ResponsePolicyDecision,
    ResponsePolicyDecisionExt, SettingsExt, URIRequestExt, WebView, WebViewExt,
};

use super::{Resolved, resolve_url};

/// What `view` sends now.
pub fn current(view: &WebView) -> Option<Resolved> {
    let settings = WebViewExt::settings(view)?;
    Some(Resolved {
        ua: settings.user_agent().map(|s| s.to_string()).unwrap_or_default(),
        quirks: settings.enables_site_specific_quirks(),
    })
}

/// Makes `view` send `want`. Returns whether anything changed.
pub fn apply(view: &WebView, want: &Resolved) -> bool {
    let Some(settings) = WebViewExt::settings(view) else {
        return false;
    };
    let mut changed = false;
    if settings.user_agent().as_deref() != Some(want.ua.as_str()) {
        settings.set_user_agent(Some(&want.ua));
        changed = true;
    }
    if settings.enables_site_specific_quirks() != want.quirks {
        settings.set_enable_site_specific_quirks(want.quirks);
        changed = true;
    }
    changed
}

/// Per-view bookkeeping of the decision handler.
#[derive(Default)]
struct State {
    /// A switch made for a navigation that may be an iframe's: its URL and
    /// what the view sent before.
    tentative: Option<(String, Resolved)>,
    /// The last main-frame page loaded again with the right string (once).
    reloaded: Option<String>,
}

/// Gives `view` the default User-Agent and switches it per site from now
/// on. `allow(url)` is the browser's navigation policy (`false` refuses the
/// navigation); this handler replaces wry's, which cannot tell the request
/// details apart.
pub fn attach(view: &WebView, allow: impl Fn(String) -> bool + 'static) {
    apply(view, &Resolved::default_ua());
    // Real-time apps (Teams, Meet) need RTCPeerConnection, which WebKitGTK
    // ships switched off (and Arch/Manjaro builds leave out entirely).
    // Camera and microphone go through the site permission prompt.
    if let Some(settings) = WebViewExt::settings(view) {
        settings.set_enable_webrtc(true);
    }
    let state = Rc::new(RefCell::new(State::default()));
    view.connect_decide_policy(move |view, decision, kind| match kind {
        PolicyDecisionType::NavigationAction => {
            let Some(nav) = decision.downcast_ref::<NavigationPolicyDecision>() else {
                return false;
            };
            let Some(mut action) = nav.navigation_action() else {
                return false;
            };
            let Some(uri) = action.request().and_then(|r| r.uri()).map(|u| u.to_string()) else {
                return false;
            };
            if !allow(uri.clone()) {
                decision.ignore();
                return true;
            }
            let main = view.uri().as_deref() == Some(uri.as_str());
            let likely_main = matches!(
                action.navigation_type(),
                NavigationType::LinkClicked
                    | NavigationType::FormSubmitted
                    | NavigationType::BackForward
                    | NavigationType::Reload
                    | NavigationType::FormResubmitted
            );
            let mut state = state.borrow_mut();
            if !main && action.is_redirect() {
                // An iframe's redirect: follow the URL a tentative switch
                // waits for.
                if let Some((pending, _)) = state.tentative.as_mut() {
                    *pending = uri.clone();
                }
            }
            if main || likely_main {
                let want = resolve_url(&uri);
                if let Some(before) = current(view).filter(|now| *now != want) {
                    apply(view, &want);
                    state.tentative = (!main).then(|| (uri.clone(), before));
                }
            }
            decision.use_();
            true
        }
        PolicyDecisionType::Response => {
            let Some(response) = decision.downcast_ref::<ResponsePolicyDecision>() else {
                return false;
            };
            let Some(request) = response.request() else {
                return false;
            };
            let uri = request.uri().map(|u| u.to_string()).unwrap_or_default();
            let mut state = state.borrow_mut();
            if !response.is_main_frame_main_resource() {
                // The tentative switch was for an iframe: put the page's
                // string back.
                if state.tentative.as_ref().is_some_and(|(pending, _)| *pending == uri)
                    && let Some((_, before)) = state.tentative.take()
                {
                    apply(view, &before);
                }
                return false;
            }
            state.tentative = None;
            let want = resolve_url(&uri);
            if current(view).is_some_and(|now| now != want) {
                apply(view, &want);
                let get = request.http_method().is_none_or(|m| m.eq_ignore_ascii_case("GET"));
                if get && state.reloaded.as_deref() != Some(uri.as_str()) {
                    state.reloaded = Some(uri.clone());
                    drop(state);
                    decision.ignore();
                    // After WebKit has dropped this load (a load started
                    // from inside the decision is cancelled with it).
                    let view = view.clone();
                    webkit2gtk::glib::idle_add_local_once(move || view.load_uri(&uri));
                    return true;
                }
            } else {
                state.reloaded = None;
            }
            false
        }
        _ => false,
    });
}
