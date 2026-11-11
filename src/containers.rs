//! Multi-Account Containers: named, coloured identities whose tabs keep their
//! own cookies, storage, IndexedDB, cache and service workers.
//!
//! The model is a global store persisted in `containers.json` in the
//! per-user config folder: the containers (id, name, colour, icon) and the
//! site rules ("always open `host` in container X"). Temporary containers
//! live only in memory; their web data goes to a private temporary folder
//! that is deleted when their last tab closes (and swept at the next start
//! if the browser crashed).
//!
//! Isolation itself is WebKit's: each container's tabs get a web context
//! whose website data manager points at `profiles/container-<id>`
//! (`data_dir`), so two containers never share a cookie jar or a storage
//! origin. "No container" uses the default profile.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::egui_chrome::Colors;

const FILE_NAME: &str = "containers.json";
/// Imported files and the store itself are small; anything bigger is refused.
pub const MAX_FILE: u64 = 1 << 20;
const MAX_CONTAINERS: usize = 200;
const MAX_RULES: usize = 5000;
const MAX_NAME_CHARS: usize = 48;
const TEMP_PREFIX: &str = "browsecraft-container-";

/// The colours a container can have (the Firefox Multi-Account Containers
/// palette), as name and RGB.
pub const COLORS: &[(&str, u32)] = &[
    ("blue", 0x37adff),
    ("turquoise", 0x00c79a),
    ("green", 0x51cd00),
    ("yellow", 0xffcb00),
    ("orange", 0xff9f00),
    ("red", 0xff613d),
    ("pink", 0xff4bda),
    ("purple", 0xaf51f5),
    ("toolbar", 0x8f8f9d),
];

/// The icons a container can have, as id and Phosphor glyph.
pub const ICONS: &[(&str, &str)] = &[
    ("fingerprint", egui_phosphor::regular::FINGERPRINT),
    ("briefcase", egui_phosphor::regular::BRIEFCASE),
    ("dollar", egui_phosphor::regular::CURRENCY_DOLLAR),
    ("cart", egui_phosphor::regular::SHOPPING_CART),
    ("vacation", egui_phosphor::regular::AIRPLANE_TILT),
    ("gift", egui_phosphor::regular::GIFT),
    ("food", egui_phosphor::regular::FORK_KNIFE),
    ("fruit", egui_phosphor::regular::ORANGE_SLICE),
    ("pet", egui_phosphor::regular::PAW_PRINT),
    ("tree", egui_phosphor::regular::TREE),
    ("chill", egui_phosphor::regular::SUNGLASSES),
    ("circle", egui_phosphor::regular::CIRCLE),
    ("fence", egui_phosphor::regular::GRID_FOUR),
    ("timer", egui_phosphor::regular::TIMER),
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub color: String,
    pub icon: String,
    /// Lives until its last tab closes; never written to disk.
    #[serde(default, skip_serializing)]
    pub temporary: bool,
}

impl Container {
    pub fn rgb(&self) -> u32 {
        color_rgb(&self.color)
    }

    pub fn color32(&self) -> egui::Color32 {
        crate::egui_chrome::rgb(self.rgb())
    }

    pub fn glyph(&self) -> &'static str {
        icon_glyph(&self.icon)
    }
}

/// "Always open `host` (and its subdomains) in `container`".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteRule {
    pub host: String,
    pub container: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub containers: Vec<Container>,
    #[serde(default)]
    pub rules: Vec<SiteRule>,
}

static STORE: RwLock<Store> = RwLock::new(Store { containers: Vec::new(), rules: Vec::new() });
static TEMPORARY: RwLock<Vec<Container>> = RwLock::new(Vec::new());

pub fn color_rgb(name: &str) -> u32 {
    COLORS.iter().find(|(n, _)| *n == name).map_or(COLORS[0].1, |(_, rgb)| *rgb)
}

pub fn icon_glyph(id: &str) -> &'static str {
    ICONS.iter().find(|(n, _)| *n == id).map_or(egui_phosphor::regular::CIRCLE, |(_, g)| *g)
}

fn file_path() -> Result<PathBuf, String> {
    crate::storage::config_file(FILE_NAME)
}

/// The data directory of a persistent container's web profile.
pub fn data_dir(id: &str) -> Result<PathBuf, String> {
    if !valid_id(id) {
        return Err("Invalid container id".into());
    }
    crate::storage::profile_dir(&format!("container-{id}"))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn new_id(prefix: &str) -> String {
    let mut bytes = [0u8; 6];
    if getrandom::fill(&mut bytes).is_err() {
        // Not secret: only has to be unique among this user's containers.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes.copy_from_slice(&nanos.to_le_bytes()[..6]);
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}{hex}")
}

fn clean_name(name: &str) -> String {
    let collapsed = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed.chars().take(MAX_NAME_CHARS).collect()
}

/// Makes a container from untrusted input valid: a known colour and icon,
/// a clean name. `None` when it has no usable id or name.
fn sanitize(mut c: Container) -> Option<Container> {
    c.name = clean_name(&c.name);
    if !valid_id(&c.id) || c.id.starts_with("tmp-") || c.name.is_empty() {
        return None;
    }
    if !COLORS.iter().any(|(n, _)| *n == c.color) {
        c.color = COLORS[0].0.into();
    }
    if !ICONS.iter().any(|(n, _)| *n == c.icon) {
        c.icon = "circle".into();
    }
    c.temporary = false;
    Some(c)
}

/// The host a rule is written for, from whatever the user typed or pasted:
/// `https://www.Example.com:8443/path` -> `www.example.com`. A leading `*.`
/// is dropped (rules always cover subdomains).
pub fn normalize_host(input: &str) -> Option<String> {
    let s = input.trim().to_ascii_lowercase();
    let s = s.split_once("://").map_or(s.as_str(), |(_, rest)| rest);
    let s = s.split(['/', '?', '#']).next().unwrap_or("");
    let s = s.rsplit_once('@').map_or(s, |(_, h)| h);
    let s = if s.starts_with('[') { s } else { s.split(':').next().unwrap_or("") };
    let s = s.trim_start_matches("*.").trim_end_matches('.');
    let ok = !s.is_empty()
        && s.len() <= 253
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'[' || b == b']' || b == b':')
        && !s.starts_with('.')
        && !s.contains("..");
    ok.then(|| s.to_string())
}

fn sanitize_store(store: Store) -> Store {
    let mut out = Store::default();
    for c in store.containers {
        if out.containers.len() == MAX_CONTAINERS {
            break;
        }
        if let Some(c) = sanitize(c)
            && !out.containers.iter().any(|o| o.id == c.id)
        {
            out.containers.push(c);
        }
    }
    for r in store.rules {
        if out.rules.len() == MAX_RULES {
            break;
        }
        let Some(host) = normalize_host(&r.host) else { continue };
        if out.containers.iter().any(|c| c.id == r.container) && !out.rules.iter().any(|o| o.host == host) {
            out.rules.push(SiteRule { host, container: r.container });
        }
    }
    out
}

/// Parses a store (the file or an import), defensively.
pub fn parse(text: &str) -> Result<Store, String> {
    if text.len() as u64 > MAX_FILE {
        return Err("The file is too large".into());
    }
    let store: Store = serde_json::from_str(text).map_err(|e| format!("Not a containers file ({e})"))?;
    Ok(sanitize_store(store))
}

/// Loads `containers.json` once at start-up. A missing or broken file means
/// no containers; it is never overwritten until the user changes something.
pub fn load() {
    let Ok(path) = file_path() else { return };
    let Ok(text) = crate::storage::read_text_limited(&path, MAX_FILE) else { return };
    match parse(&text) {
        Ok(store) => *STORE.write().unwrap_or_else(|p| p.into_inner()) = store,
        Err(e) => eprintln!("browsecraft: ignoring {}: {e}", path.display()),
    }
}

fn save(store: &Store) -> Result<(), String> {
    let path = file_path()?;
    let text = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    crate::storage::write_private(&path, text.as_bytes())
        .map_err(|e| format!("Could not save {}: {e}", path.display()))
}

/// Applies `change` to a copy of the store, saves it, and only then makes it
/// current: a failed save changes nothing.
fn modify<T>(change: impl FnOnce(&mut Store) -> Result<T, String>) -> Result<T, String> {
    let mut store = STORE.write().unwrap_or_else(|p| p.into_inner());
    let mut next = store.clone();
    let out = change(&mut next)?;
    save(&next)?;
    *store = next;
    Ok(out)
}

/// The persistent containers, in the user's order.
pub fn all() -> Vec<Container> {
    STORE.read().unwrap_or_else(|p| p.into_inner()).containers.clone()
}

pub fn rules() -> Vec<SiteRule> {
    STORE.read().unwrap_or_else(|p| p.into_inner()).rules.clone()
}

/// A container by id, persistent or temporary.
pub fn get(id: &str) -> Option<Container> {
    if let Some(c) = STORE.read().unwrap_or_else(|p| p.into_inner()).containers.iter().find(|c| c.id == id) {
        return Some(c.clone());
    }
    TEMPORARY.read().unwrap_or_else(|p| p.into_inner()).iter().find(|c| c.id == id).cloned()
}

/// A persistent container by id or (case-insensitive) name.
pub fn find(id_or_name: &str) -> Option<Container> {
    let store = STORE.read().unwrap_or_else(|p| p.into_inner());
    store
        .containers
        .iter()
        .find(|c| c.id == id_or_name)
        .or_else(|| store.containers.iter().find(|c| c.name.eq_ignore_ascii_case(id_or_name.trim())))
        .cloned()
}

pub fn create(name: &str, color: &str, icon: &str) -> Result<Container, String> {
    let draft = Container {
        id: new_id(""),
        name: name.into(),
        color: color.into(),
        icon: icon.into(),
        temporary: false,
    };
    let c = sanitize(draft).ok_or("A container needs a name")?;
    modify(|store| {
        if store.containers.len() >= MAX_CONTAINERS {
            return Err("Too many containers".into());
        }
        store.containers.push(c.clone());
        Ok(c)
    })
}

/// Renames or restyles a persistent container.
pub fn update(c: &Container) -> Result<(), String> {
    let c = sanitize(c.clone()).ok_or("A container needs a name")?;
    modify(|store| {
        let slot = store.containers.iter_mut().find(|o| o.id == c.id).ok_or("No such container")?;
        *slot = c;
        Ok(())
    })
}

/// Deletes a container and its rules. Its web data is removed by the caller
/// (`remove_data`) once its tabs are closed.
pub fn delete(id: &str) -> Result<(), String> {
    modify(|store| {
        let before = store.containers.len();
        store.containers.retain(|c| c.id != id);
        if store.containers.len() == before {
            return Err("No such container".into());
        }
        store.rules.retain(|r| r.container != id);
        Ok(())
    })
}

/// Deletes a persistent container's web data (cookies, storage, cache).
pub fn remove_data(id: &str) -> Result<(), String> {
    let dir = data_dir(id)?;
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not delete {}: {e}", dir.display())),
    }
}

/// Moves a container one place up (`-1`) or down (`1`) in the list.
pub fn reorder(id: &str, delta: isize) -> Result<(), String> {
    modify(|store| {
        let i = store.containers.iter().position(|c| c.id == id).ok_or("No such container")?;
        let j = i as isize + delta;
        if j >= 0 && (j as usize) < store.containers.len() {
            store.containers.swap(i, j as usize);
        }
        Ok(())
    })
}

/// Sets "always open `host` in `container`", replacing any rule for the host.
pub fn set_rule(host: &str, container: &str) -> Result<String, String> {
    let host = normalize_host(host).ok_or("Enter a host name such as example.com")?;
    modify(|store| {
        if !store.containers.iter().any(|c| c.id == container) {
            return Err("No such container".into());
        }
        store.rules.retain(|r| r.host != host);
        if store.rules.len() >= MAX_RULES {
            return Err("Too many site rules".into());
        }
        store.rules.push(SiteRule { host: host.clone(), container: container.into() });
        Ok(host)
    })
}

pub fn remove_rule(host: &str) -> Result<(), String> {
    modify(|store| {
        store.rules.retain(|r| r.host != host);
        Ok(())
    })
}

/// The container the rules assign to `host`: the most specific rule that
/// is the host itself or one of its parent domains.
pub fn rule_for_host(rules: &[SiteRule], host: &str) -> Option<String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    rules
        .iter()
        .filter(|r| host == r.host || host.ends_with(&format!(".{}", r.host)))
        .max_by_key(|r| r.host.len())
        .map(|r| r.container.clone())
}

/// The container the site rules assign to a web URL, if any.
pub fn rule_for_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?;
    rule_for_host(&STORE.read().unwrap_or_else(|p| p.into_inner()).rules, host)
}

/// The store as JSON, for export (temporary containers are never included).
pub fn export_json() -> String {
    let store = STORE.read().unwrap_or_else(|p| p.into_inner()).clone();
    serde_json::to_string_pretty(&store).unwrap_or_else(|_| "{}".into())
}

/// What an import changed.
#[derive(Debug, PartialEq, Eq)]
pub struct Imported {
    pub containers: usize,
    pub rules: usize,
}

/// Merges `incoming` into `store`: a container with a known id is updated,
/// others are added; rules replace the rule for the same host.
pub fn merge(store: &mut Store, incoming: Store) -> Imported {
    let mut count = Imported { containers: 0, rules: 0 };
    for c in incoming.containers {
        if let Some(slot) = store.containers.iter_mut().find(|o| o.id == c.id) {
            *slot = c;
            count.containers += 1;
        } else if store.containers.len() < MAX_CONTAINERS {
            store.containers.push(c);
            count.containers += 1;
        }
    }
    for r in incoming.rules {
        if !store.containers.iter().any(|c| c.id == r.container) {
            continue;
        }
        store.rules.retain(|o| o.host != r.host);
        if store.rules.len() < MAX_RULES {
            store.rules.push(r);
            count.rules += 1;
        }
    }
    count
}

/// Imports a containers file (an export of this browser).
pub fn import_json(text: &str) -> Result<Imported, String> {
    let incoming = parse(text)?;
    modify(|store| Ok(merge(store, incoming)))
}

pub fn import_file(path: &Path) -> Result<Imported, String> {
    let text = crate::storage::read_text_limited(path, MAX_FILE).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    import_json(&text)
}

pub fn export_file(path: &Path) -> Result<(), String> {
    crate::storage::replace_atomic(path, export_json().as_bytes())
        .map_err(|e| format!("Could not write {}: {e}", path.display()))
}

// ---- temporary containers ----

/// A new temporary container ("Temporary 3"), registered until `end_temporary`.
pub fn create_temporary() -> Container {
    let mut temp = TEMPORARY.write().unwrap_or_else(|p| p.into_inner());
    let n = (1..).find(|n| !temp.iter().any(|c| c.name == format!("Temporary {n}"))).unwrap_or(1);
    let c = Container {
        id: new_id("tmp-"),
        name: format!("Temporary {n}"),
        color: "orange".into(),
        icon: "timer".into(),
        temporary: true,
    };
    temp.push(c.clone());
    c
}

pub fn end_temporary(id: &str) {
    TEMPORARY.write().unwrap_or_else(|p| p.into_inner()).retain(|c| c.id != id);
}

pub fn temporary() -> Vec<Container> {
    TEMPORARY.read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Where temporary containers keep their data: the per-user runtime folder
/// (a tmpfs on most Linux systems), else the system temporary folder.
fn temp_base() -> PathBuf {
    #[cfg(target_os = "linux")]
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute() && p.is_dir()) {
        return dir;
    }
    std::env::temp_dir()
}

/// A new private (0700) folder for a temporary container's web data. It is
/// deleted when the returned guard is dropped.
pub fn temporary_data_dir() -> std::io::Result<tempfile::TempDir> {
    temporary_data_dir_in(&temp_base())
}

fn temporary_data_dir_in(base: &Path) -> std::io::Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new()
        .prefix(&format!("{TEMP_PREFIX}{}-", std::process::id()))
        .tempdir_in(base)?;
    // Cookies and logins live here: owner only.
    crate::storage::private_dir(dir.path())?;
    Ok(dir)
}

/// Deletes temporary-container folders left behind by a browser process
/// that is no longer running (a crash, a kill). Folders of running
/// processes (another window) are kept.
pub fn sweep_stale_temporary() -> usize {
    sweep_stale_in(&temp_base(), &|pid| process_alive(pid))
}

fn sweep_stale_in(base: &Path, alive: &dyn Fn(u32) -> bool) -> usize {
    let Ok(entries) = std::fs::read_dir(base) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(TEMP_PREFIX) else { continue };
        let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<u32>().ok()) else { continue };
        // Never follow a planted symlink: only real folders are removed.
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir && !alive(pid) && std::fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn process_alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{pid}")).exists()
    } else {
        // Without a cheap portable check, keep the folder.
        true
    }
}

// ---- the drawer panel ----

/// Things the containers panel asks the browser to do.
#[derive(Debug, Clone, PartialEq)]
pub enum ContainersEvent {
    /// Open a new tab in this container (`None`: a new temporary one).
    OpenTab(Option<String>),
    /// The container was deleted: close its tabs and delete its data.
    Deleted(String),
    /// Names, colours or icons changed: redraw the tabs.
    Changed,
    Export(PathBuf),
    Import(PathBuf),
    ExportClipboard,
    ImportClipboard,
    Notice(String),
}

#[derive(Clone)]
struct Draft {
    id: Option<String>,
    name: String,
    color: String,
    icon: String,
}

/// The Containers drawer: list, create/edit/delete, site rules, import and
/// export.
pub struct ContainersPanel {
    draft: Option<Draft>,
    confirm_delete: Option<String>,
    rule_host: String,
    rule_container: String,
    path: String,
    /// The host of the active tab, offered for a new rule.
    pub current_host: String,
}

impl Default for ContainersPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl ContainersPanel {
    pub fn new() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        Self {
            draft: None,
            confirm_delete: None,
            rule_host: String::new(),
            rule_container: String::new(),
            path: home.join("browsecraft-containers.json").to_string_lossy().into_owned(),
            current_host: String::new(),
        }
    }

    /// Draws the panel body (the drawer draws title and close button).
    pub fn ui(&mut self, ui: &mut egui::Ui, c: &Colors) -> Vec<ContainersEvent> {
        let mut events = Vec::new();
        let containers = all();
        let heading = |ui: &mut egui::Ui, text: &str| {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(text).size(12.0).strong().color(c.muted));
            ui.add_space(2.0);
        };

        ui.horizontal(|ui| {
            if ui.button(format!("{}  New container", egui_phosphor::regular::PLUS)).clicked() {
                self.draft = Some(Draft { id: None, name: String::new(), color: "blue".into(), icon: "fingerprint".into() });
            }
            if ui
                .button(format!("{}  Temporary", egui_phosphor::regular::TIMER))
                .on_hover_text("New tab in a temporary container, deleted when its last tab closes (Ctrl+Shift+N)")
                .clicked()
            {
                events.push(ContainersEvent::OpenTab(None));
            }
        });

        let mut close_draft = false;
        if let Some(draft) = self.draft.as_mut() {
            egui::Frame::NONE
                .fill(c.hover)
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.label(if draft.id.is_some() { "Edit container" } else { "New container" });
                    ui.add(egui::TextEdit::singleline(&mut draft.name).hint_text("Name, e.g. Work").desired_width(f32::INFINITY));
                    ui.horizontal_wrapped(|ui| {
                        for (name, rgb) in COLORS {
                            let color = crate::egui_chrome::rgb(*rgb);
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                            ui.painter().circle_filled(rect.center(), 8.0, color);
                            if draft.color == *name {
                                ui.painter().circle_stroke(rect.center(), 10.0, egui::Stroke::new(2.0, c.text));
                            }
                            if resp.on_hover_text(*name).clicked() {
                                draft.color = (*name).into();
                            }
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        let color = crate::egui_chrome::rgb(color_rgb(&draft.color));
                        for (id, glyph) in ICONS {
                            let selected = draft.icon == *id;
                            let text = egui::RichText::new(*glyph).size(16.0).color(if selected { color } else { c.muted });
                            if ui.add(egui::Button::new(text).selected(selected)).on_hover_text(*id).clicked() {
                                draft.icon = (*id).into();
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        let save = ui.button("Save").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if save {
                            let result = match &draft.id {
                                None => create(&draft.name, &draft.color, &draft.icon).map(|_| ()),
                                Some(id) => update(&Container {
                                    id: id.clone(),
                                    name: draft.name.clone(),
                                    color: draft.color.clone(),
                                    icon: draft.icon.clone(),
                                    temporary: false,
                                }),
                            };
                            match result {
                                Ok(()) => {
                                    events.push(ContainersEvent::Changed);
                                    close_draft = true;
                                }
                                Err(e) => events.push(ContainersEvent::Notice(e)),
                            }
                        } else if ui.button("Cancel").clicked() {
                            close_draft = true;
                        }
                    });
                });
        }
        if close_draft {
            self.draft = None;
        }

        heading(ui, "CONTAINERS");
        if containers.is_empty() {
            ui.label(egui::RichText::new("No containers yet. Each container keeps its own cookies and logins.").color(c.muted));
        }
        let mut edit: Option<Draft> = None;
        for (i, ctr) in containers.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(ctr.glyph()).size(16.0).color(ctr.color32()));
                let name = egui::Label::new(egui::RichText::new(&ctr.name).color(c.text)).truncate().sense(egui::Sense::click());
                let name = ui
                    .allocate_ui_with_layout(egui::vec2(110.0, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_min_width(110.0);
                        ui.add(name)
                    })
                    .inner;
                if name.on_hover_text("Open a new tab in this container").clicked() {
                    events.push(ContainersEvent::OpenTab(Some(ctr.id.clone())));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.confirm_delete.as_deref() == Some(&ctr.id) {
                        if ui.button("Cancel").clicked() {
                            self.confirm_delete = None;
                        }
                        if ui.button(egui::RichText::new("Delete").color(crate::egui_chrome::rgb(0xff613d))).on_hover_text("Closes its tabs and deletes its cookies and site data").clicked() {
                            match delete(&ctr.id) {
                                Ok(()) => events.push(ContainersEvent::Deleted(ctr.id.clone())),
                                Err(e) => events.push(ContainersEvent::Notice(e)),
                            }
                            self.confirm_delete = None;
                        }
                    } else {
                        if ui.small_button(egui_phosphor::regular::TRASH).on_hover_text("Delete").clicked() {
                            self.confirm_delete = Some(ctr.id.clone());
                        }
                        if ui.small_button(egui_phosphor::regular::PENCIL_SIMPLE).on_hover_text("Edit").clicked() {
                            edit = Some(Draft { id: Some(ctr.id.clone()), name: ctr.name.clone(), color: ctr.color.clone(), icon: ctr.icon.clone() });
                        }
                        if i + 1 < containers.len() && ui.small_button(egui_phosphor::regular::CARET_DOWN).on_hover_text("Move down").clicked() {
                            let _ = reorder(&ctr.id, 1);
                            events.push(ContainersEvent::Changed);
                        }
                        if i > 0 && ui.small_button(egui_phosphor::regular::CARET_UP).on_hover_text("Move up").clicked() {
                            let _ = reorder(&ctr.id, -1);
                            events.push(ContainersEvent::Changed);
                        }
                        if ui.small_button(egui_phosphor::regular::PLUS).on_hover_text("New tab in this container").clicked() {
                            events.push(ContainersEvent::OpenTab(Some(ctr.id.clone())));
                        }
                    }
                });
            });
        }
        if edit.is_some() {
            self.draft = edit;
        }
        let temps = temporary();
        if !temps.is_empty() {
            ui.label(egui::RichText::new(format!("{} temporary container(s) open", temps.len())).size(12.0).color(c.muted));
        }

        heading(ui, "ALWAYS OPEN IN");
        let rules = rules();
        if rules.is_empty() {
            ui.label(egui::RichText::new("Sites listed here always open in their container.").color(c.muted));
        }
        for rule in &rules {
            ui.horizontal(|ui| {
                let ctr = containers.iter().find(|x| x.id == rule.container);
                if let Some(ctr) = ctr {
                    ui.label(egui::RichText::new(ctr.glyph()).color(ctr.color32()));
                }
                ui.add(egui::Label::new(egui::RichText::new(&rule.host).color(c.text)).truncate());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(egui_phosphor::regular::X).on_hover_text("Remove rule").clicked() {
                        match remove_rule(&rule.host) {
                            Ok(()) => {}
                            Err(e) => events.push(ContainersEvent::Notice(e)),
                        }
                    }
                    if let Some(ctr) = ctr {
                        ui.label(egui::RichText::new(&ctr.name).size(12.0).color(c.muted));
                    }
                });
            });
        }
        if !containers.is_empty() {
            if self.rule_host.is_empty() && !self.current_host.is_empty() {
                self.rule_host = self.current_host.clone();
            }
            if !containers.iter().any(|x| x.id == self.rule_container) {
                self.rule_container = containers[0].id.clone();
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.rule_host).hint_text("example.com").desired_width(120.0));
                let selected = containers.iter().find(|x| x.id == self.rule_container).map(|x| x.name.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt("bc-rule-container")
                    .selected_text(selected)
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        for ctr in &containers {
                            ui.selectable_value(&mut self.rule_container, ctr.id.clone(), &ctr.name);
                        }
                    });
                if ui.button("Add").clicked() {
                    match set_rule(&self.rule_host, &self.rule_container) {
                        Ok(host) => {
                            events.push(ContainersEvent::Notice(format!("{host} now always opens in its container")));
                            self.rule_host.clear();
                        }
                        Err(e) => events.push(ContainersEvent::Notice(e)),
                    }
                }
            });
        }

        heading(ui, "IMPORT / EXPORT");
        ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(f32::INFINITY).hint_text("/path/to/containers.json"));
        ui.horizontal(|ui| {
            let path = PathBuf::from(self.path.trim());
            if ui.button(format!("{}  Export", egui_phosphor::regular::UPLOAD_SIMPLE)).clicked() {
                events.push(ContainersEvent::Export(path.clone()));
            }
            if ui.button(format!("{}  Import", egui_phosphor::regular::DOWNLOAD_SIMPLE)).clicked() {
                events.push(ContainersEvent::Import(path));
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Copy as JSON").clicked() {
                events.push(ContainersEvent::ExportClipboard);
            }
            if ui.button("Paste JSON").clicked() {
                events.push(ContainersEvent::ImportClipboard);
            }
        });
        events
    }
}

#[cfg(test)]
pub(crate) static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        *STORE.write().unwrap() = Store::default();
        TEMPORARY.write().unwrap().clear();
    }

    #[test]
    fn crud_persists_and_validates() {
        let _g = TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset();
        let work = create("  Work\n stuff ", "purple", "briefcase").unwrap();
        assert_eq!(work.name, "Work stuff");
        assert!(valid_id(&work.id));
        let odd = create("Bank", "neon", "rocket").unwrap();
        assert_eq!((odd.color.as_str(), odd.icon.as_str()), ("blue", "circle"));
        assert!(create("   ", "red", "pet").is_err());

        let mut renamed = work.clone();
        renamed.name = "Office".into();
        update(&renamed).unwrap();
        assert_eq!(find("office").unwrap().id, work.id);
        assert_eq!(get(&work.id).unwrap().name, "Office");

        // Saved to disk and read back.
        let text = std::fs::read_to_string(file_path().unwrap()).unwrap();
        let back = parse(&text).unwrap();
        assert_eq!(back.containers.len(), 2);
        assert!(!text.contains("temporary"), "the flag is never written");

        reorder(&odd.id, -1).unwrap();
        assert_eq!(all()[0].id, odd.id);
        set_rule("https://mail.example.com/inbox", &work.id).unwrap();
        delete(&work.id).unwrap();
        assert!(get(&work.id).is_none());
        assert!(rules().is_empty(), "a deleted container's rules go too");
        assert!(delete(&work.id).is_err());
        reset();
    }

    #[test]
    fn rules_match_hosts_and_subdomains() {
        let rules = vec![
            SiteRule { host: "example.com".into(), container: "a".into() },
            SiteRule { host: "mail.example.com".into(), container: "b".into() },
        ];
        assert_eq!(rule_for_host(&rules, "example.com").as_deref(), Some("a"));
        assert_eq!(rule_for_host(&rules, "www.example.com").as_deref(), Some("a"));
        assert_eq!(rule_for_host(&rules, "MAIL.example.com.").as_deref(), Some("b"));
        assert_eq!(rule_for_host(&rules, "x.mail.example.com").as_deref(), Some("b"));
        assert_eq!(rule_for_host(&rules, "notexample.com"), None);
        assert_eq!(rule_for_host(&rules, "example.org"), None);

        assert_eq!(normalize_host("https://user:pw@Www.Example.com:8443/a?b#c").as_deref(), Some("www.example.com"));
        assert_eq!(normalize_host("*.example.com").as_deref(), Some("example.com"));
        assert_eq!(normalize_host("example.com.").as_deref(), Some("example.com"));
        assert_eq!(normalize_host(""), None);
        assert_eq!(normalize_host("exa mple.com"), None);
        assert_eq!(normalize_host("a..b"), None);
    }

    #[test]
    fn rule_for_url_uses_the_store() {
        let _g = TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset();
        let bank = create("Bank", "green", "dollar").unwrap();
        set_rule("bank.example", &bank.id).unwrap();
        assert_eq!(rule_for_url("https://login.bank.example/x").as_deref(), Some(bank.id.as_str()));
        assert_eq!(rule_for_url("http://bank.example/").as_deref(), Some(bank.id.as_str()));
        assert_eq!(rule_for_url("file:///bank.example"), None);
        assert_eq!(rule_for_url("https://other.example/"), None);
        assert!(set_rule("bank.example", "missing").is_err());
        // A second rule for the same host replaces the first.
        let other = create("Other", "red", "pet").unwrap();
        set_rule("BANK.example", &other.id).unwrap();
        assert_eq!(rules().len(), 1);
        assert_eq!(rule_for_url("https://bank.example/").as_deref(), Some(other.id.as_str()));
        remove_rule("bank.example").unwrap();
        assert_eq!(rule_for_url("https://bank.example/"), None);
        reset();
    }

    #[test]
    fn export_then_import_round_trips_and_rejects_junk() {
        let _g = TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset();
        let a = create("Shopping", "pink", "cart").unwrap();
        set_rule("shop.example", &a.id).unwrap();
        let _t = create_temporary();
        let json = export_json();
        assert!(!json.contains("tmp-"), "temporary containers are not exported");
        reset();
        let got = import_json(&json).unwrap();
        assert_eq!(got, Imported { containers: 1, rules: 1 });
        assert_eq!(all(), vec![a.clone()]);
        // Importing again updates instead of duplicating.
        assert_eq!(import_json(&json).unwrap().containers, 1);
        assert_eq!(all().len(), 1);

        assert!(import_json("not json").is_err());
        assert!(import_json(&"x".repeat(MAX_FILE as usize + 1)).is_err());
        // Bad ids, temporary ids, duplicate ids and rules to unknown
        // containers are dropped; bad colours are fixed.
        let hostile = r##"{"containers":[
            {"id":"../../etc","name":"x","color":"blue","icon":"circle"},
            {"id":"tmp-1","name":"t","color":"blue","icon":"circle"},
            {"id":"ok","name":"Ok","color":"#fff","icon":"<b>"},
            {"id":"ok","name":"Dup","color":"red","icon":"pet"}],
            "rules":[{"host":"a.example","container":"nope"},{"host":"b.example","container":"ok"},{"host":"bad host","container":"ok"}]}"##;
        let store = parse(hostile).unwrap();
        assert_eq!(store.containers.len(), 1);
        assert_eq!(store.containers[0].color, "blue");
        assert_eq!(store.containers[0].icon, "circle");
        assert_eq!(store.rules, vec![SiteRule { host: "b.example".into(), container: "ok".into() }]);
        assert!(data_dir("../x").is_err());
        reset();
    }

    #[test]
    fn temporary_containers_and_their_data_go_away() {
        let _g = TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset();
        let t1 = create_temporary();
        let t2 = create_temporary();
        assert!(t1.temporary && t1.id.starts_with("tmp-"));
        assert_eq!((t1.name.as_str(), t2.name.as_str()), ("Temporary 1", "Temporary 2"));
        assert!(get(&t1.id).is_some());
        assert!(all().is_empty(), "never part of the persistent list");
        end_temporary(&t1.id);
        assert!(get(&t1.id).is_none());
        assert_eq!(create_temporary().name, "Temporary 1", "names are reused");

        let base = tempfile::tempdir().unwrap();
        let dir = temporary_data_dir_in(base.path()).unwrap();
        let path = dir.path().to_path_buf();
        std::fs::write(path.join("cookies"), "secret=1").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o700);
        }
        drop(dir);
        assert!(!path.exists(), "deleted with its last tab");

        // A crashed process's folders are swept, a running one's are kept.
        let stale = base.path().join(format!("{TEMP_PREFIX}4294967295-abc"));
        let live = base.path().join(format!("{TEMP_PREFIX}{}-def", std::process::id()));
        let other = base.path().join("unrelated-4294967295");
        for d in [&stale, &live, &other] {
            std::fs::create_dir_all(d.join("sub")).unwrap();
        }
        let removed = sweep_stale_in(base.path(), &|pid| pid == std::process::id());
        assert_eq!(removed, 1);
        assert!(!stale.exists() && live.exists() && other.exists());
        reset();
    }
}
