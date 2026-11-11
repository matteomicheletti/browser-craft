//! `plugin.toml`: parsing, validation and loading of a plugin folder.
//!
//! Everything here treats the folder as untrusted: size limits on every file,
//! relative paths only, and a canonical-path check so a symlink cannot point a
//! script, style or WASM entry outside the plugin folder.

use super::pattern::MatchPattern;
use super::permissions::{self, Permission, Requested};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const MANIFEST_FILE: &str = "plugin.toml";
pub const MAX_MANIFEST: u64 = 64 * 1024;
/// One script or style file.
pub const MAX_SOURCE: u64 = 1024 * 1024;
pub const MAX_WASM: u64 = 4 * 1024 * 1024;
/// A whole plugin folder (install and dev loading).
pub const MAX_TOTAL: u64 = 16 * 1024 * 1024;
pub const MAX_FILES: usize = 500;

// ---- versions ----

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: String,
}

impl Version {
    /// `MAJOR.MINOR.PATCH[-pre][+build]`, numbers without leading zeros.
    pub fn parse(s: &str) -> Result<Self, String> {
        let bad = || format!("version \"{s}\" is not semver (MAJOR.MINOR.PATCH)");
        if s.len() > 64 {
            return Err(bad());
        }
        let core = s.split('+').next().unwrap_or("");
        let (nums, pre) = match core.split_once('-') {
            Some((n, p)) => (n, p),
            None => (core, ""),
        };
        let mut it = nums.split('.');
        let mut num = || -> Result<u64, String> {
            let p = it.next().ok_or_else(bad)?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || (p.len() > 1 && p.starts_with('0')) {
                return Err(bad());
            }
            p.parse().map_err(|_| bad())
        };
        let (major, minor, patch) = (num()?, num()?, num()?);
        if it.next().is_some() {
            return Err(bad());
        }
        if core.contains('-') && (pre.is_empty() || !pre.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')) {
            return Err(bad());
        }
        Ok(Self { major, minor, patch, pre: pre.into() })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre)?;
        }
        Ok(())
    }
}

/// `"1"` or `"1.2"`: the plugin API the plugin was written for.
fn parse_api(s: &str) -> Result<(u32, u32), String> {
    let bad = || format!("api_version \"{s}\" must look like \"1\" or \"1.0\"");
    let (maj, min) = match s.split_once('.') {
        Some((a, b)) => (a, b),
        None => (s, "0"),
    };
    let n = |x: &str| {
        (!x.is_empty() && x.len() < 6 && x.bytes().all(|b| b.is_ascii_digit()))
            .then(|| x.parse::<u32>().ok())
            .flatten()
            .ok_or_else(bad)
    };
    let (major, minor) = (n(maj)?, n(min)?);
    if major != permissions::API_MAJOR {
        return Err(format!(
            "api_version {major}.{minor} is not supported: this browser implements API {}.{}",
            permissions::API_MAJOR,
            permissions::API_MINOR
        ));
    }
    if minor > permissions::API_MINOR {
        return Err(format!(
            "plugin needs API {major}.{minor}, this browser implements {}.{}: update the browser",
            permissions::API_MAJOR,
            permissions::API_MINOR
        ));
    }
    Ok((major, minor))
}

// ---- ids ----

/// Reverse-DNS: at least two dot-separated segments of `[a-z0-9_-]`.
pub fn valid_id(id: &str) -> bool {
    id.len() <= 100
        && id.split('.').count() >= 2
        && id.split('.').all(|seg| {
            !seg.is_empty()
                && seg.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
                && !seg.starts_with(['-', '_'])
        })
}

// ---- manifest ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunAt {
    DocumentStart,
    #[default]
    DocumentEnd,
    Idle,
}

impl RunAt {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DocumentStart => "document_start",
            Self::DocumentEnd => "document_end",
            Self::Idle => "idle",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ContentScript {
    pub matches: Vec<MatchPattern>,
    pub exclude_matches: Vec<MatchPattern>,
    pub js: Vec<String>,
    pub css: Vec<String>,
    pub run_at: RunAt,
    pub all_frames: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionKind {
    Bool,
    String,
    Number,
    Choice,
}

/// One user-editable setting, rendered in the manager and handed to the
/// plugin as `bc.settings.<key>`.
#[derive(Clone, Debug)]
pub struct OptionDef {
    pub key: String,
    pub kind: OptionKind,
    pub label: String,
    pub default: Value,
    pub choices: Vec<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl OptionDef {
    /// Checks a user-supplied value against the definition.
    pub fn check(&self, v: &Value) -> Result<Value, String> {
        match (self.kind, v) {
            (OptionKind::Bool, Value::Bool(_)) => Ok(v.clone()),
            (OptionKind::String, Value::String(s)) if s.len() <= 1024 && !s.contains('\0') => Ok(v.clone()),
            (OptionKind::Number, Value::Number(n)) => {
                let x = n.as_f64().filter(|x| x.is_finite()).ok_or("number out of range")?;
                if self.min.is_some_and(|m| x < m) || self.max.is_some_and(|m| x > m) {
                    return Err(format!("{} must be between {} and {}", self.key, self.min.unwrap_or(f64::MIN), self.max.unwrap_or(f64::MAX)));
                }
                Ok(v.clone())
            }
            (OptionKind::Choice, Value::String(s)) if self.choices.contains(s) => Ok(v.clone()),
            _ => Err(format!("invalid value for setting \"{}\"", self.key)),
        }
    }
}

/// How a plugin page opens (`[[pages]] open_as`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OpenAs {
    /// A normal tab showing `bc-plugin://<id>/<path>`.
    #[default]
    Tab,
    /// A view centred over the page, which is dimmed; Esc closes it.
    Modal,
}

/// A page of the plugin's own UI (API 1.1), served from its folder.
#[derive(Clone, Debug, PartialEq)]
pub struct PageDef {
    pub id: String,
    /// HTML file, relative to the plugin folder.
    pub path: String,
    pub title: String,
    pub open_as: OpenAs,
    /// Modal size in CSS pixels (shrunk to fit the page area).
    pub width: u32,
    pub height: u32,
}

/// A command the plugin adds to the palette and/or binds to keys (API 1.1).
#[derive(Clone, Debug, PartialEq)]
pub struct CommandDef {
    pub id: String,
    pub title: String,
    pub shortcut: Option<super::shortcuts::KeyCombo>,
    pub palette: bool,
    /// The page it opens (or, when that page is open, sends the command to).
    pub page: String,
    /// A bool setting that must be on for the shortcut to apply.
    pub when: Option<String>,
}

pub const MAX_PAGES: usize = 16;
pub const MAX_COMMANDS: usize = 32;

#[derive(Clone, Debug)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: Version,
    pub api: (u32, u32),
    pub description: String,
    pub author: String,
    pub homepage: String,
    pub permissions: Vec<Requested>,
    pub content_scripts: Vec<ContentScript>,
    pub options: Vec<OptionDef>,
    /// Path of the optional WASM module, relative to the plugin folder.
    pub wasm: Option<String>,
    pub pages: Vec<PageDef>,
    pub commands: Vec<CommandDef>,
    /// The page that edits the browser's screenshots (`screenshot.edit`).
    pub screenshot_editor: Option<String>,
    /// Extra https origins the pages may load scripts, styles, images and
    /// data from (added to their Content-Security-Policy).
    pub remote_sources: Vec<String>,
}

impl Manifest {
    pub fn page(&self, id: &str) -> Option<&PageDef> {
        self.pages.iter().find(|p| p.id == id)
    }

    pub fn command(&self, id: &str) -> Option<&CommandDef> {
        self.commands.iter().find(|c| c.id == id)
    }

    pub fn has(&self, p: Permission) -> bool {
        self.permissions.contains(&Requested::Api(p))
    }

    /// Does any content script of this plugin apply to `url`?
    pub fn applies_to(&self, url: &str) -> bool {
        self.content_scripts
            .iter()
            .any(|c| super::pattern::matches_any(&c.matches, &c.exclude_matches, url))
    }

    /// Default values of all options.
    pub fn default_settings(&self) -> serde_json::Map<String, Value> {
        self.options.iter().map(|o| (o.key.clone(), o.default.clone())).collect()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    id: String,
    name: String,
    version: String,
    api_version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    homepage: String,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    content_scripts: Vec<RawScript>,
    #[serde(default)]
    options: Vec<RawOption>,
    wasm: Option<String>,
    #[serde(default)]
    pages: Vec<RawPage>,
    #[serde(default)]
    commands: Vec<RawCommand>,
    screenshot_editor: Option<String>,
    #[serde(default)]
    remote_sources: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPage {
    id: String,
    path: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    open_as: OpenAs,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    id: String,
    title: String,
    shortcut: Option<String>,
    #[serde(default = "yes")]
    palette: bool,
    page: String,
    when: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScript {
    #[serde(default)]
    matches: Vec<String>,
    #[serde(default)]
    exclude_matches: Vec<String>,
    #[serde(default)]
    js: Vec<String>,
    #[serde(default)]
    css: Vec<String>,
    #[serde(default)]
    run_at: RunAt,
    #[serde(default)]
    all_frames: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOption {
    key: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    label: String,
    default: Option<toml::Value>,
    #[serde(default)]
    choices: Vec<String>,
    min: Option<f64>,
    max: Option<f64>,
}

fn toml_to_json(v: &toml::Value) -> Option<Value> {
    Some(match v {
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => Value::from(*f),
        _ => return None,
    })
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') && !s.as_bytes()[0].is_ascii_digit()
}

impl Manifest {
    /// Parses and validates the text of a `plugin.toml`. All problems found
    /// are returned together so a developer can fix them in one pass.
    pub fn parse(text: &str) -> Result<Self, Vec<String>> {
        let raw: RawManifest = toml::from_str(text).map_err(|e| vec![format!("plugin.toml: {}", e.message())])?;
        let mut errs = Vec::new();
        let mut err = |s: String| errs.push(s);

        if !valid_id(&raw.id) {
            err(format!("id \"{}\" must be reverse-DNS, lowercase (e.g. com.example.my-plugin)", raw.id));
        } else if raw.id.starts_with("builtin.") {
            err("ids starting with \"builtin.\" are reserved for the browser's own components".into());
        }
        let len_ok = |s: &str, max: usize| s.chars().count() <= max && !s.contains(char::is_control);
        if raw.name.trim().is_empty() || !len_ok(&raw.name, 80) {
            err("name is required (at most 80 characters, no control characters)".into());
        }
        if !len_ok(&raw.description, 500) || !len_ok(&raw.author, 120) {
            err("description (500) or author (120) is too long or has control characters".into());
        }
        if !raw.homepage.is_empty() && !(raw.homepage.starts_with("https://") || raw.homepage.starts_with("http://")) || raw.homepage.len() > 300 {
            err("homepage must be an http(s) address".into());
        }
        let version = Version::parse(&raw.version).map_err(&mut err).ok();
        let api = parse_api(&raw.api_version).map_err(&mut err).ok();

        let mut permissions = Vec::new();
        if raw.permissions.len() > 32 {
            err("too many permissions (32 at most)".into());
        }
        for p in raw.permissions.iter().take(32) {
            match Requested::parse(p) {
                Ok(r) if !permissions.contains(&r) => permissions.push(r),
                Ok(_) => {}
                Err(e) => err(e),
            }
        }

        let mut scripts = Vec::new();
        if raw.content_scripts.len() > 16 {
            err("too many content_scripts (16 at most)".into());
        }
        let has = |p: Permission| permissions.contains(&Requested::Api(p));
        for (i, s) in raw.content_scripts.iter().take(16).enumerate() {
            let at = |m: &str| format!("content_scripts[{i}]: {m}");
            if s.matches.is_empty() {
                err(at("matches is required"));
            }
            if s.js.is_empty() && s.css.is_empty() {
                err(at("needs at least one js or css file"));
            }
            if s.matches.len() > 32 || s.exclude_matches.len() > 32 || s.js.len() > 16 || s.css.len() > 16 {
                err(at("too many patterns or files"));
                continue;
            }
            let mut pats = |list: &[String]| -> Vec<MatchPattern> {
                list.iter().filter_map(|p| MatchPattern::parse(p).map_err(|e| err(at(&e))).ok()).collect()
            };
            let (matches, exclude_matches) = (pats(&s.matches), pats(&s.exclude_matches));
            if !s.js.is_empty() && !(has(Permission::PageRead) || has(Permission::PageModify)) {
                err(at("js needs the page.read or page.modify permission"));
            }
            if !s.css.is_empty() && !has(Permission::PageModify) {
                err(at("css needs the page.modify permission"));
            }
            scripts.push(ContentScript {
                matches,
                exclude_matches,
                js: s.js.clone(),
                css: s.css.clone(),
                run_at: s.run_at,
                all_frames: s.all_frames,
            });
        }

        let mut options: Vec<OptionDef> = Vec::new();
        if raw.options.len() > 32 {
            err("too many options (32 at most)".into());
        }
        for o in raw.options.iter().take(32) {
            let at = |m: &str| format!("options \"{}\": {m}", o.key);
            if !is_ident(&o.key) || options.iter().any(|x| x.key == o.key) {
                err(at("key must be a unique identifier"));
                continue;
            }
            let kind = match o.kind.as_str() {
                "bool" => OptionKind::Bool,
                "string" => OptionKind::String,
                "number" => OptionKind::Number,
                "choice" => OptionKind::Choice,
                _ => {
                    err(at("type must be bool, string, number or choice"));
                    continue;
                }
            };
            if kind == OptionKind::Choice && (o.choices.is_empty() || o.choices.len() > 32) {
                err(at("choice options need 1 to 32 choices"));
                continue;
            }
            let Some(default) = o.default.as_ref().and_then(toml_to_json) else {
                err(at("a scalar default is required"));
                continue;
            };
            let def = OptionDef {
                key: o.key.clone(),
                kind,
                label: if o.label.is_empty() { o.key.clone() } else { o.label.chars().take(80).collect() },
                default,
                choices: o.choices.clone(),
                min: o.min,
                max: o.max,
            };
            match def.check(&def.default) {
                Ok(_) => options.push(def),
                Err(e) => err(at(&format!("default is invalid ({e})"))),
            }
        }

        let (pages, commands, screenshot_editor, remote_sources) = check_ui(&raw, api, &permissions, &options, &mut err);

        if let Some(w) = &raw.wasm
            && !w.ends_with(".wasm")
        {
            err("wasm must name a .wasm file".into());
        }
        let manifest_ok = errs.is_empty();
        match (manifest_ok, version, api) {
            (true, Some(version), Some(api)) => Ok(Self {
                id: raw.id,
                name: raw.name.trim().into(),
                version,
                api,
                description: raw.description,
                author: raw.author,
                homepage: raw.homepage,
                permissions,
                content_scripts: scripts,
                options,
                wasm: raw.wasm,
                pages,
                commands,
                screenshot_editor,
                remote_sources,
            }),
            _ => Err(errs),
        }
    }
}

/// Plain identifier for pages and commands: `[a-z0-9-]`, 1 to 40 characters.
fn is_slug(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

type Ui = (Vec<PageDef>, Vec<CommandDef>, Option<String>, Vec<String>);

/// Validates the API 1.1 user-interface keys: pages, commands, the
/// screenshot editor and remote sources.
fn check_ui(raw: &RawManifest, api: Option<(u32, u32)>, perms: &[Requested], options: &[OptionDef], err: &mut impl FnMut(String)) -> Ui {
    let has = |p: Permission| perms.contains(&Requested::Api(p));
    let uses_ui = !raw.pages.is_empty() || !raw.commands.is_empty() || raw.screenshot_editor.is_some() || !raw.remote_sources.is_empty();
    let new_perms = [Permission::ClipboardHistory, Permission::ScreenshotEdit, Permission::ShortcutsOverride].into_iter().any(has);
    if (uses_ui || new_perms) && api.is_some_and(|a| a < (1, 1)) {
        err("pages, commands and the clipboard.history, screenshot.edit and shortcuts.override permissions need api_version \"1.1\"".into());
    }
    if raw.pages.len() > MAX_PAGES || raw.commands.len() > MAX_COMMANDS {
        err(format!("at most {MAX_PAGES} pages and {MAX_COMMANDS} commands"));
    }
    let len_ok = |s: &str, max: usize| s.chars().count() <= max && !s.contains(char::is_control);
    let mut pages: Vec<PageDef> = Vec::new();
    for p in raw.pages.iter().take(MAX_PAGES) {
        let at = |m: &str| format!("pages \"{}\": {m}", p.id);
        if !is_slug(&p.id) || pages.iter().any(|x| x.id == p.id) {
            err(at("id must be unique, lowercase letters, digits and -"));
            continue;
        }
        if !(p.path.ends_with(".html") || p.path.ends_with(".htm")) || p.path.len() > 200 {
            err(at("path must name an .html file in the plugin folder"));
            continue;
        }
        if !len_ok(&p.title, 80) {
            err(at("title is at most 80 characters"));
        }
        let (w, h) = (p.width.unwrap_or(720), p.height.unwrap_or(520));
        if !(200..=4000).contains(&w) || !(150..=4000).contains(&h) {
            err(at("width must be 200 to 4000 and height 150 to 4000"));
        }
        let title = if p.title.trim().is_empty() { raw.name.trim().to_string() } else { p.title.trim().to_string() };
        pages.push(PageDef { id: p.id.clone(), path: p.path.clone(), title, open_as: p.open_as, width: w, height: h });
    }
    let mut commands: Vec<CommandDef> = Vec::new();
    for c in raw.commands.iter().take(MAX_COMMANDS) {
        let at = |m: &str| format!("commands \"{}\": {m}", c.id);
        if !is_slug(&c.id) || commands.iter().any(|x| x.id == c.id) {
            err(at("id must be unique, lowercase letters, digits and -"));
            continue;
        }
        if c.title.trim().is_empty() || !len_ok(&c.title, 80) {
            err(at("title is required (at most 80 characters)"));
        }
        if !pages.iter().any(|p| p.id == c.page) {
            err(at(&format!("page \"{}\" is not declared in [[pages]]", c.page)));
        }
        if let Some(w) = &c.when
            && !options.iter().any(|o| o.key == *w && o.kind == OptionKind::Bool)
        {
            err(at(&format!("when = \"{w}\" must name a bool option")));
        }
        let shortcut = match c.shortcut.as_deref().map(super::shortcuts::KeyCombo::parse) {
            None => None,
            Some(Err(e)) => {
                err(at(&e));
                None
            }
            Some(Ok(k)) => {
                if let Some(other) = commands.iter().find(|x| x.shortcut.as_ref() == Some(&k)) {
                    err(at(&format!("shortcut {} is already used by command \"{}\"", k.label(), other.id)));
                }
                if let Some(what) = super::shortcuts::builtin_use(&k)
                    && !has(Permission::ShortcutsOverride)
                {
                    err(at(&format!("{} is the browser's \"{what}\" key; taking it needs the shortcuts.override permission", k.label())));
                }
                Some(k)
            }
        };
        if !c.palette && shortcut.is_none() {
            err(at("a command needs a shortcut or palette = true"));
        }
        commands.push(CommandDef { id: c.id.clone(), title: c.title.trim().into(), shortcut, palette: c.palette, page: c.page.clone(), when: c.when.clone() });
    }
    if let Some(e) = &raw.screenshot_editor {
        if !pages.iter().any(|p| p.id == *e) {
            err(format!("screenshot_editor \"{e}\" is not declared in [[pages]]"));
        }
        if !has(Permission::ScreenshotEdit) {
            err("screenshot_editor needs the screenshot.edit permission".into());
        }
    }
    if has(Permission::ClipboardHistory) && pages.is_empty() {
        err("clipboard.history is only usable from a plugin page: declare one in [[pages]]".into());
    }
    let mut remote = Vec::new();
    if raw.remote_sources.len() > 8 {
        err("at most 8 remote_sources".into());
    }
    for r in raw.remote_sources.iter().take(8) {
        match url::Url::parse(r) {
            Ok(u) if u.scheme() == "https" && u.host_str().is_some_and(|h| !h.contains('*')) && matches!(u.path(), "" | "/") && u.query().is_none() && u.username().is_empty() => {
                remote.push(u.origin().ascii_serialization());
            }
            _ => err(format!("remote_sources: \"{r}\" must be an https origin such as https://cdn.example.com")),
        }
    }
    (pages, commands, raw.screenshot_editor.clone(), remote)
}

// ---- loading a folder ----

#[derive(Debug)]
pub struct LoadedScript {
    pub matches: Vec<MatchPattern>,
    pub exclude: Vec<MatchPattern>,
    pub run_at: RunAt,
    pub all_frames: bool,
    /// (file name, source)
    pub js: Vec<(String, String)>,
    pub css: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct LoadedPlugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub scripts: Vec<LoadedScript>,
    pub wasm: Option<Vec<u8>>,
}

/// Resolves a manifest-relative path inside `root` (already canonical). The
/// result is canonical too, so a symlink leading outside is refused.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let bad = |why: &str| format!("\"{rel}\": {why}");
    if rel.is_empty() || rel.len() > 200 || rel.contains(['\\', '\0']) || rel.contains(char::is_control) {
        return Err(bad("not a valid relative path"));
    }
    let p = Path::new(rel);
    if p.is_absolute() || rel.starts_with('/') {
        return Err(bad("absolute paths are not allowed"));
    }
    if p.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err(bad("\"..\" and \".\" are not allowed"));
    }
    let full = root.join(p).canonicalize().map_err(|_| bad("file not found"))?;
    if !full.starts_with(root) {
        return Err(bad("resolves outside the plugin folder (symlink?)"));
    }
    if !full.is_file() {
        return Err(bad("not a regular file"));
    }
    Ok(full)
}

fn read_source(root: &Path, rel: &str, max: u64) -> Result<String, String> {
    let path = safe_join(root, rel)?;
    crate::storage::read_text_limited(&path, max).map_err(|e| format!("\"{rel}\": {e}"))
}

/// Reads the manifest and every file it names. Errors are user-facing.
pub fn load_dir(dir: &Path) -> Result<LoadedPlugin, Vec<String>> {
    let root = dir.canonicalize().map_err(|e| vec![format!("{}: {e}", dir.display())])?;
    if !root.is_dir() {
        return Err(vec![format!("{} is not a folder", dir.display())]);
    }
    let mpath = safe_join(&root, MANIFEST_FILE).map_err(|_| vec![format!("no {MANIFEST_FILE} in {}", dir.display())])?;
    let text = crate::storage::read_text_limited(&mpath, MAX_MANIFEST).map_err(|e| vec![format!("{MANIFEST_FILE}: {e}")])?;
    let manifest = Manifest::parse(&text)?;
    let mut errs = Vec::new();
    let mut total = text.len() as u64;
    let mut scripts = Vec::new();
    for cs in &manifest.content_scripts {
        let mut load = |files: &[String], errs: &mut Vec<String>| -> Vec<(String, String)> {
            let mut out = Vec::new();
            for f in files {
                match read_source(&root, f, MAX_SOURCE) {
                    Ok(src) => {
                        total += src.len() as u64;
                        out.push((f.clone(), src));
                    }
                    Err(e) => errs.push(e),
                }
            }
            out
        };
        let js = load(&cs.js, &mut errs);
        let css = load(&cs.css, &mut errs);
        scripts.push(LoadedScript {
            matches: cs.matches.clone(),
            exclude: cs.exclude_matches.clone(),
            run_at: cs.run_at,
            all_frames: cs.all_frames,
            js,
            css,
        });
    }
    for p in &manifest.pages {
        if let Err(e) = safe_join(&root, &p.path) {
            errs.push(format!("pages \"{}\": {e}", p.id));
        }
    }
    let wasm = match &manifest.wasm {
        Some(w) => match safe_join(&root, w).and_then(|p| crate::storage::read_limited(&p, MAX_WASM).map_err(|e| format!("\"{w}\": {e}"))) {
            Ok(b) => {
                total += b.len() as u64;
                Some(b)
            }
            Err(e) => {
                errs.push(e);
                None
            }
        },
        None => None,
    };
    if total > MAX_TOTAL {
        errs.push(format!("plugin is larger than {} MB", MAX_TOTAL >> 20));
    }
    if errs.is_empty() { Ok(LoadedPlugin { manifest, dir: root, scripts, wasm }) } else { Err(errs) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
id = "com.example.hello"
name = "Hello"
version = "1.2.3-beta.1"
api_version = "1.0"
permissions = ["page.modify", "storage", "*://*.example.com/*"]

[[content_scripts]]
matches = ["*://*.example.com/*"]
exclude_matches = ["*://www.example.com/private/*"]
js = ["a.js"]
css = ["a.css"]
run_at = "document_start"

[[options]]
key = "color"
type = "choice"
choices = ["red", "blue"]
default = "red"

[[options]]
key = "size"
type = "number"
default = 10
min = 1
max = 20
"#;

    #[test]
    fn a_good_manifest_parses() {
        let m = Manifest::parse(GOOD).unwrap();
        assert_eq!(m.id, "com.example.hello");
        assert_eq!(m.version.to_string(), "1.2.3-beta.1");
        assert_eq!(m.permissions.len(), 3);
        assert!(m.has(Permission::Storage) && !m.has(Permission::TabsRead));
        assert_eq!(m.content_scripts[0].run_at, RunAt::DocumentStart);
        assert!(m.applies_to("https://shop.example.com/x"));
        assert!(!m.applies_to("https://www.example.com/private/x"));
        assert_eq!(m.default_settings()["size"], 10);
    }

    #[test]
    fn versions() {
        assert!(Version::parse("1.0.0").is_ok());
        assert!(Version::parse("0.10.2+build5").is_ok());
        assert!(Version::parse("1.0.0") < Version::parse("1.0.1"));
        for bad in ["1", "1.0", "01.0.0", "1.0.0.0", "a.b.c", "1.0.0-", "1.0.0-a b", ""] {
            assert!(Version::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ids() {
        assert!(valid_id("com.example.my-plugin"));
        assert!(valid_id("a.b"));
        for bad in ["hello", "Com.Example", "com..x", ".com.x", "com.-x", "com.ex ample", "com.example/../x"] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn api_versions() {
        assert!(parse_api("1").is_ok() && parse_api("1.0").is_ok());
        assert!(parse_api("2").unwrap_err().contains("not supported"));
        assert!(parse_api("1.9").unwrap_err().contains("update the browser"));
        assert!(parse_api("x").is_err());
    }

    fn with(replace: (&str, &str)) -> Result<Manifest, Vec<String>> {
        Manifest::parse(&GOOD.replace(replace.0, replace.1))
    }

    #[test]
    fn validation_reports_every_problem() {
        let e = Manifest::parse("id = \"x\"\nname = \"\"\nversion = \"1\"\napi_version = \"9\"\npermissions = [\"root\"]").unwrap_err();
        assert!(e.len() >= 5, "{e:?}");
        assert!(with(("id = \"com.example.hello\"", "id = \"builtin.evil\"")).unwrap_err()[0].contains("reserved"));
        assert!(with(("\"page.modify\", ", "")).unwrap_err().iter().any(|m| m.contains("page.modify")));
        assert!(with(("matches = [\"*://*.example.com/*\"]", "matches = [\"example.com\"]")).is_err());
        assert!(with(("default = \"red\"", "default = \"green\"")).is_err());
        assert!(with(("max = 20", "max = 5")).is_err());
        assert!(Manifest::parse("not toml [").is_err());
        assert!(Manifest::parse(&format!("{GOOD}\nunknown_key = 1")).is_err(), "typos are caught");
    }

    const UI: &str = r#"
id = "com.example.ui"
name = "UI"
version = "1.0.0"
api_version = "1.1"
permissions = ["clipboard.history", "shortcuts.override", "screenshot.edit"]
screenshot_editor = "editor"

[[pages]]
id = "manager"
path = "manager.html"
title = "Clipboard"
open_as = "modal"
width = 760
height = 520

[[pages]]
id = "editor"
path = "editor.html"

[[commands]]
id = "open"
title = "Open the clipboard manager"
shortcut = "Ctrl+V"
page = "manager"
when = "open_on_ctrl_v"

[[commands]]
id = "palette-only"
title = "Edit an image"
page = "editor"

[[options]]
key = "open_on_ctrl_v"
type = "bool"
default = true
"#;

    #[test]
    fn pages_and_commands() {
        let m = Manifest::parse(UI).unwrap();
        assert_eq!(m.pages.len(), 2);
        let p = m.page("manager").unwrap();
        assert_eq!((p.open_as, p.width, p.height, p.title.as_str()), (OpenAs::Modal, 760, 520, "Clipboard"));
        let e = m.page("editor").unwrap();
        assert_eq!((e.open_as, e.width, e.title.as_str()), (OpenAs::Tab, 720, "UI"), "defaults: tab, 720 wide, the plugin's name");
        let c = m.command("open").unwrap();
        assert_eq!(c.shortcut.as_ref().map(|k| k.key.as_str()), Some("v"));
        assert!(c.palette && c.when.as_deref() == Some("open_on_ctrl_v"));
        assert_eq!(m.screenshot_editor.as_deref(), Some("editor"));
    }

    #[test]
    fn ui_validation() {
        let with = |a: &str, b: &str| Manifest::parse(&UI.replace(a, b));
        let has = |r: Result<Manifest, Vec<String>>, what: &str| r.unwrap_err().iter().any(|e| e.contains(what));
        assert!(has(with("\"shortcuts.override\", ", ""), "shortcuts.override"), "Ctrl+V needs the override permission");
        assert!(has(with("api_version = \"1.1\"", "api_version = \"1.0\""), "1.1"));
        assert!(has(with("page = \"manager\"", "page = \"nope\""), "not declared"));
        assert!(has(with("when = \"open_on_ctrl_v\"", "when = \"missing\""), "bool option"));
        assert!(has(with("shortcut = \"Ctrl+V\"", "shortcut = \"V\""), "Ctrl or Alt"));
        assert!(has(with("id = \"editor\"\npath", "id = \"manager\"\npath"), "unique"));
        assert!(has(with("path = \"editor.html\"", "path = \"editor.js\""), ".html"));
        assert!(has(with("width = 760", "width = 10"), "width"));
        assert!(has(with("open_as = \"modal\"", "open_as = \"window\""), "window"));
        assert!(has(with(", \"screenshot.edit\"", ""), "screenshot.edit"));
        assert!(has(with("screenshot_editor = \"editor\"", "screenshot_editor = \"x\""), "screenshot_editor"));
        assert!(has(with("title = \"Edit an image\"", "title = \"Edit an image\"\npalette = false"), "shortcut or palette"));
        let dup = UI.replace("title = \"Edit an image\"", "title = \"Edit an image\"\nshortcut = \"Ctrl+V\"");
        assert!(has(Manifest::parse(&dup), "already used"));
        assert!(has(with("screenshot_editor = \"editor\"", "screenshot_editor = \"editor\"\nremote_sources = [\"http://x.test\"]"), "https origin"));
        let remote = Manifest::parse(&UI.replace("screenshot_editor = \"editor\"", "screenshot_editor = \"editor\"\nremote_sources = [\"https://cdn.x.test/\"]")).unwrap();
        assert_eq!(remote.remote_sources, ["https://cdn.x.test"]);
        // clipboard.history without a page has nowhere to be used.
        let bare = "id = \"a.b\"\nname = \"B\"\nversion = \"1.0.0\"\napi_version = \"1.1\"\npermissions = [\"clipboard.history\"]";
        assert!(has(Manifest::parse(bare), "plugin page"));
    }

    #[test]
    fn page_files_must_exist() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("plugin.toml"), UI).unwrap();
        std::fs::write(d.path().join("manager.html"), "m").unwrap();
        assert!(load_dir(d.path()).unwrap_err().iter().any(|e| e.contains("editor.html")));
        std::fs::write(d.path().join("editor.html"), "e").unwrap();
        assert!(load_dir(d.path()).is_ok());
    }

    #[test]
    fn js_without_page_permission_is_rejected() {
        let t = GOOD.replace("\"page.modify\", ", "");
        let e = Manifest::parse(&t).unwrap_err();
        assert!(e.iter().any(|m| m.contains("js needs")));
    }

    #[test]
    fn option_values_are_checked() {
        let m = Manifest::parse(GOOD).unwrap();
        let size = &m.options[1];
        assert!(size.check(&Value::from(5)).is_ok());
        assert!(size.check(&Value::from(50)).is_err());
        assert!(size.check(&Value::from("5")).is_err());
        assert!(m.options[0].check(&Value::from("blue")).is_ok());
        assert!(m.options[0].check(&Value::from("pink")).is_err());
    }

    #[test]
    fn safe_join_blocks_traversal_and_symlinks() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("p");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/a.js"), "1").unwrap();
        std::fs::write(d.path().join("secret.js"), "s").unwrap();
        let root = root.canonicalize().unwrap();
        assert!(safe_join(&root, "sub/a.js").is_ok());
        for bad in ["../secret.js", "/etc/passwd", "sub/../../secret.js", "", "sub\\a.js", "./sub/a.js", "missing.js", "sub"] {
            assert!(safe_join(&root, bad).is_err(), "{bad}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.path().join("secret.js"), root.join("link.js")).unwrap();
            assert!(safe_join(&root, "link.js").unwrap_err().contains("outside"));
            std::os::unix::fs::symlink("sub/a.js", root.join("inner.js")).unwrap();
            assert!(safe_join(&root, "inner.js").is_ok(), "links inside the folder are fine");
        }
    }

    #[test]
    fn load_dir_reads_files_and_limits_sizes() {
        let d = tempfile::tempdir().unwrap();
        let t = r#"id = "com.t.x"
name = "X"
version = "1.0.0"
api_version = "1"
permissions = ["page.modify"]
[[content_scripts]]
matches = ["<all_urls>"]
js = ["a.js"]
"#;
        std::fs::write(d.path().join("plugin.toml"), t).unwrap();
        assert!(load_dir(d.path()).unwrap_err()[0].contains("not found"));
        std::fs::write(d.path().join("a.js"), "1").unwrap();
        let p = load_dir(d.path()).unwrap();
        assert_eq!(p.scripts[0].js[0].1, "1");
        std::fs::write(d.path().join("a.js"), vec![b'x'; (MAX_SOURCE + 1) as usize]).unwrap();
        assert!(load_dir(d.path()).unwrap_err()[0].contains("larger"));
        assert!(load_dir(&d.path().join("nope")).is_err());
    }
}
