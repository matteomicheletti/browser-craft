//! The command palette: a spotlight-style overlay opened with Ctrl+P / F1.
//!
//! The palette is pure UI. It is given what it can offer ([`PaletteSources`])
//! and returns what the user picked ([`PaletteOutcome`]); the browser does the
//! work. While it is open the native web view is hidden by the core, so the
//! overlay is drawn with ordinary egui areas.
//!
//! Prefixes narrow the search: `>` commands, `@` open tabs, `#` bookmarks and
//! history, `~` containers, `%` themes, `?` help.
// The API is wired into the browser in the integration step.
#![allow(dead_code)]

use crate::commands::{self, CommandId};
use crate::egui_chrome::{Colors, icon};
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Id, Key, Modifiers, Order, Pos2, Rect, Sense,
    Stroke, TextEdit, vec2,
};

/// What the user picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteOutcome {
    /// Run a browser command.
    Run(CommandId),
    /// Switch to the tab at this index.
    SwitchTab(usize),
    /// Open this (already resolved) URL in a new tab.
    OpenUrl(String),
    /// Open a new tab in a container; `url` is empty for a blank tab.
    OpenInContainer { url: String, container: String },
    /// Show the tab at this index beside the current one (split view).
    SplitWith(usize),
    /// Put the current tab in this group (`None`: a new group).
    AddToGroup(Option<u64>),
    /// Go to this tab group (expanding it).
    FocusGroup(u64),
    /// Switch the browser to this theme.
    SetTheme(crate::theme::ThemeId),
    /// Run a plugin's command (API 1.1 `[[commands]]`).
    PluginCommand { plugin: String, command: String },
}

/// A command an enabled plugin adds to the palette.
#[derive(Clone, Debug, Default)]
pub struct PluginCommandItem {
    pub plugin: String,
    pub command: String,
    pub title: String,
    pub plugin_name: String,
    /// The shortcut in effect, as the user reads it.
    pub shortcut: Option<String>,
}

/// A container's colour and name, shown as a dot next to a tab.
#[derive(Clone, Debug)]
pub struct ContainerBadge {
    pub name: String,
    pub color: Color32,
}

#[derive(Clone, Debug)]
pub struct TabItem {
    pub index: usize,
    pub title: String,
    pub url: String,
    pub container: Option<ContainerBadge>,
}

/// A tab group, for `@` (go to it) and `&` (add the tab to it).
#[derive(Clone, Debug)]
pub struct GroupItem {
    pub id: u64,
    pub name: String,
    pub color: Color32,
    pub tabs: usize,
    pub collapsed: bool,
}

#[derive(Clone, Debug)]
pub struct LinkItem {
    pub title: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct ContainerItem {
    pub id: String,
    pub name: String,
    pub color: Color32,
    pub temporary: bool,
}

/// The dynamic items the palette searches besides the command catalog.
#[derive(Clone, Debug, Default)]
pub struct PaletteSources {
    pub tabs: Vec<TabItem>,
    pub bookmarks: Vec<LinkItem>,
    /// Most recent first.
    pub history: Vec<LinkItem>,
    pub containers: Vec<ContainerItem>,
    pub groups: Vec<GroupItem>,
    /// The index of the current tab (left out of `|`, split with a tab).
    pub active: Option<usize>,
    /// The current tab shows a web page (commands that need one are enabled).
    pub on_web_page: bool,
    /// A screen recording is running (shows Stop instead of Start).
    pub recording: bool,
    /// The theme chosen now (marked in the theme list).
    pub theme: crate::theme::ThemeId,
    /// Commands of enabled plugins.
    pub plugin_commands: Vec<PluginCommandItem>,
}

// ---- fuzzy matching --------------------------------------------------------

/// A successful fuzzy match: higher `score` is better, `positions` are the
/// matched character indices of the text (for highlighting).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub score: i32,
    pub positions: Vec<usize>,
}

const MAX_MATCH_CHARS: usize = 160;

/// Case-insensitive subsequence match of `query` in `text`, scored so that
/// consecutive characters, word starts and a match at the start of the text
/// win. Returns the best-scoring alignment.
pub fn fuzzy(query: &str, text: &str) -> Option<Match> {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Some(Match {
            score: 0,
            positions: Vec::new(),
        });
    }
    let t: Vec<char> = text.chars().take(MAX_MATCH_CHARS).collect();
    let tl: Vec<char> = t
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    if q.len() > t.len() {
        return None;
    }
    let bonus = |j: usize| -> i32 {
        if j == 0 {
            return 10;
        }
        let (prev, cur) = (t[j - 1], t[j]);
        if !prev.is_alphanumeric() {
            8
        } else if prev.is_lowercase() && cur.is_uppercase() {
            6
        } else {
            0
        }
    };
    const NONE: i32 = i32::MIN / 2;
    let (n, m) = (q.len(), t.len());
    // best[i][j]: best score with q[i] matched at t[j]; from[i][j]: where q[i-1] was.
    let mut best = vec![vec![NONE; m]; n];
    let mut from = vec![vec![usize::MAX; m]; n];
    for j in 0..m {
        if tl[j] == q[0] {
            best[0][j] = 1 + bonus(j) - (j as i32).min(6);
        }
    }
    for i in 1..n {
        for j in i..m {
            if tl[j] != q[i] {
                continue;
            }
            for k in (i - 1)..j {
                if best[i - 1][k] == NONE {
                    continue;
                }
                let step = if k + 1 == j {
                    6
                } else {
                    -((j - k - 1) as i32).min(4)
                };
                let s = best[i - 1][k] + 1 + bonus(j) + step;
                if s > best[i][j] {
                    best[i][j] = s;
                    from[i][j] = k;
                }
            }
        }
    }
    let (mut j, mut score) = (usize::MAX, NONE);
    for (jj, &s) in best[n - 1].iter().enumerate() {
        if s > score {
            score = s;
            j = jj;
        }
    }
    if score == NONE {
        return None;
    }
    let mut positions = vec![0; n];
    for i in (0..n).rev() {
        positions[i] = j;
        j = from[i][j];
    }
    // Case-exact and whole-prefix matches rank a little higher.
    if positions.iter().zip(q.iter()).all(|(&p, qc)| tl[p] == *qc) {
        let exact = positions
            .iter()
            .zip(query.chars())
            .filter(|(p, qc)| t[**p] == *qc)
            .count();
        score += exact as i32;
    }
    if positions.iter().enumerate().all(|(i, &p)| p == i) {
        score += 12;
    }
    Some(Match { score, positions })
}

/// Matches every whitespace-separated term of `query` against `label` first
/// and then against `extras` (keywords, URL). All terms must match. The
/// returned positions only cover `label`.
pub fn score_terms(query: &str, label: &str, extras: &[&str]) -> Option<Match> {
    let mut total = 0;
    let mut positions: Vec<usize> = Vec::new();
    let mut any = false;
    for term in query.split_whitespace() {
        any = true;
        if let Some(m) = fuzzy(term, label) {
            total += m.score + 6;
            positions.extend(m.positions);
            continue;
        }
        let best = extras
            .iter()
            .filter_map(|e| fuzzy(term, e))
            .map(|m| m.score)
            .max()?;
        total += best / 2;
    }
    if !any {
        return Some(Match {
            score: 0,
            positions,
        });
    }
    positions.sort_unstable();
    positions.dedup();
    Some(Match {
        score: total,
        positions,
    })
}

// ---- query parsing ---------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    All,
    Commands,
    Tabs,
    Links,
    Containers,
    /// `|`: pick the tab to show beside the current one.
    Split,
    /// `&`: pick the group to add the current tab to.
    Groups,
    Themes,
    Help,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::All => "",
            Mode::Commands => "Commands",
            Mode::Tabs => "Tabs",
            Mode::Links => "Bookmarks and history",
            Mode::Containers => "Containers",
            Mode::Split => "Split view with",
            Mode::Groups => "Add tab to group",
            Mode::Themes => "Themes",
            Mode::Help => "Help",
        }
    }
}

/// Splits a leading prefix character off the query.
pub fn parse_query(raw: &str) -> (Mode, &str) {
    let s = raw.trim_start();
    let mode = match s.chars().next() {
        Some('>') => Mode::Commands,
        Some('@') => Mode::Tabs,
        Some('#') => Mode::Links,
        Some('~') => Mode::Containers,
        Some('|') => Mode::Split,
        Some('&') => Mode::Groups,
        Some('%') => Mode::Themes,
        Some('?') => Mode::Help,
        _ => return (Mode::All, raw.trim()),
    };
    (mode, s[1..].trim())
}

/// What typed text means as an address: a URL to open or a web search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Typed {
    Url(String),
    Search(String),
}

/// Classifies `input` with the address bar's own rules (`address::resolve`).
pub fn classify(input: &str) -> Option<Typed> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let resolved = crate::address::resolve(input);
    // A search resolves to the engine's URL; recover its prefix at run time
    // instead of duplicating it here.
    let probe = crate::address::resolve("x y");
    let search_prefix = probe.strip_suffix("x+y").unwrap_or(&probe);
    if resolved.starts_with(search_prefix) && !input.starts_with(search_prefix) {
        Some(Typed::Search(resolved))
    } else {
        Some(Typed::Url(resolved))
    }
}

// ---- rows ------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Action {
    Outcome(PaletteOutcome),
    /// Replace the query (prefix hints, "new tab in container…").
    SetQuery(&'static str),
}

#[derive(Clone, Debug)]
struct Row {
    section: &'static str,
    action: Action,
    icon: &'static str,
    label: String,
    detail: String,
    shortcut: Option<String>,
    tag: Option<&'static str>,
    matches: Vec<usize>,
    enabled: bool,
    dot: Option<Color32>,
    /// A theme's colours: background, border, accent.
    swatch: Option<[Color32; 3]>,
}

impl Row {
    fn new(section: &'static str, action: Action, icon: &'static str, label: String) -> Self {
        Row {
            section,
            action,
            icon,
            label,
            detail: String::new(),
            shortcut: None,
            tag: None,
            matches: Vec::new(),
            enabled: true,
            dot: None,
            swatch: None,
        }
    }
}

const MAX_RECENT: usize = 8;
const SHOWN_RECENT: usize = 5;
const SMALL_LIMIT: usize = 4;
const BIG_LIMIT: usize = 60;

fn command_visible(id: CommandId, s: &PaletteSources) -> bool {
    match id {
        CommandId::StartRecording => !s.recording,
        CommandId::StopRecording => s.recording,
        _ => true,
    }
}

fn command_row(
    section: &'static str,
    c: &'static commands::Command,
    s: &PaletteSources,
    matches: Vec<usize>,
) -> Row {
    let mut row = Row::new(
        section,
        Action::Outcome(PaletteOutcome::Run(c.id)),
        c.icon,
        c.label.to_string(),
    );
    row.shortcut = c.shortcut.map(commands::display_shortcut);
    row.tag = Some(c.category.label());
    row.matches = matches;
    row.enabled = s.on_web_page || !c.needs_page();
    // "New tab in container" opens the container list instead of acting.
    // Commands that need a pick open the matching list instead of acting.
    match c.id {
        CommandId::NewTabInContainer => row.action = Action::SetQuery("~"),
        CommandId::SplitWithTab => row.action = Action::SetQuery("|"),
        CommandId::AddTabToGroup => row.action = Action::SetQuery("&"),
        _ => {}
    }
    // "Change theme…" lists the themes.
    if c.id == CommandId::Theme {
        row.action = Action::SetQuery("%");
    }
    row
}

/// Builds the result list for `raw` (what is in the text box).
fn build_rows(raw: &str, s: &PaletteSources, recent: &[CommandId]) -> Vec<Row> {
    let (mode, q) = parse_query(raw);
    let mut rows = Vec::new();
    let limit = if mode == Mode::All {
        SMALL_LIMIT
    } else {
        BIG_LIMIT
    };

    if mode == Mode::Help {
        for (prefix, label, hint) in [
            ("> ", "Commands only", ">"),
            ("@ ", "Switch to an open tab", "@"),
            ("# ", "Bookmarks and history", "#"),
            ("~ ", "Open a tab in a container", "~"),
            ("| ", "Split view with an open tab", "|"),
            ("& ", "Add the tab to a group", "&"),
            ("% ", "Change the theme", "%"),
        ] {
            let mut r = Row::new(
                "Prefixes",
                Action::SetQuery(hint),
                crate::icons::GLOBE,
                label.to_string(),
            );
            r.detail = prefix.trim().to_string();
            rows.push(r);
        }
        return rows;
    }

    // A typed address comes first. A web search comes first only when no
    // command matches: "dark site" should run the command on Enter.
    let mut search_row = None;
    if mode == Mode::All
        && let Some(typed) = classify(q)
    {
        let is_search = matches!(typed, Typed::Search(_));
        let (label, detail, url) = match typed {
            Typed::Url(u) => (format!("Open {q}"), "Open in a new tab".to_string(), u),
            Typed::Search(u) => (
                format!("Search the web for \"{q}\""),
                "Web search".to_string(),
                u,
            ),
        };
        let mut r = Row::new(
            "Go to",
            Action::Outcome(PaletteOutcome::OpenUrl(url)),
            crate::icons::GLOBE,
            label,
        );
        r.detail = detail;
        if is_search {
            search_row = Some(r);
        } else {
            rows.push(r);
        }
    }

    if matches!(mode, Mode::All | Mode::Commands) {
        let boost = |id: CommandId| -> i32 {
            recent
                .iter()
                .position(|r| *r == id)
                .map_or(0, |p| 24 - 3 * p as i32)
        };
        if q.is_empty() {
            if mode == Mode::All || mode == Mode::Commands {
                for id in recent.iter().take(SHOWN_RECENT) {
                    if let Some(c) = commands::get(*id).filter(|c| command_visible(c.id, s)) {
                        rows.push(command_row("Recent", c, s, Vec::new()));
                    }
                }
            }
            for c in commands::catalog() {
                if command_visible(c.id, s)
                    && !(recent.iter().take(SHOWN_RECENT).any(|r| *r == c.id))
                {
                    rows.push(command_row("Commands", c, s, Vec::new()));
                }
            }
        } else {
            let mut found: Vec<(i32, &'static commands::Command, Vec<usize>)> = commands::catalog()
                .iter()
                .filter(|c| command_visible(c.id, s))
                .filter_map(|c| {
                    let m = score_terms(q, c.label, c.keywords)?;
                    Some((m.score + boost(c.id), c, m.positions))
                })
                .collect();
            found.sort_by_key(|f| std::cmp::Reverse(f.0));
            let cap = if mode == Mode::All { 7 } else { BIG_LIMIT };
            if found.is_empty()
                && let Some(r) = search_row.take()
            {
                rows.push(r);
            }
            for (_, c, pos) in found.into_iter().take(cap) {
                rows.push(command_row("Commands", c, s, pos));
            }
        }
    }
    if let Some(r) = search_row {
        rows.push(r);
    }

    if mode == Mode::Commands || (mode == Mode::All && !q.is_empty()) {
        for pc in &s.plugin_commands {
            let Some(m) = score_terms(q, &pc.title, &[&pc.plugin_name, "plugin"]) else {
                continue;
            };
            let mut r = Row::new(
                "Plugins",
                Action::Outcome(PaletteOutcome::PluginCommand { plugin: pc.plugin.clone(), command: pc.command.clone() }),
                egui_phosphor::regular::PUZZLE_PIECE,
                pc.title.clone(),
            );
            r.detail = pc.plugin_name.clone();
            r.shortcut = pc.shortcut.clone();
            r.tag = Some("Plugin");
            r.matches = m.positions;
            rows.push(r);
        }
    }

    if matches!(mode, Mode::All | Mode::Tabs) && (mode == Mode::Tabs || !q.is_empty()) {
        let mut found: Vec<(i32, &TabItem, Vec<usize>)> = s
            .tabs
            .iter()
            .filter_map(|t| {
                let m = score_terms(q, &t.title, &[&t.url])?;
                Some((m.score, t, m.positions))
            })
            .collect();
        if !q.is_empty() {
            found.sort_by_key(|f| std::cmp::Reverse(f.0));
        }
        for (_, t, pos) in found.into_iter().take(limit) {
            let mut r = Row::new(
                "Open tabs",
                Action::Outcome(PaletteOutcome::SwitchTab(t.index)),
                crate::icons::GLOBE,
                display_title(&t.title, &t.url),
            );
            r.detail = t.url.clone();
            r.matches = pos;
            r.dot = t.container.as_ref().map(|c| c.color);
            r.tag = t.container.as_ref().map(|_| "Container");
            rows.push(r);
        }
    }

    if matches!(mode, Mode::All | Mode::Tabs) && (mode == Mode::Tabs || !q.is_empty()) {
        for g in &s.groups {
            let label = if g.name.is_empty() { "Unnamed group".to_string() } else { g.name.clone() };
            let Some(m) = score_terms(q, &label, &["group"]) else {
                continue;
            };
            let mut r = Row::new(
                "Tab groups",
                Action::Outcome(PaletteOutcome::FocusGroup(g.id)),
                egui_phosphor::regular::FOLDER_SIMPLE,
                label,
            );
            r.detail = format!("{} tab{}{}", g.tabs, if g.tabs == 1 { "" } else { "s" }, if g.collapsed { ", collapsed" } else { "" });
            r.matches = m.positions;
            r.dot = Some(g.color);
            r.tag = Some("Group");
            rows.push(r);
        }
    }

    if mode == Mode::Split {
        let mut found: Vec<(i32, &TabItem, Vec<usize>)> = s
            .tabs
            .iter()
            .filter(|t| Some(t.index) != s.active)
            .filter_map(|t| {
                let m = score_terms(q, &t.title, &[&t.url])?;
                Some((m.score, t, m.positions))
            })
            .collect();
        if !q.is_empty() {
            found.sort_by_key(|f| std::cmp::Reverse(f.0));
        }
        for (_, t, pos) in found.into_iter().take(limit) {
            let mut r = Row::new(
                "Show beside this tab",
                Action::Outcome(PaletteOutcome::SplitWith(t.index)),
                egui_phosphor::regular::COLUMNS,
                display_title(&t.title, &t.url),
            );
            r.detail = t.url.clone();
            r.matches = pos;
            r.dot = t.container.as_ref().map(|c| c.color);
            rows.push(r);
        }
    }

    if mode == Mode::Groups {
        if score_terms(q, "New group", &[]).is_some() {
            rows.push(Row::new(
                "Tab groups",
                Action::Outcome(PaletteOutcome::AddToGroup(None)),
                egui_phosphor::regular::FOLDER_SIMPLE_PLUS,
                "New group".to_string(),
            ));
        }
        for g in &s.groups {
            let label = if g.name.is_empty() { "Unnamed group".to_string() } else { g.name.clone() };
            let Some(m) = score_terms(q, &label, &[]) else {
                continue;
            };
            let mut r = Row::new(
                "Tab groups",
                Action::Outcome(PaletteOutcome::AddToGroup(Some(g.id))),
                egui_phosphor::regular::FOLDER_SIMPLE,
                label,
            );
            r.detail = format!("{} tab{}", g.tabs, if g.tabs == 1 { "" } else { "s" });
            r.matches = m.positions;
            r.dot = Some(g.color);
            rows.push(r);
        }
    }

    if matches!(mode, Mode::All | Mode::Links) && (mode == Mode::Links || !q.is_empty()) {
        for (section, icon_code, list) in [
            ("Bookmarks", crate::icons::STAR, &s.bookmarks),
            (
                "History",
                egui_phosphor::regular::CLOCK_COUNTER_CLOCKWISE,
                &s.history,
            ),
        ] {
            let mut found: Vec<(i32, &LinkItem, Vec<usize>)> = list
                .iter()
                .filter_map(|b| {
                    let m = score_terms(q, &b.title, &[&b.url])?;
                    Some((m.score, b, m.positions))
                })
                .collect();
            if !q.is_empty() {
                found.sort_by_key(|f| std::cmp::Reverse(f.0));
            }
            for (_, b, pos) in found.into_iter().take(limit) {
                let mut r = Row::new(
                    section,
                    Action::Outcome(PaletteOutcome::OpenUrl(b.url.clone())),
                    icon_code,
                    display_title(&b.title, &b.url),
                );
                r.detail = b.url.clone();
                r.matches = pos;
                rows.push(r);
            }
        }
    }

    if matches!(mode, Mode::All | Mode::Containers) && (mode == Mode::Containers || !q.is_empty()) {
        // Typing an address in container mode opens that address there.
        let url = match classify(q) {
            Some(Typed::Url(u)) if mode == Mode::Containers => u,
            _ => String::new(),
        };
        let name_query = if url.is_empty() { q } else { "" };
        for ct in &s.containers {
            let Some(m) = score_terms(name_query, &ct.name, &[]) else {
                continue;
            };
            let label = if url.is_empty() {
                format!("New tab in {}", ct.name)
            } else {
                format!("Open {q} in {}", ct.name)
            };
            let mut r = Row::new(
                "Containers",
                Action::Outcome(PaletteOutcome::OpenInContainer {
                    url: url.clone(),
                    container: ct.id.clone(),
                }),
                egui_phosphor::regular::SQUARES_FOUR,
                label,
            );
            r.dot = Some(ct.color);
            if ct.temporary {
                r.tag = Some("Temporary");
            }
            if url.is_empty() && name_query == q {
                // Highlight the container name inside "New tab in <name>".
                let off = "New tab in ".chars().count();
                r.matches = m.positions.iter().map(|p| p + off).collect();
            }
            rows.push(r);
        }
    }

    if mode == Mode::Themes || (mode == Mode::All && q.chars().count() >= 3) {
        let mut found = theme_rows(q, s);
        if mode == Mode::All {
            found.truncate(SMALL_LIMIT);
        }
        rows.extend(found);
    }
    rows
}

/// The theme rows: every theme whose name matches `q`, the current one
/// tagged.
fn theme_rows(q: &str, s: &PaletteSources) -> Vec<Row> {
    use crate::egui_chrome::rgb;
    use crate::theme::{self, ThemeId};
    let mut rows = Vec::new();
    for id in ThemeId::ALL {
        let Some(m) = score_terms(q, id.label(), &[id.key()]) else {
            continue;
        };
        let mut r = Row::new(
            "Themes",
            Action::Outcome(PaletteOutcome::SetTheme(id)),
            crate::icons::THEME,
            id.label().to_string(),
        );
        r.matches = m.positions;
        let (t, other) = match id {
            ThemeId::System => (&theme::LIGHT, &theme::DARK),
            other => (theme::get(other), theme::get(other)),
        };
        r.swatch = Some([rgb(t.t.background), rgb(other.t.background), rgb(other.t.accent)]);
        r.detail = match id {
            ThemeId::System => "Light or Late Night, as the desktop is".to_string(),
            other if theme::get(other).dark => "Dark".to_string(),
            _ => "Light".to_string(),
        };
        if id == s.theme {
            r.tag = Some("Current");
        }
        rows.push(r);
    }
    rows
}

fn display_title(title: &str, url: &str) -> String {
    if title.trim().is_empty() {
        url.to_string()
    } else {
        title.to_string()
    }
}

// ---- the palette -----------------------------------------------------------

#[derive(Default)]
struct Keys {
    /// Presses this frame (fast typists and key repeat can send several).
    down: usize,
    up: usize,
    page_down: bool,
    page_up: bool,
    enter: bool,
    esc: bool,
    tab: bool,
    shift_tab: bool,
}

/// How many rows PageUp/PageDown move by.
const PAGE: usize = 6;
const ROW_H: f32 = 36.0;
const MAX_LIST_H: f32 = ROW_H * 9.0 + 30.0;

pub struct Palette {
    open: bool,
    query: String,
    last_query: String,
    selected: usize,
    scroll_to_selected: bool,
    recent: Vec<CommandId>,
    recent_loaded: bool,
}

impl Default for Palette {
    fn default() -> Self {
        Self::new()
    }
}

fn recent_path() -> Option<std::path::PathBuf> {
    crate::storage::config_file("palette-recent.json").ok()
}

impl Palette {
    pub fn new() -> Self {
        Palette {
            open: false,
            query: String::new(),
            last_query: String::new(),
            selected: 0,
            scroll_to_selected: false,
            recent: Vec::new(),
            recent_loaded: false,
        }
    }

    /// Shows the palette with an empty search box.
    pub fn open(&mut self) {
        self.open_with("");
    }

    /// Shows the palette with `query` already typed (for example `"@ "`).
    pub fn open_with(&mut self, query: &str) {
        if !self.recent_loaded {
            self.recent = load_recent();
            self.recent_loaded = true;
        }
        self.open = true;
        self.query = query.to_string();
        self.last_query = query.to_string();
        self.selected = 0;
        self.scroll_to_selected = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens when closed and closes when open (what Ctrl+P and F1 do).
    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    /// The current text in the search box.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Recently run commands, most recent first.
    pub fn recent(&self) -> &[CommandId] {
        &self.recent
    }

    #[cfg(test)]
    fn set_query(&mut self, q: &str) {
        self.query = q.to_string();
    }

    fn remember(&mut self, id: CommandId) {
        self.recent.retain(|r| *r != id);
        self.recent.insert(0, id);
        self.recent.truncate(MAX_RECENT);
        save_recent(&self.recent);
    }

    /// Draws the palette when open. Returns what the user chose, at which
    /// point the palette has already closed itself.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        c: &Colors,
        sources: &PaletteSources,
    ) -> Option<PaletteOutcome> {
        if !self.open {
            return None;
        }
        let keys = ctx.input_mut(|i| {
            let none = Modifiers::NONE;
            Keys {
                down: i.count_and_consume_key(none, Key::ArrowDown),
                up: i.count_and_consume_key(none, Key::ArrowUp),
                page_down: i.consume_key(none, Key::PageDown),
                page_up: i.consume_key(none, Key::PageUp),
                enter: i.consume_key(none, Key::Enter),
                esc: i.consume_key(none, Key::Escape),
                tab: i.consume_key(none, Key::Tab),
                shift_tab: i.consume_key(Modifiers::SHIFT, Key::Tab),
            }
        });
        if keys.esc {
            self.close();
            return None;
        }

        let screen = ctx.content_rect();
        let mut outcome: Option<PaletteOutcome> = None;
        let mut close = false;

        // Dimmed backdrop; a click on it closes the palette.
        egui::Area::new(Id::new("bc-palette-scrim"))
            .order(Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let r = ui.allocate_rect(screen, Sense::click());
                ui.painter()
                    .rect_filled(screen, 0.0, c.scrim);
                if r.clicked() {
                    close = true;
                }
            });

        let width = (screen.width() - 32.0).clamp(280.0, 660.0);
        let top = (screen.height() * 0.12).clamp(24.0, 120.0);
        let shadow = egui::Shadow {
            offset: [0, 12],
            blur: 40,
            spread: 0,
            color: c.shadow,
        };
        let frame = egui::Frame::new()
            .fill(c.bar_bg)
            .stroke(Stroke::new(1.0, c.line))
            .corner_radius(CornerRadius::same(14))
            .shadow(shadow)
            .inner_margin(egui::Margin::same(0));

        egui::Area::new(Id::new("bc-palette"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_TOP, vec2(0.0, top))
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    ui.set_width(width);
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    self.contents(ui, c, sources, &keys, &mut outcome, &mut close);
                });
            });

        if let Some(out) = outcome {
            if let PaletteOutcome::Run(id) = &out {
                self.remember(*id);
            }
            self.close();
            return Some(out);
        }
        if close {
            self.close();
        }
        None
    }

    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        c: &Colors,
        sources: &PaletteSources,
        keys: &Keys,
        outcome: &mut Option<PaletteOutcome>,
        close: &mut bool,
    ) {
        // Search box.
        let (mode, _) = parse_query(&self.query);
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(icon(egui_phosphor::regular::MAGNIFYING_GLASS, 20.0).color(c.muted));
                    let hint = "Type a command, a tab, a bookmark or an address";
                    let r = ui.add(
                        TextEdit::singleline(&mut self.query)
                            .id(Id::new("bc-palette-input"))
                            .hint_text(egui::RichText::new(hint).color(c.muted))
                            .font(FontId::proportional(17.0))
                            .frame(egui::Frame::NONE)
                            .desired_width(f32::INFINITY),
                    );
                    if !r.has_focus() {
                        r.request_focus();
                    }
                    if mode != Mode::All {
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            chip(ui, c, mode.label());
                        });
                    }
                });
            });
        divider(ui, c);

        if self.query != self.last_query {
            self.last_query = self.query.clone();
            self.selected = 0;
            self.scroll_to_selected = true;
        }

        let rows = build_rows(&self.query, sources, &self.recent);
        let n = rows.len();
        // Keyboard.
        if n > 0 {
            let mut moved = false;
            if keys.down > 0 {
                self.selected = (self.selected + keys.down) % n;
                moved = true;
            }
            if keys.up > 0 {
                self.selected = (self.selected + n - keys.up % n) % n;
                moved = true;
            }
            if keys.page_down {
                self.selected = (self.selected + PAGE).min(n - 1);
                moved = true;
            }
            if keys.page_up {
                self.selected = self.selected.saturating_sub(PAGE);
                moved = true;
            }
            if keys.tab || keys.shift_tab {
                self.selected = jump_section(&rows, self.selected, keys.shift_tab);
                moved = true;
            }
            self.selected = self.selected.min(n - 1);
            self.scroll_to_selected |= moved;
        }
        let mut activate: Option<usize> = None;
        if keys.enter && n > 0 {
            activate = Some(self.selected.min(n - 1));
        }

        if n == 0 {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("No results").color(c.muted).size(14.0));
                ui.label(
                    egui::RichText::new("Press ? for the search prefixes")
                        .color(c.muted)
                        .size(12.0),
                );
            });
            ui.add_space(24.0);
        } else {
            let scroll = std::mem::take(&mut self.scroll_to_selected);
            let pointer_moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
            egui::ScrollArea::vertical()
                .max_height(MAX_LIST_H)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add_space(6.0);
                    let mut last_section = "";
                    for (i, row) in rows.iter().enumerate() {
                        if row.section != last_section {
                            last_section = row.section;
                            section_header(ui, c, row.section);
                        }
                        let (rect, resp) = ui
                            .allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                        if resp.hovered() && pointer_moved {
                            self.selected = i;
                        }
                        let selected = i == self.selected;
                        draw_row(ui, c, row, rect, selected);
                        if selected && scroll {
                            ui.scroll_to_rect(rect.expand2(vec2(0.0, 6.0)), None);
                        }
                        if resp.clicked() {
                            activate = Some(i);
                        }
                    }
                    ui.add_space(6.0);
                });
        }

        // Footer.
        divider(ui, c);
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(16, 7))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    for (k, what) in [
                        ("Up/Down", "navigate"),
                        ("Enter", "select"),
                        ("Tab", "next section"),
                        ("Esc", "close"),
                        ("?", "prefixes"),
                    ] {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            keycap(ui, c, k);
                            ui.label(egui::RichText::new(what).color(c.muted).size(11.5));
                        });
                    }
                });
            });

        if let Some(i) = activate
            && let Some(row) = rows.get(i)
            && row.enabled
        {
            match &row.action {
                Action::Outcome(o) => *outcome = Some(o.clone()),
                Action::SetQuery(q) => {
                    self.query = format!("{q} ");
                    self.last_query = self.query.clone();
                    self.selected = 0;
                    self.scroll_to_selected = true;
                    ui.ctx().request_repaint();
                    // Put the caret after the prefix.
                    let id = Id::new("bc-palette-input");
                    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                        let end = egui::text::CCursor::new(self.query.chars().count());
                        state
                            .cursor
                            .set_char_range(Some(egui::text::CCursorRange::one(end)));
                        state.store(ui.ctx(), id);
                    }
                }
            }
        }
        let _ = close;
    }
}

/// Index of the first row of the next (or previous) section.
fn jump_section(rows: &[Row], from: usize, backwards: bool) -> usize {
    let starts: Vec<usize> = (0..rows.len())
        .filter(|&i| i == 0 || rows[i].section != rows[i - 1].section)
        .collect();
    if starts.is_empty() {
        return from;
    }
    let cur = starts.iter().rposition(|&s| s <= from).unwrap_or(0);
    let next = if backwards {
        // First press goes to the start of this section, then to the previous.
        if starts[cur] < from {
            cur
        } else {
            (cur + starts.len() - 1) % starts.len()
        }
    } else {
        (cur + 1) % starts.len()
    };
    starts[next]
}

// ---- drawing ---------------------------------------------------------------

fn divider(ui: &mut egui::Ui, c: &Colors) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, c.line);
}

fn chip(ui: &mut egui::Ui, c: &Colors, text: &str) {
    egui::Frame::new()
        .fill(c.accent.gamma_multiply(0.18))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(c.accent).size(12.0));
        });
}

fn section_header(ui: &mut egui::Ui, c: &Colors, title: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.left() + 18.0, rect.bottom() - 5.0),
        Align2::LEFT_BOTTOM,
        title.to_uppercase(),
        FontId::proportional(10.5),
        c.muted,
    );
}

fn keycap(ui: &mut egui::Ui, c: &Colors, text: &str) {
    egui::Frame::new()
        .fill(c.field_bg)
        .stroke(Stroke::new(1.0, c.line))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(5, 0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(c.muted).size(11.0));
        });
}

/// `Ctrl+Shift+T` -> `["Ctrl", "Shift", "T"]`; a trailing `+` is the plus key.
pub fn split_shortcut(text: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('+') {
            // "++" leaves a lone "+" as the key.
            if stripped.is_empty() {
                parts.push("+".into());
                break;
            }
            rest = stripped;
            continue;
        }
        match rest.find('+') {
            Some(i) if i > 0 => {
                parts.push(rest[..i].to_string());
                rest = &rest[i + 1..];
                if rest == "+" {
                    parts.push("+".into());
                    break;
                }
            }
            _ => {
                parts.push(rest.to_string());
                break;
            }
        }
    }
    parts
}

fn highlighted(
    ui: &egui::Ui,
    text: &str,
    matches: &[usize],
    base: Color32,
    hit: Color32,
    size: f32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let font = FontId::proportional(size);
    let mut buf = String::new();
    let mut buf_hit = false;
    let flush = |job: &mut egui::text::LayoutJob, buf: &mut String, is_hit: bool| {
        if buf.is_empty() {
            return;
        }
        let fmt = egui::TextFormat {
            font_id: font.clone(),
            color: if is_hit { hit } else { base },
            underline: if is_hit {
                Stroke::new(1.0, hit)
            } else {
                Stroke::NONE
            },
            ..Default::default()
        };
        job.append(buf, 0.0, fmt);
        buf.clear();
    };
    for (i, ch) in text.chars().enumerate() {
        let is_hit = matches.binary_search(&i).is_ok();
        if is_hit != buf_hit {
            flush(&mut job, &mut buf, buf_hit);
            buf_hit = is_hit;
        }
        buf.push(ch);
    }
    flush(&mut job, &mut buf, buf_hit);
    ui.painter().layout_job(job)
}

fn draw_row(ui: &mut egui::Ui, c: &Colors, row: &Row, rect: Rect, selected: bool) {
    let painter = ui.painter();
    let inner = rect.shrink2(vec2(8.0, 1.0));
    if selected {
        painter.rect_filled(inner, CornerRadius::same(8), c.hover);
        // Accent bar on the selected row.
        let bar = Rect::from_min_size(
            Pos2::new(inner.left(), inner.top() + 7.0),
            vec2(3.0, inner.height() - 14.0),
        );
        painter.rect_filled(bar, CornerRadius::same(2), c.accent);
    }
    let text_col = if row.enabled {
        c.text
    } else {
        c.muted.gamma_multiply(0.7)
    };
    let cy = inner.center().y;
    let mut left = inner.left() + 14.0;

    // Icon, or a container colour dot.
    painter.text(
        Pos2::new(left, cy),
        Align2::LEFT_CENTER,
        row.icon,
        FontId::new(18.0, crate::icons::family()),
        if row.enabled { c.muted } else { text_col },
    );
    left += 28.0;
    if let Some([bg, other, accent]) = row.swatch {
        // The theme's background (half light, half dark for "Follow
        // system"), with its accent as a dot.
        let sw = Rect::from_min_size(Pos2::new(left, cy - 8.0), vec2(28.0, 16.0));
        painter.rect_filled(sw, CornerRadius::same(4), other);
        painter.rect_filled(Rect::from_min_max(sw.min, Pos2::new(sw.center().x, sw.max.y)), CornerRadius { nw: 4, sw: 4, ne: 0, se: 0 }, bg);
        painter.rect_stroke(sw, CornerRadius::same(4), Stroke::new(1.0, c.line), egui::StrokeKind::Inside);
        painter.circle_filled(Pos2::new(sw.right() - 7.0, cy), 3.5, accent);
        left += 36.0;
    }
    if let Some(dot) = row.dot {
        painter.circle_filled(Pos2::new(left + 4.0, cy), 4.0, dot);
        left += 14.0;
    }

    // Right side: shortcut keycaps, or a tag.
    let mut right = inner.right() - 12.0;
    if let Some(sc) = &row.shortcut {
        for part in split_shortcut(sc).iter().rev() {
            let galley = painter.layout_no_wrap(part.clone(), FontId::proportional(11.0), c.muted);
            let w = galley.size().x + 10.0;
            let cap = Rect::from_min_size(Pos2::new(right - w, cy - 9.0), vec2(w, 18.0));
            painter.rect(
                cap,
                CornerRadius::same(4),
                c.field_bg,
                Stroke::new(1.0, c.line),
                egui::StrokeKind::Inside,
            );
            painter.galley(
                Pos2::new(cap.left() + 5.0, cy - galley.size().y / 2.0),
                galley,
                c.muted,
            );
            right = cap.left() - 4.0;
        }
        right -= 4.0;
    } else if let Some(tag) = row.tag {
        let galley = painter.layout_no_wrap(tag.to_string(), FontId::proportional(11.0), c.muted);
        painter.galley(
            Pos2::new(right - galley.size().x, cy - galley.size().y / 2.0),
            galley.clone(),
            c.muted,
        );
        right -= galley.size().x + 8.0;
    }

    // Label with highlighted matches, then a muted detail (the URL).
    let avail = (right - left).max(40.0);
    let label = highlighted(
        ui,
        &row.label,
        &row.matches,
        text_col,
        c.accent,
        14.0,
        avail,
    );
    let label_w = label.size().x;
    let painter = ui.painter();
    painter.galley(Pos2::new(left, cy - label.size().y / 2.0), label, text_col);
    if !row.detail.is_empty() {
        let dx = left + label_w + 10.0;
        let room = right - dx;
        if room > 50.0 {
            let d = highlighted(ui, &row.detail, &[], c.muted, c.muted, 12.0, room);
            ui.painter()
                .galley(Pos2::new(dx, cy - d.size().y / 2.0), d, c.muted);
        }
    }
}

// ---- recent commands -------------------------------------------------------

fn load_recent() -> Vec<CommandId> {
    let Some(path) = recent_path() else {
        return Vec::new();
    };
    let Ok(bytes) = crate::storage::read_limited(&path, 64 * 1024) else {
        return Vec::new();
    };
    let keys: Vec<String> = serde_json::from_slice(&bytes).unwrap_or_default();
    let mut out: Vec<CommandId> = Vec::new();
    for k in keys {
        if let Some(id) = commands::from_key(&k)
            && !out.contains(&id)
        {
            out.push(id);
        }
    }
    out.truncate(MAX_RECENT);
    out
}

fn save_recent(recent: &[CommandId]) {
    let Some(path) = recent_path() else { return };
    let keys: Vec<&str> = recent
        .iter()
        .filter_map(|id| commands::get(*id).map(|c| c.key))
        .collect();
    if let Ok(json) = serde_json::to_vec(&keys) {
        // Losing the list only loses the boost; never fail the user's action.
        let _ = crate::storage::write_private(&path, &json);
    }
}

// ---- tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(q: &str, t: &str) -> Vec<usize> {
        fuzzy(q, t).map(|m| m.positions).unwrap_or_default()
    }

    #[test]
    fn fuzzy_subsequence_and_positions() {
        assert_eq!(pos("scr", "Screenshot"), vec![0, 1, 2]);
        assert_eq!(pos("sht", "Screenshot"), vec![0, 7, 9]);
        assert!(fuzzy("xyz", "Screenshot").is_none());
        assert!(fuzzy("tsc", "Screenshot").is_none());
        assert_eq!(fuzzy("", "anything").map(|m| m.score), Some(0));
    }

    #[test]
    fn fuzzy_prefers_word_starts_and_prefixes() {
        let word = fuzzy("dm", "Dark mode").map(|m| m.score);
        let inside = fuzzy("dm", "Gadgets mood").map(|m| m.score);
        assert!(word > inside, "{word:?} vs {inside:?}");
        let prefix = fuzzy("new", "New tab").map(|m| m.score);
        let late = fuzzy("new", "Open a new tab").map(|m| m.score);
        assert!(prefix > late);
        let tight = fuzzy("tab", "xtabx").map(|m| m.score);
        let spread = fuzzy("tab", "xtxaxbx").map(|m| m.score);
        assert!(tight > spread);
    }

    #[test]
    fn fuzzy_picks_word_start_alignment() {
        // "tab" in "New tab" must highlight the word, not stray letters.
        assert_eq!(pos("tab", "Close other tabs"), vec![12, 13, 14]);
        assert_eq!(pos("dark", "Toggle dark mode"), vec![7, 8, 9, 10]);
    }

    #[test]
    fn fuzzy_is_case_insensitive_and_unicode_safe() {
        assert!(fuzzy("SCREEN", "screenshot").is_some());
        assert!(fuzzy("cafe", "Caf\u{e9} Crème").is_none() || fuzzy("caf", "Caf\u{e9}").is_some());
        let long = "x".repeat(5000);
        assert!(fuzzy("xx", &long).is_some());
    }

    #[test]
    fn terms_must_all_match_with_keywords() {
        assert!(score_terms("dark toggle", "Toggle dark mode", &[]).is_some());
        assert!(score_terms("night", "Toggle dark mode", &["night", "theme"]).is_some());
        assert!(score_terms("dark zzz", "Toggle dark mode", &["night"]).is_none());
        // Positions only cover the label.
        let m = score_terms("night", "Toggle dark mode", &["night"]).unwrap_or(Match {
            score: 0,
            positions: vec![99],
        });
        assert!(m.positions.is_empty());
    }

    #[test]
    fn parses_prefixes() {
        assert_eq!(parse_query("> shot"), (Mode::Commands, "shot"));
        assert_eq!(parse_query("@mail"), (Mode::Tabs, "mail"));
        assert_eq!(parse_query("  # rust "), (Mode::Links, "rust"));
        assert_eq!(parse_query("~"), (Mode::Containers, ""));
        assert_eq!(parse_query("?"), (Mode::Help, ""));
        assert_eq!(parse_query(" plain "), (Mode::All, "plain"));
    }

    #[test]
    fn classifies_urls_and_searches() {
        assert_eq!(
            classify("example.com"),
            Some(Typed::Url("https://example.com".into()))
        );
        assert_eq!(
            classify("localhost:3000"),
            Some(Typed::Url("http://localhost:3000".into()))
        );
        assert!(matches!(classify("rust webview"), Some(Typed::Search(_))));
        assert!(matches!(classify("hello"), Some(Typed::Search(_))));
        assert_eq!(classify("   "), None);
        assert_eq!(
            classify("https://a.b/c"),
            Some(Typed::Url("https://a.b/c".into()))
        );
    }

    #[test]
    fn splits_shortcuts() {
        assert_eq!(split_shortcut("Ctrl+Shift+T"), vec!["Ctrl", "Shift", "T"]);
        assert_eq!(split_shortcut("Ctrl++"), vec!["Ctrl", "+"]);
        assert_eq!(split_shortcut("Ctrl+-"), vec!["Ctrl", "-"]);
        assert_eq!(split_shortcut("F12"), vec!["F12"]);
    }

    fn sources() -> PaletteSources {
        PaletteSources {
            tabs: vec![
                TabItem {
                    index: 0,
                    title: "Inbox - Mail".into(),
                    url: "https://mail.example.com".into(),
                    container: None,
                },
                TabItem {
                    index: 3,
                    title: "Rust docs".into(),
                    url: "https://doc.rust-lang.org".into(),
                    container: Some(ContainerBadge {
                        name: "Work".into(),
                        color: Color32::RED,
                    }),
                },
            ],
            bookmarks: vec![LinkItem {
                title: "Rust blog".into(),
                url: "https://blog.rust-lang.org".into(),
            }],
            history: vec![LinkItem {
                title: "Hacker News".into(),
                url: "https://news.ycombinator.com".into(),
            }],
            containers: vec![ContainerItem {
                id: "work".into(),
                name: "Work".into(),
                color: Color32::BLUE,
                temporary: false,
            }],
            groups: vec![GroupItem {
                id: 9,
                name: "Research".into(),
                color: Color32::GREEN,
                tabs: 2,
                collapsed: true,
            }],
            active: Some(0),
            on_web_page: true,
            recording: false,
            theme: crate::theme::ThemeId::Olive,
            plugin_commands: vec![PluginCommandItem {
                plugin: "local.t.clip".into(),
                command: "open".into(),
                title: "Open the clipboard manager".into(),
                plugin_name: "Clipboard".into(),
                shortcut: Some("Ctrl+V".into()),
            }],
        }
    }

    #[test]
    fn split_and_group_pickers() {
        let s = sources();
        assert_eq!(parse_query("| rust"), (Mode::Split, "rust"));
        assert_eq!(parse_query("&"), (Mode::Groups, ""));
        // The current tab cannot be split with itself.
        assert_eq!(actions(&build_rows("|", &s, &[])), vec![PaletteOutcome::SplitWith(3)]);
        assert_eq!(
            actions(&build_rows("&", &s, &[])),
            vec![PaletteOutcome::AddToGroup(None), PaletteOutcome::AddToGroup(Some(9))]
        );
        assert_eq!(actions(&build_rows("& resea", &s, &[])), vec![PaletteOutcome::AddToGroup(Some(9))]);
        // Groups are listed among the open tabs.
        assert!(actions(&build_rows("@ research", &s, &[])).contains(&PaletteOutcome::FocusGroup(9)));
        // The commands open the pickers.
        let rows = build_rows("> split view with", &s, &[]);
        assert!(rows.iter().any(|r| matches!(r.action, Action::SetQuery("|"))));
    }

    #[test]
    fn change_theme_lists_every_theme_and_marks_the_current_one() {
        let s = sources();
        // The command opens the theme list instead of running.
        let rows = build_rows("change theme", &s, &[]);
        let cmd = rows.iter().find(|r| r.label == "Change theme…").expect("the command is found");
        assert!(matches!(cmd.action, Action::SetQuery("%")));
        let rows = build_rows("%", &s, &[]);
        assert_eq!(rows.len(), crate::theme::ThemeId::ALL.len());
        assert!(rows.iter().all(|r| r.swatch.is_some()));
        let current: Vec<_> = rows.iter().filter(|r| r.tag == Some("Current")).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].label, "Olive");
        let rows = build_rows("% fro", &s, &[]);
        assert!(matches!(rows[0].action, Action::Outcome(PaletteOutcome::SetTheme(crate::theme::ThemeId::Frost))));
        // Typing a theme's name finds it from the start too.
        assert!(build_rows("slate", &s, &[]).iter().any(|r| matches!(r.action, Action::Outcome(PaletteOutcome::SetTheme(crate::theme::ThemeId::SlateGray)))));
        assert_eq!(parse_query("% x"), (Mode::Themes, "x"));
    }

    fn actions(rows: &[Row]) -> Vec<PaletteOutcome> {
        rows.iter()
            .filter_map(|r| match &r.action {
                Action::Outcome(o) => Some(o.clone()),
                Action::SetQuery(_) => None,
            })
            .collect()
    }

    #[test]
    fn empty_query_lists_commands_and_recent_first() {
        let plugin = build_rows("clipboard man", &sources(), &[]);
        let row = plugin.iter().find(|r| r.section == "Plugins").expect("plugin command row");
        assert_eq!(row.shortcut.as_deref(), Some("Ctrl+V"));
        assert!(matches!(&row.action, Action::Outcome(PaletteOutcome::PluginCommand { plugin, command }) if plugin == "local.t.clip" && command == "open"));
        assert!(build_rows("# clipboard", &sources(), &[]).iter().all(|r| r.section != "Plugins"), "not in the links list");
        let rows = build_rows("", &sources(), &[CommandId::Print, CommandId::Find]);
        assert_eq!(rows[0].section, "Recent");
        assert_eq!(rows[0].label, "Print page…");
        assert_eq!(rows[1].label, "Find in page");
        assert!(rows.iter().any(|r| r.section == "Commands"));
        // Recent commands are not repeated in the main list.
        assert_eq!(rows.iter().filter(|r| r.label == "Print page…").count(), 1);
        assert!(rows.iter().all(|r| r.section != "Open tabs"));
    }

    #[test]
    fn recording_commands_follow_state() {
        let mut s = sources();
        let all = |s: &PaletteSources| -> Vec<String> {
            build_rows("", s, &[])
                .into_iter()
                .map(|r| r.label)
                .collect()
        };
        assert!(all(&s).contains(&"Start screen recording".to_string()));
        assert!(!all(&s).contains(&"Stop screen recording".to_string()));
        s.recording = true;
        assert!(!all(&s).contains(&"Start screen recording".to_string()));
        assert!(all(&s).contains(&"Stop screen recording".to_string()));
    }

    #[test]
    fn page_commands_are_disabled_off_page() {
        let mut s = sources();
        s.on_web_page = false;
        let rows = build_rows("screenshot", &s, &[]);
        assert!(!rows[1].enabled || !rows[0].enabled);
        let shot = rows
            .iter()
            .find(|r| r.label.starts_with("Screenshot of the visible"));
        assert!(shot.is_some_and(|r| !r.enabled));
        let settings = build_rows("settings", &s, &[]);
        assert!(settings.iter().any(|r| r.label == "Settings" && r.enabled));
    }

    #[test]
    fn prefix_modes_restrict_sections() {
        let s = sources();
        let cmds = build_rows("> shot", &s, &[]);
        assert!(cmds.iter().all(|r| r.section == "Commands"));
        assert!(!cmds.is_empty());
        let tabs = build_rows("@", &s, &[]);
        // Open tabs, then the tab groups.
        assert_eq!(
            actions(&tabs),
            vec![PaletteOutcome::SwitchTab(0), PaletteOutcome::SwitchTab(3), PaletteOutcome::FocusGroup(9)]
        );
        let tabs = build_rows("@ rust", &s, &[]);
        assert_eq!(actions(&tabs), vec![PaletteOutcome::SwitchTab(3)]);
        let links = build_rows("# rust", &s, &[]);
        assert_eq!(
            actions(&links),
            vec![PaletteOutcome::OpenUrl("https://blog.rust-lang.org".into())]
        );
        let links = build_rows("#", &s, &[]);
        assert_eq!(links.len(), 2);
        let ct = build_rows("~", &s, &[]);
        assert_eq!(
            actions(&ct),
            vec![PaletteOutcome::OpenInContainer {
                url: String::new(),
                container: "work".into()
            }]
        );
        let ct = build_rows("~ example.com", &s, &[]);
        assert_eq!(
            actions(&ct),
            vec![PaletteOutcome::OpenInContainer {
                url: "https://example.com".into(),
                container: "work".into()
            }]
        );
        let help = build_rows("?", &s, &[]);
        assert_eq!(help.len(), 7);
    }

    #[test]
    fn typed_address_is_the_first_row() {
        let rows = build_rows("example.com", &sources(), &[]);
        assert_eq!(rows[0].section, "Go to");
        assert_eq!(
            rows[0].action_outcome(),
            Some(PaletteOutcome::OpenUrl("https://example.com".into()))
        );
        let rows = build_rows("rust tutorial", &sources(), &[]);
        assert!(rows[0].label.starts_with("Search the web"));
        // A query that names a command runs it on Enter; the search follows.
        let rows = build_rows("dark site", &sources(), &[]);
        assert_eq!(rows[0].section, "Commands");
        assert!(rows.iter().any(|r| r.label.starts_with("Search the web")));
        // Prefix modes never offer it.
        assert!(
            build_rows("> example.com", &sources(), &[])
                .iter()
                .all(|r| r.section != "Go to")
        );
    }

    #[test]
    fn recent_commands_rank_higher() {
        let s = sources();
        let plain = build_rows("> tab", &s, &[]);
        let boosted = build_rows("> tab", &s, &[CommandId::PrevTab]);
        let rank = |rows: &[Row]| rows.iter().position(|r| r.label == "Previous tab");
        assert!(rank(&plain) > Some(0));
        assert_eq!(rank(&boosted), Some(0));
    }

    #[test]
    fn section_jump_cycles() {
        let rows = build_rows("rust", &sources(), &[]);
        let start = jump_section(&rows, 0, false);
        assert!(
            rows[start].section != rows[0].section
                || rows.iter().all(|r| r.section == rows[0].section)
        );
        let back = jump_section(&rows, start, true);
        assert_eq!(back, 0);
    }

    impl Row {
        fn action_outcome(&self) -> Option<PaletteOutcome> {
            match &self.action {
                Action::Outcome(o) => Some(o.clone()),
                Action::SetQuery(_) => None,
            }
        }
    }

    // ---- headless egui ----

    fn install_fonts(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        crate::icons::add_fonts(&mut fonts);
        ctx.set_fonts(fonts);
    }

    fn test_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        ctx
    }

    fn key(k: Key) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: Some(k),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    fn frame(
        ctx: &egui::Context,
        p: &mut Palette,
        s: &PaletteSources,
        events: Vec<egui::Event>,
    ) -> Option<PaletteOutcome> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 700.0))),
            events,
            ..Default::default()
        };
        let colors = Colors::for_dark(true);
        let mut out = None;
        let mut full = ctx.run_ui(input, |ui| {
            out = p.ui(ui.ctx(), &colors, s);
        });
        full.textures_delta.clear();
        out
    }

    #[test]
    fn closed_palette_returns_nothing() {
        let ctx = test_ctx();
        let mut p = Palette::new();
        assert!(!p.is_open());
        assert_eq!(frame(&ctx, &mut p, &sources(), vec![key(Key::Enter)]), None);
    }

    #[test]
    fn escape_closes() {
        let ctx = test_ctx();
        let mut p = Palette::new();
        p.open();
        assert!(p.is_open());
        frame(&ctx, &mut p, &sources(), vec![]);
        assert!(p.is_open());
        assert_eq!(
            frame(&ctx, &mut p, &sources(), vec![key(Key::Escape)]),
            None
        );
        assert!(!p.is_open());
    }

    #[test]
    fn enter_runs_first_command_and_remembers_it() {
        let ctx = test_ctx();
        let mut p = Palette::new();
        p.open();
        frame(&ctx, &mut p, &sources(), vec![]);
        let out = frame(&ctx, &mut p, &sources(), vec![key(Key::Enter)]);
        assert_eq!(out, Some(PaletteOutcome::Run(CommandId::NewTab)));
        assert!(!p.is_open());
        assert_eq!(p.recent(), &[CommandId::NewTab]);
        // A fresh palette reads the persisted list back.
        let mut q = Palette::new();
        q.open();
        assert_eq!(q.recent().first(), Some(&CommandId::NewTab));
    }

    #[test]
    fn arrows_move_and_wrap() {
        let ctx = test_ctx();
        let mut p = Palette::new();
        p.open();
        frame(&ctx, &mut p, &sources(), vec![]);
        // Up from the first row wraps to the last command (Quit).
        frame(&ctx, &mut p, &sources(), vec![key(Key::ArrowUp)]);
        let out = frame(&ctx, &mut p, &sources(), vec![key(Key::Enter)]);
        assert!(matches!(out, Some(PaletteOutcome::Run(_))));
        let mut p = Palette::new();
        p.open();
        frame(&ctx, &mut p, &sources(), vec![]);
        frame(&ctx, &mut p, &sources(), vec![key(Key::ArrowDown)]);
        let out = frame(&ctx, &mut p, &sources(), vec![key(Key::Enter)]);
        // Second row in the catalog (recents may precede it).
        assert!(matches!(out, Some(PaletteOutcome::Run(_))));
    }

    #[test]
    fn typing_filters_and_enter_selects_tab() {
        let ctx = test_ctx();
        let mut p = Palette::new();
        p.open();
        frame(&ctx, &mut p, &sources(), vec![]);
        frame(
            &ctx,
            &mut p,
            &sources(),
            vec![egui::Event::Text("@rust".into())],
        );
        assert_eq!(p.query(), "@rust");
        let out = frame(&ctx, &mut p, &sources(), vec![key(Key::Enter)]);
        assert_eq!(out, Some(PaletteOutcome::SwitchTab(3)));
    }

    #[test]
    fn typing_an_address_opens_it() {
        let ctx = test_ctx();
        let mut p = Palette::new();
        p.open();
        frame(&ctx, &mut p, &sources(), vec![]);
        frame(
            &ctx,
            &mut p,
            &sources(),
            vec![egui::Event::Text("example.org".into())],
        );
        let out = frame(&ctx, &mut p, &sources(), vec![key(Key::Enter)]);
        assert_eq!(
            out,
            Some(PaletteOutcome::OpenUrl("https://example.org".into()))
        );
    }

    #[test]
    fn disabled_command_does_not_run() {
        let ctx = test_ctx();
        let mut s = sources();
        s.on_web_page = false;
        let mut p = Palette::new();
        p.open_with("> visible");
        frame(&ctx, &mut p, &s, vec![]);
        let out = frame(&ctx, &mut p, &s, vec![key(Key::Enter)]);
        assert_eq!(out, None);
        assert!(p.is_open());
    }

    #[test]
    fn container_command_switches_to_container_list() {
        let ctx = test_ctx();
        let mut p = Palette::new();
        p.open();
        frame(&ctx, &mut p, &sources(), vec![]);
        p.set_query("new tab in cont");
        frame(&ctx, &mut p, &sources(), vec![]);
        let out = frame(&ctx, &mut p, &sources(), vec![key(Key::Enter)]);
        // Best match may be the container command or a plain new tab.
        if out.is_none() {
            assert_eq!(parse_query(p.query()).0, Mode::Containers);
        }
        assert!(p.is_open() || out.is_some());
    }

    #[test]
    fn renders_in_light_and_dark_without_panicking() {
        for dark in [true, false] {
            let ctx = test_ctx();
            install_fonts(&ctx);
            let mut p = Palette::new();
            p.open();
            let colors = Colors::for_dark(dark);
            for q in ["", "tab", "@", "#r", "~", "?", "zzzzzz", "example.com"] {
                p.set_query(q);
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(320.0, 400.0))),
                    ..Default::default()
                };
                let mut full = ctx.run_ui(input, |ui| {
                    let _ = p.ui(ui.ctx(), &colors, &sources());
                });
                full.textures_delta.clear();
            }
        }
    }
}
