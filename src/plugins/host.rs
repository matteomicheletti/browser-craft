//! The privileged bridge of plugin pages (API 1.1): `bc.host` in pages served
//! from `bc-plugin://<id>/` (see `ui_pages`).
//!
//! Wire format: `window.ipc.postMessage("bc-host:" + JSON)` with
//! `{n: nonce, i: call id, m: method, a: args}`. The plugin is not named in
//! the message: it is the host of the sender's address, which WebKit reports
//! (the web view's committed URI), so a page can only ever act as the plugin
//! whose page it is, and a web page (any other scheme) gets nothing. The
//! nonce is a second, separate secret that only the host runtime holds; it is
//! never given to content scripts, which run in web pages' JavaScript world.
//! Each method needs the permission listed in `METHODS`.

use super::permissions::Permission;
use super::registry::Registry;
use super::ui_pages::{self, PageRef};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

pub const PREFIX: &str = "bc-host:";
/// A whole message: images travel as data URLs.
pub const MAX_MESSAGE: usize = 48 * 1024 * 1024;
const MAX_CALLS_PER_SECOND: u32 = 400;
const RUNTIME: &str = include_str!("host.js");

/// The host runtime's secret (one per process).
pub fn nonce() -> &'static str {
    static N: OnceLock<String> = OnceLock::new();
    N.get_or_init(|| {
        let mut b = [0u8; 16];
        if getrandom::fill(&mut b).is_err() {
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(7);
            b = (t ^ 0x5bd1_e995_u128).to_le_bytes();
        }
        b.iter().map(|x| format!("{x:02x}")).collect()
    })
}

/// One method of `bc.host`.
pub struct Method {
    pub wire: &'static str,
    pub js: &'static str,
    pub needs: Option<Permission>,
    pub description: &'static str,
}

pub const METHODS: &[Method] = &[
    Method { wire: "page.info", js: "bc.host.info()", needs: None, description: "The plugin, page, launch command, settings and theme." },
    Method { wire: "page.close", js: "bc.host.close()", needs: None, description: "Close this modal (a tab page closes its tab)." },
    Method { wire: "settings.get", js: "bc.host.settings.get()", needs: None, description: "The plugin's settings (manifest options)." },
    Method { wire: "settings.set", js: "bc.host.settings.set(key, value)", needs: None, description: "Change one of the plugin's settings (checked against the manifest)." },
    Method { wire: "notify", js: "bc.host.notify(title, body)", needs: Some(Permission::Notifications), description: "Show a notice in the browser." },
    Method { wire: "clipboard.list", js: "bc.host.clipboard.list(query, limit)", needs: Some(Permission::ClipboardHistory), description: "History items, newest first, filtered by a search." },
    Method { wire: "clipboard.get", js: "bc.host.clipboard.get(id)", needs: Some(Permission::ClipboardHistory), description: "One item: text, HTML, image address, file list." },
    Method { wire: "clipboard.paste", js: "bc.host.clipboard.paste(id, {plain})", needs: Some(Permission::ClipboardHistory), description: "Close the modal and paste the item where the keyboard was." },
    Method { wire: "clipboard.copy", js: "bc.host.clipboard.copy(id)", needs: Some(Permission::ClipboardHistory), description: "Put the item back on the clipboard." },
    Method { wire: "clipboard.delete", js: "bc.host.clipboard.remove(id)", needs: Some(Permission::ClipboardHistory), description: "Delete an item." },
    Method { wire: "clipboard.pin", js: "bc.host.clipboard.pin(id, pinned)", needs: Some(Permission::ClipboardHistory), description: "Pin (never evicted) or unpin an item." },
    Method { wire: "clipboard.clear", js: "bc.host.clipboard.clear({pinnedToo})", needs: Some(Permission::ClipboardHistory), description: "Delete every item (pinned ones only when asked)." },
    Method { wire: "clipboard.usage", js: "bc.host.clipboard.usage()", needs: Some(Permission::ClipboardHistory), description: "Items and bytes used, and the limits." },
    Method { wire: "screenshot.get", js: "bc.host.screenshot.get()", needs: Some(Permission::ScreenshotEdit), description: "The screenshot this page was opened with: {url, width, height, name, mode}." },
    Method { wire: "screenshot.copy", js: "bc.host.screenshot.copyImage(pngDataUrl)", needs: Some(Permission::ScreenshotEdit), description: "Put a PNG on the system clipboard." },
    Method { wire: "screenshot.save", js: "bc.host.screenshot.saveFile(name, pngDataUrl, {ask})", needs: Some(Permission::ScreenshotEdit), description: "Save a PNG to Pictures/BrowseCraft (ask: choose where)." },
];

pub fn method(wire: &str) -> Option<&'static Method> {
    METHODS.iter().find(|m| m.wire == wire)
}

/// A well-formed message, not yet authorized.
#[derive(Clone, Debug, PartialEq)]
pub struct HostCall {
    pub nonce: String,
    pub call_id: u64,
    pub wire: String,
    pub args: Value,
}

/// Shape and size only; `authorize` does the rest.
pub fn parse_message(body: &str) -> Option<HostCall> {
    if body.len() > MAX_MESSAGE {
        return None;
    }
    let v: Value = serde_json::from_str(body.strip_prefix(PREFIX)?).ok()?;
    let o = v.as_object()?;
    let nonce = o.get("n")?.as_str()?.to_string();
    let call_id = o.get("i")?.as_u64().filter(|i| *i > 0 && *i < (1 << 53))?;
    let wire = o.get("m")?.as_str()?.to_string();
    let args = o.get("a").cloned().unwrap_or(Value::Null);
    if o.len() > 4 || nonce.len() > 64 || method(&wire).is_none() {
        return None;
    }
    Some(HostCall { nonce, call_id, wire, args })
}

/// A checked request, with typed arguments.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Info,
    Close,
    SettingsGet,
    SettingsSet { key: String, value: Value },
    Notify { title: String, body: String },
    ClipList { query: String, limit: usize },
    ClipGet(u64),
    ClipPaste { id: u64, plain: bool },
    ClipCopy(u64),
    ClipDelete(u64),
    ClipPin { id: u64, pinned: bool },
    ClipClear { keep_pinned: bool },
    ClipUsage,
    ShotGet,
    ShotCopy { png: Vec<u8> },
    ShotSave { name: String, png: Vec<u8>, ask: bool },
}

fn text(a: &Value, key: &str, max: usize) -> Result<String, String> {
    match a.get(key) {
        Some(Value::String(s)) if s.len() <= max => Ok(s.clone()),
        _ => Err(format!("\"{key}\" must be a string of at most {max} bytes")),
    }
}

fn id_arg(a: &Value) -> Result<u64, String> {
    a.get("id").and_then(Value::as_u64).ok_or_else(|| "\"id\" must be an item id".into())
}

fn flag(a: &Value, key: &str) -> bool {
    a.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Decodes a `data:image/png;base64,...` URL and checks it is a PNG.
pub fn png_from_data_url(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let b64 = s.strip_prefix("data:image/png;base64,").ok_or("expected a data:image/png;base64 URL")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|_| "the image data is not valid base64")?;
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("the image is not a PNG".into());
    }
    Ok(bytes)
}

fn request(wire: &str, a: &Value) -> Result<Request, String> {
    Ok(match wire {
        "page.info" => Request::Info,
        "page.close" => Request::Close,
        "settings.get" => Request::SettingsGet,
        "settings.set" => Request::SettingsSet { key: text(a, "key", 64)?, value: a.get("value").cloned().unwrap_or(Value::Null) },
        "notify" => Request::Notify { title: text(a, "title", 200)?, body: text(a, "body", 1000)? },
        "clipboard.list" => Request::ClipList {
            query: text(a, "query", 500).unwrap_or_default(),
            limit: a.get("limit").and_then(Value::as_u64).unwrap_or(200).clamp(1, 1000) as usize,
        },
        "clipboard.get" => Request::ClipGet(id_arg(a)?),
        "clipboard.paste" => Request::ClipPaste { id: id_arg(a)?, plain: flag(a, "plain") },
        "clipboard.copy" => Request::ClipCopy(id_arg(a)?),
        "clipboard.delete" => Request::ClipDelete(id_arg(a)?),
        "clipboard.pin" => Request::ClipPin { id: id_arg(a)?, pinned: flag(a, "pinned") },
        "clipboard.clear" => Request::ClipClear { keep_pinned: a.get("keep_pinned").and_then(Value::as_bool).unwrap_or(true) },
        "clipboard.usage" => Request::ClipUsage,
        "screenshot.get" => Request::ShotGet,
        "screenshot.copy" => Request::ShotCopy { png: png_from_data_url(&text(a, "data", MAX_MESSAGE)?)? },
        "screenshot.save" => Request::ShotSave {
            name: text(a, "name", 200).unwrap_or_default(),
            png: png_from_data_url(&text(a, "data", MAX_MESSAGE)?)?,
            ask: flag(a, "ask"),
        },
        _ => return Err(format!("unknown method \"{wire}\"")),
    })
}

/// Settles the page's promise for call `id`.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply(pub u64);

impl Reply {
    pub fn ok(&self, v: &Value) -> String {
        deliver(self.0, true, v)
    }

    pub fn err(&self, msg: &str) -> String {
        deliver(self.0, false, &Value::String(msg.into()))
    }
}

fn deliver(id: u64, ok: bool, v: &Value) -> String {
    let json = super::script::js_literal(&Value::String(v.to_string()));
    format!("window.__bcHostDeliver&&window.__bcHostDeliver({id},{ok},{json})")
}

/// Script that raises `bc.host.on(name, ...)` listeners in a plugin page.
pub fn event_script(name: &str, detail: &Value) -> String {
    let js = |v: &Value| super::script::js_literal(v);
    format!("window.__bcHostEvent&&window.__bcHostEvent({},{})", js(&Value::String(name.into())), js(&Value::String(detail.to_string())))
}

/// A request that passed every check.
#[derive(Debug, PartialEq)]
pub struct Authorized {
    pub page: PageRef,
    pub request: Request,
    pub reply: Reply,
}

/// Why a call was refused: `reply` is a script that rejects the page's
/// promise, `None` when the call is dropped silently (bad nonce, not a
/// plugin page: whoever sent it is not owed an answer).
#[derive(Debug, PartialEq)]
pub struct Refused {
    pub reply: Option<String>,
}

fn rate_ok(plugin: &str) -> bool {
    static RATE: Mutex<Option<HashMap<String, (Instant, u32)>>> = Mutex::new(None);
    let mut guard = RATE.lock().unwrap_or_else(PoisonError::into_inner);
    let map = guard.get_or_insert_with(HashMap::new);
    let now = Instant::now();
    let slot = map.entry(plugin.to_string()).or_insert((now, 0));
    if now.duration_since(slot.0) >= Duration::from_secs(1) {
        *slot = (now, 0);
    }
    slot.1 += 1;
    slot.1 <= MAX_CALLS_PER_SECOND
}

/// Checks a call from the page at `sender` (the web view's address as WebKit
/// reports it): nonce, plugin page origin, plugin enabled, permission, rate
/// and arguments.
pub fn authorize(reg: &Registry, sender: &str, call: HostCall) -> Result<Authorized, Refused> {
    if call.nonce != nonce() {
        return Err(Refused { reply: None });
    }
    let Some(page) = ui_pages::parse(sender) else {
        return Err(Refused { reply: None });
    };
    let reply = Reply(call.call_id);
    let fail = |m: &str| Err(Refused { reply: Some(reply.err(m)) });
    let Some(manifest) = reg.get(&page.plugin).and_then(|e| e.manifest()).filter(|_| reg.is_enabled(&page.plugin)) else {
        return fail("plugin is not enabled");
    };
    if manifest.pages.is_empty() {
        return fail("this plugin has no pages");
    }
    let Some(m) = method(&call.wire) else {
        return fail("unknown method");
    };
    if let Some(p) = m.needs
        && !manifest.has(p)
    {
        return fail(&format!("permission \"{}\" is required for {}", p.name(), m.js));
    }
    if !rate_ok(&page.plugin) {
        return fail("too many calls; slow down");
    }
    match request(&call.wire, &call.args) {
        Ok(request) => Ok(Authorized { page, request, reply }),
        Err(e) => fail(&e),
    }
}

// ---- runtime script and theme ----

/// CSS variables for the theme, as `:root { --bc-...: #rrggbb; }`.
pub fn theme_css(theme: &crate::theme::Theme) -> String {
    use crate::theme::css_hex;
    let t = &theme.t;
    let vars = [
        ("background", t.background),
        ("surface", t.surface),
        ("surface2", t.surface2),
        ("border", t.border),
        ("text", t.text),
        ("text-muted", t.text_muted),
        ("accent", t.accent),
        ("accent-text", t.accent_text),
        ("danger", t.danger),
        ("danger-text", t.danger_text),
        ("warning", t.warning),
        ("success", t.success),
        ("field", t.address_bar),
        ("drawer", t.drawer),
        ("page", t.page),
        ("card", t.card),
        ("hover", crate::theme::mix(t.surface, t.text, 0.08)),
        ("selection", crate::theme::mix(t.surface, t.accent, 0.28)),
    ];
    let mut css = String::from(":root{");
    for (k, v) in vars {
        css.push_str(&format!("--bc-{k}:{};", css_hex(v)));
    }
    css.push_str(&format!(
        "--bc-font:system-ui,-apple-system,'Segoe UI','Cantarell','Noto Sans',sans-serif;--bc-radius:8px;color-scheme:{};}}",
        if theme.dark { "dark" } else { "light" }
    ));
    css
}

/// The runtime for web views. `modal` marks the dedicated modal view (Esc
/// closes it).
pub fn runtime_script(modal: bool) -> String {
    let theme = crate::internal::current();
    let data = json!({
        "nonce": nonce(),
        "api": format!("{}.{}", super::permissions::API_MAJOR, super::permissions::API_MINOR),
        "modal": modal,
        "theme": {"css": theme_css(theme), "dark": theme.dark, "name": theme.name},
    });
    RUNTIME.replace("__BC_HOST__", &super::script::js_literal(&data))
}

/// Script that switches a plugin page to `theme` live.
pub fn theme_script(theme: &crate::theme::Theme) -> String {
    let js = |v: Value| super::script::js_literal(&v);
    format!(
        "window.__bcHostTheme&&window.__bcHostTheme({},{},{})",
        js(Value::String(theme_css(theme))),
        theme.dark,
        js(Value::String(theme.name.into()))
    )
}

// ---- blobs (the screenshot being edited) ----

struct Blob {
    plugin: String,
    mime: &'static str,
    bytes: Arc<Vec<u8>>,
    at: Instant,
}

static BLOBS: Mutex<Vec<(String, Blob)>> = Mutex::new(Vec::new());
const MAX_BLOBS: usize = 16;
const MAX_BLOB_BYTES: usize = 256 * 1024 * 1024;

/// Keeps `bytes` for `plugin`'s pages at `/__host/blob/<token>`; the token is
/// unguessable and only valid on that plugin's origin.
pub fn blob_put(plugin: &str, mime: &'static str, bytes: Vec<u8>) -> String {
    let mut b = [0u8; 12];
    let _ = getrandom::fill(&mut b);
    let token: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let mut blobs = BLOBS.lock().unwrap_or_else(PoisonError::into_inner);
    blobs.push((token.clone(), Blob { plugin: plugin.into(), mime, bytes: Arc::new(bytes), at: Instant::now() }));
    // Oldest first out, by count and by size.
    blobs.sort_by_key(|(_, b)| std::cmp::Reverse(b.at));
    let mut total = 0;
    let mut keep = 0;
    for (_, b) in blobs.iter() {
        total += b.bytes.len();
        if keep >= MAX_BLOBS || (keep > 0 && total > MAX_BLOB_BYTES) {
            break;
        }
        keep += 1;
    }
    blobs.truncate(keep);
    token
}

pub fn blob_get(plugin: &str, token: &str) -> Option<(&'static str, Arc<Vec<u8>>)> {
    let blobs = BLOBS.lock().unwrap_or_else(PoisonError::into_inner);
    blobs.iter().find(|(t, b)| t == token && b.plugin == plugin).map(|(_, b)| (b.mime, b.bytes.clone()))
}

// ---- navigation grants ----

/// Web views may only show a plugin page the browser itself opened there:
/// a web page cannot navigate a tab (or a frame) to `bc-plugin:`.
#[derive(Default)]
struct Grants {
    /// One-shot: exact addresses the browser is about to load.
    once: Vec<(String, Instant)>,
    /// Tabs showing a plugin's pages: navigations within that plugin stay
    /// allowed until the tab goes anywhere else.
    tabs: HashMap<u64, String>,
}

static GRANTS: Mutex<Option<Grants>> = Mutex::new(None);

/// The browser is about to load `url` in a new or existing tab.
pub fn grant(url: &str) {
    let mut g = GRANTS.lock().unwrap_or_else(PoisonError::into_inner);
    let g = g.get_or_insert_with(Grants::default);
    g.once.retain(|(_, at)| at.elapsed() < Duration::from_secs(30));
    g.once.push((url.to_string(), Instant::now()));
}

/// The navigation policy for tab `tab`: `url` is any address it is about to
/// load (main frame or sub-frame).
pub fn navigation_allowed(tab: u64, url: &str) -> bool {
    let mut guard = GRANTS.lock().unwrap_or_else(PoisonError::into_inner);
    let g = guard.get_or_insert_with(Grants::default);
    let Some(page) = ui_pages::parse(url) else {
        // Leaving for a web page ends the grant.
        if !url.starts_with("about:") {
            g.tabs.remove(&tab);
        }
        return true;
    };
    if let Some(i) = g.once.iter().position(|(u, at)| u == url && at.elapsed() < Duration::from_secs(30)) {
        g.once.remove(i);
        g.tabs.insert(tab, page.plugin);
        return true;
    }
    g.tabs.get(&tab).is_some_and(|p| *p == page.plugin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::registry::Registry;

    fn msg(n: &str, i: u64, m: &str, a: Value) -> String {
        format!("{PREFIX}{}", json!({"n": n, "i": i, "m": m, "a": a}))
    }

    fn reg(perms: &str) -> (tempfile::TempDir, Registry) {
        let d = tempfile::tempdir().unwrap();
        let dev = d.path().join("dev");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(
            dev.join("plugin.toml"),
            format!("id = \"com.t.h\"\nname = \"H\"\nversion = \"1.0.0\"\napi_version = \"1.1\"\npermissions = [{perms}]\n[[pages]]\nid = \"main\"\npath = \"i.html\"\n"),
        )
        .unwrap();
        std::fs::write(dev.join("i.html"), "x").unwrap();
        let mut r = Registry::open(d.path().join("cfg"));
        r.add_dev_folder(&dev).unwrap();
        (d, r)
    }

    #[test]
    fn parsing_is_defensive() {
        assert!(parse_message(&msg("n", 1, "page.info", Value::Null)).is_some());
        assert!(parse_message(&msg("n", 0, "page.info", Value::Null)).is_none(), "every call has an id");
        assert!(parse_message(&msg("n", 1, "nope", Value::Null)).is_none());
        assert!(parse_message("bc-plugin:{}").is_none());
        assert!(parse_message("bc-host:[1]").is_none());
        let extra = format!("{PREFIX}{}", json!({"n":"n","i":1,"m":"page.info","a":null,"p":"x"}));
        assert!(parse_message(&extra).is_none());
    }

    #[test]
    fn only_plugin_pages_with_the_permission_get_through() {
        let (_d, r) = reg("\"clipboard.history\"");
        let call = |m: &str, a: Value| parse_message(&msg(nonce(), 3, m, a)).unwrap();
        let page = "bc-plugin://com.t.h/i.html";
        // A web page, whatever it sends, gets nothing (not even an answer).
        for sender in ["https://evil.test/", "file:///tmp/x.html", "about:blank", "bc-plugin://NOT/i.html"] {
            assert_eq!(authorize(&r, sender, call("clipboard.list", json!({}))), Err(Refused { reply: None }), "{sender}");
        }
        // The right page with the wrong nonce: dropped.
        let forged = parse_message(&msg("guess", 3, "page.info", Value::Null)).unwrap();
        assert_eq!(authorize(&r, page, forged), Err(Refused { reply: None }));
        // The plugin's own page: allowed, and only what it declared.
        let ok = authorize(&r, page, call("clipboard.list", json!({"query": "a"}))).unwrap();
        assert_eq!(ok.request, Request::ClipList { query: "a".into(), limit: 200 });
        assert_eq!(ok.page.plugin, "com.t.h");
        let no = authorize(&r, page, call("screenshot.get", Value::Null)).unwrap_err();
        assert!(no.reply.unwrap().contains("screenshot.edit"));
        // Another plugin's page cannot act for this one (it is not enabled here).
        let other = authorize(&r, "bc-plugin://com.t.other/i.html", call("clipboard.list", json!({}))).unwrap_err();
        assert!(other.reply.unwrap().contains("not enabled"));
        // Bad arguments are refused with a message.
        let bad = authorize(&r, page, call("clipboard.get", json!({"id": "x"}))).unwrap_err();
        assert!(bad.reply.unwrap().contains("id"));
    }

    #[test]
    fn disabled_plugins_and_images() {
        let (_d, mut r) = reg("\"screenshot.edit\"");
        let page = "bc-plugin://com.t.h/i.html";
        let call = |m: &str, a: Value| parse_message(&msg(nonce(), 4, m, a)).unwrap();
        let png = format!("data:image/png;base64,{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"\x89PNG\r\n\x1a\nrest"));
        let ok = authorize(&r, page, call("screenshot.save", json!({"name": "a.png", "data": png}))).unwrap();
        assert!(matches!(ok.request, Request::ShotSave { ref name, .. } if name == "a.png"));
        assert!(authorize(&r, page, call("screenshot.copy", json!({"data": "data:image/png;base64,AAAA"}))).is_err(), "not a PNG");
        r.set_enabled("com.t.h", false).unwrap();
        assert!(authorize(&r, page, call("page.info", Value::Null)).unwrap_err().reply.unwrap().contains("not enabled"));
    }

    #[test]
    fn scripts_are_injection_safe() {
        let s = Reply(5).ok(&json!("</script>\u{2028}\"); alert(1)"));
        assert!(!s.contains('<') && !s.contains('\u{2028}'));
        let e = event_script("command\"", &json!({"id": "x"}));
        assert!(e.starts_with("window.__bcHostEvent&&window.__bcHostEvent(\"command\\\"\","));
        let rt = runtime_script(true);
        assert!(rt.contains(nonce()) && rt.contains("\"modal\":true") && !rt.contains("__BC_HOST__"));
        assert!(theme_css(&crate::theme::DARK).contains("--bc-accent:#") && theme_css(&crate::theme::LIGHT).contains("color-scheme:light"));
    }

    #[test]
    fn navigation_needs_a_grant() {
        let url = "bc-plugin://com.t.nav/i.html";
        assert!(!navigation_allowed(901, url), "a page cannot open plugin pages");
        grant(url);
        assert!(!navigation_allowed(902, "bc-plugin://com.t.nav/other.html"), "grants are for exact addresses");
        assert!(navigation_allowed(902, url));
        assert!(navigation_allowed(902, "bc-plugin://com.t.nav/other.html"), "the tab may move within the plugin");
        assert!(!navigation_allowed(902, "bc-plugin://com.t.else/i.html"), "but not to another plugin");
        assert!(navigation_allowed(902, "https://example.com/"));
        assert!(!navigation_allowed(902, url), "leaving ends the grant");
    }

    #[test]
    fn blobs_are_scoped_to_their_plugin() {
        let t = blob_put("com.t.blob", "image/png", vec![1, 2, 3]);
        assert_eq!(blob_get("com.t.blob", &t).unwrap().1.as_slice(), &[1, 2, 3]);
        assert!(blob_get("com.t.other", &t).is_none());
        assert!(blob_get("com.t.blob", "nope").is_none());
    }
}
