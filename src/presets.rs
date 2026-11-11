//! Session presets: named snapshots of one window or of every open window
//! ("Lavoro", "Research"), opened again on demand or at start-up.
//!
//! A preset keeps, per window, its name and its tabs (address, title,
//! container, tab group, split view pairing); never page state: tabs reopen
//! from their address and load when first shown, as with session restore.
//! Tabs in temporary containers are left out.
//!
//! The presets live in `presets.json` in the config folder (owner-only,
//! replaced atomically, with a schema `version`). A file written by a newer
//! BrowseCraft is read but never overwritten.
//!
//! Window snapshots use the session file's window shape (`tabs`, `groups`,
//! `split` by tab position, `name`, `active`), so the browser captures them
//! with `session_value` and opens them with its session restore.

use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::flags::Startup;
use crate::tab_groups::TabGroup;

pub const FILE_NAME: &str = "presets.json";
/// The schema version this build writes.
pub const VERSION: u32 = 1;
pub const MAX_PRESETS: usize = 200;
pub const MAX_NAME: usize = 60;
const MAX_WINDOWS: usize = 50;
const MAX_TABS: usize = 500;
const MAX_URL: usize = 8 * 1024;
const MAX_FILE: u64 = 8 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetTab {
    pub url: String,
    #[serde(default)]
    pub title: String,
    /// A persistent container's id; the default profile when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// The id of its group in the window's `groups`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u64>,
}

/// The split view: two tabs by their position in `tabs`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetSplit {
    pub left: usize,
    pub right: usize,
    #[serde(default = "half")]
    pub ratio: f32,
}

fn half() -> f32 {
    0.5
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PresetWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The tab shown first (position in `tabs`).
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub tabs: Vec<PresetTab>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<TabGroup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<PresetSplit>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: u64,
    pub name: String,
    /// Unix seconds.
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub updated: u64,
    #[serde(default)]
    pub windows: Vec<PresetWindow>,
}

impl Preset {
    pub fn tab_count(&self) -> usize {
        self.windows.iter().map(|w| w.tabs.len()).sum()
    }

    /// "2 windows, 9 tabs" / "4 tabs".
    pub fn summary(&self) -> String {
        let tabs = self.tab_count();
        let t = format!("{tabs} tab{}", if tabs == 1 { "" } else { "s" });
        match self.windows.len() {
            0 | 1 => t,
            n => format!("{n} windows, {t}"),
        }
    }

    /// Whether a tab shows a local file (`file:`).
    pub fn has_file_tabs(&self) -> bool {
        self.windows.iter().flat_map(|w| &w.tabs).any(|t| t.url.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("file:")))
    }

    /// How many tabs name a container that `exists` does not know.
    pub fn missing_containers(&self, exists: &dyn Fn(&str) -> bool) -> usize {
        self.windows.iter().flat_map(|w| &w.tabs).filter(|t| t.container.as_deref().is_some_and(|c| !exists(c))).count()
    }
}

/// What `presets.json` holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default = "version_one")]
    pub version: u32,
    #[serde(default)]
    pub next_id: u64,
    /// In the user's order.
    #[serde(default)]
    pub presets: Vec<Preset>,
    /// The preset opened at start-up ("On startup: Open a preset").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_preset: Option<u64>,
    /// Read from a file written by a newer BrowseCraft: never saved.
    #[serde(skip)]
    pub read_only: bool,
}

fn version_one() -> u32 {
    1
}

impl Default for Store {
    fn default() -> Self {
        Store { version: VERSION, next_id: 1, presets: Vec::new(), startup_preset: None, read_only: false }
    }
}

/// A preset name as typed: one line, trimmed, at most `MAX_NAME` characters.
pub fn clean_name(name: &str) -> String {
    let one_line: String = name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let words = one_line.split_whitespace().collect::<Vec<_>>().join(" ");
    words.chars().take(MAX_NAME).collect::<String>().trim().to_string()
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn allowed_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    url.len() <= MAX_URL
        && (lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("file:")
            || lower.starts_with("about:home")
            || lower.starts_with("browsecraft://"))
}

impl PresetWindow {
    /// A window snapshot from a session window entry (`Browser::session_value`).
    /// Tabs of temporary containers and addresses a tab may not open are
    /// left out; the split view and the active tab follow the tabs kept.
    pub fn from_session(w: &Value) -> PresetWindow {
        let mut tabs = Vec::new();
        // Position in the entry -> position kept.
        let mut kept: Vec<Option<usize>> = Vec::new();
        for t in w.get("tabs").and_then(Value::as_array).into_iter().flatten().take(MAX_TABS) {
            let url = t.get("url").and_then(Value::as_str).unwrap_or("");
            let container = t.get("container").and_then(Value::as_str).map(str::to_string);
            if url.is_empty() || !allowed_url(url) || container.as_deref().is_some_and(crate::containers::is_temporary_id) {
                kept.push(None);
                continue;
            }
            kept.push(Some(tabs.len()));
            tabs.push(PresetTab {
                url: url.to_string(),
                title: t.get("title").and_then(Value::as_str).unwrap_or("").chars().take(300).collect(),
                container,
                group: t.get("group").and_then(Value::as_u64),
            });
        }
        let map = |raw: Option<u64>| raw.and_then(|r| kept.get(r as usize).copied().flatten());
        let active = map(w.get("active").and_then(Value::as_u64)).unwrap_or(0);
        let used: std::collections::HashSet<u64> = tabs.iter().filter_map(|t| t.group).collect();
        let groups: Vec<TabGroup> = w
            .get("groups")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(crate::tab_groups::MAX_GROUPS)
            .filter_map(|g| serde_json::from_value::<TabGroup>(g.clone()).ok())
            .filter(|g| used.contains(&g.id))
            .collect();
        let split = w.get("split").and_then(|s| {
            let left = map(s.get("left").and_then(Value::as_u64))?;
            let right = map(s.get("right").and_then(Value::as_u64))?;
            let ratio = s.get("ratio").and_then(Value::as_f64).unwrap_or(0.5) as f32;
            (left != right).then_some(PresetSplit { left, right, ratio: crate::split::clamp_ratio(ratio) })
        });
        let name = w.get("name").and_then(Value::as_str).map(clean_name).filter(|n| !n.is_empty());
        PresetWindow { name, active, tabs, groups, split }
    }

    /// The session window entry for this snapshot (for session restore).
    pub fn to_session(&self) -> Value {
        let mut v = json!({ "active": self.active, "tabs": self.tabs });
        if !self.groups.is_empty() {
            v["groups"] = json!(self.groups);
        }
        if let Some(s) = self.split {
            v["split"] = json!(s);
        }
        if let Some(n) = &self.name {
            v["name"] = json!(n);
        }
        v
    }

    /// Drops what a hand-edited or imported file may get wrong.
    fn sanitize(&mut self) {
        self.tabs.retain(|t| allowed_url(&t.url) && !t.container.as_deref().is_some_and(crate::containers::is_temporary_id));
        self.tabs.truncate(MAX_TABS);
        self.groups.truncate(crate::tab_groups::MAX_GROUPS);
        let n = self.tabs.len();
        if self.active >= n {
            self.active = 0;
        }
        self.split = self.split.filter(|s| s.left < n && s.right < n && s.left != s.right);
        self.name = self.name.as_deref().map(clean_name).filter(|n| !n.is_empty());
    }
}

/// Windows captured from session entries: the empty ones are left out.
pub fn capture(session_windows: &[Value]) -> Vec<PresetWindow> {
    session_windows.iter().map(PresetWindow::from_session).filter(|w| !w.tabs.is_empty()).take(MAX_WINDOWS).collect()
}

impl Store {
    pub fn parse(text: &str) -> Result<Store, String> {
        let mut store: Store = serde_json::from_str(text).map_err(|e| e.to_string())?;
        store.read_only = store.version > VERSION;
        store.presets.truncate(MAX_PRESETS);
        let mut max = 0;
        for p in &mut store.presets {
            p.name = clean_name(&p.name);
            p.windows.truncate(MAX_WINDOWS);
            p.windows.iter_mut().for_each(PresetWindow::sanitize);
            max = max.max(p.id);
        }
        store.next_id = store.next_id.max(max.saturating_add(1));
        if store.startup_preset.is_some_and(|id| store.get(id).is_none()) && !store.read_only {
            store.startup_preset = None;
        }
        Ok(store)
    }

    pub fn to_json(&self) -> String {
        let mut out = self.clone();
        out.version = out.version.max(VERSION);
        serde_json::to_string_pretty(&out).unwrap_or_default()
    }

    pub fn get(&self, id: u64) -> Option<&Preset> {
        self.presets.iter().find(|p| p.id == id)
    }

    /// The preset named `name` (case-insensitive).
    pub fn find(&self, name: &str) -> Option<&Preset> {
        let name = clean_name(name).to_lowercase();
        self.presets.iter().find(|p| p.name.to_lowercase() == name)
    }

    fn check_writable(&self) -> Result<(), String> {
        if self.read_only {
            return Err("presets.json was written by a newer BrowseCraft: presets cannot be changed here".into());
        }
        Ok(())
    }

    fn valid_new_name(&self, name: &str, except: Option<u64>) -> Result<String, String> {
        let name = clean_name(name);
        if name.is_empty() {
            return Err("Give the preset a name".into());
        }
        if self.find(&name).is_some_and(|p| Some(p.id) != except) {
            return Err(format!("A preset named \u{201c}{name}\u{201d} already exists"));
        }
        Ok(name)
    }

    /// Adds a preset at the end; returns its id.
    pub fn add(&mut self, name: &str, windows: Vec<PresetWindow>, at: u64) -> Result<u64, String> {
        self.check_writable()?;
        let name = self.valid_new_name(name, None)?;
        if windows.iter().all(|w| w.tabs.is_empty()) {
            return Err("There are no tabs to save (tabs in temporary containers are left out)".into());
        }
        if self.presets.len() >= MAX_PRESETS {
            return Err(format!("At most {MAX_PRESETS} presets"));
        }
        let id = self.next_id.max(1);
        // A hand-edited file may hold any id: never wrap to reuse one.
        self.next_id = id.checked_add(1).ok_or("No preset ids are left")?;
        self.presets.push(Preset { id, name, created: at, updated: at, windows });
        Ok(id)
    }

    /// Replaces a preset's windows with `windows` (its name stays).
    pub fn update(&mut self, id: u64, windows: Vec<PresetWindow>, at: u64) -> Result<(), String> {
        self.check_writable()?;
        if windows.iter().all(|w| w.tabs.is_empty()) {
            return Err("There are no tabs to save".into());
        }
        let p = self.presets.iter_mut().find(|p| p.id == id).ok_or("No such preset")?;
        p.windows = windows;
        p.updated = at;
        Ok(())
    }

    pub fn rename(&mut self, id: u64, name: &str, at: u64) -> Result<(), String> {
        self.check_writable()?;
        let name = self.valid_new_name(name, Some(id))?;
        let p = self.presets.iter_mut().find(|p| p.id == id).ok_or("No such preset")?;
        p.name = name;
        p.updated = at;
        Ok(())
    }

    pub fn delete(&mut self, id: u64) -> Result<String, String> {
        self.check_writable()?;
        let i = self.presets.iter().position(|p| p.id == id).ok_or("No such preset")?;
        if self.startup_preset == Some(id) {
            self.startup_preset = None;
        }
        Ok(self.presets.remove(i).name)
    }

    /// Moves a preset one place up (`-1`) or down (`1`).
    pub fn shift(&mut self, id: u64, delta: isize) -> Result<(), String> {
        self.check_writable()?;
        let i = self.presets.iter().position(|p| p.id == id).ok_or("No such preset")?;
        let j = i as isize + delta;
        if j < 0 || j as usize >= self.presets.len() {
            return Ok(());
        }
        self.presets.swap(i, j as usize);
        Ok(())
    }

    pub fn set_startup(&mut self, id: Option<u64>) -> Result<(), String> {
        self.check_writable()?;
        if id.is_some_and(|id| self.get(id).is_none()) {
            return Err("No such preset".into());
        }
        self.startup_preset = id;
        Ok(())
    }

    /// The export file: every preset, in this file's own format.
    pub fn export(&self) -> String {
        Store { startup_preset: None, ..self.clone() }.to_json()
    }

    /// Adds the presets of an exported file (or of a single preset). Names
    /// already taken get " (2)", " (3)"…; ids are new. Returns how many.
    pub fn import(&mut self, text: &str, at: u64) -> Result<usize, String> {
        self.check_writable()?;
        let value: Value = serde_json::from_str(text).map_err(|e| format!("Not a presets file: {e}"))?;
        let incoming: Vec<Preset> = if value.get("presets").is_some() {
            let s = Store::parse(text).map_err(|e| format!("Not a presets file: {e}"))?;
            if s.read_only {
                return Err("This file was written by a newer BrowseCraft".into());
            }
            s.presets
        } else {
            let mut p: Preset = serde_json::from_value(value).map_err(|e| format!("Not a presets file: {e}"))?;
            p.windows.iter_mut().for_each(PresetWindow::sanitize);
            vec![p]
        };
        let mut added = 0;
        for p in incoming {
            let base = clean_name(&p.name);
            let base = if base.is_empty() { "Imported".to_string() } else { base };
            let mut name = base.clone();
            let mut n = 2;
            while self.find(&name).is_some() {
                let suffix = format!(" ({n})");
                name = format!("{}{suffix}", base.chars().take(MAX_NAME - suffix.chars().count()).collect::<String>());
                n += 1;
            }
            if self.add(&name, p.windows, at).is_ok() {
                if let Some(q) = self.presets.last_mut() {
                    q.created = if p.created > 0 { p.created } else { at };
                }
                added += 1;
            }
        }
        if added == 0 {
            return Err("No presets with tabs in this file".into());
        }
        Ok(added)
    }
}

// ---- the store ----

static STORE: RwLock<Option<Store>> = RwLock::new(None);

fn file_path() -> Result<std::path::PathBuf, String> {
    crate::storage::config_file(FILE_NAME)
}

fn read_file() -> Store {
    let Ok(path) = file_path() else { return Store::default() };
    let Ok(text) = crate::storage::read_text_limited(&path, MAX_FILE) else { return Store::default() };
    match Store::parse(&text) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("browsecraft: ignoring {}: {e}", path.display());
            // Never overwrite a file that could not be read.
            Store { read_only: true, ..Store::default() }
        }
    }
}

/// A copy of the presets (read from the file on first use).
pub fn store() -> Store {
    if let Some(s) = STORE.read().unwrap_or_else(|p| p.into_inner()).as_ref() {
        return s.clone();
    }
    let mut guard = STORE.write().unwrap_or_else(|p| p.into_inner());
    guard.get_or_insert_with(read_file).clone()
}

/// The presets, in order.
pub fn all() -> Vec<Preset> {
    store().presets
}

/// Each preset's id, name and summary ("2 windows, 9 tabs"), in order:
/// for menus and the palette, which are drawn every frame (no tab lists
/// are copied).
pub fn summaries() -> Vec<(u64, String, String)> {
    let list = |s: &Store| s.presets.iter().map(|p| (p.id, p.name.clone(), p.summary())).collect();
    if let Some(s) = STORE.read().unwrap_or_else(|p| p.into_inner()).as_ref() {
        return list(s);
    }
    list(&store())
}

pub fn get(id: u64) -> Option<Preset> {
    store().get(id).cloned()
}

pub fn find(name: &str) -> Option<Preset> {
    store().find(name).cloned()
}

/// Applies `change` to a copy, saves it, and only then makes it current.
pub fn modify<T>(change: impl FnOnce(&mut Store) -> Result<T, String>) -> Result<T, String> {
    let mut guard = STORE.write().unwrap_or_else(|p| p.into_inner());
    let current = guard.get_or_insert_with(read_file);
    let mut next = current.clone();
    let out = change(&mut next)?;
    next.check_writable()?;
    let path = file_path()?;
    crate::storage::write_private(&path, next.to_json().as_bytes()).map_err(|e| format!("Could not save {}: {e}", path.display()))?;
    *current = next;
    Ok(out)
}

/// Where a preset opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenTarget {
    /// Each of its windows as a new window.
    NewWindow,
    /// Its tabs appended to this window (groups and the split view too).
    Here,
    /// Its tabs in place of this window's (asked for confirmation first).
    Replace,
}

/// How a window's tabs change when presets windows open in it
/// ([`OpenTarget::Here`] or [`OpenTarget::Replace`]): the tabs to add at
/// the end, their groups and split view, by position in `tabs`.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenPlan {
    pub tabs: Vec<PresetTab>,
    /// Each group with the positions of its tabs (groups of different
    /// windows stay apart even when their saved ids are equal).
    pub groups: Vec<(TabGroup, Vec<usize>)>,
    pub split: Option<PresetSplit>,
    /// The tab to show.
    pub active: Option<usize>,
    /// Close the window's own tabs afterwards.
    pub close_existing: bool,
    /// A name for the window (replacing only).
    pub name: Option<String>,
    /// Tabs whose container is gone (they open without one).
    pub missing_containers: usize,
}

/// Plans opening `windows` in a window that has `existing` tabs and
/// (`has_split`) a split view. Every window's tabs are appended in order;
/// the first split view that fits is recreated (a window has one, so not
/// when appending to a window that has one already); containers that
/// `exists` does not know are dropped. At most `MAX_TABS` tabs in all.
pub fn plan_open(windows: &[PresetWindow], replace: bool, existing: usize, has_split: bool, exists: &dyn Fn(&str) -> bool) -> OpenPlan {
    let room = if replace { MAX_TABS } else { MAX_TABS.saturating_sub(existing) };
    let mut plan = OpenPlan {
        tabs: Vec::new(),
        groups: Vec::new(),
        split: None,
        active: None,
        close_existing: replace,
        name: if replace { windows.first().and_then(|w| w.name.clone()) } else { None },
        missing_containers: 0,
    };
    for w in windows {
        let base = plan.tabs.len();
        let take = w.tabs.len().min(room - base.min(room));
        for t in &w.tabs[..take] {
            let mut t = t.clone();
            if t.container.as_deref().is_some_and(|c| !exists(c) || crate::containers::is_temporary_id(c)) {
                t.container = None;
                plan.missing_containers += 1;
            }
            t.group = None;
            plan.tabs.push(t);
        }
        for g in &w.groups {
            let members: Vec<usize> = w.tabs[..take].iter().enumerate().filter(|(_, t)| t.group == Some(g.id)).map(|(i, _)| base + i).collect();
            if !members.is_empty() {
                plan.groups.push((g.clone(), members));
            }
        }
        if plan.split.is_none()
            && (replace || !has_split)
            && let Some(s) = w.split.filter(|s| s.left < take && s.right < take)
        {
            plan.split = Some(PresetSplit { left: base + s.left, right: base + s.right, ratio: s.ratio });
        }
        if plan.active.is_none() && take > 0 {
            plan.active = Some(base + w.active.min(take - 1));
        }
    }
    if plan.tabs.is_empty() {
        plan.close_existing = false;
        plan.name = None;
    }
    plan
}

// ---- start-up ----

/// What the browser opens at start-up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupPlan {
    /// The addresses given on the command line.
    CommandLine,
    /// The previous session (when tabs are remembered).
    Session,
    /// A new tab.
    NewTab,
    /// This preset.
    Preset(u64),
    /// The chooser: previous session, a preset, or a blank page.
    Ask,
}

/// Decides what to open at start-up, with a notice when the choice could
/// not be honoured. Addresses on the command line come first, then
/// `--preset`, then the "On startup" setting. A preset that is gone falls
/// back to the previous session.
pub fn startup_plan(cli_tabs: bool, cli_preset: Option<&str>, mode: Startup, store: &Store) -> (StartupPlan, Option<String>) {
    if cli_tabs {
        return (StartupPlan::CommandLine, None);
    }
    if let Some(name) = cli_preset {
        return match store.find(name) {
            Some(p) => (StartupPlan::Preset(p.id), None),
            None => (StartupPlan::Session, Some(format!("No preset named \u{201c}{}\u{201d}: the previous session was restored", clean_name(name)))),
        };
    }
    match mode {
        Startup::Continue => (StartupPlan::Session, None),
        Startup::NewTab => (StartupPlan::NewTab, None),
        Startup::Ask => (StartupPlan::Ask, None),
        Startup::Preset => match store.startup_preset.and_then(|id| store.get(id)) {
            Some(p) => (StartupPlan::Preset(p.id), None),
            None => (StartupPlan::Session, Some("The start-up preset no longer exists: the previous session was restored".into())),
        },
    }
}

/// Takes `--preset NAME` (or `--preset=NAME`) out of the command line.
pub fn take_cli_preset(args: &mut Vec<String>) -> Result<Option<String>, String> {
    let mut found = None;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--preset" {
            let Some(name) = args.get(i + 1).cloned() else {
                return Err("--preset needs a preset name".into());
            };
            args.drain(i..i + 2);
            found = Some(name);
        } else if let Some(name) = args[i].strip_prefix("--preset=") {
            found = Some(name.to_string());
            args.remove(i);
        } else {
            i += 1;
        }
    }
    match found {
        Some(n) if clean_name(&n).is_empty() => Err("--preset needs a preset name".into()),
        other => Ok(other),
    }
}

/// The file the "Ask every time" chooser restores the previous session from.
pub const PREVIOUS_SESSION_FILE: &str = "session-previous.json";

#[cfg(test)]
mod tests {
    use super::*;

    fn window(urls: &[&str]) -> PresetWindow {
        PresetWindow {
            tabs: urls.iter().map(|u| PresetTab { url: u.to_string(), title: String::new(), container: None, group: None }).collect(),
            ..PresetWindow::default()
        }
    }

    #[test]
    fn presets_names_are_cleaned_and_unique() {
        assert_eq!(clean_name("  Lavoro\n  mattina "), "Lavoro mattina");
        assert_eq!(clean_name(&"x".repeat(200)).chars().count(), MAX_NAME);
        let mut s = Store::default();
        let id = s.add(" Lavoro ", vec![window(&["https://a.test/"])], 10).unwrap();
        assert_eq!(s.get(id).unwrap().name, "Lavoro");
        assert!(s.add("lavoro", vec![window(&["https://b.test/"])], 11).is_err(), "case-insensitive");
        assert!(s.add("   ", vec![window(&["https://b.test/"])], 11).is_err());
        assert!(s.add("Empty", vec![window(&[])], 11).is_err(), "no tabs");
        assert_eq!(s.find("LAVORO").map(|p| p.id), Some(id));
    }

    #[test]
    fn presets_store_round_trips_and_versions() {
        let mut s = Store::default();
        let a = s.add("A", vec![window(&["https://a.test/", "about:home"])], 100).unwrap();
        let b = s.add("B", vec![window(&["https://b.test/"]), window(&["https://c.test/"])], 200).unwrap();
        s.set_startup(Some(b)).unwrap();
        let text = s.to_json();
        assert!(text.contains("\"version\": 1"));
        let back = Store::parse(&text).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.get(b).unwrap().summary(), "2 windows, 2 tabs");
        assert_eq!(back.get(a).unwrap().summary(), "2 tabs");
        // Missing fields take their defaults; bad entries are dropped.
        let old = r#"{"presets":[{"id":7,"name":"Old","windows":[{"tabs":[{"url":"https://x.test/"},{"url":"javascript:alert(1)"}],"split":{"left":0,"right":1}}]}]}"#;
        let s = Store::parse(old).unwrap();
        assert_eq!((s.version, s.next_id), (1, 8));
        assert_eq!(s.presets[0].windows[0].tabs.len(), 1);
        assert_eq!(s.presets[0].windows[0].split, None, "the split named a dropped tab");
        // A newer file is read but not changed.
        let mut newer = Store::parse(r#"{"version":9,"presets":[{"id":1,"name":"N","windows":[]}],"future":true}"#).unwrap();
        assert!(newer.read_only);
        assert!(newer.add("X", vec![window(&["https://a.test/"])], 1).is_err());
    }

    #[test]
    fn presets_rename_delete_reorder() {
        let mut s = Store::default();
        let a = s.add("A", vec![window(&["https://a.test/"])], 1).unwrap();
        let b = s.add("B", vec![window(&["https://b.test/"])], 1).unwrap();
        assert!(s.rename(a, "B", 2).is_err());
        s.rename(a, "Alpha", 2).unwrap();
        assert_eq!((s.get(a).unwrap().name.as_str(), s.get(a).unwrap().updated), ("Alpha", 2));
        s.rename(a, "alpha", 3).unwrap(); // its own name, other case
        s.shift(b, -1).unwrap();
        assert_eq!(s.presets.iter().map(|p| p.id).collect::<Vec<_>>(), [b, a]);
        s.shift(b, -1).unwrap();
        assert_eq!(s.presets[0].id, b, "already first");
        s.set_startup(Some(a)).unwrap();
        assert_eq!(s.delete(a).unwrap(), "alpha");
        assert_eq!(s.startup_preset, None, "the start-up preset goes with it");
        s.update(b, vec![window(&["https://new.test/"])], 9).unwrap();
        assert_eq!(s.get(b).unwrap().windows[0].tabs[0].url, "https://new.test/");
        assert_eq!(s.get(b).unwrap().updated, 9);
    }

    #[test]
    fn presets_export_import() {
        let mut s = Store::default();
        s.add("Lavoro", vec![window(&["https://a.test/"])], 1).unwrap();
        let text = s.export();
        assert_eq!(s.import(&text, 5).unwrap(), 1);
        assert_eq!(s.presets[1].name, "Lavoro (2)");
        assert_eq!(s.import(&text, 5).unwrap(), 1);
        assert_eq!(s.presets[2].name, "Lavoro (3)");
        let single = serde_json::to_string(&s.presets[0]).unwrap();
        assert_eq!(s.import(&single, 6).unwrap(), 1);
        assert!(s.import("not json", 1).is_err());
        assert!(s.import(r#"{"presets":[]}"#, 1).is_err());
        assert_ne!(s.presets[0].id, s.presets[1].id);
    }

    #[test]
    fn presets_capture_maps_groups_split_and_skips_temporary() {
        let session = json!({
            "active": 3,
            "name": "Work",
            "bounds": {"x": 1, "y": 2, "w": 800, "h": 600},
            "tabs": [
                {"url": "https://a.test/", "title": "A", "container": null, "group": 5},
                {"url": "https://tmp.test/", "title": "T", "container": "tmp-1"},
                {"url": "https://b.test/", "title": "B", "container": "work", "group": 5},
                {"url": "https://c.test/", "title": "C"}
            ],
            "groups": [{"id": 5, "name": "Mail", "color": "blue", "collapsed": true}, {"id": 6, "name": "Gone"}],
            "split": {"left": 2, "right": 3, "ratio": 0.3}
        });
        let w = PresetWindow::from_session(&session);
        assert_eq!(w.name.as_deref(), Some("Work"));
        assert_eq!(w.tabs.iter().map(|t| t.url.as_str()).collect::<Vec<_>>(), ["https://a.test/", "https://b.test/", "https://c.test/"]);
        assert_eq!(w.active, 2, "follows the tabs kept");
        assert_eq!(w.tabs[1].container.as_deref(), Some("work"));
        assert_eq!(w.groups.len(), 1, "only groups with tabs");
        assert_eq!((w.groups[0].name.as_str(), w.groups[0].color, w.groups[0].collapsed), ("Mail", crate::tab_groups::GroupColor::Blue, true));
        assert_eq!(w.split, Some(PresetSplit { left: 1, right: 2, ratio: 0.3 }));
        // A split with a temporary tab is dropped.
        let w2 = PresetWindow::from_session(&json!({"tabs": [{"url": "https://a.test/"}, {"url": "https://b.test/", "container": "tmp-x"}], "split": {"left": 0, "right": 1}}));
        assert_eq!(w2.split, None);
        // Back to a session entry: no bounds, same layout.
        let v = w.to_session();
        assert!(v.get("bounds").is_none());
        assert_eq!(v["split"]["left"], 1);
        assert_eq!(v["tabs"][0]["group"], 5);
        assert_eq!(PresetWindow::from_session(&v), w);
        // Only windows with tabs are captured.
        assert_eq!(capture(&[session, json!({"tabs": []})]).len(), 1);
    }

    #[test]
    fn presets_huge_ids_do_not_overflow() {
        let mut s = Store::parse(&format!(r#"{{"presets":[{{"id":{},"name":"Max","windows":[]}}]}}"#, u64::MAX)).unwrap();
        assert_eq!(s.next_id, u64::MAX);
        assert!(s.add("New", vec![window(&["https://a.test/"])], 1).is_err(), "no id is reused");
        assert_eq!(s.presets.len(), 1);
    }

    #[test]
    fn presets_know_their_file_tabs() {
        let mut s = Store::default();
        let id = s.add("Web", vec![window(&["https://a.test/"])], 1).unwrap();
        assert!(!s.get(id).unwrap().has_file_tabs());
        let id = s.add("Local", vec![window(&["https://a.test/"]), window(&["FILE:///home/a/notes.html"])], 1).unwrap();
        assert!(s.get(id).unwrap().has_file_tabs());
    }

    #[test]
    fn presets_missing_containers_are_counted() {
        let mut w = window(&["https://a.test/", "https://b.test/"]);
        w.tabs[1].container = Some("gone".into());
        let p = Preset { id: 1, name: "P".into(), created: 0, updated: 0, windows: vec![w] };
        assert_eq!(p.missing_containers(&|c| c == "work"), 1);
        assert_eq!(p.missing_containers(&|_| true), 0);
    }

    #[test]
    fn presets_open_appends_or_replaces() {
        let mut a = window(&["https://a.test/", "https://b.test/", "https://c.test/"]);
        a.name = Some("Work".into());
        a.active = 2;
        a.groups = vec![TabGroup { id: 1, name: "Mail".into(), color: crate::tab_groups::GroupColor::Blue, collapsed: false }];
        a.tabs[0].group = Some(1);
        a.tabs[1].group = Some(1);
        a.tabs[2].container = Some("gone".into());
        a.split = Some(PresetSplit { left: 0, right: 2, ratio: 0.4 });
        let mut b = window(&["https://d.test/"]);
        b.groups = vec![TabGroup { id: 1, name: "Other".into(), color: crate::tab_groups::GroupColor::Red, collapsed: true }];
        b.tabs[0].group = Some(1);
        b.split = None;
        let exists = |c: &str| c == "work";

        // Appending: every window's tabs at the end, groups kept apart.
        let plan = plan_open(&[a.clone(), b.clone()], false, 4, false, &exists);
        assert_eq!(plan.tabs.len(), 4);
        assert!(!plan.close_existing && plan.name.is_none());
        assert_eq!(plan.groups.iter().map(|(g, m)| (g.name.as_str(), m.clone())).collect::<Vec<_>>(), [("Mail", vec![0, 1]), ("Other", vec![3])]);
        assert_eq!(plan.split, Some(PresetSplit { left: 0, right: 2, ratio: 0.4 }));
        assert_eq!(plan.active, Some(2));
        assert_eq!((plan.missing_containers, plan.tabs[2].container.clone()), (1, None));
        // The window has a split view already: it stays, none is added.
        assert_eq!(plan_open(&[a.clone()], false, 4, true, &exists).split, None);

        // Replacing: the window's tabs go, its name and split come from the preset.
        let plan = plan_open(&[a.clone()], true, 4, true, &exists);
        assert!(plan.close_existing);
        assert_eq!(plan.name.as_deref(), Some("Work"));
        assert!(plan.split.is_some());

        // A second window's split is used when the first has none.
        b.tabs.push(PresetTab { url: "https://e.test/".into(), title: String::new(), container: None, group: None });
        b.split = Some(PresetSplit { left: 0, right: 1, ratio: 0.5 });
        let mut c = a.clone();
        c.split = None;
        assert_eq!(plan_open(&[c, b], false, 0, false, &exists).split.map(|s| (s.left, s.right)), Some((3, 4)));

        // Nothing to open: nothing is closed.
        let plan = plan_open(&[window(&[])], true, 3, false, &exists);
        assert!(plan.tabs.is_empty() && !plan.close_existing && plan.active.is_none());
        // The tab cap counts the window's tabs when appending.
        let big = window(&vec!["https://x.test/"; 20]);
        assert_eq!(plan_open(std::slice::from_ref(&big), false, MAX_TABS - 5, false, &exists).tabs.len(), 5);
        assert_eq!(plan_open(&[big], true, MAX_TABS - 5, false, &exists).tabs.len(), 20);
    }

    #[test]
    fn presets_startup_decision() {
        let mut s = Store::default();
        let id = s.add("Lavoro", vec![window(&["https://a.test/"])], 1).unwrap();
        assert_eq!(startup_plan(true, Some("Lavoro"), Startup::Ask, &s).0, StartupPlan::CommandLine, "addresses win");
        assert_eq!(startup_plan(false, Some("lavoro"), Startup::NewTab, &s), (StartupPlan::Preset(id), None));
        let (plan, notice) = startup_plan(false, Some("Nope"), Startup::NewTab, &s);
        assert_eq!(plan, StartupPlan::Session);
        assert!(notice.unwrap().contains("Nope"));
        assert_eq!(startup_plan(false, None, Startup::Continue, &s).0, StartupPlan::Session);
        assert_eq!(startup_plan(false, None, Startup::NewTab, &s).0, StartupPlan::NewTab);
        assert_eq!(startup_plan(false, None, Startup::Ask, &s).0, StartupPlan::Ask);
        let (plan, notice) = startup_plan(false, None, Startup::Preset, &s);
        assert_eq!((plan, notice.is_some()), (StartupPlan::Session, true), "no start-up preset chosen");
        s.set_startup(Some(id)).unwrap();
        assert_eq!(startup_plan(false, None, Startup::Preset, &s), (StartupPlan::Preset(id), None));
    }

    #[test]
    fn presets_cli_argument() {
        let mut args: Vec<String> = ["browsecraft", "--preset", "Lavoro", "https://a.test/"].iter().map(|s| s.to_string()).collect();
        assert_eq!(take_cli_preset(&mut args).unwrap().as_deref(), Some("Lavoro"));
        assert_eq!(args, ["browsecraft", "https://a.test/"]);
        let mut args: Vec<String> = ["browsecraft", "--preset=Deep work"].iter().map(|s| s.to_string()).collect();
        assert_eq!(take_cli_preset(&mut args).unwrap().as_deref(), Some("Deep work"));
        assert_eq!(args.len(), 1);
        let mut args: Vec<String> = ["browsecraft", "--preset"].iter().map(|s| s.to_string()).collect();
        assert!(take_cli_preset(&mut args).is_err());
        let mut args: Vec<String> = vec!["browsecraft".into()];
        assert_eq!(take_cli_preset(&mut args).unwrap(), None);
    }
}
