//! The plugin host wired into the browser window (API 1.1): plugin pages in
//! tabs and in a modal view, plugin commands and their shortcuts, the
//! clipboard history watcher, and the screenshot hand-off. The pieces that
//! know nothing about the window live in `crate::plugins` (`host`,
//! `ui_pages`, `shortcuts`, `clipboard_history`, `clipboard_watch`).
//!
//! The modal is a dedicated WebKit view, a child window of the browser
//! window like the tabs' views. While it is open the page's view is hidden
//! and its snapshot drawn dimmed (the same mechanism as the menus,
//! `egui_popup_open`), so no two native views ever overlap.
//!
//! Hooks elsewhere (kept small for merges): `browser.rs` calls
//! `ipc_message`, `navigation_policy`, `register_protocol`, `gtk_key`,
//! `plugin_chrome_key`, `plugin_host_message`, `plugin_modal_open`,
//! `plugin_theme_changed` and `plugin_window_step`; `features.rs` calls
//! `plugin_host_tick`, `plugin_modal_frame`, `plugin_screenshot` and adds
//! the palette's plugin commands.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use winit::event::{ElementState, KeyEvent};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use super::{Browser, UserMessage, WebPane};
use crate::plugins::clipboard_history::{self as history, Capture, Item, Kind, Limits, Source};
use crate::plugins::host::{self, Authorized, HostCall, Request};
use crate::plugins::manifest::{OpenAs, PageDef};
use crate::plugins::permissions::Permission;
use crate::plugins::{self, shortcuts, ui_pages};

/// Messages for the plugin host, from web views, the clipboard watcher and
/// screenshot callbacks.
#[derive(Debug)]
pub enum HostMessage {
    /// A `bc.host` call from a plugin page: in the modal, or in tab `tab`.
    Call { modal: bool, tab: u64, sender: String, call: HostCall },
    /// A plugin command's keys were pressed in a page (`address`: a chrome
    /// text field had the keyboard).
    Command { plugin: String, command: String, tab: u64, address: bool },
    /// The clipboard changed.
    Captured(Capture),
    /// A screenshot for the editor plugin.
    Screenshot { result: Result<Vec<u8>, String>, mode: ShotMode },
    /// A script for a plugin page (answers that finish later).
    Eval { modal: bool, tab: u64, script: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShotMode {
    Visible,
    FullPage,
    /// The visible page, opened over the page itself to pick an area.
    Area,
}

impl ShotMode {
    fn key(self) -> &'static str {
        match self {
            ShotMode::Visible => "visible",
            ShotMode::FullPage => "full",
            ShotMode::Area => "area",
        }
    }
}

/// Where a paste from the clipboard manager goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Page(u64),
    Address,
    Nothing,
}

struct Modal {
    view: wry::WebView,
    plugin: String,
    page: String,
    /// CSS pixels; `None` fills the page area.
    size: Option<(u32, u32)>,
    target: Target,
    /// The tab under the modal; switching tabs closes it.
    tab: u64,
    bounds: Option<(i32, i32, u32, u32)>,
}

enum Pending {
    Paste { target: Target, plain: bool, text: String },
    Focus(Target),
}

/// The plugin host's state (owned by `Features`).
pub(super) struct HostState {
    modal: Option<Modal>,
    /// The data store of modal plugin pages (their own, not a web profile).
    context: Option<wry::WebContext>,
    #[cfg(target_os = "linux")]
    watcher: Option<crate::plugins::clipboard_watch::Watcher>,
    #[cfg(target_os = "linux")]
    dialog: Option<gtk::FileChooserNative>,
    /// What the shortcut table was built from.
    table_key: Option<String>,
    pending: Vec<(Instant, Pending)>,
    last_expire: Option<Instant>,
}

impl HostState {
    pub fn new() -> Self {
        HostState {
            modal: None,
            context: None,
            #[cfg(target_os = "linux")]
            watcher: None,
            #[cfg(target_os = "linux")]
            dialog: None,
            table_key: None,
            pending: Vec::new(),
            last_expire: None,
        }
    }
}

// ---- hooks for web views (free functions: no `Browser` there) ----

/// Handles an IPC body from a tab's web view if it is a `bc.host` call.
pub(super) fn ipc_message(body: &str, uri: &str, tab: u64, proxy: &EventLoopProxy<UserMessage>) -> bool {
    if !body.starts_with(host::PREFIX) {
        return false;
    }
    if let Some(call) = host::parse_message(body) {
        let _ = proxy.send_event(UserMessage::PluginHost(HostMessage::Call { modal: false, tab, sender: uri.to_string(), call }));
    }
    true
}

/// The navigation policy for plugin pages in tab `tab`: `Some(allowed)` for
/// a `bc-plugin:` address, `None` for anything else (with the side effect
/// of ending the tab's grant when it leaves for a web page).
pub(super) fn navigation_policy(tab: u64, url: &str) -> Option<bool> {
    let allowed = host::navigation_allowed(tab, url);
    url.starts_with(ui_pages::SCHEME).then_some(allowed)
}

/// Adds the `bc-plugin:` scheme to a web view builder unless its data store
/// already has it (wry registers schemes per `WebContext`, once).
pub(super) fn register_protocol(builder: wry::WebViewBuilder<'_>, needed: bool) -> wry::WebViewBuilder<'_> {
    if !needed {
        return builder;
    }
    builder.with_custom_protocol(ui_pages::SCHEME.into(), |_id, request| ui_pages::handle(&request))
}

/// What the GTK key handler of a web view does with a key press.
#[derive(Debug, PartialEq)]
pub(super) enum GtkKey {
    /// A plugin command's shortcut.
    Command(String, String),
    /// A plugin page keeps its editing keys (Ctrl+C, Ctrl+S, Ctrl+Z, ...).
    PageOwns,
    /// Not the plugins' business.
    Browser,
}

/// Browser keys that still work on plugin pages (to leave them).
fn kept_on_plugin_pages(ctrl: bool, shift: bool, key: &str) -> bool {
    ctrl && match key {
        "t" | "w" | "tab" | "pageup" | "pagedown" | "q" | "p" | "l" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => true,
        "n" => shift,
        _ => false,
    }
}

/// Decides for a key typed in a web view showing `uri`. `key` is the key's
/// name as `shortcuts::KeyCombo` spells it.
pub(super) fn key_decision(uri: &str, ctrl: bool, shift: bool, alt: bool, key: &str) -> GtkKey {
    if uri.starts_with(ui_pages::SCHEME) {
        let function = key.starts_with('f') && key.len() > 1;
        if (ctrl || alt) && !function && !kept_on_plugin_pages(ctrl, shift, key) {
            return GtkKey::PageOwns;
        }
        return GtkKey::Browser;
    }
    match shortcuts::lookup(ctrl, shift, alt, key) {
        Some((p, c)) => GtkKey::Command(p, c),
        None => GtkKey::Browser,
    }
}

/// The GTK key handler's hook (Linux).
#[cfg(target_os = "linux")]
pub(super) fn gtk_key(view: &webkit2gtk::WebView, event: &gtk::gdk::EventKey) -> GtkKey {
    use gtk::gdk::ModifierType;
    use webkit2gtk::WebViewExt as _;
    let state = event.state();
    let ctrl = state.contains(ModifierType::CONTROL_MASK);
    let shift = state.contains(ModifierType::SHIFT_MASK);
    let alt = state.contains(ModifierType::MOD1_MASK);
    let Some(key) = gtk_key_name(event.keyval()) else { return GtkKey::Browser };
    let uri = view.uri().map(|u| u.to_string()).unwrap_or_default();
    key_decision(&uri, ctrl, shift, alt, &key)
}

/// A GDK key as `KeyCombo` names it: lower-case character or key name.
#[cfg(target_os = "linux")]
fn gtk_key_name(kv: gtk::gdk::keys::Key) -> Option<String> {
    use gtk::gdk::keys::constants as k;
    let named = match kv {
        k::Return | k::KP_Enter => "enter",
        k::Escape => "escape",
        k::Tab | k::ISO_Left_Tab => "tab",
        k::space => "space",
        k::Delete | k::KP_Delete => "delete",
        k::BackSpace => "backspace",
        k::Insert => "insert",
        k::Home => "home",
        k::End => "end",
        k::Page_Up => "pageup",
        k::Page_Down => "pagedown",
        k::Up => "up",
        k::Down => "down",
        k::Left => "left",
        k::Right => "right",
        k::F1 => "f1",
        k::F2 => "f2",
        k::F3 => "f3",
        k::F4 => "f4",
        k::F5 => "f5",
        k::F6 => "f6",
        k::F7 => "f7",
        k::F8 => "f8",
        k::F9 => "f9",
        k::F10 => "f10",
        k::F11 => "f11",
        k::F12 => "f12",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.into());
    }
    kv.to_lower().to_unicode().filter(|c| !c.is_control()).map(|c| c.to_lowercase().to_string())
}

/// A winit key as `KeyCombo` names it.
fn winit_key_name(key: &Key) -> Option<String> {
    Some(match key {
        Key::Character(c) => c.to_lowercase(),
        Key::Named(n) => match n {
            NamedKey::Enter => "enter",
            NamedKey::Escape => "escape",
            NamedKey::Tab => "tab",
            NamedKey::Space => "space",
            NamedKey::Delete => "delete",
            NamedKey::Backspace => "backspace",
            NamedKey::Insert => "insert",
            NamedKey::Home => "home",
            NamedKey::End => "end",
            NamedKey::PageUp => "pageup",
            NamedKey::PageDown => "pagedown",
            NamedKey::ArrowUp => "up",
            NamedKey::ArrowDown => "down",
            NamedKey::ArrowLeft => "left",
            NamedKey::ArrowRight => "right",
            NamedKey::F1 => "f1",
            NamedKey::F2 => "f2",
            NamedKey::F3 => "f3",
            NamedKey::F4 => "f4",
            NamedKey::F5 => "f5",
            NamedKey::F6 => "f6",
            NamedKey::F7 => "f7",
            NamedKey::F8 => "f8",
            NamedKey::F9 => "f9",
            NamedKey::F10 => "f10",
            NamedKey::F11 => "f11",
            NamedKey::F12 => "f12",
            _ => return None,
        }
        .to_string(),
        _ => return None,
    })
}

/// A file name for a saved image: the page's name without path or odd
/// characters, ending in `.png`.
fn clean_file_name(name: &str, fallback: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let stem: String = base
        .trim_end_matches(".png")
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.' | '(' | ')') { c } else { '_' })
        .take(120)
        .collect();
    let stem = stem.trim().trim_start_matches('.').to_string();
    if stem.is_empty() { fallback.to_string() } else { format!("{stem}.png") }
}

/// Writes `bytes` to `dir/name`, adding `-2`, `-3`, ... instead of replacing
/// a file.
fn write_unique(dir: &std::path::Path, name: &str, bytes: &[u8]) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stem = name.trim_end_matches(".png");
    for n in 1..1000 {
        let path = if n == 1 { dir.join(name) } else { dir.join(format!("{stem}-{n}.png")) };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                std::io::Write::write_all(&mut f, bytes).map_err(|e| e.to_string())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("too many files with that name".into())
}

fn uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let raw = uri.strip_prefix("file://")?;
    let b = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(x) = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(x);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(std::path::PathBuf::from(std::ffi::OsString::from_vec(out)))
    }
    #[cfg(not(unix))]
    Some(std::path::PathBuf::from(String::from_utf8_lossy(&out).into_owned()))
}

fn decode_png(png: &[u8]) -> Result<image::RgbaImage, String> {
    image::load_from_memory_with_format(png, image::ImageFormat::Png).map(|i| i.to_rgba8()).map_err(|e| format!("not a PNG: {e}"))
}

/// The limits set in the settings of the plugin that keeps the history.
fn history_limits(reg: &plugins::Registry) -> Option<(String, Limits)> {
    reg.enabled()
        .find(|p| p.manifest.has(Permission::ClipboardHistory))
        .map(|p| (p.manifest.id.clone(), Limits::from_settings(&reg.settings(&p.manifest.id))))
}

impl Browser {
    // ---- every second ----

    /// Keeps the shortcut table, the clipboard watcher and the history's
    /// retention in step with the plugins; runs deferred pastes.
    pub(super) fn plugin_host_tick(&mut self) -> Option<Instant> {
        let now = Instant::now();
        let (key, history) = {
            let reg = plugins::shared();
            let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let settings: Vec<Value> = reg.enabled().filter(|p| !p.manifest.commands.is_empty()).map(|p| Value::Object(reg.settings(&p.manifest.id))).collect();
            (format!("{}:{}", reg.generation(), Value::Array(settings)), history_limits(&reg))
        };
        if self.feat.host.table_key.as_deref() != Some(key.as_str()) {
            self.refresh_plugin_shortcuts();
            self.feat.host.table_key = Some(key);
        }
        self.sync_clipboard_watcher(history.is_some());
        if let Some((_, limits)) = &history
            && self.feat.host.last_expire.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(60))
        {
            self.feat.host.last_expire = Some(now);
            let _ = history::with(|s| s.enforce(limits, history::now()));
        }
        // A plugin page whose modal lost its plugin.
        if let Some(m) = &self.feat.host.modal
            && !plugins::is_enabled(&m.plugin)
        {
            self.close_plugin_modal(false);
        }
        let due: Vec<Pending> = {
            let (ready, later): (Vec<_>, Vec<_>) = self.feat.host.pending.drain(..).partition(|(at, _)| *at <= now);
            self.feat.host.pending = later;
            ready.into_iter().map(|(_, p)| p).collect()
        };
        for p in due {
            self.run_pending(p);
        }
        self.feat.host.pending.iter().map(|(at, _)| *at).min()
    }

    fn refresh_plugin_shortcuts(&mut self) {
        let (table, _) = {
            let reg = plugins::shared();
            let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            shortcuts::resolve(&reg)
        };
        shortcuts::install(table);
    }

    #[cfg(target_os = "linux")]
    fn sync_clipboard_watcher(&mut self, wanted: bool) {
        if wanted && self.feat.host.watcher.is_none() {
            let proxy = self.proxy.clone();
            self.feat.host.watcher = crate::plugins::clipboard_watch::Watcher::start(move |c| {
                let _ = proxy.send_event(UserMessage::PluginHost(HostMessage::Captured(c)));
            });
        } else if !wanted && self.feat.host.watcher.is_some() {
            self.feat.host.watcher = None;
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn sync_clipboard_watcher(&mut self, _wanted: bool) {}

    // ---- messages ----

    pub(super) fn plugin_host_message(&mut self, m: HostMessage) {
        match m {
            HostMessage::Call { modal, tab, sender, call } => {
                let auth = {
                    let reg = plugins::shared();
                    let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    host::authorize(&reg, &sender, call)
                };
                match auth {
                    Ok(a) => self.host_request(modal, tab, a),
                    Err(refused) => {
                        if let Some(script) = refused.reply {
                            self.eval_in_plugin_view(modal, tab, &script);
                        }
                    }
                }
            }
            HostMessage::Command { plugin, command, tab, address } => {
                let target = if address { Target::Address } else if tab == self.tab_id { Target::Page(tab) } else { Target::Nothing };
                self.run_plugin_command(&plugin, &command, target);
            }
            HostMessage::Captured(c) => self.clipboard_captured(c),
            HostMessage::Screenshot { result, mode } => match result {
                Ok(png) => self.open_screenshot_editor(png, mode),
                Err(e) => self.set_notice(format!("Screenshot failed: {e}")),
            },
            HostMessage::Eval { modal, tab, script } => self.eval_in_plugin_view(modal, tab, &script),
        }
    }

    /// Runs `script` in a plugin page, if that view still shows one.
    fn eval_in_plugin_view(&self, modal: bool, tab: u64, script: &str) {
        if modal {
            if let Some(m) = &self.feat.host.modal {
                let _ = m.view.evaluate_script(script);
            }
            return;
        }
        if let Some(view) = self.view_of(tab)
            && view.url().is_ok_and(|u| u.starts_with(ui_pages::SCHEME))
        {
            let _ = view.evaluate_script(script);
        }
    }

    fn clipboard_captured(&mut self, c: Capture) {
        let Some((plugin, limits)) = ({
            let reg = plugins::shared();
            let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            history_limits(&reg)
        }) else {
            return;
        };
        let focused = self.gfx.as_ref().is_some_and(|g| g.window.has_focus());
        let private = self.container.as_deref().is_some_and(|c| c.starts_with("tmp-")) && self.web_page;
        if focused && private && limits.ignore_private {
            return;
        }
        let c = Capture { source: Some(if focused { Source::Browser } else { Source::App }), ..c };
        let added = history::with(|s| s.add(c, &limits, history::now()));
        if let Ok(Ok(id)) = added
            && let Some(m) = &self.feat.host.modal
            && m.plugin == plugin
        {
            let _ = m.view.evaluate_script(&host::event_script("clipboard-changed", &json!({"id": id})));
        }
    }

    fn reply(&self, modal: bool, tab: u64, a: &Authorized, result: Result<Value, String>) {
        let script = match result {
            Ok(v) => a.reply.ok(&v),
            Err(e) => a.reply.err(&e),
        };
        self.eval_in_plugin_view(modal, tab, &script);
    }

    fn host_request(&mut self, modal: bool, tab: u64, a: Authorized) {
        let plugin = a.page.plugin.clone();
        let result: Result<Value, String> = match &a.request {
            Request::Info => Ok(self.plugin_page_info(&a, modal)),
            Request::Close => {
                self.reply(modal, tab, &a, Ok(Value::Null));
                if modal {
                    self.close_plugin_modal(true);
                } else if let Some(i) = self.tab_index(tab) {
                    self.close_tab(i);
                }
                return;
            }
            Request::SettingsGet => {
                let reg = plugins::shared();
                let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                Ok(Value::Object(reg.settings(&plugin)))
            }
            Request::SettingsSet { key, value } => {
                let r = {
                    let reg = plugins::shared();
                    let mut reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    reg.set_setting(&plugin, key, value.clone()).map(|()| Value::Object(reg.settings(&plugin)))
                };
                if r.is_ok() {
                    self.refresh_plugin_shortcuts();
                }
                r
            }
            Request::Notify { title, body } => {
                let name = self.plugin_display_name(&plugin);
                self.set_notice(if body.is_empty() { format!("{name}: {title}") } else { format!("{name}: {title}. {body}") });
                Ok(Value::Null)
            }
            Request::ClipList { query, limit } => {
                let limits = self.limits_of(&plugin);
                history::with(|s| {
                    let items: Vec<Value> = s.search(query, *limit).into_iter().map(history::summary).collect();
                    json!({"items": items, "usage": {"items": s.items().len(), "bytes": s.total_bytes(), "maxItems": limits.max_items, "maxBytes": limits.max_total}})
                })
            }
            Request::ClipGet(id) => history::with(|s| s.get(*id).map(history::detail)).and_then(|v| v.ok_or_else(|| "no such item".into())),
            Request::ClipUsage => {
                let limits = self.limits_of(&plugin);
                history::with(|s| json!({"items": s.items().len(), "bytes": s.total_bytes(), "maxItems": limits.max_items, "maxBytes": limits.max_total, "maxItemBytes": limits.max_item}))
            }
            Request::ClipDelete(id) => history::with(|s| Value::Bool(s.delete(*id))),
            Request::ClipPin { id, pinned } => history::with(|s| Value::Bool(s.pin(*id, *pinned))),
            Request::ClipClear { keep_pinned } => history::with(|s| json!(s.clear(*keep_pinned))),
            Request::ClipCopy(id) => match history::with(|s| s.get(*id).cloned()) {
                Ok(Some(item)) => {
                    let r = self.put_item_on_clipboard(&item, false);
                    if r.is_ok() {
                        let _ = history::with(|s| s.touch(item.id, history::now()));
                        self.set_notice("Copied to the clipboard");
                    }
                    r.map(|_| Value::Null)
                }
                _ => Err("no such item".into()),
            },
            Request::ClipPaste { id, plain } => match history::with(|s| s.get(*id).cloned()) {
                Ok(Some(item)) => match self.put_item_on_clipboard(&item, *plain) {
                    Ok(text) => {
                        let _ = history::with(|s| s.touch(item.id, history::now()));
                        self.reply(modal, tab, &a, Ok(Value::Null));
                        let target = self.feat.host.modal.as_ref().map_or(Target::Page(self.tab_id), |m| m.target);
                        if modal {
                            self.close_plugin_modal(false);
                        }
                        // Let the page's view come back and take the keyboard first.
                        self.feat.host.pending.push((Instant::now() + Duration::from_millis(180), Pending::Paste { target, plain: *plain, text }));
                        return;
                    }
                    Err(e) => Err(e),
                },
                _ => Err("no such item".into()),
            },
            Request::ShotGet => match a.page.param("shot") {
                Some(token) if host::blob_get(&plugin, token).is_some() => Ok(json!({
                    "url": format!("/__host/blob/{token}"),
                    "name": a.page.param("name").unwrap_or("screenshot.png"),
                    "mode": a.page.param("mode").unwrap_or("visible"),
                })),
                _ => Err("this page was not opened with a screenshot".into()),
            },
            Request::ShotCopy { png } => self.copy_png(png).map(|()| {
                self.set_notice("Image copied to the clipboard");
                Value::Null
            }),
            Request::ShotSave { name, png, ask } => {
                let name = clean_file_name(name, &super::screenshot_name(now_secs()));
                if *ask {
                    self.save_png_as(modal, tab, a.reply.clone(), name, png.clone());
                    return;
                }
                let dir = super::pictures_dir().map(|d| d.join("BrowseCraft"));
                match dir.and_then(|d| write_unique(&d, &name, png)) {
                    Ok(path) => {
                        self.set_notice_long(format!("Saved to {}", path.display()));
                        Ok(json!({"path": path.display().to_string()}))
                    }
                    Err(e) => Err(format!("could not save: {e}")),
                }
            }
        };
        self.reply(modal, tab, &a, result);
    }

    fn plugin_page_info(&self, a: &Authorized, modal: bool) -> Value {
        let reg = plugins::shared();
        let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let m = reg.get(&a.page.plugin).and_then(|e| e.manifest());
        let page = m.and_then(|m| m.pages.iter().find(|p| p.path == a.page.path).or(m.pages.first()));
        let theme = crate::internal::current();
        json!({
            "plugin": a.page.plugin,
            "name": m.map(|m| m.name.clone()),
            "page": page.map(|p| p.id.clone()),
            "command": a.page.param("cmd"),
            "modal": modal,
            "settings": Value::Object(reg.settings(&a.page.plugin)),
            "permissions": m.map(|m| m.permissions.iter().map(|p| p.label()).collect::<Vec<_>>()),
            "theme": {"name": theme.name, "dark": theme.dark},
            "target": self.feat.host.modal.as_ref().filter(|_| modal).map(|m| match m.target {
                Target::Page(_) => "page",
                Target::Address => "address",
                Target::Nothing => "none",
            }),
            "platform": std::env::consts::OS,
        })
    }

    fn plugin_display_name(&self, id: &str) -> String {
        let reg = plugins::shared();
        let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.get(id).and_then(|e| e.manifest().map(|m| m.name.clone())).unwrap_or_else(|| id.to_string())
    }

    fn limits_of(&self, plugin: &str) -> Limits {
        let reg = plugins::shared();
        let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Limits::from_settings(&reg.settings(plugin))
    }

    // ---- the clipboard ----

    /// Puts a history item on the system clipboard. Returns the text a paste
    /// into a chrome field uses.
    fn put_item_on_clipboard(&mut self, item: &Item, plain: bool) -> Result<String, String> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let Some(cb) = self.clipboard.as_mut() else {
            return Err("the clipboard is not available".into());
        };
        let fail = |e: arboard::Error| format!("could not use the clipboard: {e}");
        match item.kind {
            Kind::Text => cb.set_text(item.text.clone()).map_err(fail)?,
            Kind::Html if plain => cb.set_text(item.text.clone()).map_err(fail)?,
            Kind::Html => cb.set_html(item.html.clone().unwrap_or_default(), Some(item.text.clone())).map_err(fail)?,
            Kind::Files => {
                let paths: Vec<std::path::PathBuf> = item.files.iter().filter_map(|u| uri_to_path(u)).collect();
                let text = paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
                if plain || paths.is_empty() {
                    cb.set_text(text.clone()).map_err(fail)?;
                } else {
                    cb.set().file_list(&paths).map_err(fail)?;
                }
                return Ok(text);
            }
            Kind::Image => {
                let png = history::image_bytes(item.id).ok_or("the image is gone")?;
                let img = decode_png(&png)?;
                let (w, h) = img.dimensions();
                cb.set_image(arboard::ImageData { width: w as usize, height: h as usize, bytes: img.into_raw().into() }).map_err(fail)?;
                return Ok(String::new());
            }
        }
        Ok(item.text.clone())
    }

    fn copy_png(&mut self, png: &[u8]) -> Result<(), String> {
        let img = decode_png(png)?;
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let cb = self.clipboard.as_mut().ok_or("the clipboard is not available")?;
        let (w, h) = img.dimensions();
        cb.set_image(arboard::ImageData { width: w as usize, height: h as usize, bytes: img.into_raw().into() })
            .map_err(|e| format!("could not use the clipboard: {e}"))
    }

    fn run_pending(&mut self, p: Pending) {
        match p {
            Pending::Focus(target) => self.focus_target(target),
            Pending::Paste { target, plain, text } => {
                self.focus_target(target);
                match target {
                    Target::Page(tab) => {
                        #[cfg(target_os = "linux")]
                        if let Some(WebPane::Wry(wv)) = self.view_of(tab) {
                            use webkit2gtk::WebViewExt as _;
                            use wry::WebViewExtUnix as _;
                            wv.webview().execute_editing_command(if plain { "PasteAsPlainText" } else { "Paste" });
                        }
                        #[cfg(not(target_os = "linux"))]
                        let _ = (tab, plain);
                    }
                    Target::Address => {
                        if !text.is_empty() {
                            self.pending_egui_events.push(egui::Event::Paste(text));
                        }
                    }
                    Target::Nothing => {}
                }
            }
        }
        self.request_redraw();
    }

    fn focus_target(&mut self, target: Target) {
        match target {
            Target::Page(tab) if tab == self.tab_id => {
                if let Some(WebPane::Wry(wv)) = self.view_of(tab) {
                    let _ = wv.focus();
                }
            }
            Target::Address => {
                if let Some(egui) = &self.egui {
                    egui.ctx().memory_mut(|m| m.request_focus(crate::egui_chrome::address_id()));
                }
                self.take_keyboard();
            }
            _ => {}
        }
    }

    #[cfg(target_os = "linux")]
    fn save_png_as(&mut self, modal: bool, tab: u64, reply: host::Reply, name: String, png: Vec<u8>) {
        use gtk::prelude::*;
        let dialog = gtk::FileChooserNative::new(Some("Save image"), None::<&gtk::Window>, gtk::FileChooserAction::Save, Some("Save"), Some("Cancel"));
        dialog.set_do_overwrite_confirmation(true);
        dialog.set_current_name(&name);
        if let Ok(dir) = super::pictures_dir() {
            let dir = dir.join("BrowseCraft");
            let _ = std::fs::create_dir_all(&dir);
            let _ = dialog.set_current_folder(&dir);
        }
        let filter = gtk::FileFilter::new();
        filter.add_mime_type("image/png");
        filter.set_name(Some("PNG image"));
        dialog.add_filter(filter);
        let proxy = self.proxy.clone();
        dialog.connect_response(move |d, response| {
            let script = if response == gtk::ResponseType::Accept {
                match d.filename() {
                    Some(path) => match std::fs::write(&path, &png) {
                        Ok(()) => {
                            let _ = proxy.send_event(UserMessage::Notice(format!("Saved to {}", path.display())));
                            reply.ok(&json!({"path": path.display().to_string()}))
                        }
                        Err(e) => reply.err(&format!("could not save: {e}")),
                    },
                    None => reply.err("no file chosen"),
                }
            } else {
                reply.ok(&Value::Null)
            };
            let _ = proxy.send_event(UserMessage::PluginHost(HostMessage::Eval { modal, tab, script }));
            d.destroy();
        });
        dialog.show();
        self.feat.host.dialog = Some(dialog);
    }

    #[cfg(not(target_os = "linux"))]
    fn save_png_as(&mut self, modal: bool, tab: u64, reply: host::Reply, _name: String, _png: Vec<u8>) {
        self.eval_in_plugin_view(modal, tab, &reply.err("choosing a folder is not available on this platform"));
    }

    // ---- commands and pages ----

    /// The palette's plugin commands: (plugin, command, title, plugin name,
    /// shortcut label).
    pub(super) fn plugin_palette_commands(&self) -> Vec<crate::palette::PluginCommandItem> {
        let reg = plugins::shared();
        let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let table = shortcuts::table();
        reg.enabled()
            .flat_map(|p| {
                let m = &p.manifest;
                let table = &table;
                m.commands.iter().filter(|c| c.palette).map(move |c| crate::palette::PluginCommandItem {
                    plugin: m.id.clone(),
                    command: c.id.clone(),
                    title: c.title.clone(),
                    plugin_name: m.name.clone(),
                    shortcut: table.iter().find(|a| a.plugin == m.id && a.command == c.id).map(|a| a.combo.label()),
                })
            })
            .collect()
    }

    /// Runs plugin command `command`: opens its page (or, when that page is
    /// open, tells it about the command).
    pub(super) fn run_plugin_command(&mut self, plugin: &str, command: &str, target: Target) {
        let found = {
            let reg = plugins::shared();
            let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            reg.enabled()
                .find(|p| p.manifest.id == plugin)
                .and_then(|p| p.manifest.command(command).and_then(|c| p.manifest.page(&c.page)).cloned())
        };
        let Some(page) = found else {
            self.set_notice("That plugin command is not available (is the plugin turned on?)");
            return;
        };
        let event = host::event_script("command", &json!({"id": command}));
        if let Some(m) = &self.feat.host.modal
            && m.plugin == plugin
            && m.page == page.id
        {
            let _ = m.view.evaluate_script(&event);
            return;
        }
        let query = format!("cmd={command}");
        match page.open_as {
            OpenAs::Modal => self.open_plugin_modal(plugin, &page, &query, target, Some((page.width, page.height))),
            OpenAs::Tab => {
                // A tab already showing the page gets the command.
                let url_prefix = ui_pages::page_url(plugin, &page.path, "");
                if let Some(i) = (0..self.tabs.len()).find(|&i| self.tab_url(i).starts_with(&url_prefix)) {
                    self.switch_tab(i);
                    let id = self.tab_id_at(i);
                    self.eval_in_plugin_view(false, id, &event);
                } else {
                    self.open_plugin_tab(plugin, &page, &query);
                }
            }
        }
    }

    fn open_plugin_tab(&mut self, plugin: &str, page: &PageDef, query: &str) {
        let url = ui_pages::page_url(plugin, &page.path, query);
        host::grant(&url);
        self.open_in_new_tab(url);
    }

    /// Whether a plugin modal is showing (the page view hides behind its
    /// snapshot meanwhile).
    pub(super) fn plugin_modal_open(&self) -> bool {
        self.feat.host.modal.is_some()
    }

    fn open_plugin_modal(&mut self, plugin: &str, page: &PageDef, query: &str, target: Target, size: Option<(u32, u32)>) {
        if self.feat.host.modal.is_some() {
            self.close_plugin_modal(false);
        }
        if !self.gpu_chrome {
            self.set_notice("Plugin windows need the GPU interface");
            return;
        }
        let Some(window) = self.gfx.as_ref().map(|g| g.window.clone()) else {
            return;
        };
        if self.feat.host.context.is_none() {
            let dir = crate::storage::profile_dir("plugin-pages").ok().filter(|d| crate::storage::private_dir(d).is_ok());
            self.feat.host.context = Some(wry::WebContext::new(dir));
        }
        let Some(context) = self.feat.host.context.as_mut() else {
            return;
        };
        let needs_protocol = !context.is_custom_protocol_registered(ui_pages::SCHEME);
        let url = ui_pages::page_url(plugin, &page.path, query);
        let (ipc_proxy, own) = (self.proxy.clone(), plugin.to_string());
        let builder = wry::WebViewBuilder::new_with_web_context(context)
            .with_initialization_script_for_main_only(host::runtime_script(true), true)
            .with_devtools(cfg!(debug_assertions))
            .with_visible(true)
            .with_bounds(wry::Rect {
                position: wry::dpi::Position::Physical(wry::dpi::PhysicalPosition::new(0, 0)),
                size: wry::dpi::Size::Physical(wry::dpi::PhysicalSize::new(1, 1)),
            })
            .with_navigation_handler(move |u| u == "about:blank" || ui_pages::parse(&u).is_some_and(|p| p.plugin == own))
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_ipc_handler(move |request| {
                if let Some(call) = host::parse_message(request.body()) {
                    let sender = request.uri().to_string();
                    let _ = ipc_proxy.send_event(UserMessage::PluginHost(HostMessage::Call { modal: true, tab: 0, sender, call }));
                }
            })
            .with_url(url);
        let builder = register_protocol(builder, needs_protocol);
        match builder.build_as_child(&window) {
            Ok(view) => {
                let _ = view.focus();
                self.feat.host.modal = Some(Modal { view, plugin: plugin.into(), page: page.id.clone(), size, target, tab: self.tab_id, bounds: None });
                self.request_redraw();
            }
            Err(e) => self.set_notice(format!("Could not open the plugin page: {e}")),
        }
    }

    /// Closes the modal; `refocus` gives the keyboard back to where it was.
    pub(super) fn close_plugin_modal(&mut self, refocus: bool) {
        if let Some(m) = self.feat.host.modal.take() {
            let _ = m.view.set_visible(false);
            drop(m.view);
            if refocus {
                self.feat.host.pending.push((Instant::now() + Duration::from_millis(120), Pending::Focus(m.target)));
            }
        }
        self.request_redraw();
    }

    /// Draws the dimmed backdrop over the page area and keeps the modal
    /// centred in it (every frame while it is open).
    pub(super) fn plugin_modal_frame(&mut self, ctx: &egui::Context) {
        let Some(m) = &self.feat.host.modal else { return };
        if m.tab != self.tab_id || !self.web_page && !matches!(m.target, Target::Address | Target::Nothing) {
            self.close_plugin_modal(false);
            return;
        }
        let Some(page) = self.egui_page else { return };
        let ppp = ctx.pixels_per_point();
        let area = egui::Rect::from_min_size(egui::pos2(page.x / ppp, page.y / ppp), egui::vec2(page.w / ppp, page.h / ppp));
        let (w, h) = match m.size {
            Some((w, h)) => ((w as f32).min(area.width() - 32.0).max(120.0), (h as f32).min(area.height() - 32.0).max(100.0)),
            None => (area.width(), area.height()),
        };
        let rect = egui::Rect::from_center_size(area.center(), egui::vec2(w, h));
        let bounds = ((rect.min.x * ppp).round() as i32, (rect.min.y * ppp).round() as i32, (w * ppp).round() as u32, (h * ppp).round() as u32);
        let mut close = false;
        let colors = crate::egui_chrome::Colors::from_theme(self.theme());
        egui::Area::new(egui::Id::new("plugin-modal-backdrop"))
            .order(egui::Order::Middle)
            .fixed_pos(area.min)
            .show(ctx, |ui| {
                let (r, resp) = ui.allocate_exact_size(area.size(), egui::Sense::click());
                ui.painter().rect_filled(r, 0.0, egui::Color32::from_black_alpha(150));
                // A frame round the view (it is drawn over the middle).
                ui.painter().rect_filled(rect.expand(1.0), 9.0, colors.line);
                if resp.clicked() && resp.interact_pointer_pos().is_some_and(|p| !rect.contains(p)) {
                    close = true;
                }
            });
        if close {
            self.close_plugin_modal(true);
            return;
        }
        if let Some(m) = self.feat.host.modal.as_mut()
            && m.bounds != Some(bounds)
        {
            let _ = m.view.set_bounds(wry::Rect {
                position: wry::dpi::Position::Physical(wry::dpi::PhysicalPosition::new(bounds.0, bounds.1)),
                size: wry::dpi::Size::Physical(wry::dpi::PhysicalSize::new(bounds.2.max(1), bounds.3.max(1))),
            });
            m.bounds = Some(bounds);
        }
    }

    /// The theme changed: plugin pages follow it live.
    pub(super) fn plugin_theme_changed(&self) {
        let script = host::theme_script(crate::internal::current());
        if let Some(m) = &self.feat.host.modal {
            let _ = m.view.evaluate_script(&script);
        }
        for view in self.all_views() {
            if view.url().is_ok_and(|u| u.starts_with(ui_pages::SCHEME)) {
                let _ = view.evaluate_script(&script);
            }
        }
    }

    /// A key the chrome window received (winit): a plugin shortcut runs its
    /// command instead of reaching egui.
    pub(super) fn plugin_chrome_key(&mut self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed || event.repeat || self.feat.host.modal.is_some() {
            return false;
        }
        let m = self.modifiers;
        let ctrl = m.control_key() || (cfg!(target_os = "macos") && m.super_key());
        let Some(key) = winit_key_name(&event.key_without_modifiers()) else { return false };
        let Some((plugin, command)) = shortcuts::lookup(ctrl, m.shift_key(), m.alt_key(), &key) else { return false };
        if self.feat.palette.is_open() {
            return false;
        }
        let address = self.egui.as_ref().is_some_and(|e| e.ctx().egui_wants_keyboard_input());
        let target = if address { Target::Address } else if self.web_page { Target::Page(self.tab_id) } else { Target::Nothing };
        if self.web_page && !address && self.current_url().starts_with(ui_pages::SCHEME) {
            return false;
        }
        self.run_plugin_command(&plugin, &command, target);
        true
    }

    // ---- screenshots ----

    /// The enabled plugin that edits screenshots, and its editor page.
    fn screenshot_editor(&self) -> Option<(String, PageDef)> {
        let reg = plugins::shared();
        let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.enabled().find_map(|p| {
            let m = &p.manifest;
            let page = m.screenshot_editor.as_ref().filter(|_| m.has(Permission::ScreenshotEdit)).and_then(|id| m.page(id))?;
            Some((m.id.clone(), page.clone()))
        })
    }

    /// Takes a screenshot for the editor plugin, if there is one. False:
    /// no editor (the browser saves the screenshot itself).
    pub(super) fn plugin_screenshot(&mut self, mode: ShotMode) -> bool {
        if self.screenshot_editor().is_none() {
            if mode == ShotMode::Area {
                self.set_notice("Selecting an area needs a screenshot editor plugin (see Plugins)");
                return true;
            }
            return false;
        }
        #[cfg(target_os = "linux")]
        if self.web_page
            && let Some(WebPane::Wry(wv)) = &self.webview
        {
            use wry::WebViewExtUnix as _;
            let proxy = self.proxy.clone();
            crate::headless_webkit::snapshot_png(&wv.webview(), mode == ShotMode::FullPage, move |r| {
                let result = r.map(|(png, _, _)| png);
                let _ = proxy.send_event(UserMessage::PluginHost(HostMessage::Screenshot { result, mode }));
            });
            if mode == ShotMode::FullPage {
                self.set_notice("Capturing the whole page…");
            }
            return true;
        }
        match self.capture_page_rgba() {
            Ok((w, h, rgba)) => {
                let mut png = Vec::new();
                let ok = image::RgbaImage::from_raw(w, h, rgba)
                    .is_some_and(|img| image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).is_ok());
                if ok {
                    self.open_screenshot_editor(png, mode);
                } else {
                    self.set_notice("Screenshot failed");
                }
            }
            Err(e) => self.set_notice(format!("Screenshot failed: {e}")),
        }
        true
    }

    fn open_screenshot_editor(&mut self, png: Vec<u8>, mode: ShotMode) {
        let Some((plugin, page)) = self.screenshot_editor() else {
            self.set_notice("The screenshot editor plugin was turned off");
            return;
        };
        let token = host::blob_put(&plugin, "image/png", png);
        let name = super::screenshot_name(now_secs());
        let query = format!("shot={token}&mode={}&name={name}", mode.key());
        let target = Target::Page(self.tab_id);
        match (mode, page.open_as) {
            // Picking an area happens over the page itself.
            (ShotMode::Area, _) if self.web_page => self.open_plugin_modal(&plugin, &page, &query, target, None),
            (_, OpenAs::Modal) => self.open_plugin_modal(&plugin, &page, &query, target, Some((page.width, page.height))),
            _ => self.open_plugin_tab(&plugin, &page, &query),
        }
    }

    // ---- the debug window script ----

    /// `BROWSECRAFT_WINDOW_SCRIPT` steps with an argument (debug builds):
    /// `command:<key>`, `plugin-command:<plugin>:<command>`, `open:<url>`,
    /// `modal-js:<file>`, `tab-js:<file>`, `key:<combo>` (a plugin shortcut
    /// as if typed in the page).
    #[cfg(debug_assertions)]
    pub(super) fn plugin_window_step(&mut self, op: &str) {
        let Some((verb, arg)) = op.split_once(':') else { return };
        match verb {
            "command" => match crate::commands::from_key(arg) {
                Some(id) => self.run_command(id),
                None => eprintln!("browsecraft: window test hook: no command {arg}"),
            },
            "plugin-command" => {
                if let Some((plugin, command)) = arg.split_once(':') {
                    let target = if self.web_page { Target::Page(self.tab_id) } else { Target::Nothing };
                    self.run_plugin_command(plugin, command, target);
                }
            }
            "key" => match shortcuts::KeyCombo::parse(arg) {
                Ok(k) => {
                    let uri = self.current_url();
                    match key_decision(&uri, k.ctrl, k.shift, k.alt, &k.key) {
                        GtkKey::Command(p, c) => {
                            let tab = self.tab_id;
                            self.plugin_host_message(HostMessage::Command { plugin: p, command: c, tab, address: false });
                        }
                        other => eprintln!("browsecraft: window test hook: {arg} -> {other:?}"),
                    }
                }
                Err(e) => eprintln!("browsecraft: window test hook: {e}"),
            },
            "open" => self.open_in_new_tab(arg.to_string()),
            "modal-js" | "tab-js" => match std::fs::read_to_string(arg) {
                Ok(script) if verb == "modal-js" => {
                    if let Some(m) = &self.feat.host.modal {
                        let _ = m.view.evaluate_script(&script);
                    }
                }
                Ok(script) => {
                    if let Some(view) = self.view_of(self.tab_id) {
                        let _ = view.evaluate_script(&script);
                    }
                }
                Err(e) => eprintln!("browsecraft: window test hook: {arg}: {e}"),
            },
            _ => eprintln!("browsecraft: window test hook: unknown step {op}"),
        }
        self.request_redraw();
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::Key;

    /// `shortcuts::BROWSER_KEYS` (what plugins may not take without
    /// `shortcuts.override`) lists exactly the keys `shortcut_for` uses.
    #[test]
    fn plugin_shortcut_table_matches_the_browsers_keys() {
        for (k, what) in shortcuts::BROWSER_KEYS {
            let c = shortcuts::KeyCombo::parse(k).unwrap();
            let key = match c.key.as_str() {
                "tab" => Key::Named(NamedKey::Tab),
                "pageup" => Key::Named(NamedKey::PageUp),
                "pagedown" => Key::Named(NamedKey::PageDown),
                "delete" => Key::Named(NamedKey::Delete),
                "left" => Key::Named(NamedKey::ArrowLeft),
                "right" => Key::Named(NamedKey::ArrowRight),
                "home" => Key::Named(NamedKey::Home),
                "f1" => Key::Named(NamedKey::F1),
                "f5" => Key::Named(NamedKey::F5),
                "f6" => Key::Named(NamedKey::F6),
                "f11" => Key::Named(NamedKey::F11),
                "f12" => Key::Named(NamedKey::F12),
                ch => Key::Character(ch.into()),
            };
            assert!(super::super::shortcut_for(&key, c.ctrl, c.shift, c.alt, false).is_some(), "{k} ({what}) is not a browser key");
        }
        // And every Ctrl / Ctrl+Shift letter or digit the browser takes is listed.
        let mut missing = Vec::new();
        for ch in "abcdefghijklmnopqrstuvwxyz0123456789,=-+".chars() {
            for shift in [false, true] {
                let key = Key::Character(ch.to_string().into());
                if super::super::shortcut_for(&key, true, shift, false, false).is_some() {
                    let combo = shortcuts::KeyCombo { ctrl: true, shift, alt: false, key: ch.to_string() };
                    if shortcuts::builtin_use(&combo).is_none() {
                        missing.push(format!("Ctrl+{}{ch}", if shift { "Shift+" } else { "" }));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "missing from BROWSER_KEYS: {missing:?}");
    }

    #[test]
    fn keys_in_plugin_pages_and_web_pages() {
        shortcuts::install(vec![shortcuts::Active {
            combo: shortcuts::KeyCombo::parse("Ctrl+V").unwrap(),
            plugin: "local.t.clip".into(),
            command: "open".into(),
            overrides: Some("paste"),
        }]);
        assert_eq!(key_decision("https://a.test/", true, false, false, "v"), GtkKey::Command("local.t.clip".into(), "open".into()));
        assert_eq!(key_decision("https://a.test/", true, true, false, "v"), GtkKey::Browser);
        // Plugin pages keep their editing keys and never trigger plugin commands.
        let page = "bc-plugin://local.t.shot/editor.html";
        assert_eq!(key_decision(page, true, false, false, "v"), GtkKey::PageOwns);
        assert_eq!(key_decision(page, true, false, false, "s"), GtkKey::PageOwns);
        assert_eq!(key_decision(page, true, true, false, "z"), GtkKey::PageOwns);
        assert_eq!(key_decision(page, true, false, false, "w"), GtkKey::Browser, "closing the tab still works");
        assert_eq!(key_decision(page, false, false, false, "f11"), GtkKey::Browser);
        shortcuts::install(Vec::new());
        assert_eq!(key_decision("https://a.test/", true, false, false, "v"), GtkKey::Browser, "plugin off: Ctrl+V pastes");
    }

    #[test]
    fn saved_file_names_are_clean_and_unique() {
        assert_eq!(clean_file_name("../../etc/passwd", "x.png"), "passwd.png");
        assert_eq!(clean_file_name("shot 1.png", "x.png"), "shot 1.png");
        assert_eq!(clean_file_name("", "fallback.png"), "fallback.png");
        assert_eq!(clean_file_name("..png", "f.png"), "f.png");
        assert_eq!(clean_file_name("a<b>:c", "f.png"), "a_b__c.png");
        let d = tempfile::tempdir().unwrap();
        let a = write_unique(d.path(), "s.png", b"1").unwrap();
        let b = write_unique(d.path(), "s.png", b"2").unwrap();
        assert_ne!(a, b);
        assert!(b.ends_with("s-2.png"));
        assert_eq!(uri_to_path("file:///home/u/My%20Doc.pdf").unwrap(), std::path::PathBuf::from("/home/u/My Doc.pdf"));
        assert!(uri_to_path("https://x").is_none());
    }
}
