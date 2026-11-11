//! User-settable behavior, atomically persisted in the per-user config folder.
//!
//! BrowseCraft's own experiment switches (no Chromium involved): a tiny
//! `key=value` store. They load once at process start and are written back whenever they change,
//! as `key=value` lines. The internal `browsecraft://flags` page (the settings
//! page) edits them; a few (theme, Library sidebar) also have toolbar/panel shortcuts.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use crate::internal::{self, FormToken};

const FILE_NAME: &str = "flags.txt";

/// The section the Library sidebar (Ctrl+B) shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LibrarySection {
    #[default]
    Bookmarks,
    History,
}

/// The Library sidebar's width limits, in points.
pub const LIBRARY_MIN_WIDTH: u32 = 200;
pub const LIBRARY_MAX_WIDTH: u32 = 600;

/// The chosen theme (see `crate::theme`).
pub use crate::theme::ThemeId as Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Chrome {
    #[default]
    Auto,
    Egui,
    Engine,
}

/// What the browser opens at start-up ("On startup" on the settings page).
/// Addresses on the command line and `--preset` come first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Startup {
    /// The previous session's windows and tabs.
    #[default]
    Continue,
    /// A new tab (the session is not remembered).
    NewTab,
    /// The preset chosen on the settings page (`crate::presets`).
    Preset,
    /// A chooser: the previous session, a preset or a blank page.
    Ask,
}

impl Startup {
    pub const ALL: [Startup; 4] = [Startup::Continue, Startup::NewTab, Startup::Preset, Startup::Ask];

    pub fn key(self) -> &'static str {
        match self {
            Startup::Continue => "continue",
            Startup::NewTab => "new-tab",
            Startup::Preset => "preset",
            Startup::Ask => "ask",
        }
    }

    pub fn from_key(key: &str) -> Option<Startup> {
        Startup::ALL.into_iter().find(|s| s.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Startup::Continue => "Continue previous session",
            Startup::NewTab => "Open a new tab",
            Startup::Preset => "Open a preset",
            Startup::Ask => "Ask every time",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
    pub theme: Theme,
    pub chrome: Chrome,
    /// The bookmarks bar under the address bar.
    pub bookmarks_bar: bool,
    /// Minutes a background tab keeps its web view before it is unloaded
    /// (its address is kept and it reloads when shown). 0 = never.
    pub suspend_after_min: u32,
    /// The most web views kept alive at once; the least recently used
    /// background tabs are unloaded beyond it.
    pub max_live_tabs: u32,
    /// Remember the session (every window's tabs) so it can be reopened at
    /// start-up; off exactly when `startup` is `NewTab`.
    pub restore_tabs: bool,
    /// What opens at start-up.
    pub startup: Startup,
    /// Use the desktop's own title bar and window frame instead of the
    /// browser's (tabs in the title bar, window buttons on the right).
    pub system_titlebar: bool,
    /// Resting the pointer on a tab shows a card with its title, site and
    /// a picture of the page (Chrome's hover cards).
    pub hover_cards: bool,
    /// The hover card shows the page's picture.
    pub hover_card_images: bool,
    /// How long the pointer rests on a tab before the first card shows.
    pub hover_card_delay_ms: u32,
    /// The Library sidebar (bookmarks and history, on the left) is open.
    pub library_open: bool,
    /// The section it shows.
    pub library_section: LibrarySection,
    /// Its width in points (clamped to `LIBRARY_MIN_WIDTH..=LIBRARY_MAX_WIDTH`).
    pub library_width: u32,
    /// Days of browsing history kept (0: until cleared).
    pub history_days: u32,
    /// The most visits kept; the oldest go first (0: no cap).
    pub history_max_visits: u32,
    /// The new-tab page offers to import from another browser (until
    /// dismissed, and only while there are no bookmarks).
    pub import_hint: bool,
}

/// The choices offered for `suspend_after_min` and `max_live_tabs`.
pub const SUSPEND_CHOICES: [u32; 5] = [0, 5, 15, 30, 60];
pub const LIVE_TAB_CHOICES: [u32; 4] = [4, 8, 16, 32];
/// The choices offered for `hover_card_delay_ms` (Chrome waits about 0.5 s).
pub const HOVER_DELAY_CHOICES: [u32; 5] = [0, 250, 500, 800, 1200];
/// The choices offered for `history_days` (0: forever) and
/// `history_max_visits`.
pub const HISTORY_DAY_CHOICES: [u32; 5] = [30, 90, 365, 1825, 0];
pub const HISTORY_MAX_CHOICES: [u32; 3] = [100_000, 500_000, 2_000_000];

impl Default for Flags {
    fn default() -> Self {
        DEFAULT_FLAGS
    }
}

const DEFAULT_FLAGS: Flags = Flags {
    theme: Theme::LateNight,
    chrome: Chrome::Auto,
    bookmarks_bar: true,
    suspend_after_min: 30,
    max_live_tabs: 16,
    restore_tabs: true,
    startup: Startup::Continue,
    system_titlebar: false,
    hover_cards: true,
    hover_card_images: true,
    hover_card_delay_ms: 500,
    library_open: false,
    library_section: LibrarySection::Bookmarks,
    library_width: 300,
    history_days: 365,
    history_max_visits: 500_000,
    import_hint: true,
};

static FLAGS: RwLock<Flags> = RwLock::new(DEFAULT_FLAGS);
static SAVE_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn file_path() -> Result<PathBuf, String> {
    crate::storage::config_file(FILE_NAME)
}

/// Loads the persisted flags from disk, merging into the defaults.
fn load_from_mut(flags: &mut Flags) {
    let Ok(path) = file_path() else {
        return;
    };
    let Ok(text) = crate::storage::read_text_limited(&path, crate::storage::MAX_SETTINGS_FILE) else {
        return;
    };
    let mut startup_line = false;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match (key.trim(), value.trim()) {
            // Tabs are always on top now: the old `tabs-position` and
            // `sidebar-side` lines are ignored.
            ("theme", v) => {
                if let Some(id) = Theme::from_key(v) {
                    flags.theme = id;
                }
            }
            ("chrome", "auto") => flags.chrome = Chrome::Auto,
            ("chrome", "egui") => flags.chrome = Chrome::Egui,
            ("chrome", "engine") => flags.chrome = Chrome::Engine,
            ("bookmarks-bar", "on") => flags.bookmarks_bar = true,
            ("bookmarks-bar", "off") => flags.bookmarks_bar = false,
            ("suspend-after", v) => {
                if let Ok(n) = v.parse::<u32>()
                    && n <= 24 * 60
                {
                    flags.suspend_after_min = n;
                }
            }
            ("max-live-tabs", v) => {
                if let Ok(n) = v.parse::<u32>()
                    && (1..=256).contains(&n)
                {
                    flags.max_live_tabs = n;
                }
            }
            ("restore-tabs", "on") => flags.restore_tabs = true,
            ("restore-tabs", "off") => flags.restore_tabs = false,
            ("startup", v) => {
                if let Some(mode) = Startup::from_key(v) {
                    flags.startup = mode;
                    startup_line = true;
                }
            }
            ("system-titlebar", "on") => flags.system_titlebar = true,
            ("system-titlebar", "off") => flags.system_titlebar = false,
            ("hover-cards", "on") => flags.hover_cards = true,
            ("hover-cards", "off") => flags.hover_cards = false,
            ("hover-card-images", "on") => flags.hover_card_images = true,
            ("hover-card-images", "off") => flags.hover_card_images = false,
            ("hover-card-delay", v) => {
                if let Ok(n) = v.parse::<u32>()
                    && n <= 5000
                {
                    flags.hover_card_delay_ms = n;
                }
            }
            ("library", "open") => flags.library_open = true,
            ("library", "closed") => flags.library_open = false,
            ("library-section", "bookmarks") => flags.library_section = LibrarySection::Bookmarks,
            ("library-section", "history") => flags.library_section = LibrarySection::History,
            ("library-width", v) => {
                if let Ok(n) = v.parse::<u32>() {
                    flags.library_width = n.clamp(LIBRARY_MIN_WIDTH, LIBRARY_MAX_WIDTH);
                }
            }
            ("history-days", v) => {
                if let Ok(n) = v.parse::<u32>()
                    && n <= 100 * 365
                {
                    flags.history_days = n;
                }
            }
            ("history-max-visits", v) => {
                if let Ok(n) = v.parse::<u32>() {
                    flags.history_max_visits = n;
                }
            }
            ("import-hint", "on") => flags.import_hint = true,
            ("import-hint", "off") => flags.import_hint = false,
            _ => {}
        }
    }
    // Files from before "On startup": the switch decides.
    if !startup_line {
        flags.startup = if flags.restore_tabs { Startup::Continue } else { Startup::NewTab };
    }
    flags.restore_tabs = flags.startup != Startup::NewTab;
}

fn save_to(path: &Path, flags: &Flags) -> std::io::Result<()> {
    let mut text = String::new();
    text += &format!("theme={}\n", flags.theme.key());
    match flags.chrome {
        Chrome::Auto => text += "chrome=auto\n",
        Chrome::Egui => text += "chrome=egui\n",
        Chrome::Engine => text += "chrome=engine\n",
    }
    text += if flags.bookmarks_bar {
        "bookmarks-bar=on\n"
    } else {
        "bookmarks-bar=off\n"
    };
    text += &format!("suspend-after={}\n", flags.suspend_after_min);
    text += &format!("max-live-tabs={}\n", flags.max_live_tabs);
    text += if flags.restore_tabs { "restore-tabs=on\n" } else { "restore-tabs=off\n" };
    text += &format!("startup={}\n", flags.startup.key());
    text += if flags.system_titlebar { "system-titlebar=on\n" } else { "system-titlebar=off\n" };
    text += if flags.hover_cards { "hover-cards=on\n" } else { "hover-cards=off\n" };
    text += if flags.hover_card_images { "hover-card-images=on\n" } else { "hover-card-images=off\n" };
    text += &format!("hover-card-delay={}\n", flags.hover_card_delay_ms);
    text += if flags.library_open { "library=open\n" } else { "library=closed\n" };
    text += match flags.library_section {
        LibrarySection::Bookmarks => "library-section=bookmarks\n",
        LibrarySection::History => "library-section=history\n",
    };
    text += &format!("library-width={}\n", flags.library_width);
    text += &format!("history-days={}\n", flags.history_days);
    text += &format!("history-max-visits={}\n", flags.history_max_visits);
    text += if flags.import_hint { "import-hint=on\n" } else { "import-hint=off\n" };
    crate::storage::write_private(path, text.as_bytes())
}

/// Loads persisted settings once at startup without writing a new file.
pub fn load() {
    let mut flags = get();
    load_from_mut(&mut flags);
    *FLAGS.write().unwrap_or_else(|p| p.into_inner()) = flags;
}

/// The current flags (cheap: a copy).
pub fn get() -> Flags {
    *FLAGS.read().unwrap_or_else(|p| p.into_inner())
}

/// Persist atomically before changing in-memory flags; report failures to UI.
pub fn set(flags: Flags) -> Result<(), String> {
    let mut current = FLAGS.write().unwrap_or_else(|p| p.into_inner());
    let result = file_path().and_then(|path| {
        save_to(&path, &flags)
            .map_err(|e| format!("Could not save settings to {}: {e}", path.display()))
    });
    *SAVE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = result.as_ref().err().cloned();
    if result.is_ok() {
        *current = flags;
    }
    result
}

// ---- browsecraft://flags page ---------------------------------------------------

/// The effective theme for the browser chrome: the flag, or the OS theme.
pub fn effective_theme(window_dark: bool) -> &'static crate::theme::Theme {
    get().theme.resolve(window_dark)
}

/// Whether the effective theme is a dark one.
#[cfg(test)]
pub fn effective_dark(window_dark: bool) -> bool {
    effective_theme(window_dark).dark
}

/// Saves a new theme choice (the palette and the toolbar menu use this; the
/// settings page goes through its form).
pub fn set_theme(id: Theme) -> Result<(), String> {
    let mut flags = get();
    if flags.theme == id {
        return Ok(());
    }
    flags.theme = id;
    set(flags)
}

/// Saves the title bar choice (the debug test hook; the settings page goes
/// through its form).
#[cfg(any(test, debug_assertions))]
pub fn set_system_titlebar(on: bool) -> Result<(), String> {
    let mut flags = get();
    if flags.system_titlebar == on {
        return Ok(());
    }
    flags.system_titlebar = on;
    set(flags)
}

/// The one-time token the settings page's links must send back. Only that
/// page can change the flags: a link, a pop-up, another page's form or a
/// back/forward replay of an earlier submission cannot know it.
static FORM_TOKEN: FormToken = FormToken::new();

/// Applies a submission of the settings page (the URL query, without the
/// `?`). Returns whether any flag changed. Without an outstanding token it
/// does nothing; with it, the token is used up.
pub fn apply_form(query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    let Some(pairs) = FORM_TOKEN.take(query) else {
        *SAVE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(
            "This settings link has expired or was already used. Review the values and choose again."
                .into(),
        );
        return false;
    };
    *SAVE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let pairs: Vec<(String, String)> = pairs.into_iter().map(|(k, v)| (k, v.to_string())).collect();
    // The site user agents have their own file (see `crate::useragent`).
    let agents = match crate::useragent::apply_form_pairs(&pairs) {
        Ok(changed) => changed,
        Err(error) => {
            *SAVE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
            false
        }
    };
    let permissions = match crate::permissions::apply_form_pairs(&pairs) {
        Ok(changed) => changed,
        Err(error) => {
            *SAVE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
            false
        }
    };
    apply_query(pairs.into_iter()) || agents || permissions
}

fn apply_query(pairs: impl Iterator<Item = (String, String)>) -> bool {
    let mut flags = get();
    let mut changed = false;
    for (key, value) in pairs {
        match (key.as_str(), value.as_str()) {
            ("theme", v) => {
                if let Some(id) = Theme::from_key(v) {
                    changed |= flags.theme != id;
                    flags.theme = id;
                }
            }
            ("chrome", "auto") => {
                changed |= flags.chrome != Chrome::Auto;
                flags.chrome = Chrome::Auto;
            }
            ("chrome", "egui") => {
                changed |= flags.chrome != Chrome::Egui;
                flags.chrome = Chrome::Egui;
            }
            ("chrome", "engine") => {
                changed |= flags.chrome != Chrome::Engine;
                flags.chrome = Chrome::Engine;
            }
            ("bookmarks-bar", "on") => {
                changed |= !flags.bookmarks_bar;
                flags.bookmarks_bar = true;
            }
            ("bookmarks-bar", "off") => {
                changed |= flags.bookmarks_bar;
                flags.bookmarks_bar = false;
            }
            ("suspend-after", v) => {
                if let Some(n) = v.parse::<u32>().ok().filter(|n| SUSPEND_CHOICES.contains(n)) {
                    changed |= flags.suspend_after_min != n;
                    flags.suspend_after_min = n;
                }
            }
            ("max-live-tabs", v) => {
                if let Some(n) = v.parse::<u32>().ok().filter(|n| LIVE_TAB_CHOICES.contains(n)) {
                    changed |= flags.max_live_tabs != n;
                    flags.max_live_tabs = n;
                }
            }
            ("restore-tabs", "on") => {
                changed |= !flags.restore_tabs;
                flags.restore_tabs = true;
                if flags.startup == Startup::NewTab {
                    flags.startup = Startup::Continue;
                }
            }
            ("restore-tabs", "off") => {
                changed |= flags.restore_tabs;
                flags.restore_tabs = false;
                flags.startup = Startup::NewTab;
            }
            ("startup", v) => {
                if let Some(mode) = Startup::from_key(v) {
                    changed |= flags.startup != mode;
                    flags.startup = mode;
                    flags.restore_tabs = mode != Startup::NewTab;
                }
            }
            ("startup-preset", v) => {
                // Kept with the presets (`presets.json`).
                let id = v.parse::<u64>().ok();
                if id.is_some_and(|id| crate::presets::store().startup_preset != Some(id))
                    && crate::presets::modify(|s| s.set_startup(id)).is_ok()
                {
                    changed = true;
                    // Choosing a preset also means opening it.
                    flags.startup = Startup::Preset;
                    flags.restore_tabs = true;
                }
            }
            ("system-titlebar", "on") => {
                changed |= !flags.system_titlebar;
                flags.system_titlebar = true;
            }
            ("system-titlebar", "off") => {
                changed |= flags.system_titlebar;
                flags.system_titlebar = false;
            }
            ("hover-cards", v @ ("on" | "off")) => {
                let on = v == "on";
                changed |= flags.hover_cards != on;
                flags.hover_cards = on;
            }
            ("hover-card-images", v @ ("on" | "off")) => {
                let on = v == "on";
                changed |= flags.hover_card_images != on;
                flags.hover_card_images = on;
            }
            ("hover-card-delay", v) => {
                if let Some(n) = v.parse::<u32>().ok().filter(|n| HOVER_DELAY_CHOICES.contains(n)) {
                    changed |= flags.hover_card_delay_ms != n;
                    flags.hover_card_delay_ms = n;
                }
            }
            ("history-days", v) => {
                if let Some(n) = v.parse::<u32>().ok().filter(|n| HISTORY_DAY_CHOICES.contains(n)) {
                    changed |= flags.history_days != n;
                    flags.history_days = n;
                }
            }
            ("history-max-visits", v) => {
                if let Some(n) = v.parse::<u32>().ok().filter(|n| HISTORY_MAX_CHOICES.contains(n)) {
                    changed |= flags.history_max_visits != n;
                    flags.history_max_visits = n;
                }
            }
            _ => {}
        }
    }
    if changed {
        return set(flags).is_ok();
    }
    changed
}

/// The HTML source of the `browsecraft://flags` (settings) page, reflecting
/// the current flags. Each call issues a new form token.
pub fn page_html() -> String {
    let flags = get();
    let token = match FORM_TOKEN.issue() {
        Ok(token) => token,
        Err(error) => return internal::unavailable_page("Settings", &error),
    };
    let error = SAVE_ERROR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_deref()
        .map(|error| {
            format!(
                "<div class=\"card\"><div class=\"row first\"><div class=\"bad\" role=alert>{}</div></div></div>",
                internal::escape(error)
            )
        })
        .unwrap_or_default();
    // Every option is a link back to the page carrying the token.
    let to = |key: &str, value: &str| format!("browsecraft://flags?token={token}&{key}={value}");

    let render = |label: &'static str, value: &str, on: bool| (label, to("chrome", value), on);

    let appearance = theme_row(flags.theme, &to);
    let titlebar = internal::switch_row(
        false,
        "Use system title bar",
        "Show the desktop's own window frame instead of BrowseCraft's, which keeps the tabs and the window buttons in one row.",
        flags.system_titlebar,
        &to("system-titlebar", if flags.system_titlebar { "off" } else { "on" }),
    );
    let bar = internal::switch_row(
        true,
        "Bookmarks bar",
        "Show your bookmarks under the address bar.",
        flags.bookmarks_bar,
        &to("bookmarks-bar", if flags.bookmarks_bar { "off" } else { "on" }),
    );
    let renderer = internal::segmented_row(
        true,
        "Interface renderer",
        "Auto uses the GPU when there is one. Applies after a restart.",
        &[
            render("Auto", "auto", flags.chrome == Chrome::Auto),
            render("GPU", "egui", flags.chrome == Chrome::Egui),
            render("Software", "engine", flags.chrome == Chrome::Engine),
        ],
    );
    let suspend_labels = ["Never", "5 min", "15 min", "30 min", "1 hour"];
    let suspend: Vec<(&'static str, String, bool)> = SUSPEND_CHOICES
        .iter()
        .zip(suspend_labels)
        .map(|(n, label)| (label, to("suspend-after", &n.to_string()), flags.suspend_after_min == *n))
        .collect();
    let suspend = internal::segmented_row(
        true,
        "Unload background tabs after",
        "Frees the memory of tabs you have not looked at for a while. They reload when you switch to them.",
        &suspend,
    );
    let live_labels = ["4", "8", "16", "32"];
    let live: Vec<(&'static str, String, bool)> = LIVE_TAB_CHOICES
        .iter()
        .zip(live_labels)
        .map(|(n, label)| (label, to("max-live-tabs", &n.to_string()), flags.max_live_tabs == *n))
        .collect();
    let live = internal::segmented_row(
        false,
        "Tabs kept in memory",
        "Beyond this, the least recently used background tabs are unloaded.",
        &live,
    );
    let restore = startup_rows(flags.startup, &to);
    let hover = internal::switch_row(
        false,
        "Tab hover cards",
        "Rest the pointer on a tab to see its title, site and a picture of the page.",
        flags.hover_cards,
        &to("hover-cards", if flags.hover_cards { "off" } else { "on" }),
    );
    let hover_images = internal::switch_row(
        false,
        "Pictures in hover cards",
        "Show a picture of the page in the card (taken when you leave the tab).",
        flags.hover_card_images,
        &to("hover-card-images", if flags.hover_card_images { "off" } else { "on" }),
    );
    let delay_labels = ["None", "0.25 s", "0.5 s", "0.8 s", "1.2 s"];
    let delays: Vec<(&'static str, String, bool)> = HOVER_DELAY_CHOICES
        .iter()
        .zip(delay_labels)
        .map(|(n, label)| (label, to("hover-card-delay", &n.to_string()), flags.hover_card_delay_ms == *n))
        .collect();
    let hover_delay = internal::segmented_row(
        false,
        "Hover card delay",
        "How long the pointer rests on a tab before the first card shows; moving to the next tab is immediate.",
        &delays,
    );
    let agents = crate::useragent::settings_card(&to, &token);
    let permissions = crate::permissions::settings_card(&to);
    let day_labels = ["30 days", "90 days", "1 year", "5 years", "Forever"];
    let days: Vec<(&'static str, String, bool)> = HISTORY_DAY_CHOICES
        .iter()
        .zip(day_labels)
        .map(|(n, label)| (label, to("history-days", &n.to_string()), flags.history_days == *n))
        .collect();
    let history_days = internal::segmented_row(
        true,
        "Keep history for",
        "Older visits are deleted at start-up. Imported history follows the same rule.",
        &days,
    );
    let max_labels = ["100k", "500k", "2M"];
    let maxes: Vec<(&'static str, String, bool)> = HISTORY_MAX_CHOICES
        .iter()
        .zip(max_labels)
        .map(|(n, label)| (label, to("history-max-visits", &n.to_string()), flags.history_max_visits == *n))
        .collect();
    let history_max = internal::segmented_row(
        false,
        "Most visits kept",
        "Beyond this, the oldest visits are deleted at start-up.",
        &maxes,
    );
    let body = format!(
        "<h1>Settings</h1><p class=\"sub\">Changes apply immediately.</p>{error}\
         <div class=\"card\"><h2>Appearance</h2>{appearance}{titlebar}</div>\
         <div class=\"card\"><h2>Layout</h2>{bar}{hover}{hover_images}{hover_delay}</div>\
         <div class=\"card\"><h2>Tabs and memory</h2>{suspend}{live}{restore}</div>\
         <div class=\"card\"><h2>History</h2>{history_days}{history_max}</div>\
         <div class=\"card\"><h2>Advanced</h2>{renderer}</div>{agents}{permissions}\
         <div class=\"card\"><h2>Shortcuts to your data</h2>\
         <div class=\"row first\"><div class=\"grow\"><div class=\"t\">Bookmarks</div><div class=\"d\">Pages you saved.</div></div>\
         <a class=\"btn ghost\" href=\"browsecraft://bookmarks\">Open</a></div>\
         <div class=\"row\"><div class=\"grow\"><div class=\"t\">History</div><div class=\"d\">Pages you visited.</div></div>\
         <a class=\"btn ghost\" href=\"browsecraft://history\">Open</a></div>\
         <div class=\"row\"><div class=\"grow\"><div class=\"t\">Import from another browser</div><div class=\"d\">Bookmarks and history from Firefox, Chrome and others.</div></div>\
         <a class=\"btn ghost\" href=\"browsecraft://import\">Open</a></div>\
         <div class=\"row\"><div class=\"grow\"><div class=\"t\">Passwords</div><div class=\"d\">Your KeePass (.kdbx) vault.</div></div>\
         <a class=\"btn ghost\" href=\"browsecraft://passwords\">Open</a></div></div>"
    );
    internal::page("Settings", &body)
}

/// "On startup" and, for "Open a preset", the preset to open. `to(key,
/// value)` builds a link carrying the form token.
fn startup_rows(current: Startup, to: &dyn Fn(&str, &str) -> String) -> String {
    let options: Vec<(&'static str, String, bool)> =
        Startup::ALL.iter().map(|m| (m.label(), to("startup", m.key()), *m == current)).collect();
    let mut out = internal::segmented_row(
        false,
        "On startup",
        "Previous session reopens every window's tabs (not temporary containers), each loading when first shown. \
         Addresses on the command line and --preset always win.",
        &options,
    );
    if current == Startup::Preset {
        let store = crate::presets::store();
        if store.presets.is_empty() {
            out.push_str(
                "<div class=\"row\"><div class=\"grow\"><div class=\"t\">Preset to open</div>\
                 <div class=\"d\">No presets yet: save one with \u{201c}Save session as preset\u{2026}\u{201d}. Until then the previous session opens.</div></div>\
                 <a class=\"btn ghost\" href=\"browsecraft://sessions\">Presets</a></div>",
            );
        } else {
            let mut links = String::new();
            for p in &store.presets {
                links.push_str(&format!(
                    "<a{} href=\"{}\">{}</a>",
                    if store.startup_preset == Some(p.id) { " class=\"on\"" } else { "" },
                    internal::escape(&to("startup-preset", &p.id.to_string())),
                    internal::escape(&p.name)
                ));
            }
            out.push_str(&format!(
                "<div class=\"row\"><div class=\"grow\"><div class=\"t\">Preset to open</div>\
                 <div class=\"d\">If it is deleted, the previous session opens instead.</div></div><div class=\"seg\">{links}</div></div>"
            ));
        }
    }
    out
}

/// The theme picker of the settings page: one swatch per theme, the current
/// one marked. `to(key, value)` builds a link carrying the form token.
fn theme_row(current: Theme, to: &dyn Fn(&str, &str) -> String) -> String {
    use crate::theme::css_hex;
    let mut cards = String::new();
    for id in Theme::ALL {
        // "Follow system" shows its light and dark halves.
        let (a, b) = match id {
            Theme::System => (crate::theme::LIGHT.t, crate::theme::DARK.t),
            other => (crate::theme::get(other).t, crate::theme::get(other).t),
        };
        cards.push_str(&format!(
            "<a class=\"theme{}\" href=\"{}\"><span class=\"sw3\" style=\"background: {}; border-color: {}\">\
             <i style=\"background: {}\"></i><i style=\"background: {}\"></i><i style=\"background: {}\"></i></span>{}</a>",
            if id == current { " on" } else { "" },
            internal::escape(&to("theme", id.key())),
            css_hex(a.background),
            css_hex(b.border),
            css_hex(a.surface),
            css_hex(b.background),
            css_hex(b.accent),
            internal::escape(id.label()),
        ));
    }
    format!(
        "<div class=\"row first\"><div class=\"grow\"><div class=\"t\">Theme</div>\
         <div class=\"d\">The colours of the browser and of its own pages. Also in the command palette: Change theme.</div>\
         <div class=\"themes\">{cards}</div></div></div>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flags are a process-global and persist to a file; tests that touch
    /// them must not run at the same time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn query_sets_flags() {
        let _guard = SERIAL.lock().unwrap();
        let before = get();
        set(Flags::default()).unwrap();
        let pairs = vec![
            ("theme".into(), "light".into()),
            ("chrome".into(), "egui".into()),
            ("bookmarks-bar".into(), "off".into()),
        ];
        assert!(apply_query(pairs.into_iter()));
        let after = get();
        assert_eq!(after.theme, Theme::Light);
        assert_eq!(after.chrome, Chrome::Egui);
        assert!(!after.bookmarks_bar);
        assert!(!apply_query(vec![].into_iter()));
        assert!(!effective_dark(false));
        set(before).unwrap();
    }

    #[test]
    fn dark_is_the_default_theme() {
        assert_eq!(Flags::default().theme, Theme::LateNight);
        assert!(Flags::default().bookmarks_bar);
        assert!(!Flags::default().system_titlebar, "the browser's own title bar by default");
    }

    #[test]
    fn themes_and_title_bar_persist() {
        let _guard = SERIAL.lock().unwrap();
        let before = get();
        set(Flags::default()).unwrap();
        assert!(apply_query(vec![("theme".into(), "olive".into()), ("system-titlebar".into(), "on".into())].into_iter()));
        assert!(!apply_query(vec![("theme".into(), "neon".into())].into_iter()), "unknown themes are ignored");
        let mut flags = Flags::default();
        load_from_mut(&mut flags);
        assert_eq!(flags.theme, Theme::Olive);
        assert!(flags.system_titlebar);
        set_theme(Theme::Frost).unwrap();
        set_system_titlebar(false).unwrap();
        let mut flags = Flags::default();
        load_from_mut(&mut flags);
        assert_eq!((flags.theme, flags.system_titlebar), (Theme::Frost, false));
        assert!(effective_dark(false), "Frost is dark whatever the desktop");
        set_theme(Theme::System).unwrap();
        assert!(!effective_dark(false) && effective_dark(true));
        set(before).unwrap();
    }

    /// The token from a freshly rendered settings page.
    fn rendered_token() -> String {
        let html = page_html();
        let marker = "token=";
        let start = html.find(marker).unwrap() + marker.len();
        html[start..start + 32].to_string()
    }

    #[test]
    fn only_the_settings_page_can_change_flags() {
        let _guard = SERIAL.lock().unwrap();
        let before = get();
        set(Flags::default()).unwrap();
        let query = |token: &str| format!("token={token}&theme=light");

        // No token, a guessed token, or a link with no page rendered yet.
        FORM_TOKEN.clear();
        assert!(!apply_form("theme=light"));
        assert!(!apply_form("token=&theme=light"));
        let token = rendered_token();
        assert!(!apply_form("theme=light"));
        assert!(!apply_form(&query(&"0".repeat(32))));
        assert_eq!(get().theme, Theme::LateNight);

        // The real link works once; replaying it (back/forward) does not.
        assert!(apply_form(&query(&token)));
        assert_eq!(get().theme, Theme::Light);
        set(Flags::default()).unwrap();
        assert!(!apply_form(&query(&token)));
        assert_eq!(get().theme, Theme::LateNight);

        // Rendering again issues a different token.
        assert_ne!(rendered_token(), token);
        set(before).unwrap();
    }

    #[test]
    fn unknown_keys_ignored() {
        let _guard = SERIAL.lock().unwrap();
        let before = get();
        set(Flags::default()).unwrap();
        assert!(!apply_query(vec![("bogus".into(), "1".into())].into_iter()));
        set(before).unwrap();
    }

    #[test]
    fn page_marks_the_current_values() {
        let _guard = SERIAL.lock().unwrap();
        let before = get();
        set(Flags {
            theme: Theme::Light,
            chrome: Chrome::Egui,
            bookmarks_bar: false,
            restore_tabs: false,
            hover_cards: false,
            hover_card_images: false,
            ..Flags::default()
        }).unwrap();
        let html = page_html();
        assert!(html.contains("chrome=auto\">Auto"), "unselected links have no class");
        assert!(!html.contains("tabs-position"), "tabs are always on top");
        assert!(html.contains("class=\"on\" href=\"browsecraft://flags?token="));
        assert_eq!(html.matches("class=\"on\"").count(), 7, "one selected option per control");
        assert_eq!(html.matches("class=\"theme on\"").count(), 1, "one theme selected");
        assert!(html.contains("theme=sand"), "every theme is offered");
        // The bookmarks-bar switch is off and offers to turn it on.
        assert!(html.contains("bookmarks-bar=on"));
        assert!(!html.contains("class=\"sw on\""));
        set(Flags { bookmarks_bar: true, hover_cards: false, hover_card_images: false, ..Flags::default() }).unwrap();
        assert!(page_html().contains("class=\"sw on\""));
        set(before).unwrap();
    }

    #[test]
    fn roundtrip_via_file() {
        let _guard = SERIAL.lock().unwrap();
        let before = get();
        set(Flags {
            library_open: true,
            library_section: LibrarySection::History,
            library_width: 420,
            theme: Theme::Light,
            chrome: Chrome::Egui,
            bookmarks_bar: false,
            ..Flags::default()
        }).unwrap();
        let mut flags = Flags::default();
        load_from_mut(&mut flags);
        assert!(flags.library_open);
        assert_eq!((flags.library_section, flags.library_width), (LibrarySection::History, 420));
        assert_eq!(flags.theme, Theme::Light);
        assert_eq!(flags.chrome, Chrome::Egui);
        assert!(!flags.bookmarks_bar);
        set(before).unwrap();
    }

    #[test]
    fn tab_and_memory_flags_round_trip_and_validate() {
        let _guard = SERIAL.lock().unwrap();
        let before = get();
        set(Flags::default()).unwrap();
        let pairs = vec![
            ("suspend-after".into(), "5".into()),
            ("max-live-tabs".into(), "4".into()),
            ("restore-tabs".into(), "off".into()),
        ];
        assert!(apply_query(pairs.into_iter()));
        // Values the page never offers are ignored.
        assert!(!apply_query(vec![("suspend-after".into(), "7".into()), ("max-live-tabs".into(), "0".into())].into_iter()));
        let mut flags = Flags::default();
        load_from_mut(&mut flags);
        assert_eq!((flags.suspend_after_min, flags.max_live_tabs, flags.restore_tabs), (5, 4, false));
        set(before).unwrap();
    }

    #[test]
    fn startup_choice_round_trips_and_follows_the_old_switch() {
        let _guard = SERIAL.lock().unwrap();
        let before = get();
        set(Flags::default()).unwrap();
        assert_eq!(get().startup, Startup::Continue, "the previous session by default");
        assert!(apply_query(vec![("startup".into(), "ask".into())].into_iter()));
        assert!(get().restore_tabs, "asking needs the session");
        assert!(!apply_query(vec![("startup".into(), "sometimes".into())].into_iter()));
        let mut flags = Flags::default();
        load_from_mut(&mut flags);
        assert_eq!((flags.startup, flags.restore_tabs), (Startup::Ask, true));
        assert!(apply_query(vec![("startup".into(), "new-tab".into())].into_iter()));
        assert!(!get().restore_tabs, "a new tab: the session is not kept");
        assert!(page_html().contains("On startup"));
        // A file from before the setting: the old switch decides.
        let path = file_path().unwrap();
        std::fs::write(&path, "restore-tabs=off\n").unwrap();
        let mut flags = Flags::default();
        load_from_mut(&mut flags);
        assert_eq!((flags.startup, flags.restore_tabs), (Startup::NewTab, false));
        std::fs::write(&path, "restore-tabs=off\nstartup=preset\n").unwrap();
        let mut flags = Flags::default();
        load_from_mut(&mut flags);
        assert_eq!((flags.startup, flags.restore_tabs), (Startup::Preset, true), "the choice wins");
        for m in Startup::ALL {
            assert_eq!(Startup::from_key(m.key()), Some(m));
        }
        set(before).unwrap();
    }

    #[test]
    fn history_retention_flags_round_trip_and_validate() {
        let _guard = SERIAL.lock().unwrap();
        let before = get();
        set(Flags::default()).unwrap();
        assert_eq!((get().history_days, get().history_max_visits), (365, 500_000));
        let pairs = vec![("history-days".into(), "0".into()), ("history-max-visits".into(), "2000000".into())];
        assert!(apply_query(pairs.into_iter()));
        assert!(!apply_query(vec![("history-days".into(), "7".into())].into_iter()), "only the offered choices");
        let mut flags = Flags::default();
        load_from_mut(&mut flags);
        assert_eq!((flags.history_days, flags.history_max_visits), (0, 2_000_000));
        assert!(page_html().contains("Keep history for"));
        set(before).unwrap();
    }

    #[test]
    fn hover_card_flags_round_trip_and_validate() {
        let _guard = SERIAL.lock().unwrap();
        let before = get();
        set(Flags::default()).unwrap();
        assert!(get().hover_cards && get().hover_card_images, "hover cards with pictures by default");
        assert_eq!(get().hover_card_delay_ms, 500, "Chrome's half second");
        let pairs = vec![
            ("hover-cards".into(), "off".into()),
            ("hover-card-images".into(), "off".into()),
            ("hover-card-delay".into(), "250".into()),
        ];
        assert!(apply_query(pairs.into_iter()));
        assert!(!apply_query(vec![("hover-card-delay".into(), "7".into())].into_iter()), "only the offered delays");
        let mut flags = Flags::default();
        load_from_mut(&mut flags);
        assert_eq!((flags.hover_cards, flags.hover_card_images, flags.hover_card_delay_ms), (false, false, 250));
        set(before).unwrap();
    }

    #[test]
    fn old_tab_sidebar_lines_load_as_tabs_on_top() {
        let _guard = SERIAL.lock().unwrap();
        let before = get();
        set(before).unwrap(); // creates the folder
        let path = file_path().unwrap();
        std::fs::write(&path, "tabs-position=left\nsidebar-side=right\ntheme=light\nlibrary=open\nlibrary-width=9999\n").unwrap();
        let mut flags = Flags::default();
        load_from_mut(&mut flags);
        assert_eq!(flags.theme, Theme::Light, "the rest still loads");
        assert!(flags.library_open);
        assert_eq!(flags.library_width, LIBRARY_MAX_WIDTH, "clamped");
        set(before).unwrap();
    }

    #[test]
    fn settings_page_lists_site_user_agents() {
        let _guard = SERIAL.lock().unwrap();
        let _ua = crate::useragent::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let html = page_html();
        assert!(html.contains("Site user agents") && html.contains("name=\"ua-pattern\""));
        let token = rendered_token();
        assert!(apply_form(&format!("token={token}&ua-pattern=127.0.0.1&ua-preset=chrome")));
        assert!(page_html().contains("ua-remove=127.0.0.1"));
        let token = rendered_token();
        assert!(apply_form(&format!("token={token}&ua-remove=127.0.0.1")));
        assert!(!page_html().contains("ua-remove=127.0.0.1"));
        let token = rendered_token();
        assert!(!apply_form(&format!("token={token}&ua-pattern=a%20b&ua-preset=chrome")));
        assert!(page_html().contains("not a host name"), "the error is shown");
    }

    #[test]
    fn retained_pages_have_independent_single_use_tokens() {
        let _guard = SERIAL.lock().unwrap();
        set(Flags::default()).unwrap();
        FORM_TOKEN.clear();
        let first = rendered_token();
        let second = rendered_token();
        assert_ne!(first, second);
        assert!(apply_form(&format!("token={first}&theme=light")));
        assert!(apply_form(&format!("token={second}&theme=auto")));
        assert!(!apply_form(&format!("token={first}&theme=dark")));
        assert!(!apply_form(&format!("token={second}&theme=dark")));
        assert_eq!(get().theme, Theme::System);
        assert!(page_html().contains("already used"));
        set(Flags::default()).unwrap();
    }

    #[test]
    fn failed_persistence_preserves_flags_and_displays_the_error() {
        let _guard = SERIAL.lock().unwrap();
        let before = get();
        let path = file_path().unwrap();
        std::fs::create_dir_all(&path).unwrap(); // a directory cannot be atomically replaced by a file
        let mut next = before;
        next.theme = Theme::Light;
        assert!(set(next).is_err());
        assert_eq!(get(), before);
        assert!(page_html().contains("Could not save settings"));
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        std::fs::remove_dir(path).unwrap();
        set(before).unwrap();
    }
}
