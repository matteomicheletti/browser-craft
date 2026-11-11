//! Several browser windows in one process.
//!
//! [`App`] is the winit application: it owns one [`Browser`] per window and
//! routes every event to the right one (window events by window id, web
//! view events by tab id, engine loads by their generation's window key).
//! What the windows share lives in [`Shared`]: the web engine's data stores
//! (one per container, so a tab moved between windows keeps its cookies
//! and its live page), tab ids, the closed-tab stack (Ctrl+Shift+T works
//! across windows), the page pictures and the list of windows for menus.
//!
//! A window asks for things that involve other windows through
//! [`AppRequest`]s (a new window, moving a tab, going to another window's
//! tab, a tab dropped outside the window); the app applies them after each
//! event.
//!
//! Moving a tab keeps its page: its web view (wry's X11 container window)
//! is reparented into the destination window (`x11::reparent_webview`).
//! When that is not possible (not X11) the web view is dropped and the tab
//! reloads its address in the new window, keeping its history.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::window::WindowId;

use super::{Browser, ClosedTab, SESSION_FILE, StartTab, TabMessage, TabState, UserMessage, WebCtx, WebPane};
use crate::flags;
use crate::font::Fonts;
use crate::loader::Request;

/// Window keys go in the top bits of each window's engine load generation
/// and automation job ids, so their messages find their window.
pub(super) const KEY_SHIFT: u32 = 40;
/// How often the session file is written (when it changed).
const SESSION_INTERVAL: Duration = Duration::from_secs(5);

/// State every window shares.
pub(super) struct Shared {
    /// The web engine's data stores, one per container ("" = no container).
    pub contexts: HashMap<String, WebCtx>,
    pub next_tab_id: u64,
    pub closed_tabs: Vec<ClosedTab>,
    /// Page pictures by tab id (overview, hover cards).
    pub thumbs: crate::thumbnails::ThumbnailCache,
    /// Favicons by tab id, as RGBA, so another window can draw them.
    pub favicons: HashMap<u64, (usize, usize, std::sync::Arc<Vec<u8>>)>,
    /// The system web view works (found by the first window).
    pub wry_ok: Option<bool>,
    /// A chrome text field has the keyboard in the active window: pages
    /// forward keys to it (`attach_webview_shortcuts_gtk`).
    pub chrome_keys: Rc<std::cell::Cell<bool>>,
    /// Bumped when a web view stops showing the page it showed; pop-ups and
    /// fullscreen requests from an older page are ignored.
    pub web_epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The windows, for menus: (key, label).
    pub windows: Vec<(u64, String)>,
    /// What every window shows, for the tab overview.
    pub summaries: Vec<super::overview::WindowSummary>,
    /// The automation server runs (in one of the windows).
    pub automation_on: bool,
}

impl Shared {
    pub fn new() -> Self {
        Shared {
            contexts: HashMap::new(),
            next_tab_id: 1,
            closed_tabs: Vec::new(),
            thumbs: crate::thumbnails::ThumbnailCache::default(),
            favicons: HashMap::new(),
            wry_ok: None,
            chrome_keys: Rc::new(std::cell::Cell::new(false)),
            web_epoch: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            windows: Vec::new(),
            summaries: Vec::new(),
            automation_on: false,
        }
    }

    pub fn alloc_tab_id(&mut self) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        id
    }

    /// The data store of `container`'s tabs, created on first use.
    pub fn web_context(&mut self, container: Option<&str>) -> Result<Option<&mut wry::WebContext>, String> {
        let key = container.unwrap_or("").to_string();
        if !self.contexts.contains_key(&key) {
            match super::new_web_context(container)? {
                Some(ctx) => {
                    self.contexts.insert(key.clone(), ctx);
                }
                None => return Ok(None),
            }
        }
        Ok(self.contexts.get_mut(&key).map(|c| &mut c.context))
    }
}

/// Where a tab goes.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum MoveTarget {
    /// A new window, at this screen position (physical pixels) if given.
    NewWindow(Option<(i32, i32)>),
    /// Window `key`, at this tab index (the end when `None`).
    Window(u64, Option<usize>),
}

/// What a window asks of the application.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum AppRequest {
    /// Ctrl+N: a new window with a new tab.
    NewWindow,
    MoveTab { tab: u64, to: MoveTarget },
    /// Go to this tab, in whatever window it is (the overview).
    ActivateTab(u64),
    /// Close this tab, in whatever window it is (the overview).
    CloseTab(u64),
    /// A tab dragged out of the strip was released at this screen
    /// position (physical pixels), outside its window.
    DropTab { tab: u64, at: (f64, f64) },
    /// Drop the data stores no tab in any window uses.
    ReleaseContexts,
    /// Automation was turned off in a window that does not run the server.
    StopAutomation,
    /// New automation settings from a window that does not run the server.
    AutomationSettings(crate::automation::Settings),
    /// Open a session preset (`crate::presets`).
    OpenPreset { id: u64, target: crate::presets::OpenTarget },
    /// Save this window's tabs (`all`: every window's) as a new preset.
    SavePreset { name: String, all: bool },
    /// Replace a preset's tabs with the current ones.
    UpdatePreset { id: u64, all: bool },
    /// The start-up chooser's "Previous session".
    RestorePrevious,
}

/// How a window starts.
#[derive(Default)]
pub(super) struct WindowSetup {
    pub position: Option<(i32, i32)>,
    pub size: Option<(u32, u32)>,
    pub maximized: bool,
    pub name: Option<String>,
}

impl WindowSetup {
    /// The window part of a session window entry.
    fn from_session(w: &Value) -> Self {
        let bounds = w.get("bounds");
        let num = |k: &str| bounds.and_then(|b| b.get(k)).and_then(Value::as_i64);
        let position = match (num("x"), num("y")) {
            (Some(x), Some(y)) if x.abs() < 100_000 && y.abs() < 100_000 => Some((x as i32, y as i32)),
            _ => None,
        };
        let size = match (num("w"), num("h")) {
            (Some(w), Some(h)) if (200..=20_000).contains(&w) && (200..=20_000).contains(&h) => Some((w as u32, h as u32)),
            _ => None,
        };
        WindowSetup {
            position,
            size,
            maximized: w.get("maximized").and_then(Value::as_bool).unwrap_or(false),
            name: w.get("name").and_then(Value::as_str).map(clean_window_name).filter(|n| !n.is_empty()),
        }
    }
}

/// A tab on its way to another window.
pub(super) struct MovedTab {
    pub state: TabState,
    pub console: Option<crate::automation::console::ConsoleLog>,
}

/// The longest window name kept.
pub(super) const MAX_WINDOW_NAME: usize = 60;

/// A window name as typed: one line, trimmed, at most `MAX_WINDOW_NAME`
/// characters.
pub(super) fn clean_window_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(MAX_WINDOW_NAME).collect::<String>().trim().to_string()
}

/// The label of the window at `position` (0-based) named `name`.
pub(super) fn window_label(name: Option<&str>, position: usize) -> String {
    match name {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => format!("Window {}", position + 1),
    }
}

/// The tab index a tab dropped at `x` goes to, from the chips' rectangles
/// (`(tab index, left, right)` in strip order): before the first chip whose
/// middle is right of `x`, or at the end.
pub(super) fn drop_index(chips: &[(usize, f32, f32)], x: f32, count: usize) -> usize {
    chips.iter().find(|(_, l, r)| x < (l + r) / 2.0).map_or(count, |(i, _, _)| *i)
}

/// The new order of `order` with tab `id` moved to index `to` (an index
/// in the old order, as `drop_index` gives).
pub(super) fn reordered(order: &[u64], id: u64, to: usize) -> Vec<u64> {
    let Some(from) = order.iter().position(|t| *t == id) else {
        return order.to_vec();
    };
    let mut out = order.to_vec();
    out.remove(from);
    let to = if to > from { to - 1 } else { to };
    out.insert(to.min(out.len()), id);
    out
}

/// The tab that becomes active when the active tab `i` of `len` leaves the
/// window: its split partner, else the next tab, else the previous one.
pub(super) fn successor(i: usize, len: usize, partner: Option<usize>) -> Option<usize> {
    if len < 2 {
        return None;
    }
    partner.filter(|p| *p != i && *p < len).or(Some(if i + 1 < len { i + 1 } else { i.saturating_sub(1) }))
}

/// Where a tab arriving in a window of `len` tabs goes: `index`, or the end.
pub(super) fn insert_index(index: Option<usize>, len: usize) -> usize {
    index.unwrap_or(len).min(len)
}

/// A tab leaving its window leaves its group (an emptied group goes) and the
/// split view (which ends). Returns the split that remains.
pub(super) fn leave_layout(groups: &mut crate::tab_groups::TabGroups, split: Option<crate::split::Split>, tab: u64) -> Option<crate::split::Split> {
    groups.forget_tab(tab);
    split.and_then(|s| s.after_close(tab))
}

/// The window entries of a session file: `windows`, or the root itself in
/// files from before multiple windows.
pub(super) fn session_windows(text: &str) -> Vec<Value> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    match value.get("windows").and_then(Value::as_array) {
        Some(list) => list.iter().filter(|w| w.is_object()).take(super::MAX_SESSION_WINDOWS).cloned().collect(),
        None if value.is_object() => vec![value],
        None => Vec::new(),
    }
}

/// The session file for these window entries. The first window is also
/// written at the root, so an older version still restores it.
pub(super) fn session_text(windows: &[Value]) -> String {
    let mut root = windows.first().cloned().unwrap_or_else(|| json!({ "active": 0, "tabs": [] }));
    root["windows"] = json!(windows);
    root.to_string()
}

/// The notice after opening `name`, mentioning tabs that lost their
/// container.
pub(super) fn preset_notice(name: &str, missing: usize) -> String {
    let opened = if name == "the previous session" { "Restored the previous session".to_string() } else { format!("Opened \u{201c}{name}\u{201d}") };
    match missing {
        0 => opened,
        1 => format!("{opened}; 1 tab opened without its container, which no longer exists"),
        n => format!("{opened}; {n} tabs opened without their containers, which no longer exist"),
    }
}

/// The application: every browser window.
/// How often bookmarks.json is checked for changes by other processes.
const BOOKMARKS_POLL: Duration = Duration::from_secs(2);

pub struct App {
    proxy: EventLoopProxy<UserMessage>,
    shared: Rc<RefCell<Shared>>,
    browsers: Vec<Browser>,
    start_tabs: Vec<StartTab>,
    /// `--preset NAME` from the command line.
    start_preset: Option<String>,
    first_fonts: Option<Fonts>,
    next_key: u64,
    /// The window the user last used (keys from pages, new tabs from
    /// automation, the notices of background work go there).
    last_active: u64,
    saved_session: String,
    last_save: Instant,
    quit: bool,
    /// Closed windows, hidden, dropped a moment later: destroying a window
    /// that still has the X focus makes winit fail on the focus-out event
    /// that follows (BadWindow in its input method code).
    dying: Vec<(Instant, Browser)>,
    /// When bookmarks.json was last checked for changes made by another
    /// process (`browsecraft import`).
    bookmarks_checked: Instant,
    /// Debug test hook steps for the application: (when, step).
    #[cfg(debug_assertions)]
    script: Vec<(Instant, String)>,
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserMessage>, fonts: Fonts, start_tabs: Vec<StartTab>, start_preset: Option<String>) -> Self {
        let imports = proxy.clone();
        crate::import::set_notifier(move |event| {
            let _ = imports.send_event(UserMessage::Import(event));
        });
        App {
            bookmarks_checked: Instant::now(),
            proxy,
            shared: Rc::new(RefCell::new(Shared::new())),
            browsers: Vec::new(),
            start_tabs,
            start_preset,
            first_fonts: Some(fonts),
            next_key: 1,
            last_active: 1,
            saved_session: String::new(),
            last_save: Instant::now(),
            quit: false,
            dying: Vec::new(),
            #[cfg(debug_assertions)]
            script: Vec::new(),
        }
    }

    fn fonts(&mut self) -> Option<Fonts> {
        if let Some(f) = self.first_fonts.take() {
            return Some(f);
        }
        match Fonts::new() {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("browsecraft: {e}");
                None
            }
        }
    }

    /// A new (empty) browser for a new window; `None` when its fonts or
    /// window cannot be created.
    fn spawn(&mut self, event_loop: &ActiveEventLoop, setup: WindowSetup, start: Vec<StartTab>) -> Option<usize> {
        let fonts = self.fonts()?;
        let key = self.next_key;
        self.next_key += 1;
        let mut b = Browser::new(self.proxy.clone(), fonts, start, self.shared.clone(), key);
        b.win_name = setup.name.clone();
        self.browsers.push(b);
        self.refresh_window_list();
        let i = self.browsers.len() - 1;
        if !self.browsers[i].open_window(event_loop, &setup) {
            self.browsers.pop();
            self.refresh_window_list();
            return None;
        }
        self.last_active = key;
        Some(i)
    }

    fn index_of_key(&self, key: u64) -> Option<usize> {
        self.browsers.iter().position(|b| b.win_key == key)
    }

    fn index_of_window(&self, id: WindowId) -> Option<usize> {
        self.browsers.iter().position(|b| b.gfx.as_ref().is_some_and(|g| g.window.id() == id))
    }

    fn owner_of_tab(&self, tab: u64) -> Option<usize> {
        self.browsers.iter().position(|b| b.tab_index(tab).is_some())
    }

    fn active_index(&self) -> usize {
        self.index_of_key(self.last_active).unwrap_or(0)
    }

    /// Updates what the windows know about each other (labels for menus,
    /// the automation flag, which one is active).
    fn refresh_window_list(&mut self) {
        let list: Vec<(u64, String)> =
            self.browsers.iter().enumerate().map(|(i, b)| (b.win_key, window_label(b.win_name.as_deref(), i))).collect();
        let on = self.browsers.iter().any(|b| b.feat.automation.is_some());
        let mut sh = self.shared.borrow_mut();
        sh.windows = list;
        sh.automation_on = on;
        drop(sh);
        let active = self.last_active;
        for b in &mut self.browsers {
            b.is_last_active = b.win_key == active;
        }
    }

    /// What every window shows, for the overview (only while one is open).
    fn refresh_summaries(&mut self) {
        if !self.browsers.iter().any(|b| b.overview.open) {
            return;
        }
        let summaries = self.browsers.iter().enumerate().map(|(i, b)| b.summary(i)).collect();
        self.shared.borrow_mut().summaries = summaries;
    }

    /// Applies what the windows asked for, until nothing is left.
    fn process(&mut self, event_loop: &ActiveEventLoop) {
        for _ in 0..8 {
            let mut requests = Vec::new();
            for b in &mut self.browsers {
                for r in b.app_requests.drain(..) {
                    requests.push((b.win_key, r));
                }
            }
            if requests.is_empty() {
                break;
            }
            for (from, r) in requests {
                self.request(event_loop, from, r);
            }
        }
        if self.browsers.iter().any(|b| b.quit_app) {
            self.quit = true;
        }
        // Windows that closed (their last tab, the close button).
        let closing: Vec<u64> = self.browsers.iter().filter(|b| b.quit).map(|b| b.win_key).collect();
        for key in closing {
            self.close_window(event_loop, key);
        }
        if self.quit {
            self.save_session(true);
            event_loop.exit();
        }
        self.refresh_window_list();
    }

    fn request(&mut self, event_loop: &ActiveEventLoop, from: u64, r: AppRequest) {
        match r {
            AppRequest::NewWindow => {
                if let Some(i) = self.spawn(event_loop, WindowSetup::default(), Vec::new()) {
                    self.browsers[i].new_window_tab();
                }
            }
            AppRequest::MoveTab { tab, to } => self.move_tab(event_loop, tab, to),
            AppRequest::ActivateTab(tab) => {
                if let Some(i) = self.owner_of_tab(tab) {
                    let b = &mut self.browsers[i];
                    b.overview.hide();
                    if let Some(j) = b.tab_index(tab) {
                        b.switch_tab(j);
                    }
                    b.focus_window();
                    self.last_active = b.win_key;
                }
            }
            AppRequest::CloseTab(tab) => {
                if let Some(i) = self.owner_of_tab(tab) {
                    let b = &mut self.browsers[i];
                    if let Some(j) = b.tab_index(tab) {
                        b.close_tab(j);
                    }
                    b.request_redraw();
                }
                if let Some(i) = self.index_of_key(from) {
                    self.browsers[i].request_redraw();
                }
            }
            AppRequest::DropTab { tab, at } => self.drop_tab(event_loop, from, tab, at),
            AppRequest::ReleaseContexts => self.release_contexts(),
            AppRequest::AutomationSettings(s) => {
                for b in &mut self.browsers {
                    b.feat.automation_settings = s.clone();
                    #[cfg(unix)]
                    if let Some(h) = &b.feat.automation {
                        h.set_settings(s.clone());
                    }
                }
            }
            AppRequest::StopAutomation => {
                for b in &mut self.browsers {
                    b.feat.automation = None;
                    b.feat.automation_settings.enabled = false;
                    b.request_redraw();
                }
            }
            AppRequest::OpenPreset { id, target } => {
                let result = self.open_preset(event_loop, from, id, target);
                self.preset_result(result);
            }
            AppRequest::SavePreset { name, all } => {
                let result = self.save_preset(from, &name, all).map(|_| ());
                let result = result.map(|()| format!("Saved preset \u{201c}{}\u{201d}", crate::presets::clean_name(&name)));
                self.preset_result(result);
            }
            AppRequest::UpdatePreset { id, all } => {
                let windows = self.capture_windows(from, all);
                let result = crate::presets::modify(|s| s.update(id, windows, crate::presets::now()))
                    .map(|()| format!("Updated preset \u{201c}{}\u{201d}", crate::presets::get(id).map(|p| p.name).unwrap_or_default()));
                self.preset_result(result);
            }
            AppRequest::RestorePrevious => {
                let result = self.restore_previous(event_loop, from);
                self.preset_result(result);
            }
        }
    }

    // ---- session presets ----

    /// Shows the outcome of a preset action: a notice in the active
    /// window, and on the presets page wherever it is open.
    fn preset_result(&mut self, result: Result<String, String>) {
        crate::presets_page::set_message(result.clone());
        let i = self.active_index();
        if let Some(b) = self.browsers.get_mut(i) {
            match result {
                Ok(m) | Err(m) => b.set_notice_long(m),
            }
        }
        for b in &mut self.browsers {
            b.refresh_if_internal(crate::presets_page::URL);
        }
    }

    /// Window snapshots of window `from`, or of every window (`all`).
    fn capture_windows(&self, from: u64, all: bool) -> Vec<crate::presets::PresetWindow> {
        let values: Vec<Value> = self.browsers.iter().filter(|b| all || b.win_key == from).map(Browser::session_value).collect();
        crate::presets::capture(&values)
    }

    /// Saves window `from` (or every window) as a new preset.
    fn save_preset(&mut self, from: u64, name: &str, all: bool) -> Result<u64, String> {
        let windows = self.capture_windows(from, all);
        crate::presets::modify(|s| s.add(name, windows, crate::presets::now()))
    }

    /// Opens `windows` as new windows (each with its name), loading only
    /// each one's shown tab. Returns how many opened.
    fn open_new_windows(&mut self, event_loop: &ActiveEventLoop, windows: &[crate::presets::PresetWindow]) -> usize {
        let mut opened = 0;
        for w in windows {
            let setup = WindowSetup { name: w.name.clone(), ..WindowSetup::default() };
            if let Some(i) = self.spawn(event_loop, setup, Vec::new()) {
                if !self.browsers[i].restore_window(&w.to_session()) {
                    self.browsers[i].new_window_tab();
                }
                opened += 1;
            }
        }
        opened
    }

    /// Opens preset `id` in new windows or in window `from`.
    fn open_preset(&mut self, event_loop: &ActiveEventLoop, from: u64, id: u64, target: crate::presets::OpenTarget) -> Result<String, String> {
        use crate::presets::OpenTarget;
        let p = crate::presets::get(id).ok_or("That preset no longer exists")?;
        let missing = match target {
            OpenTarget::NewWindow => {
                let gone = p.missing_containers(&|c| crate::containers::get(c).is_some());
                if self.open_new_windows(event_loop, &p.windows) == 0 {
                    return Err(format!("Could not open \u{201c}{}\u{201d}", p.name));
                }
                gone
            }
            OpenTarget::Here | OpenTarget::Replace => {
                let i = self.index_of_key(from).ok_or("The window is gone")?;
                let (opened, gone) = self.browsers[i].open_preset_here(&p.windows, target == OpenTarget::Replace);
                if opened == 0 {
                    return Err(format!("\u{201c}{}\u{201d} has no tabs to open here", p.name));
                }
                self.last_active = from;
                gone
            }
        };
        Ok(preset_notice(&p.name, missing))
    }

    /// The start-up chooser's "Previous session": its first window in
    /// place of window `from`'s tabs, the others as new windows.
    fn restore_previous(&mut self, event_loop: &ActiveEventLoop, from: u64) -> Result<String, String> {
        let path = crate::storage::config_file(crate::presets::PREVIOUS_SESSION_FILE)?;
        let text = crate::storage::read_text_limited(&path, crate::storage::MAX_SETTINGS_FILE).map_err(|_| "There is no previous session".to_string())?;
        let mut windows = session_windows(&text);
        if windows.is_empty() {
            return Err("There is no previous session".into());
        }
        let rest = windows.split_off(1);
        let first = crate::presets::PresetWindow::from_session(&windows[0]);
        let i = self.index_of_key(from).ok_or("The window is gone")?;
        let (_, missing) = self.browsers[i].open_preset_here(std::slice::from_ref(&first), true);
        self.restore_rest(event_loop, rest);
        Ok(preset_notice("the previous session", missing))
    }

    /// A tab dragged out of window `from` was released at screen point
    /// `at`: onto another window (its strip position, or the end), or
    /// outside every window (a new window there).
    fn drop_tab(&mut self, event_loop: &ActiveEventLoop, from: u64, tab: u64, at: (f64, f64)) {
        let target = self.browsers.iter().find(|b| b.win_key != from && b.contains_screen_point(at)).map(|b| b.win_key);
        match target {
            Some(key) => {
                let index = self.index_of_key(key).and_then(|i| self.browsers[i].strip_drop_index(at));
                self.move_tab(event_loop, tab, MoveTarget::Window(key, index));
            }
            None => {
                let inside_source = self.index_of_key(from).is_some_and(|i| self.browsers[i].contains_screen_point(at));
                if !inside_source {
                    let pos = (at.0.round() as i32 - 120, at.1.round() as i32 - 16);
                    self.move_tab(event_loop, tab, MoveTarget::NewWindow(Some(pos)));
                }
            }
        }
    }

    /// Moves tab `tab` (with its live page) to another window.
    fn move_tab(&mut self, event_loop: &ActiveEventLoop, tab: u64, to: MoveTarget) {
        let Some(src) = self.owner_of_tab(tab) else { return };
        let src_key = self.browsers[src].win_key;
        let alone = self.browsers[src].tabs.len() == 1;
        match &to {
            MoveTarget::Window(key, _) if *key == src_key => return,
            MoveTarget::NewWindow(_) if alone => {
                self.browsers[src].set_notice("This is the only tab in this window");
                return;
            }
            MoveTarget::Window(key, _) if self.index_of_key(*key).is_none() => return,
            _ => {}
        }
        let dst = match to {
            MoveTarget::Window(key, _) => self.index_of_key(key),
            MoveTarget::NewWindow(pos) => {
                let setup = WindowSetup {
                    position: pos,
                    size: self.browsers[src].gfx.as_ref().map(|g| {
                        let s = g.window.inner_size();
                        (s.width, s.height)
                    }),
                    ..WindowSetup::default()
                };
                self.spawn(event_loop, setup, Vec::new())
            }
        };
        let Some(dst) = dst else { return };
        let Some(moved) = self.browsers[src].take_tab(tab) else { return };
        let index = match to {
            MoveTarget::Window(_, index) => index,
            MoveTarget::NewWindow(_) => None,
        };
        let fresh = matches!(to, MoveTarget::NewWindow(_));
        let placeholder = fresh.then(|| self.browsers[dst].tab_id);
        self.browsers[dst].adopt_tab(moved, index);
        if let Some(id) = placeholder
            && let Some(j) = self.browsers[dst].tab_index(id)
        {
            self.browsers[dst].close_tab(j);
        }
        self.browsers[dst].focus_window();
        self.last_active = self.browsers[dst].win_key;
        if self.browsers[src].tabs.is_empty() {
            // Its last tab left: the window goes (nothing to remember).
            self.browsers[src].quit = true;
            self.browsers[src].moved_out = true;
        } else {
            self.browsers[src].request_redraw();
        }
        self.browsers[dst].request_redraw();
    }

    /// Closes window `key`: its tabs go to the closed-tab stack (unless they
    /// were all moved out). The last window keeps its tabs for the session
    /// and ends the application.
    fn close_window(&mut self, event_loop: &ActiveEventLoop, key: u64) {
        let Some(i) = self.index_of_key(key) else { return };
        if self.browsers.len() == 1 {
            self.quit = true;
            self.save_session(true);
            event_loop.exit();
            return;
        }
        let mut b = self.browsers.remove(i);
        if !b.moved_out {
            b.remember_all_tabs();
        }
        // The automation server lives in one window: hand it to another.
        if b.feat.automation.is_some()
            && let Some(next) = self.browsers.first_mut()
        {
            super::features::hand_over_automation(&mut b, next);
        }
        b.retire();
        self.dying.push((Instant::now() + Duration::from_secs(1), b));
        // The window the user goes back to.
        if let Some(next) = self.browsers.first() {
            next.focus_window();
        }
        if self.last_active == key {
            self.last_active = self.browsers.first().map_or(0, |b| b.win_key);
        }
        self.release_contexts();
        self.refresh_window_list();
        self.save_session(true);
    }

    /// Drops the data stores no tab uses; a temporary container ends with
    /// its last tab in any window.
    fn release_contexts(&mut self) {
        let mut used = std::collections::HashSet::new();
        for b in &self.browsers {
            used.extend(b.used_containers());
        }
        self.shared.borrow_mut().contexts.retain(|key, _| key.is_empty() || used.contains(key));
        for temp in crate::containers::temporary() {
            if !used.contains(&temp.id) {
                crate::containers::end_temporary(&temp.id);
            }
        }
        // Pictures and favicons of tabs that are gone.
        let live: std::collections::HashSet<u64> = self.browsers.iter().flat_map(|b| b.tab_order()).collect();
        let mut sh = self.shared.borrow_mut();
        sh.thumbs.retain(|id| live.contains(&id));
        sh.favicons.retain(|id, _| live.contains(id));
    }

    /// Writes the session (every window) when it changed, or deletes it
    /// when tabs are not to be restored.
    fn save_session(&mut self, now: bool) {
        if !now && self.last_save.elapsed() < SESSION_INTERVAL {
            return;
        }
        self.last_save = Instant::now();
        let Ok(path) = crate::storage::config_file(SESSION_FILE) else {
            return;
        };
        if !flags::get().restore_tabs {
            if !self.saved_session.is_empty() || path.exists() {
                let _ = std::fs::remove_file(&path);
                self.saved_session.clear();
            }
            return;
        }
        let windows: Vec<Value> = self
            .browsers
            .iter()
            .map(Browser::session_value)
            .filter(|w| w.get("tabs").and_then(Value::as_array).is_some_and(|t| !t.is_empty()))
            .collect();
        if windows.is_empty() {
            return;
        }
        let text = session_text(&windows);
        if text != self.saved_session && crate::storage::write_private(&path, text.as_bytes()).is_ok() {
            self.saved_session = text;
        }
    }

    /// Restores the other windows of the session (the first one went to
    /// the first window), then the saved page pictures.
    fn restore_rest(&mut self, event_loop: &ActiveEventLoop, rest: Vec<Value>) {
        for w in rest {
            let setup = WindowSetup::from_session(&w);
            if let Some(i) = self.spawn(event_loop, setup, Vec::new())
                && !self.browsers[i].restore_window(&w)
            {
                self.browsers[i].new_window_tab();
            }
        }
        if let Some(first) = self.browsers.first() {
            self.last_active = first.win_key;
        }
        // Pictures of the restored tabs, read in the background; then the
        // pictures of addresses no longer open are deleted.
        let mut wanted: Vec<(u64, String)> = Vec::new();
        for b in &self.browsers {
            for i in 0..b.tabs.len() {
                let url = b.tab_url(i);
                if !url.is_empty() && b.tab_container(i).is_none_or(|c| !c.starts_with("tmp-")) {
                    wanted.push((b.tab_id_at(i), url));
                }
            }
        }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            for (id, url) in &wanted {
                if let Some((w, h, rgba)) = crate::thumbnails::load(url) {
                    let _ = proxy.send_event(UserMessage::Tab(*id, TabMessage::Thumbnail { w, h, rgba, save: false }));
                }
            }
            let urls: Vec<String> = wanted.into_iter().map(|(_, u)| u).collect();
            crate::thumbnails::prune(&urls);
        });
    }

    /// Where a message from a background task goes.
    fn route(&self, message: &UserMessage) -> Option<usize> {
        let by_key = |key: u64| self.index_of_key(key);
        match message {
            UserMessage::Load(generation, _) => by_key(generation >> KEY_SHIFT),
            UserMessage::AutomationScript { job, .. } => by_key(job >> KEY_SHIFT),
            UserMessage::Tab(tab, _) | UserMessage::PageClicked(tab) => self.owner_of_tab(*tab),
            UserMessage::RecordingStopped(_) => {
                self.browsers.iter().position(|b| b.feat.recording_stopping || b.feat.recorder.elapsed().is_some()).or(Some(self.active_index()))
            }
            _ => Some(self.active_index()),
        }
    }

    /// An automation request: `tabs.list` covers every window; a request
    /// naming a tab goes to its window, `tabs.open` to the `window` asked
    /// for; the rest to the active window.
    fn automation(&mut self, event_loop: &ActiveEventLoop, req: crate::automation::Request) {
        use crate::automation::{Method, RpcError};
        if !self.browsers.iter().any(|b| b.feat.automation.is_some()) {
            req.respond(Err(RpcError::denied("automation is disabled")));
            return;
        }
        if matches!(req.method, Method::SessionsList | Method::SessionsOpen | Method::SessionsSave) {
            let result = self.automation_sessions(event_loop, &req);
            req.respond(result);
            return;
        }
        if req.method == Method::TabsList {
            let mut tabs = Vec::new();
            let mut windows = Vec::new();
            for (n, b) in self.browsers.iter().enumerate() {
                tabs.extend((0..b.tabs.len()).map(|i| b.tab_info(i)));
                windows.push(b.window_info(n));
            }
            let result = crate::automation::protocol::TabsListResult { tabs, windows };
            req.respond(serde_json::to_value(&result).map_err(|e| RpcError::new(RpcError::INTERNAL, e.to_string())));
            return;
        }
        let target = if req.method == Method::TabsOpen {
            match req.params.get("window").and_then(Value::as_u64) {
                Some(key) => match self.index_of_key(key) {
                    Some(i) => i,
                    None => {
                        req.respond(Err(RpcError::browser(format!("no window with id {key} (see tabs.list)"))));
                        return;
                    }
                },
                None => self.active_index(),
            }
        } else {
            match req.params.get("tab").and_then(Value::as_u64) {
                Some(tab) => self.owner_of_tab(tab).unwrap_or_else(|| self.active_index()),
                None => self.active_index(),
            }
        };
        if let Some(b) = self.browsers.get_mut(target) {
            b.automation_request(req);
        }
    }

    /// `sessions.list`, `sessions.open` and `sessions.save`.
    fn automation_sessions(&mut self, event_loop: &ActiveEventLoop, req: &crate::automation::Request) -> Result<Value, crate::automation::RpcError> {
        use crate::automation::protocol as p;
        use crate::automation::{Method, RpcError};
        use crate::presets::OpenTarget;
        let info = |pr: &crate::presets::Preset| p::SessionInfo {
            name: pr.name.clone(),
            windows: pr.windows.len(),
            tabs: pr.tab_count(),
            created: pr.created,
            updated: pr.updated,
        };
        fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, RpcError> {
            serde_json::to_value(v).map_err(|e| RpcError::new(RpcError::INTERNAL, e.to_string()))
        }
        match req.method {
            Method::SessionsList => {
                let sessions = crate::presets::all().iter().map(info).collect();
                to_value(&p::SessionsListResult { sessions })
            }
            Method::SessionsOpen => {
                let params: p::SessionsOpenParams = req.params()?;
                let preset = crate::presets::find(&params.name).ok_or_else(|| RpcError::browser(format!("no preset named {:?} (see sessions.list)", params.name)))?;
                // As for tabs.open: no local files unless allowed.
                let allow_files = self.browsers.iter().find(|b| b.feat.automation.is_some()).is_some_and(|b| b.feat.automation_settings.allow_file_urls);
                if !allow_files && preset.has_file_tabs() {
                    return Err(RpcError::denied("this preset opens file: URLs, which are disabled (allow_file_urls in automation.toml)"));
                }
                let target = match params.window {
                    p::SessionWindow::New => OpenTarget::NewWindow,
                    p::SessionWindow::Current => OpenTarget::Here,
                };
                let from = self.browsers.get(self.active_index()).map_or(0, |b| b.win_key);
                let notice = self.open_preset(event_loop, from, preset.id, target).map_err(RpcError::browser)?;
                self.refresh_window_list();
                to_value(&p::SessionResult { session: info(&preset), notice: Some(notice) })
            }
            _ => {
                let params: p::SessionsSaveParams = req.params()?;
                let from = self.browsers.get(self.active_index()).map_or(0, |b| b.win_key);
                let id = self.save_preset(from, &params.name, params.scope == p::SessionScope::All).map_err(RpcError::browser)?;
                for b in &mut self.browsers {
                    b.refresh_if_internal(crate::presets_page::URL);
                }
                let saved = crate::presets::get(id).ok_or_else(|| RpcError::browser("the preset was not saved"))?;
                to_value(&p::SessionResult { session: info(&saved), notice: None })
            }
        }
    }

    /// Runs the debug hook's due steps; returns when the next one is due.
    /// Steps act on the first window unless they end in `@N` (window N).
    #[cfg(debug_assertions)]
    fn run_script(&mut self, event_loop: &ActiveEventLoop, now: Instant) -> Option<Instant> {
        let due: Vec<String> = self.script.iter().filter(|(at, _)| *at <= now).map(|(_, s)| s.clone()).collect();
        self.script.retain(|(at, _)| *at > now);
        for step in due {
            let (op, target) = match step.rsplit_once('@') {
                Some((op, n)) if n.parse::<usize>().is_ok() => (op.to_string(), n.parse::<usize>().unwrap_or(1).saturating_sub(1)),
                _ => (step.clone(), 0),
            };
            eprintln!("browsecraft: window test hook: {op} (window {})", target + 1);
            let key = self.browsers.get(target).map(|b| b.win_key);
            if let Some(rest) = op.strip_prefix("move-tab:") {
                // move-tab:<tab index>:<window number | new>
                let mut parts = rest.split(':');
                let (Some(i), Some(dest)) = (parts.next().and_then(|i| i.parse::<usize>().ok()), parts.next()) else { continue };
                let Some(tab) = self.browsers.get(target).map(|b| b.tab_id_at(i)) else { continue };
                let to = match dest {
                    "new" => MoveTarget::NewWindow(None),
                    n => match n.parse::<usize>().ok().and_then(|n| self.browsers.get(n.saturating_sub(1))) {
                        Some(b) => MoveTarget::Window(b.win_key, None),
                        None => continue,
                    },
                };
                self.move_tab(event_loop, tab, to);
            } else if let Some(rest) = op.strip_prefix("drop-tab:") {
                // drop-tab:<tab index>:<screen x>:<screen y>: a strip drag
                // released there.
                let v: Vec<f64> = rest.split(':').filter_map(|p| p.parse().ok()).collect();
                if let ([i, x, y], Some(key)) = (v.as_slice(), key)
                    && let Some(tab) = self.browsers.get(target).map(|b| b.tab_id_at(*i as usize))
                {
                    self.drop_tab(event_loop, key, tab, (*x, *y));
                }
            } else if let Some(rest) = op.strip_prefix("go:") {
                // go:<window number>:<tab index>: the overview's click on
                // a tab of that window.
                let mut parts = rest.split(':').filter_map(|p| p.parse::<usize>().ok());
                if let (Some(w), Some(i), Some(key)) = (parts.next(), parts.next(), key)
                    && let Some(tab) = self.browsers.get(w.saturating_sub(1)).map(|b| b.tab_id_at(i))
                {
                    if let Some(b) = self.browsers.get_mut(target) {
                        b.overview.hide();
                    }
                    self.request(event_loop, key, AppRequest::ActivateTab(tab));
                }
            } else if op == "new-window" {
                if let Some(i) = self.spawn(event_loop, WindowSetup::default(), Vec::new()) {
                    self.browsers[i].new_window_tab();
                }
            } else if op == "close-window" {
                if let Some(b) = self.browsers.get_mut(target) {
                    b.quit = true;
                }
            } else if op == "focus" {
                if let Some(b) = self.browsers.get(target) {
                    b.focus_window();
                    self.last_active = b.win_key;
                }
            } else if let Some(b) = self.browsers.get_mut(target) {
                b.run_script_op(&op);
            }
            if let Some(b) = self.browsers.get(target) {
                eprintln!("browsecraft: window test hook: {} windows; window {} has {} tabs", self.browsers.len(), target + 1, b.tabs.len());
            }
        }
        self.process(event_loop);
        self.script.iter().map(|(at, _)| *at).min()
    }
}

impl ApplicationHandler<UserMessage> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.browsers.is_empty() {
            return;
        }
        // What to open: command-line addresses, `--preset`, or the
        // "On startup" setting (`crate::presets::startup_plan`).
        use crate::presets::StartupPlan;
        let mut start = std::mem::take(&mut self.start_tabs);
        let store = crate::presets::store();
        let (plan, notice) = crate::presets::startup_plan(!start.is_empty(), self.start_preset.as_deref(), flags::get().startup, &store);
        let saved = flags::get()
            .restore_tabs
            .then(|| crate::storage::config_file(SESSION_FILE).ok())
            .flatten()
            .and_then(|path| crate::storage::read_text_limited(&path, crate::storage::MAX_SETTINGS_FILE).ok());
        // The session's windows: the first opens now, the rest after it.
        let mut session = Vec::new();
        let mut preset = Vec::new();
        match &plan {
            StartupPlan::Session => {
                if let Some(text) = saved {
                    session = session_windows(&text);
                    self.saved_session = text;
                }
            }
            StartupPlan::Ask => {
                // The chooser restores it from a copy: session.json is
                // rewritten with what opens now.
                if let Ok(path) = crate::storage::config_file(crate::presets::PREVIOUS_SESSION_FILE) {
                    match saved.filter(|t| !session_windows(t).is_empty()) {
                        Some(text) => drop(crate::storage::write_private(&path, text.as_bytes())),
                        None => drop(std::fs::remove_file(&path)),
                    }
                }
                start = vec![StartTab { url: format!("{}?startup=1", crate::presets_page::URL), container: super::ContainerChoice::None }];
            }
            StartupPlan::Preset(id) => preset = store.get(*id).map(|p| p.windows.clone()).unwrap_or_default(),
            StartupPlan::CommandLine | StartupPlan::NewTab => {}
        }
        let setup = match (session.first(), preset.first()) {
            (Some(w), _) => WindowSetup::from_session(w),
            (None, Some(w)) => WindowSetup { name: w.name.clone(), ..WindowSetup::default() },
            _ => WindowSetup::default(),
        };
        let Some(i) = self.spawn(event_loop, setup, start) else {
            event_loop.exit();
            return;
        };
        let rest = if session.is_empty() { Vec::new() } else { session.split_off(1) };
        let mut notice = notice;
        match plan {
            StartupPlan::NewTab => self.browsers[i].new_window_tab(),
            StartupPlan::Preset(id) if !preset.is_empty() => {
                if !self.browsers[i].restore_window(&preset[0].to_session()) {
                    self.browsers[i].new_window_tab();
                }
                self.open_new_windows(event_loop, &preset[1..]);
                let missing = store.get(id).map_or(0, |p| p.missing_containers(&|c| crate::containers::get(c).is_some()));
                if missing > 0 {
                    notice = Some(preset_notice(&store.get(id).map(|p| p.name.clone()).unwrap_or_default(), missing));
                }
            }
            _ => self.browsers[i].open_start_tabs(session.first()),
        }
        if let Some(n) = notice {
            self.browsers[i].set_notice_long(n);
        }
        // The first window is the active one.
        self.last_active = self.browsers[i].win_key;
        self.browsers[i].start_automation();
        if !rest.is_empty() || !session.is_empty() {
            self.restore_rest(event_loop, rest);
        }
        #[cfg(debug_assertions)]
        {
            let now = Instant::now();
            self.script = crate::titlebar::test_script().into_iter().map(|(after, op)| (now + after, op)).collect();
        }
        self.process(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(i) = self.index_of_window(id) else {
            // The hover card's own window: draw it again when exposed.
            #[cfg(target_os = "linux")]
            if matches!(event, WindowEvent::RedrawRequested) {
                for b in &mut self.browsers {
                    if let Some(card) = b.card.as_mut().filter(|c| c.window.id() == id) {
                        card.redraw();
                    }
                }
            }
            return;
        };
        match &event {
            WindowEvent::Focused(true) | WindowEvent::MouseInput { .. } | WindowEvent::KeyboardInput { .. } => {
                if self.last_active != self.browsers[i].win_key {
                    self.last_active = self.browsers[i].win_key;
                    self.refresh_window_list();
                }
            }
            WindowEvent::RedrawRequested => self.refresh_summaries(),
            _ => {}
        }
        self.browsers[i].on_window_event(event_loop, event);
        self.process(event_loop);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.save_session(true);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, message: UserMessage) {
        if let UserMessage::Automation(req) = message {
            self.automation(event_loop, req);
            self.process(event_loop);
            return;
        }
        // An import's progress shows on every window's import page; when it
        // ends, the bookmarks bar and the bookmarks and history pages too.
        if let UserMessage::Import(event) = message {
            let done = matches!(event, crate::import::Event::Done(_));
            for b in &mut self.browsers {
                b.refresh_if_internal("browsecraft://import");
                if done {
                    b.data_changed();
                }
            }
            if let crate::import::Event::Done(line) = event {
                let i = self.active_index();
                self.browsers[i].set_notice_long(line);
            }
            self.process(event_loop);
            return;
        }
        if let UserMessage::PageClicked(tab) = &message
            && let Some(i) = self.owner_of_tab(*tab)
        {
            self.last_active = self.browsers[i].win_key;
            self.refresh_window_list();
        }
        // A page picture: kept for every window, and saved for the session.
        if let UserMessage::Tab(tab, TabMessage::Thumbnail { w, h, rgba, save }) = message {
            let owner = self.owner_of_tab(tab);
            let url = owner.map(|i| {
                let b = &self.browsers[i];
                b.tab_index(tab).map(|j| (b.tab_url(j), b.tab_container(j))).unwrap_or_default()
            });
            let Some((url, container)) = url else { return };
            let mut sh = self.shared.borrow_mut();
            if sh.thumbs.insert(tab, w, h, rgba).is_some()
                && save
                && flags::get().restore_tabs
                && !url.is_empty()
                && container.as_deref().is_none_or(|c| !c.starts_with("tmp-"))
                && let Some(thumb) = sh.thumbs.peek(tab)
            {
                crate::thumbnails::save(&url, thumb);
            }
            drop(sh);
            for b in &self.browsers {
                if b.overview.open || b.hover.shown().is_some() {
                    b.request_redraw();
                }
            }
            return;
        }
        if let Some(i) = self.route(&message) {
            self.browsers[i].handle_message(message);
        }
        self.process(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.quit {
            event_loop.exit();
            return;
        }
        let now = Instant::now();
        let mut wake: Option<Instant> = None;
        let mut sooner = |t: Instant| wake = Some(wake.map_or(t, |w| w.min(t)));
        // WebKitGTK talks to its web processes through the GLib main
        // context, which winit does not poll: service it here.
        #[cfg(target_os = "linux")]
        if self.shared.borrow().wry_ok == Some(true) || self.browsers.iter().any(|b| b.webview.is_some()) {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            sooner(now + Duration::from_millis(8));
        }
        for b in &mut self.browsers {
            if let Some(t) = b.tick(now) {
                sooner(t);
            }
        }
        self.dying.retain(|(at, _)| *at > now);
        if let Some(t) = self.dying.iter().map(|(at, _)| *at).min() {
            sooner(t);
        }
        #[cfg(debug_assertions)]
        if let Some(t) = self.run_script(event_loop, now) {
            sooner(t);
        }
        // `browsecraft import` may have changed the bookmarks.
        if now.duration_since(self.bookmarks_checked) >= BOOKMARKS_POLL {
            self.bookmarks_checked = now;
            if crate::bookmarks::reload_if_changed() {
                for b in &mut self.browsers {
                    b.data_changed();
                }
            }
        }
        sooner(self.bookmarks_checked + BOOKMARKS_POLL);
        self.process(event_loop);
        self.save_session(false);
        sooner(self.last_save + SESSION_INTERVAL);
        event_loop.set_control_flow(match wake {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }
}

// ---- moving tabs: the window side ----

impl Browser {
    /// Takes tab `id` out of this window, to move it to another: it leaves
    /// its group and the split view; when it was the active tab, the next
    /// one (or its split partner) becomes active. Its web view is hidden
    /// and stays alive.
    pub(super) fn take_tab(&mut self, id: u64) -> Option<MovedTab> {
        let i = self.tab_index(id)?;
        self.overview.hide();
        if i == self.active {
            let partner = self.split.and_then(|s| s.other(id)).and_then(|p| self.tab_index(p));
            match successor(i, self.tabs.len(), partner) {
                Some(next) => self.switch_tab(next),
                None => self.park_active_tab(),
            }
        }
        let i = self.tab_index(id)?;
        self.split = leave_layout(&mut self.groups, self.split, id);
        if self.edit_group.is_some_and(|g| self.groups.get(g).is_none()) {
            self.edit_group = None;
        }
        let state = self.tabs.remove(i);
        if i < self.active {
            self.active -= 1;
        }
        if let Some(view) = &state.view {
            view.set_visible(false);
        }
        self.favicons.remove(&id);
        self.thumb_tex.remove(&id);
        let console = self.feat.console.remove(&id);
        self.request_redraw();
        Some(MovedTab { state, console })
    }

    /// Puts a tab from another window here, at `index` (the end when
    /// `None`), and shows it. Its web view moves into this window; where
    /// it cannot, the tab reloads its address.
    pub(super) fn adopt_tab(&mut self, moved: MovedTab, index: Option<usize>) {
        let MovedTab { mut state, console } = moved;
        let kept = match (&state.view, &self.gfx) {
            #[cfg(target_os = "linux")]
            (Some(WebPane::Wry(wv)), Some(g)) => super::x11::reparent_webview(wv, &g.window),
            (None, _) => true,
            _ => false,
        };
        if !kept {
            // The page is reloaded from its address in this window.
            state.view = None;
            state.loading = false;
            state.web_expect = false;
            state.web_committed = false;
        }
        let id = state.id;
        if let Some(log) = console {
            self.feat.console.insert(id, log);
        }
        self.park_active_tab();
        let at = insert_index(index, self.tabs.len());
        self.tabs.insert(at, state);
        if at <= self.active && self.tabs.len() > 1 {
            self.active += 1;
        }
        self.activate_pantry(at);
        // Its favicon, drawn by this window's egui from now on.
        let icon = self.shared.borrow().favicons.get(&id).cloned();
        if let Some((w, h, rgba)) = icon {
            self.set_favicon(id, w, h, rgba.to_vec());
        }
        self.request_redraw();
    }

    /// A closed window before it is dropped: its pages and its card go,
    /// and the window is hidden (so it loses the focus while it exists).
    pub(super) fn retire(&mut self) {
        #[cfg(target_os = "linux")]
        {
            self.card = None;
            self.release_clipboard_watcher();
        }
        self.webview = None;
        for t in &mut self.tabs {
            t.view = None;
        }
        self.app_requests.clear();
        if let Some(g) = &self.gfx {
            g.window.set_visible(false);
        }
    }

    /// The containers this window's tabs use.
    pub(super) fn used_containers(&self) -> Vec<String> {
        (0..self.tabs.len()).filter_map(|i| self.tab_container(i)).collect()
    }

    /// Before the window closes: its tabs go to the closed-tab stack, so
    /// Ctrl+Shift+T in another window reopens them.
    pub(super) fn remember_all_tabs(&mut self) {
        self.park_active_tab();
        let tabs = std::mem::take(&mut self.tabs);
        for state in tabs {
            self.remember_closed(state);
        }
    }

    /// A new window's first tab: the start page with the address bar
    /// focused.
    pub(super) fn new_window_tab(&mut self) {
        self.load(Request::Get(super::NEW_TAB_URL.to_string()), true);
        self.address.set("");
        self.focus_address();
        self.request_redraw();
    }

    /// Raises this window and gives it the keyboard.
    pub(super) fn focus_window(&self) {
        if let Some(g) = &self.gfx {
            g.window.focus_window();
            g.window.request_redraw();
        }
    }

    /// Whether the screen point `at` (physical pixels) is inside this window.
    pub(super) fn contains_screen_point(&self, at: (f64, f64)) -> bool {
        let Some(g) = &self.gfx else { return false };
        let (Ok(pos), size) = (g.window.inner_position(), g.window.inner_size()) else { return false };
        let (x, y) = (at.0 - f64::from(pos.x), at.1 - f64::from(pos.y));
        x >= 0.0 && y >= 0.0 && x < f64::from(size.width) && y < f64::from(size.height)
    }

    /// Where a tab dropped at screen point `at` goes in this window's strip.
    pub(super) fn strip_drop_index(&self, at: (f64, f64)) -> Option<usize> {
        let g = self.gfx.as_ref()?;
        let pos = g.window.inner_position().ok()?;
        let ppp = self.egui.as_ref().map_or(1.0, |e| e.ctx().pixels_per_point());
        let x = ((at.0 - f64::from(pos.x)) as f32) / ppp;
        Some(drop_index(&self.strip_chips, x, self.tabs.len()))
    }

    /// This window as the overview shows it.
    pub(super) fn summary(&self, position: usize) -> super::overview::WindowSummary {
        let tabs = (0..self.tabs.len())
            .map(|i| {
                let id = self.tab_id_at(i);
                super::overview::TabSummary {
                    id,
                    title: self.tab_title(i),
                    url: self.tab_url(i),
                    container: self.tab_container(i),
                    group: self.groups.group_of(id).map(|g| g.id),
                    split_with: self.split.and_then(|s| s.other(id)),
                    active: i == self.active,
                    unloaded: self.tab_unloaded(i),
                }
            })
            .collect();
        super::overview::WindowSummary {
            key: self.win_key,
            label: window_label(self.win_name.as_deref(), position),
            tabs,
            groups: self.groups.all().to_vec(),
        }
    }

    /// This window for `tabs.list`.
    pub(super) fn window_info(&self, position: usize) -> crate::automation::protocol::WindowInfo {
        crate::automation::protocol::WindowInfo {
            id: self.win_key,
            name: self.win_name.clone(),
            label: window_label(self.win_name.as_deref(), position),
            tabs: self.tabs.len(),
            active: self.is_last_active,
        }
    }
}

/// How far below the strip (points) a dragged tab still reorders; further
/// down (or outside the window) it leaves for a window.
const DETACH_MARGIN: f32 = 80.0;

/// The window's name drawn quietly in the title bar, ending at `right_x`.
/// Returns its width.
pub(super) fn window_name_label(ui: &mut egui::Ui, c: &crate::egui_chrome::Colors, name: &str, right_x: f32, center_y: f32) -> f32 {
    let text = super::display_text(name, 24);
    let galley = ui.painter().layout_no_wrap(text, egui::FontId::proportional(12.0), c.muted);
    let size = galley.size();
    let pos = egui::pos2(right_x - size.x, center_y - size.y / 2.0);
    let pill = egui::Rect::from_min_size(pos, size).expand2(egui::vec2(8.0, 3.0));
    ui.painter().rect_filled(pill, egui::CornerRadius::same(8), c.hover);
    ui.painter().galley(pos, galley, c.muted);
    size.x + 16.0
}

// ---- the window side: dragging chips, the window's name ----

impl Browser {
    /// The pointer in points, and the window's size in points.
    fn pointer_points(&self) -> ((f32, f32), (f32, f32)) {
        let ppp = self.egui.as_ref().map_or(1.0, |e| e.ctx().pixels_per_point());
        let (w, h) = self.window_size();
        ((self.mouse.0 / ppp, self.mouse.1 / ppp), (w / ppp, h / ppp))
    }

    /// A left press: on a chip of the strip, a drag may start. Found from
    /// last frame's chips rather than egui's drag, which a slow frame can
    /// miss (a press and the first move in one frame hit-test at the move).
    pub(super) fn strip_press_at_pointer(&mut self) {
        let ((x, y), _) = self.pointer_points();
        let top = self.strip_bottom - super::TAB_STRIP_HEIGHT;
        self.strip_press = (y >= top && y <= self.strip_bottom)
            .then(|| self.strip_chips.iter().find(|(_, l, r)| x >= *l && x <= *r).map(|(i, _, _)| *i))
            .flatten()
            .map(|i| (self.tab_id_at(i), (x, y)));
    }

    /// The pointer moved: a press on a chip becomes a drag past a few points.
    pub(super) fn strip_drag_moved(&mut self) {
        let Some((tab, (px, py))) = self.strip_press else { return };
        let ((x, y), _) = self.pointer_points();
        if self.strip_drag.is_none() && ((x - px).powi(2) + (y - py).powi(2)).sqrt() > 6.0 {
            self.strip_drag = Some(tab);
            self.hover.cancel();
            self.hide_hover_card();
        }
    }

    /// Where the dragged chip would land in the strip, while it is over it.
    pub(super) fn strip_drag_marker(&self) -> Option<usize> {
        self.strip_drag?;
        let ((x, y), (w, _)) = self.pointer_points();
        let over = x >= 0.0 && x < w && y >= 0.0 && y <= self.strip_bottom + DETACH_MARGIN;
        over.then(|| drop_index(&self.strip_chips, x, self.tabs.len()))
    }

    /// The pointer was released while a chip was dragged: reorder within
    /// the strip, tear the tab off into a new window further down, or hand
    /// it to the application when released outside the window.
    pub(super) fn end_strip_drag(&mut self) {
        self.strip_press = None;
        let Some(tab) = self.strip_drag.take() else { return };
        let ((x, y), (w, h)) = self.pointer_points();
        let inside = x >= 0.0 && y >= 0.0 && x < w && y < h;
        let screen = self.gfx.as_ref().and_then(|g| g.window.inner_position().ok()).map(|p| {
            (f64::from(p.x) + f64::from(self.mouse.0), f64::from(p.y) + f64::from(self.mouse.1))
        });
        #[cfg(debug_assertions)]
        eprintln!("browsecraft: strip drag of tab {tab} ended at ({x:.0}, {y:.0}) in a {w:.0}x{h:.0} window, screen {screen:?}");
        if inside && y <= self.strip_bottom + DETACH_MARGIN {
            let to = drop_index(&self.strip_chips, x, self.tabs.len());
            let order = reordered(&self.tab_order(), tab, to);
            // Groups stay together.
            let order = self.groups.normalize(&order);
            self.apply_order(&order);
            #[cfg(debug_assertions)]
            eprintln!("browsecraft: strip order now {:?}", self.tab_order());
        } else if inside {
            let at = screen.map(|(sx, sy)| (sx.round() as i32 - 120, sy.round() as i32 - 16));
            self.app_requests.push(AppRequest::MoveTab { tab, to: MoveTarget::NewWindow(at) });
        } else if let Some(at) = screen {
            self.app_requests.push(AppRequest::DropTab { tab, at });
        }
        self.request_redraw();
    }

    /// Names this window ("" removes the name).
    pub(super) fn set_window_name(&mut self, name: &str) {
        let name = clean_window_name(name);
        self.win_name = (!name.is_empty()).then_some(name);
        self.rename = None;
        self.update_window_title();
        self.request_redraw();
    }

    /// The "Name window" dialog while it is open; returns the name when
    /// saved.
    pub(super) fn rename_dialog(&mut self, ctx: &egui::Context, c: &crate::egui_chrome::Colors) -> Option<String> {
        let text = self.rename.as_mut()?;
        let mut saved = None;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("bc-name-window")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label(egui::RichText::new("Name window").size(15.0).strong().color(c.text));
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Shown in the title bar, the tab menus and the overview.").size(12.0).color(c.muted));
            ui.add_space(8.0);
            let edit = ui.add(egui::TextEdit::singleline(text).hint_text("Window 1").desired_width(f32::INFINITY).char_limit(MAX_WINDOW_NAME));
            if !edit.has_focus() && !edit.lost_focus() {
                edit.request_focus();
            }
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                saved = Some(text.clone());
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    saved = Some(text.clone());
                }
                if ui.button("Remove name").clicked() {
                    saved = Some(String::new());
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if modal.should_close() || cancel {
            self.rename = None;
            ctx.request_repaint();
        }
        saved
    }
}

impl Drop for Browser {
    /// The web views and the hover card are child windows of the browser
    /// window: they go first, so they never outlive their parent.
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            self.card = None;
        }
        self.webview = None;
        for t in &mut self.tabs {
            t.view = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_names_are_one_short_line() {
        assert_eq!(clean_window_name("  Work\n stuff  "), "Work stuff");
        assert_eq!(clean_window_name(&"x".repeat(200)).chars().count(), MAX_WINDOW_NAME);
        assert_eq!(window_label(None, 0), "Window 1");
        assert_eq!(window_label(Some(""), 2), "Window 3");
        assert_eq!(window_label(Some("Research"), 1), "Research");
    }

    #[test]
    fn drops_land_between_chips() {
        // Three chips 100 px wide from x = 0.
        let chips = [(0, 0.0, 100.0), (1, 100.0, 200.0), (2, 200.0, 300.0)];
        assert_eq!(drop_index(&chips, 10.0, 3), 0);
        assert_eq!(drop_index(&chips, 60.0, 3), 1);
        assert_eq!(drop_index(&chips, 160.0, 3), 2);
        assert_eq!(drop_index(&chips, 290.0, 3), 3);
        assert_eq!(drop_index(&[], 5.0, 0), 0);
    }

    #[test]
    fn reordering_moves_one_tab() {
        let order = [1, 2, 3, 4];
        assert_eq!(reordered(&order, 1, 3), vec![2, 3, 1, 4]);
        assert_eq!(reordered(&order, 1, 4), vec![2, 3, 4, 1]);
        assert_eq!(reordered(&order, 4, 0), vec![4, 1, 2, 3]);
        assert_eq!(reordered(&order, 2, 2), vec![1, 2, 3, 4], "dropped on itself");
        assert_eq!(reordered(&order, 9, 0), vec![1, 2, 3, 4], "unknown tab");
    }

    #[test]
    fn the_partner_or_a_neighbour_takes_over() {
        assert_eq!(successor(0, 1, None), None, "the window empties");
        assert_eq!(successor(1, 3, None), Some(2), "the next tab");
        assert_eq!(successor(2, 3, None), Some(1), "the last tab: the previous");
        assert_eq!(successor(0, 4, Some(3)), Some(3), "the split partner first");
        assert_eq!(successor(0, 4, Some(9)), Some(1), "a stale partner is ignored");
        assert_eq!(insert_index(None, 3), 3);
        assert_eq!(insert_index(Some(1), 3), 1);
        assert_eq!(insert_index(Some(7), 3), 3);
    }

    #[test]
    fn a_moved_tab_leaves_its_group_and_the_split() {
        let mut groups = crate::tab_groups::TabGroups::default();
        let g = groups.create("Work", None);
        let order = groups.add_tab(1, g, &[1, 2, 3]);
        let order = groups.add_tab(2, g, &order);
        let split = crate::split::Split::new(2, 3);
        // Tab 3 leaves: the split ends, the group stays.
        let rest = leave_layout(&mut groups, split, 3);
        assert!(rest.is_none());
        assert_eq!(groups.members(g, &order), vec![1, 2]);
        // A tab outside the split keeps it.
        let split = crate::split::Split::new(2, 3);
        assert_eq!(leave_layout(&mut groups, split, 1), split);
        assert_eq!(groups.members(g, &order), vec![2]);
        // The group's last tab leaves: the group goes.
        leave_layout(&mut groups, None, 2);
        assert!(groups.get(g).is_none());
        // The tab arrives in the other window with no group.
        assert!(groups.group_of(2).is_none());
    }

    #[test]
    fn sessions_keep_every_window_and_read_old_files() {
        let w1 = json!({ "active": 0, "tabs": [{ "url": "https://a.test/", "title": "A" }], "name": "Work", "bounds": { "x": 10, "y": 20, "w": 900, "h": 700 } });
        let w2 = json!({ "active": 1, "tabs": [{ "url": "https://b.test/" }, { "url": "https://c.test/" }] });
        let text = session_text(&[w1.clone(), w2.clone()]);
        let back = session_windows(&text);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0]["name"], "Work");
        assert_eq!(back[1]["tabs"].as_array().map(Vec::len), Some(2));
        // The first window is also at the root, for older versions.
        let root: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(root["tabs"][0]["url"], "https://a.test/");
        // A file from before windows: one window.
        let old = json!({ "active": 0, "tabs": [{ "url": "https://old.test/" }] }).to_string();
        assert_eq!(session_windows(&old).len(), 1);
        assert!(session_windows("not json").is_empty());
        assert!(session_windows("[1, 2]").is_empty());
        let setup = WindowSetup::from_session(&back[0]);
        assert_eq!(setup.position, Some((10, 20)));
        assert_eq!(setup.size, Some((900, 700)));
        assert_eq!(setup.name.as_deref(), Some("Work"));
        let hostile = json!({ "bounds": { "x": 1e12, "y": 0, "w": 5, "h": 1e9 }, "name": "\u{0007}" });
        let setup = WindowSetup::from_session(&hostile);
        assert_eq!((setup.position, setup.size, setup.name), (None, None, None));
    }
}
