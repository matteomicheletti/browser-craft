//! The fork's feature modules wired into the browser window: the command
//! palette and its commands, the drawer panels (authenticator, dark mode,
//! plugins, automation and JS console), their toolbar buttons, the page
//! scripts every web view gets and the messages those scripts send back.
//!
//! The modules themselves (`palette`, `authenticator`, `darkmode`,
//! `plugins`, `automation`, `recording`) know nothing about the browser;
//! this file is the glue (docs/CONTRACTS.md).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::{
    Action, Browser, ContainerChoice, Drawer, NEW_TAB_URL, Request, UserMessage, WebPane, create_screenshot_file,
    pictures_dir,
};
use crate::authenticator::{AuthEvent, AuthenticatorPanel};
use crate::automation::console::{ConsoleLog, Level, PageConsoleMessage};
use crate::automation::panel::{AutomationEvent, AutomationPanel, ServerState};
use crate::commands::CommandId;
use crate::darkmode::{self, DarkEvent, DarkModePanel, Decision, Mode, SiteMode};
use crate::egui_chrome::{self, Colors};
use crate::flags::{self, TabsPosition};
use crate::palette::{self, Palette, PaletteOutcome, PaletteSources};
use crate::plugins::{self, PageCall, PluginEvent, PluginsEvent, PluginsPanel};
use crate::recording::{self, Recorder};
use crate::{bookmarks, containers, history, passwords};

/// How often plugin development folders are checked for edits.
const PLUGIN_POLL: Duration = Duration::from_secs(1);
/// "Clear browsing data" runs only when chosen twice within this time.
const CLEAR_CONFIRM: Duration = Duration::from_secs(10);
/// Recent history entries offered by the palette.
const PALETTE_HISTORY: usize = 50;

#[cfg(unix)]
type AutomationHandle = crate::automation::Handle;
#[cfg(not(unix))]
type AutomationHandle = ();

/// A `page.*` automation request answered by scripts in a tab.
pub(super) struct PageJob {
    pub job: crate::automation::scripts::PageJob,
    pub req: crate::automation::Request,
    pub tab: u64,
}

/// A navigation an automation client waits for (`wait: true`).
pub(super) struct NavWait {
    pub tab: u64,
    pub req: crate::automation::Request,
    pub deadline: Instant,
    /// The new document has started loading in the tab.
    pub started: bool,
}

/// What the drawers and the toolbar buttons ask for, applied after the
/// egui frame is built.
pub(super) enum FeatureAction {
    Command(CommandId),
    Palette(PaletteOutcome),
    Auth(AuthEvent),
    Dark(DarkEvent),
    Plugins(PluginsEvent),
    Automation(AutomationEvent),
    /// Rebuild the web views so they run the current plugin scripts.
    ReloadForPlugins,
}

/// State of the feature modules, owned by the browser.
pub(super) struct Features {
    pub palette: Palette,
    pub auth: AuthenticatorPanel,
    pub auth_unlocked: bool,
    pub dark: DarkModePanel,
    pub plugins: PluginsPanel,
    /// The registry generation the web views' init script was built from.
    pub plugins_generation: u64,
    /// Open tabs run an older plugin script than the registry's.
    pub plugins_stale: bool,
    pub last_plugin_poll: Instant,
    pub automation_panel: AutomationPanel,
    pub automation: Option<AutomationHandle>,
    /// The settings in effect (enabled includes `--automation`).
    pub automation_settings: crate::automation::Settings,
    /// `enabled` as saved in `automation.toml`.
    pub automation_file_enabled: bool,
    pub automation_error: Option<String>,
    /// Each tab's console, by tab id.
    pub console: HashMap<u64, ConsoleLog>,
    pub recorder: Recorder,
    pub recording_stopping: bool,
    pub jobs: HashMap<u64, PageJob>,
    /// Scripts to run after the poll interval: (when, job, script).
    pub delayed: Vec<(Instant, u64, String)>,
    pub next_job: u64,
    pub nav_waits: Vec<NavWait>,
    pub clear_armed: Option<Instant>,
}

impl Features {
    pub fn new() -> Self {
        let generation = plugins::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner).generation();
        let file = crate::automation::Settings::load();
        let mut effective = file.clone();
        effective.enabled |= crate::automation::requested();
        Features {
            palette: Palette::new(),
            auth: AuthenticatorPanel::new(),
            auth_unlocked: false,
            dark: DarkModePanel::new(),
            plugins: PluginsPanel::new(),
            plugins_generation: generation,
            plugins_stale: false,
            last_plugin_poll: Instant::now(),
            automation_panel: AutomationPanel::new(),
            automation: None,
            automation_file_enabled: file.enabled,
            automation_settings: effective,
            automation_error: None,
            console: HashMap::new(),
            recorder: Recorder::new(),
            recording_stopping: false,
            jobs: HashMap::new(),
            delayed: Vec::new(),
            next_job: 1,
            nav_waits: Vec::new(),
            clear_armed: None,
        }
    }
}

/// Minutes after local midnight (for the dark mode schedule).
fn local_minutes() -> u16 {
    #[cfg(unix)]
    {
        // SAFETY: `time` and `localtime_r` only write to the locals passed.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let now = unsafe { libc::time(std::ptr::null_mut()) };
        if !unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
            return (tm.tm_hour * 60 + tm.tm_min).clamp(0, 1439) as u16;
        }
    }
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    ((secs / 60) % 1440) as u16
}

/// The host of an http(s) URL.
fn web_host(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .and_then(|u| u.host_str().map(str::to_string))
}

/// A toolbar button whose icon is tinted while its feature is active.
fn toggle_button(ui: &mut egui::Ui, code: &str, tip: &str, active: bool, c: &Colors) -> egui::Response {
    let mut text = egui_chrome::icon(code, 16.0);
    if active {
        text = text.color(c.accent);
    }
    let button = egui::Button::new(text).min_size(egui::vec2(32.0, 32.0)).corner_radius(egui::CornerRadius::same(10));
    ui.add(button).on_hover_text(tip)
}

impl Browser {
    /// A notice that stays five seconds longer, for one that names a file.
    pub(super) fn set_notice_long(&mut self, message: impl Into<String>) {
        self.set_notice(message);
        if let Some((_, at)) = &mut self.notice {
            *at += Duration::from_secs(5);
        }
    }

    // ---- page scripts and messages ----

    /// The feature scripts for a new web view, with whether each runs in
    /// the main frame only: dark mode and plugins in every frame (as their
    /// docs require), the console capture in the main frame.
    pub(super) fn feature_page_scripts(&self) -> Vec<(String, bool)> {
        let mut scripts = Vec::new();
        if plugins::is_enabled("builtin.darkmode") {
            scripts.push((darkmode::page_script(self.feat.dark.settings(), Some(self.dark())), false));
        }
        {
            let reg = plugins::shared();
            let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            scripts.push((plugins::page_script(&reg), false));
        }
        scripts.push((crate::automation::console::console_capture_script(), true));
        scripts
    }

    /// The live web view of tab `id` (active or in the background).
    pub(super) fn view_of(&self, id: u64) -> Option<&WebPane> {
        if id == self.tab_id {
            self.webview.as_ref()
        } else {
            self.tabs.iter().find(|t| t.id == id).and_then(|t| t.view.as_ref())
        }
    }

    pub(super) fn eval_in_tab(&self, id: u64, script: &str) {
        if let Some(view) = self.view_of(id) {
            let _ = view.evaluate_script(script);
        }
    }

    /// The index of the tab with this id.
    pub(super) fn tab_index(&self, id: u64) -> Option<usize> {
        (0..self.tabs.len()).find(|&i| self.tab_id_at(i) == id)
    }

    pub(super) fn console_message(&mut self, tab: u64, url: &str, msg: PageConsoleMessage) {
        self.feat.console.entry(tab).or_default().push(msg, url);
        if self.drawer == Some(Drawer::Automation) && tab == self.tab_id {
            self.request_redraw();
        }
    }

    pub(super) fn dark_message(&mut self, message: darkmode::PageMessage) {
        let darkmode::PageMessage::NativeDark { host, dark } = message;
        self.feat.dark.note_native_dark(&host, dark);
    }

    /// A `bc.*` call from a plugin's content script in tab `tab`.
    pub(super) fn plugin_call(&mut self, tab: u64, sender: String, call: PageCall) {
        let outcome = {
            let reg = plugins::shared();
            let mut reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            plugins::dispatch(&mut reg, call, Some(&sender))
        };
        if let Some(script) = outcome.reply {
            self.eval_in_tab(tab, &script);
        }
        let Some(event) = outcome.event else {
            return;
        };
        match event {
            PluginEvent::Notify { plugin, title, body } => {
                let name = self.plugin_name(&plugin);
                self.set_notice(format!("{name}: {title}. {body}"));
            }
            PluginEvent::ClipboardWrite { plugin, text } => {
                let name = self.plugin_name(&plugin);
                match self.clipboard.as_mut().map(|c| c.set_text(text)) {
                    Some(Ok(())) => self.set_notice(format!("{name} copied text to the clipboard")),
                    _ => self.set_notice("Could not use the clipboard"),
                }
            }
            PluginEvent::OpenTab { url, .. } => self.open_in_new_tab(url),
            PluginEvent::QueryTabs { reply, .. } => {
                let tabs: Vec<Value> = (0..self.tabs.len())
                    .map(|i| {
                        json!({
                            "id": self.tab_id_at(i),
                            "title": self.tab_title(i),
                            "url": self.tab_url(i),
                            "active": i == self.active,
                        })
                    })
                    .collect();
                let script = reply.ok(&Value::Array(tabs));
                self.eval_in_tab(tab, &script);
            }
            PluginEvent::Log { plugin, text } => {
                let msg = PageConsoleMessage {
                    level: Level::Info,
                    text: format!("[{plugin}] {text}"),
                    kind: "plugin".into(),
                    source: None,
                    line: None,
                    column: None,
                    dropped: 0,
                };
                self.console_message(tab, &sender, msg);
            }
        }
    }

    fn plugin_name(&self, id: &str) -> String {
        let reg = plugins::shared();
        let reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.get(id).and_then(|e| e.manifest().map(|m| m.name.clone())).unwrap_or_else(|| id.to_string())
    }

    /// The registry changed (panel, dev folder edit): new web views get the
    /// new script; the open ones keep theirs until rebuilt.
    fn plugins_changed(&mut self) {
        let generation = plugins::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner).generation();
        if generation == self.feat.plugins_generation {
            return;
        }
        self.feat.plugins_generation = generation;
        // Built-ins turned off take effect at once.
        if !plugins::is_enabled("builtin.darkmode") {
            let mut off = self.feat.dark.settings().clone();
            off.enabled = false;
            self.evaluate_in_all_tabs(&darkmode::update_script(&off, Some(self.dark())));
        }
        if !plugins::is_enabled("builtin.authenticator") && self.feat.auth.is_unlocked() {
            self.feat.auth.lock();
            self.feat.auth_unlocked = false;
        }
        if self.all_views().next().is_some() {
            self.feat.plugins_stale = true;
            self.set_notice("Plugins changed: new tabs use them. Reload open tabs from the Plugins panel to apply");
        }
    }

    /// Drops every web view so each is built again with the current init
    /// scripts: the active tab reloads now, the others when shown.
    fn rebuild_web_views(&mut self) {
        for t in &mut self.tabs {
            if t.view.take().is_some() {
                t.loading = false;
            }
        }
        if self.webview.as_ref().is_some_and(WebPane::is_native) {
            self.webview = None;
            self.web_visible = false;
            self.web_bounds = None;
            if self.web_page {
                let url = self.web_url.clone();
                self.reload_web_tab(&url);
            }
        }
        self.feat.plugins_stale = false;
        self.set_notice("Tabs reloaded with the current plugins");
    }

    /// Dark mode settings changed: apply them to every open page.
    fn dark_settings_changed(&mut self) {
        let script = darkmode::update_script(self.feat.dark.settings(), Some(self.dark()));
        self.evaluate_in_all_tabs(&script);
        self.request_redraw();
    }

    /// The browser theme changed: pages following the system preference
    /// follow it too.
    pub(super) fn dark_theme_changed(&self) {
        if plugins::is_enabled("builtin.darkmode") {
            let script = darkmode::update_script(self.feat.dark.settings(), Some(self.dark()));
            self.evaluate_in_all_tabs(&script);
        }
    }

    /// Whether the dark theme applies to `host` now (ignoring the page's
    /// own "already dark" check, which only the page can make).
    fn dark_applies(&self, host: &str) -> bool {
        matches!(self.feat.dark.settings().decide(host, self.dark(), local_minutes()), Decision::Dark)
    }

    fn page_host(&self) -> Option<String> {
        if self.web_page { web_host(&self.web_url) } else { None }
    }

    // ---- every frame ----

    /// Timers of the feature modules; returns when to be woken next.
    pub(super) fn tick_features(&mut self) -> Option<Instant> {
        let now = Instant::now();
        let mut wake: Option<Instant> = None;
        let mut sooner = |t: Instant| wake = Some(wake.map_or(t, |w: Instant| w.min(t)));
        // The authenticator auto-locks even while its drawer is closed.
        for ev in self.feat.auth.tick() {
            self.auth_event(ev);
        }
        if self.feat.auth_unlocked {
            sooner(now + Duration::from_secs(1));
        }
        if now.duration_since(self.feat.last_plugin_poll) >= PLUGIN_POLL {
            self.feat.last_plugin_poll = now;
            let changed = plugins::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner).poll_changes();
            if changed {
                self.plugins_changed();
            }
            // Console logs of closed tabs.
            let live: Vec<u64> = (0..self.tabs.len()).map(|i| self.tab_id_at(i)).collect();
            self.feat.console.retain(|id, _| live.contains(id));
            // A recorder whose ffmpeg died.
            let was = self.feat.recorder.path().is_some();
            if was && !self.feat.recorder.is_recording() && !self.feat.recording_stopping {
                self.set_notice("The screen recording stopped unexpectedly");
            }
        }
        sooner(self.feat.last_plugin_poll + PLUGIN_POLL);
        // Automation: scripts waiting for the poll interval, navigations
        // that took too long.
        let due: Vec<(u64, String)> = {
            let (ready, later): (Vec<_>, Vec<_>) = self.feat.delayed.drain(..).partition(|(at, _, _)| *at <= now);
            self.feat.delayed = later;
            ready.into_iter().map(|(_, job, script)| (job, script)).collect()
        };
        for (job, script) in due {
            self.run_job_script(job, &script);
        }
        for (at, _, _) in &self.feat.delayed {
            sooner(*at);
        }
        let mut i = 0;
        while i < self.feat.nav_waits.len() {
            if self.feat.nav_waits[i].deadline <= now {
                let w = self.feat.nav_waits.remove(i);
                w.req.respond(Err(crate::automation::RpcError::timeout("the page did not finish loading in time")));
            } else {
                sooner(self.feat.nav_waits[i].deadline);
                i += 1;
            }
        }
        wake
    }

    // ---- palette ----

    /// Ctrl+P, F1: opens the palette, or closes it.
    pub(super) fn toggle_palette(&mut self) {
        if !self.gpu_chrome {
            self.set_notice("The command palette needs the GPU interface");
            return;
        }
        self.feat.palette.toggle();
        if self.feat.palette.is_open()
            && let Some(wv) = &self.webview
        {
            // The page's window is hidden while the palette shows: give the
            // keyboard back to the browser window.
            wv.focus_parent();
        }
        self.request_redraw();
    }

    pub(super) fn palette_sources(&self) -> PaletteSources {
        let badge = |id: Option<String>| {
            id.and_then(|id| containers::get(&id))
                .map(|c| palette::ContainerBadge { name: c.name.clone(), color: c.color32() })
        };
        let tabs = (0..self.tabs.len())
            .map(|i| palette::TabItem {
                index: i,
                title: self.tab_title(i),
                url: self.tab_url(i),
                container: badge(self.tab_container(i)),
            })
            .collect();
        let bookmarks = bookmarks::all()
            .into_iter()
            .map(|b| palette::LinkItem { title: b.title, url: b.url })
            .collect();
        let history = history::search("", PALETTE_HISTORY)
            .into_iter()
            .map(|(url, title)| palette::LinkItem { title, url })
            .collect();
        let containers = containers::all()
            .into_iter()
            .filter(|c| !c.temporary)
            .map(|c| palette::ContainerItem { color: c.color32(), id: c.id, name: c.name, temporary: false })
            .collect();
        PaletteSources {
            tabs,
            bookmarks,
            history,
            containers,
            on_web_page: self.web_page,
            recording: self.feat.recorder.path().is_some(),
        }
    }

    pub(super) fn palette_outcome(&mut self, outcome: PaletteOutcome) {
        match outcome {
            PaletteOutcome::Run(id) => self.run_command(id),
            PaletteOutcome::SwitchTab(i) => self.switch_tab(i),
            PaletteOutcome::OpenUrl(url) => self.open_in_new_tab(url),
            PaletteOutcome::OpenInContainer { url, container } => {
                let url = (!url.is_empty()).then_some(url);
                self.new_tab_in(ContainerChoice::Id(container), url);
            }
        }
    }

    /// Runs a palette command (also used by the toolbar buttons).
    pub(super) fn run_command(&mut self, id: CommandId) {
        use CommandId::*;
        let needs_page = crate::commands::needs_page(id);
        if needs_page && !self.web_page && !matches!(id, Reload | ZoomIn | ZoomOut | ZoomReset | ViewSource | DuplicateTab | Bookmark | CopyUrl) {
            self.set_notice("This command works on web pages");
            return;
        }
        match id {
            NewTab => self.new_tab(),
            NewTabInContainer => {
                self.feat.palette.open_with("~ ");
            }
            NewTemporaryContainerTab => self.new_tab_in(ContainerChoice::Temporary, None),
            DuplicateTab => {
                let url = self.tab_url(self.active);
                let choice = self.container.clone().map_or(ContainerChoice::None, ContainerChoice::Id);
                let url = (!url.is_empty() && url != NEW_TAB_URL).then_some(url);
                self.new_tab_in(choice, url);
            }
            CloseTab => self.close_tab(self.active),
            CloseOtherTabs => self.close_other_tabs(self.active),
            ReopenClosedTab => self.reopen_closed_tab(),
            NextTab => self.cycle_tab(1),
            PrevTab => self.cycle_tab(-1),
            ToggleTabsPosition => {
                let mut f = flags::get();
                f.tabs_position = if f.tabs_position == TabsPosition::Top { TabsPosition::Left } else { TabsPosition::Top };
                if let Err(e) = flags::set(f) {
                    self.set_notice(e);
                }
                self.web_bounds = None;
            }
            Back => self.go(-1),
            Forward => self.go(1),
            Reload => self.reload(),
            HardReload => self.hard_reload(),
            Home => self.home(),
            FocusAddress => self.focus_address(),
            CopyUrl => {
                let url = self.current_url();
                match self.clipboard.as_mut().map(|c| c.set_text(url)) {
                    Some(Ok(())) => self.set_notice("Address copied"),
                    _ => self.set_notice("Could not use the clipboard"),
                }
            }
            Find => self.open_find(),
            ZoomIn => self.zoom(Some(true)),
            ZoomOut => self.zoom(Some(false)),
            ZoomReset => self.zoom(None),
            Fullscreen => self.toggle_window_fullscreen(),
            Print => self.print_page(),
            SavePage => self.save_page(),
            ViewSource => self.view_source(),
            Screenshot => self.save_screenshot(),
            ScreenshotFullPage => {
                if !self.webkit_screenshot(true) {
                    self.set_notice("Full-page screenshots are not available yet on this platform");
                }
            }
            StartRecording => self.start_recording(),
            StopRecording => self.stop_recording(),
            DevTools => self.open_devtools(),
            JsConsole => {
                self.feat.automation_panel.view = crate::automation::panel::View::Console;
                self.open_drawer(Drawer::Automation);
            }
            AutomationToggle => {
                let mut s = self.feat.automation_settings.clone();
                s.enabled = !s.enabled;
                self.automation_event(AutomationEvent::Settings(s));
                let on = self.feat.automation.is_some();
                if on {
                    self.set_notice("Automation control server started");
                } else if self.feat.automation_settings.enabled {
                    let e = self.feat.automation_error.clone().unwrap_or_default();
                    self.set_notice(format!("Automation could not start: {e}"));
                } else {
                    self.set_notice("Automation control server stopped");
                }
            }
            Bookmark => self.toggle_bookmark(),
            BookmarksBar => self.toggle_bookmarks_bar(),
            BookmarksPage => self.load(Request::Get("browsecraft://bookmarks".into()), true),
            History => self.open_history_page(),
            ClearBrowsingData => self.clear_browsing_data(),
            Containers => self.open_drawer(Drawer::Containers),
            Authenticator => {
                if plugins::is_enabled("builtin.authenticator") {
                    self.open_drawer(Drawer::Authenticator);
                } else {
                    self.set_notice("The authenticator is turned off in Plugins");
                }
            }
            Passwords => self.load(Request::Get("browsecraft://passwords".into()), true),
            DarkModeToggle | DarkModeSiteToggle | DarkModeSettings if !plugins::is_enabled("builtin.darkmode") => {
                self.set_notice("Dark mode is turned off in Plugins");
            }
            DarkModeToggle => {
                let on = !matches!(self.feat.dark.settings().mode, Mode::Light | Mode::Off)
                    && (self.feat.dark.settings().mode == Mode::Dark || self.dark())
                    && self.feat.dark.settings().enabled;
                let r = self.feat.dark.update(|s| {
                    s.enabled = true;
                    s.mode = if on { Mode::Off } else { Mode::Dark };
                });
                self.dark_settings_changed();
                match r {
                    Ok(()) if on => self.set_notice("Dark mode off for pages"),
                    Ok(()) => self.set_notice("Dark mode on for pages"),
                    Err(e) => self.set_notice(e),
                }
            }
            DarkModeSiteToggle => self.toggle_dark_for_site(),
            DarkModeSettings => self.open_drawer(Drawer::DarkMode),
            Plugins => self.open_drawer(Drawer::Plugins),
            ReloadPlugins => {
                plugins::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner).reload();
                self.plugins_changed();
                if self.feat.plugins_stale {
                    self.rebuild_web_views();
                } else {
                    self.set_notice("Plugins reloaded");
                }
            }
            Settings => self.load(Request::Get("browsecraft://flags".into()), true),
            Quit => self.quit = true,
        }
        self.request_redraw();
    }

    fn hard_reload(&mut self) {
        #[cfg(target_os = "linux")]
        if self.web_page
            && let Some(WebPane::Wry(wv)) = &self.webview
        {
            use webkit2gtk::WebViewExt as _;
            use wry::WebViewExtUnix as _;
            wv.webview().reload_bypass_cache();
            self.loading = true;
            return;
        }
        self.reload();
    }

    fn print_page(&mut self) {
        match &self.webview {
            Some(WebPane::Wry(wv)) if self.web_page => {
                if let Err(e) = wv.print() {
                    self.set_notice(format!("Could not print: {e}"));
                }
            }
            _ => self.set_notice("Printing works on web pages"),
        }
    }

    /// Saves the page as a single MHTML file in Downloads (WebKitGTK).
    fn save_page(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(WebPane::Wry(wv)) = &self.webview {
            use webkit2gtk::WebViewExt as _;
            use wry::WebViewExtUnix as _;
            let Some(dir) = downloads_dir() else {
                self.set_notice("No Downloads folder to save to");
                return;
            };
            let stem: String = self
                .live_title()
                .chars()
                .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' })
                .take(80)
                .collect();
            let stem = if stem.trim().is_empty() { "page".to_string() } else { stem.trim().to_string() };
            let mut path = dir.join(format!("{stem}.mhtml"));
            let mut n = 2;
            while path.exists() && n < 1000 {
                path = dir.join(format!("{stem} ({n}).mhtml"));
                n += 1;
            }
            let file = gtk::gio::File::for_path(&path);
            let proxy = self.proxy.clone();
            wv.webview().save_to_file(&file, webkit2gtk::SaveMode::Mhtml, None::<&gtk::gio::Cancellable>, move |r| {
                let notice = match r {
                    Ok(()) => format!("Page saved to {}", path.display()),
                    Err(e) => format!("Could not save the page: {e}"),
                };
                let _ = proxy.send_event(UserMessage::Notice(notice));
            });
            return;
        }
        self.set_notice("Saving pages is not available yet on this platform");
    }

    /// The visible page or the whole document as a PNG in Pictures, rendered
    /// by WebKitGTK (Linux). False when this platform cannot.
    pub(super) fn webkit_screenshot(&mut self, full_page: bool) -> bool {
        #[cfg(target_os = "linux")]
        if self.web_page
            && let Some(WebPane::Wry(wv)) = &self.webview
        {
            use wry::WebViewExtUnix as _;
            let proxy = self.proxy.clone();
            crate::headless_webkit::snapshot_png(&wv.webview(), full_page, move |r| {
                let notice = match r.and_then(|(png, _, _)| {
                    let dir = pictures_dir()?;
                    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
                    let (path, mut file) = create_screenshot_file(&dir, secs).map_err(|e| e.to_string())?;
                    std::io::Write::write_all(&mut file, &png).map_err(|e| e.to_string())?;
                    Ok(path)
                }) {
                    Ok(path) => format!("Screenshot saved to {}", path.display()),
                    Err(e) => format!("Screenshot failed: {e}"),
                };
                let _ = proxy.send_event(UserMessage::Notice(notice));
            });
            if full_page {
                self.set_notice("Capturing the whole page…");
            }
            return true;
        }
        let _ = full_page;
        false
    }

    fn start_recording(&mut self) {
        if self.feat.recorder.is_recording() || self.feat.recording_stopping {
            self.set_notice("A recording is already running");
            return;
        }
        let Some(window) = self.gfx.as_ref().map(|g| g.window.clone()) else {
            return;
        };
        let Some(id) = recording::x11_window_id(&window) else {
            self.set_notice("Screen recording is not available yet on this platform (it needs X11)");
            return;
        };
        match self.feat.recorder.start(&recording::Options::new(recording::Target::Window(id))) {
            Ok(path) => self.set_notice_long(format!("Recording the window to {}", path.display())),
            Err(e) => self.set_notice(format!("Could not start recording: {e}")),
        }
    }

    /// Stops the recording on a worker thread (ffmpeg may take seconds to
    /// finish the file); the notice comes with `RecordingStopped`.
    fn stop_recording(&mut self) {
        if !self.feat.recorder.is_recording() {
            self.set_notice("No recording is running");
            return;
        }
        let mut rec = std::mem::take(&mut self.feat.recorder);
        self.feat.recording_stopping = true;
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new().name("recording-stop".into()).spawn(move || {
            let result = rec.stop();
            let _ = proxy.send_event(UserMessage::RecordingStopped(result));
        });
        if spawned.is_err() {
            self.feat.recording_stopping = false;
            self.set_notice("Could not stop the recording");
        } else {
            self.set_notice("Finishing the recording…");
        }
    }

    pub(super) fn recording_stopped(&mut self, result: Result<PathBuf, String>) {
        self.feat.recording_stopping = false;
        match result {
            Ok(path) => self.set_notice_long(format!("Recording saved to {}", path.display())),
            Err(e) => self.set_notice_long(format!("Recording failed: {e}")),
        }
    }

    /// History and every open container's cookies, storage and cache.
    /// Asks for a second run within ten seconds first.
    fn clear_browsing_data(&mut self) {
        let armed = self.feat.clear_armed.is_some_and(|t| t.elapsed() < CLEAR_CONFIRM);
        if !armed {
            self.feat.clear_armed = Some(Instant::now());
            self.set_notice("Run \"Clear browsing data\" again within 10 s to delete history, cookies, cache and site data");
            return;
        }
        self.feat.clear_armed = None;
        history::clear();
        let mut cleared = 0;
        for view in self.all_views() {
            if let WebPane::Wry(wv) = view
                && wv.clear_all_browsing_data().is_ok()
            {
                cleared += 1;
            }
        }
        self.refresh_if_internal("browsecraft://history");
        if cleared == 0 {
            self.set_notice("History cleared (no open web page to clear site data from)");
        } else {
            self.set_notice("History, cookies, cache and site data cleared");
        }
    }

    fn toggle_dark_for_site(&mut self) {
        let Some(host) = self.page_host() else {
            self.set_notice("Dark mode per site works on web pages");
            return;
        };
        let on = self.feat.dark.settings().enabled && self.dark_applies(&host);
        let r = self.feat.dark.update(|s| {
            s.enabled = true;
            s.set_site_mode(&host, if on { SiteMode::Off } else { SiteMode::On });
        });
        self.dark_settings_changed();
        match r {
            Ok(()) if on => self.set_notice(format!("Dark mode off for {host}")),
            Ok(()) => self.set_notice(format!("Dark mode on for {host}")),
            Err(e) => self.set_notice(e),
        }
    }

    // ---- drawer events ----

    fn auth_event(&mut self, event: AuthEvent) {
        match event {
            AuthEvent::Copy(code) => match passwords::copy_to_clipboard(&code) {
                Ok(()) => self.set_notice("Code copied. It leaves the clipboard in 30 seconds"),
                Err(e) => self.set_notice(e),
            },
            AuthEvent::Unlocked => self.feat.auth_unlocked = true,
            AuthEvent::Locked => self.feat.auth_unlocked = false,
        }
        self.request_redraw();
    }

    fn automation_event(&mut self, event: AutomationEvent) {
        match event {
            AutomationEvent::ClearConsole => {
                if let Some(log) = self.feat.console.get_mut(&self.tab_id) {
                    log.clear();
                }
            }
            AutomationEvent::Copy(text) => match self.clipboard.as_mut().map(|c| c.set_text(text)) {
                Some(Ok(())) => self.set_notice("Copied"),
                _ => self.set_notice("Could not use the clipboard"),
            },
            AutomationEvent::Settings(s) => {
                if s.enabled != self.feat.automation_settings.enabled {
                    self.feat.automation_file_enabled = s.enabled;
                }
                let mut saved = s.clone();
                saved.enabled = self.feat.automation_file_enabled;
                if let Err(e) = saved.save() {
                    self.set_notice(format!("Could not save automation settings: {e}"));
                }
                self.feat.automation_settings = s.clone();
                if s.enabled {
                    #[cfg(unix)]
                    if let Some(h) = &self.feat.automation {
                        h.set_settings(s);
                    }
                    if self.feat.automation.is_none() {
                        self.start_automation();
                    }
                } else {
                    // Dropping the handle stops the server and removes the socket.
                    self.feat.automation = None;
                    self.feat.automation_error = None;
                }
            }
        }
        self.request_redraw();
    }

    /// Starts the control server when the settings or `--automation` ask
    /// for it. Requests reach the event loop as `UserMessage::Automation`.
    pub(super) fn start_automation(&mut self) {
        if self.feat.automation.is_some() || !self.feat.automation_settings.enabled {
            return;
        }
        let proxy = std::sync::Mutex::new(self.proxy.clone());
        match crate::automation::start(move |req| {
            if let Ok(p) = proxy.lock() {
                let _ = p.send_event(UserMessage::Automation(req));
            }
        }) {
            Ok(handle) => {
                #[cfg(unix)]
                handle.set_settings(self.feat.automation_settings.clone());
                self.feat.automation = Some(handle);
                self.feat.automation_error = None;
            }
            Err(e) => {
                eprintln!("browsecraft: automation: {e}");
                self.feat.automation_error = Some(e);
            }
        }
    }

    pub(super) fn feature_action(&mut self, action: FeatureAction) {
        match action {
            FeatureAction::Command(id) => self.run_command(id),
            FeatureAction::Palette(outcome) => self.palette_outcome(outcome),
            FeatureAction::Auth(ev) => self.auth_event(ev),
            FeatureAction::Dark(DarkEvent::SettingsChanged) => self.dark_settings_changed(),
            FeatureAction::Plugins(PluginsEvent::ScriptsChanged) => self.plugins_changed(),
            FeatureAction::Plugins(PluginsEvent::OpenUrl(url)) => self.open_in_new_tab(url),
            FeatureAction::Automation(ev) => self.automation_event(ev),
            FeatureAction::ReloadForPlugins => self.rebuild_web_views(),
        }
    }

    // ---- egui ----

    /// The feature buttons on the toolbar (drawn right to left, after the
    /// password key and before the settings gear). On a narrow window they
    /// go into an overflow menu.
    pub(super) fn feature_toolbar(&mut self, ui: &mut egui::Ui, c: &Colors, actions: &mut Vec<Action>) {
        use egui_phosphor::regular as ph;
        let cmd = |a: &mut Vec<Action>, id| a.push(Action::Feature(FeatureAction::Command(id)));
        // Recording: always visible while it runs.
        if self.feat.recorder.path().is_some() {
            let label = egui::RichText::new(format!("{}  Stop", ph::RECORD)).color(egui_chrome::rgb(0xe5484d));
            if ui.add(egui::Button::new(label).corner_radius(egui::CornerRadius::same(10))).on_hover_text("Stop the screen recording").clicked() {
                cmd(actions, CommandId::StopRecording);
            }
        }
        let auth_on = plugins::is_enabled("builtin.authenticator");
        if auth_on {
            let unlocked = self.feat.auth_unlocked;
            let (glyph, tip) = if unlocked {
                (ph::SHIELD_CHECK, "Authenticator (unlocked)")
            } else {
                (ph::SHIELD, "Authenticator (locked)")
            };
            let open = self.drawer == Some(Drawer::Authenticator);
            if toggle_button(ui, glyph, tip, unlocked || open, c).clicked() {
                actions.push(Action::ToggleDrawer(Drawer::Authenticator));
            }
        }
        let narrow = ui.available_width() < 640.0;
        let dark_on = plugins::is_enabled("builtin.darkmode");
        let host = self.page_host();
        let dark_here = host.as_deref().is_some_and(|h| self.dark_applies(h));
        if narrow {
            let more = egui_chrome::icon_button(ui, true, ph::DOTS_THREE_VERTICAL, "More tools", 16.0);
            egui::Popup::menu(&more).align(egui::RectAlign::BOTTOM_END).width(230.0).show(|ui| {
                let item = |ui: &mut egui::Ui, glyph: &str, text: &str| {
                    ui.button(egui::RichText::new(format!("{glyph}  {text}")).size(13.0)).clicked()
                };
                if dark_on && host.is_some() {
                    let text = if dark_here { "Dark mode off for this site" } else { "Dark mode on for this site" };
                    if item(ui, ph::CIRCLE_HALF, text) {
                        cmd(actions, CommandId::DarkModeSiteToggle);
                    }
                }
                if dark_on && item(ui, ph::CIRCLE_HALF, "Dark mode settings") {
                    actions.push(Action::ToggleDrawer(Drawer::DarkMode));
                }
                if item(ui, ph::PUZZLE_PIECE, "Plugins") {
                    actions.push(Action::ToggleDrawer(Drawer::Plugins));
                }
                if item(ui, ph::STACK, "Containers") {
                    actions.push(Action::ToggleDrawer(Drawer::Containers));
                }
                if item(ui, ph::ROBOT, "Automation and console") {
                    actions.push(Action::ToggleDrawer(Drawer::Automation));
                }
                if item(ui, ph::COMMAND, "Command palette") {
                    actions.push(Action::Palette);
                }
            });
            return;
        }
        if toggle_button(ui, ph::PUZZLE_PIECE, "Plugins", self.drawer == Some(Drawer::Plugins), c).clicked() {
            actions.push(Action::ToggleDrawer(Drawer::Plugins));
        }
        if dark_on {
            let tip = match &host {
                Some(h) if dark_here => format!("Dark mode is on for {h}: click to turn it off. Right-click: settings"),
                Some(h) => format!("Dark mode is off for {h}: click to turn it on. Right-click: settings"),
                None => "Dark mode settings".to_string(),
            };
            let r = toggle_button(ui, ph::CIRCLE_HALF, &tip, dark_here, c);
            if r.secondary_clicked() || r.long_touched() || (r.clicked() && host.is_none()) {
                actions.push(Action::ToggleDrawer(Drawer::DarkMode));
            } else if r.clicked() {
                cmd(actions, CommandId::DarkModeSiteToggle);
            }
        }
        let open = self.drawer == Some(Drawer::Containers);
        if toggle_button(ui, Drawer::Containers.icon(), "Containers", open, c).clicked() {
            actions.push(Action::ToggleDrawer(Drawer::Containers));
        }
    }

    /// Draws the open palette; its outcome becomes an action.
    pub(super) fn feature_palette(&mut self, ctx: &egui::Context, c: &Colors, actions: &mut Vec<Action>) {
        if !self.feat.palette.is_open() {
            return;
        }
        let sources = self.palette_sources();
        if let Some(outcome) = self.feat.palette.ui(ctx, c, &sources) {
            actions.push(Action::Feature(FeatureAction::Palette(outcome)));
        }
    }

    /// The body of a feature drawer.
    pub(super) fn feature_drawer(&mut self, ui: &mut egui::Ui, drawer: Drawer, c: &Colors, actions: &mut Vec<Action>) {
        let mut push = |a: FeatureAction| actions.push(Action::Feature(a));
        match drawer {
            Drawer::Containers => {}
            Drawer::Authenticator => {
                if !plugins::is_enabled("builtin.authenticator") {
                    ui.label(egui::RichText::new("The authenticator is turned off in Plugins.").color(c.muted));
                    return;
                }
                for ev in self.feat.auth.ui(ui, c) {
                    push(FeatureAction::Auth(ev));
                }
            }
            Drawer::DarkMode => {
                if !plugins::is_enabled("builtin.darkmode") {
                    ui.label(egui::RichText::new("Dark mode is turned off in Plugins.").color(c.muted));
                    return;
                }
                let host = self.page_host();
                for ev in self.feat.dark.ui(ui, c, host.as_deref()) {
                    push(FeatureAction::Dark(ev));
                }
            }
            Drawer::Plugins => {
                if self.feat.plugins_stale {
                    egui::Frame::NONE
                        .fill(c.hover)
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(egui::Margin::same(8))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("Open tabs still run the previous plugin scripts.").color(c.text));
                            if ui.button("Reload open tabs").clicked() {
                                push(FeatureAction::ReloadForPlugins);
                            }
                        });
                    ui.add_space(8.0);
                }
                for ev in self.feat.plugins.ui(ui, c) {
                    push(FeatureAction::Plugins(ev));
                }
            }
            Drawer::Automation => {
                let forced = crate::automation::requested();
                let state = ServerState {
                    running: self.feat.automation.is_some(),
                    socket: crate::automation::socket_path().map(|p| p.display().to_string()),
                    error: self.feat.automation_error.as_deref(),
                    forced,
                };
                let settings = self.feat.automation_settings.clone();
                let log = self.feat.console.get(&self.tab_id);
                for ev in self.feat.automation_panel.ui(ui, c, &settings, &state, log) {
                    push(FeatureAction::Automation(ev));
                }
            }
        }
    }
}

/// `~/Downloads` (created if needed), else the home folder.
#[cfg(target_os = "linux")]
fn downloads_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let dir = home.join("Downloads");
    if dir.is_dir() || std::fs::create_dir_all(&dir).is_ok() { Some(dir) } else { Some(home) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_minutes() {
        assert_eq!(web_host("https://Example.com/a").as_deref(), Some("example.com"));
        assert_eq!(web_host("browsecraft://flags"), None);
        assert!(local_minutes() < 1440);
    }
}
