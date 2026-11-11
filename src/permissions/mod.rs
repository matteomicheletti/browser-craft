//! Site permissions: what a page may use (camera, microphone, screen,
//! location, notifications, clipboard, storage access), asked through a
//! prompt bar under the toolbar and, when the user ticks "Remember for this
//! site", kept per host in `permissions.json` (owner-only, replaced
//! atomically).
//!
//! Nothing is ever allowed without an "allow" saved by the user, or the
//! user's click on the prompt itself. The engine hook that holds WebKit's
//! requests is `webkit.rs` (Linux); other platforms never ask (their
//! engines keep their own defaults).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

#[cfg(target_os = "linux")]
pub mod webkit;

/// What a site asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Microphone,
    Camera,
    Screen,
    Location,
    Notifications,
    Clipboard,
    Storage,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::Microphone,
        Kind::Camera,
        Kind::Screen,
        Kind::Location,
        Kind::Notifications,
        Kind::Clipboard,
        Kind::Storage,
    ];

    /// The name in `permissions.json`. Never rename.
    pub fn key(self) -> &'static str {
        match self {
            Kind::Microphone => "microphone",
            Kind::Camera => "camera",
            Kind::Screen => "screen",
            Kind::Location => "location",
            Kind::Notifications => "notifications",
            Kind::Clipboard => "clipboard",
            Kind::Storage => "storage-access",
        }
    }

    pub fn from_key(key: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Microphone => "Microphone",
            Kind::Camera => "Camera",
            Kind::Screen => "Screen sharing",
            Kind::Location => "Location",
            Kind::Notifications => "Notifications",
            Kind::Clipboard => "Clipboard",
            Kind::Storage => "Cookies in other sites",
        }
    }

    /// What the prompt says the site wants to do.
    fn phrase(self) -> &'static str {
        match self {
            Kind::Microphone => "your microphone",
            Kind::Camera => "your camera",
            Kind::Screen => "share your screen",
            Kind::Location => "know your location",
            Kind::Notifications => "show notifications",
            Kind::Clipboard => "see text and images copied to the clipboard",
            Kind::Storage => "use its cookies and site data on the page you are visiting",
        }
    }

    /// Whether an "allow" may be remembered. Screen sharing is asked every
    /// time (as Chrome does): a saved allow would let the site start
    /// capturing the whole screen silently.
    pub fn allow_rememberable(self) -> bool {
        self != Kind::Screen
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Block,
}

impl Decision {
    pub fn key(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Block => "block",
        }
    }

    fn from_key(key: &str) -> Option<Decision> {
        match key {
            "allow" => Some(Decision::Allow),
            "block" => Some(Decision::Block),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Decision::Allow => "Allowed",
            Decision::Block => "Blocked",
        }
    }
}

/// "<host> wants to use your microphone and camera".
pub fn prompt_text(host: &str, kinds: &[Kind]) -> String {
    let uses: Vec<&str> = kinds
        .iter()
        .filter(|k| matches!(k, Kind::Microphone | Kind::Camera))
        .map(|k| k.phrase())
        .collect();
    let mut parts = Vec::new();
    if !uses.is_empty() {
        parts.push(format!("use {}", uses.join(" and ").replacen(" and your ", " and ", 1)));
    }
    for k in kinds.iter().filter(|k| !matches!(k, Kind::Microphone | Kind::Camera)) {
        parts.push(k.phrase().to_string());
    }
    if parts.is_empty() {
        return format!("{host} wants a permission");
    }
    format!("{host} wants to {}", parts.join(" and "))
}

// ---- the saved decisions ---------------------------------------------------

/// The longest list kept: a site cannot fill the disk by asking.
const MAX_SITES: usize = 2000;
const FILE_NAME: &str = "permissions.json";

/// Decisions per host, then per kind.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Store {
    sites: BTreeMap<String, BTreeMap<Kind, Decision>>,
}

impl Store {
    pub fn get(&self, host: &str, kind: Kind) -> Option<Decision> {
        self.sites.get(host).and_then(|m| m.get(&kind)).copied()
    }

    /// Saves (or with `None` forgets) a decision. A screen-sharing allow
    /// is never saved; a new host past the cap is refused.
    pub fn set(&mut self, host: &str, kind: Kind, decision: Option<Decision>) -> bool {
        let host = host.trim().to_ascii_lowercase();
        if host.is_empty() {
            return false;
        }
        match decision {
            Some(Decision::Allow) if !kind.allow_rememberable() => false,
            Some(d) => {
                if !self.sites.contains_key(&host) && self.sites.len() >= MAX_SITES {
                    return false;
                }
                self.sites.entry(host).or_default().insert(kind, d) != Some(d)
            }
            None => {
                let Some(map) = self.sites.get_mut(&host) else { return false };
                let changed = map.remove(&kind).is_some();
                if map.is_empty() {
                    self.sites.remove(&host);
                }
                changed
            }
        }
    }

    pub fn reset_host(&mut self, host: &str) -> bool {
        self.sites.remove(host).is_some()
    }

    /// Every saved decision, by host then kind.
    pub fn entries(&self) -> Vec<(String, Kind, Decision)> {
        self.sites
            .iter()
            .flat_map(|(h, m)| m.iter().map(move |(k, d)| (h.clone(), *k, *d)))
            .collect()
    }

    /// What to do with a request for `kinds` without asking: allow when
    /// every kind has a saved allow, block when any is blocked, else ask
    /// (`None`).
    pub fn decide(&self, host: &str, kinds: &[Kind]) -> Option<bool> {
        if kinds.is_empty() {
            return Some(false);
        }
        let saved: Vec<Option<Decision>> = kinds.iter().map(|k| self.get(host, *k)).collect();
        if saved.contains(&Some(Decision::Block)) {
            Some(false)
        } else if saved.iter().all(|d| *d == Some(Decision::Allow)) {
            Some(true)
        } else {
            None
        }
    }

    /// Device information (the names of cameras and microphones) follows
    /// the camera and microphone decisions: shown when either is allowed,
    /// never asked for on its own.
    pub fn device_info(&self, host: &str) -> bool {
        [Kind::Camera, Kind::Microphone].iter().any(|k| self.get(host, *k) == Some(Decision::Allow))
    }

    pub fn parse(text: &str) -> Store {
        let raw: BTreeMap<String, serde_json::Value> = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| v.get("sites").cloned())
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let mut store = Store::default();
        for (host, kinds) in raw {
            let Some(kinds) = kinds.as_object() else { continue };
            for (k, d) in kinds {
                if let (Some(kind), Some(decision)) = (Kind::from_key(k), d.as_str().and_then(Decision::from_key)) {
                    store.set(&host, kind, Some(decision));
                }
            }
        }
        store
    }

    pub fn serialize(&self) -> String {
        let sites: serde_json::Map<String, serde_json::Value> = self
            .sites
            .iter()
            .map(|(h, m)| {
                let kinds: serde_json::Map<String, serde_json::Value> =
                    m.iter().map(|(k, d)| (k.key().to_string(), d.key().into())).collect();
                (h.clone(), kinds.into())
            })
            .collect();
        let mut text = serde_json::to_string_pretty(&serde_json::json!({ "version": 1, "sites": sites })).unwrap_or_default();
        text.push('\n');
        text
    }
}

static STORE: RwLock<Option<Store>> = RwLock::new(None);
static SAVE_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn file_path() -> Result<PathBuf, String> {
    crate::storage::config_file(FILE_NAME)
}

fn load_from(path: &Path) -> Store {
    crate::storage::read_text_limited(path, crate::storage::MAX_SETTINGS_FILE)
        .map(|text| Store::parse(&text))
        .unwrap_or_default()
}

/// The saved decisions (loaded from disk on first use).
pub fn store() -> Store {
    if let Some(store) = STORE.read().unwrap_or_else(|p| p.into_inner()).as_ref() {
        return store.clone();
    }
    let loaded = file_path().map(|p| load_from(&p)).unwrap_or_default();
    let mut slot = STORE.write().unwrap_or_else(|p| p.into_inner());
    slot.get_or_insert(loaded).clone()
}

/// Applies `change` to the saved decisions and writes them; on failure
/// nothing changes.
fn update(change: impl FnOnce(&mut Store) -> bool) -> Result<bool, String> {
    let mut next = store();
    if !change(&mut next) {
        return Ok(false);
    }
    let mut slot = STORE.write().unwrap_or_else(|p| p.into_inner());
    let result = file_path().and_then(|path| {
        crate::storage::write_private(&path, next.serialize().as_bytes())
            .map_err(|e| format!("Could not save the site permissions to {}: {e}", path.display()))
    });
    *SAVE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = result.as_ref().err().cloned();
    result?;
    *slot = Some(next);
    drop(slot);
    // Requests waiting for a prompt that the new decisions answer.
    #[cfg(target_os = "linux")]
    webkit::settle_saved();
    Ok(true)
}

pub fn set(host: &str, kind: Kind, decision: Option<Decision>) -> Result<bool, String> {
    update(|s| s.set(host, kind, decision))
}

pub fn reset_host(host: &str) -> Result<bool, String> {
    update(|s| s.reset_host(host))
}

pub fn reset_all() -> Result<bool, String> {
    update(|s| std::mem::take(s) != Store::default())
}

// ---- prompts (the engine hook holds the requests) --------------------------

/// A request waiting for the user, as the prompt bar shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Prompt {
    pub id: u64,
    pub host: String,
    pub kinds: Vec<Kind>,
}

impl Prompt {
    pub fn text(&self) -> String {
        prompt_text(&self.host, &self.kinds)
    }

    /// Whether "Remember for this site" can save an allow.
    pub fn rememberable(&self) -> bool {
        self.kinds.iter().all(|k| k.allow_rememberable())
    }
}

/// The oldest request of tab `tab` still waiting for an answer.
pub fn prompt_for(tab: u64) -> Option<Prompt> {
    #[cfg(target_os = "linux")]
    return webkit::prompt_for(tab);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = tab;
        None
    }
}

/// The user answered prompt `id`. With `remember`, the decision is saved
/// for the site (an allow of screen sharing never is).
pub fn answer(id: u64, allow: bool, remember: bool) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let prompt = webkit::answer(id, allow);
    #[cfg(not(target_os = "linux"))]
    let prompt: Option<Prompt> = {
        let _ = (id, allow);
        None
    };
    let Some(prompt) = prompt else { return Ok(()) };
    if remember {
        let decision = if allow { Decision::Allow } else { Decision::Block };
        update(|s| prompt.kinds.iter().fold(false, |changed, k| s.set(&prompt.host, *k, Some(decision)) | changed))?;
    }
    Ok(())
}

// ---- WebRTC availability ---------------------------------------------------

/// Sites whose calls need WebRTC (`RTCPeerConnection`).
const CALL_HOSTS: &[&str] = &[
    "teams.microsoft.com",
    "teams.live.com",
    "teams.cloud.microsoft",
    "meet.google.com",
    "zoom.us",
    "zoom.com",
    "meet.jit.si",
    "8x8.vc",
];

/// Whether `host` (or a parent domain of it) is a call site.
pub fn is_call_host(host: &str) -> bool {
    CALL_HOSTS
        .iter()
        .any(|h| host == *h || host.strip_suffix(h).is_some_and(|rest| rest.ends_with('.')))
}

/// What is known about the engine's WebRTC: 0 unknown, 1 present, 2 missing.
static WEBRTC: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn webrtc_known() -> Option<bool> {
    match WEBRTC.load(std::sync::atomic::Ordering::Relaxed) {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

pub fn set_webrtc(present: bool) {
    WEBRTC.store(if present { 1 } else { 2 }, std::sync::atomic::Ordering::Relaxed);
}

/// The WebRTC notice is shown once per run.
static WEBRTC_NOTICE_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True the first time it is called.
pub fn take_webrtc_notice() -> bool {
    !WEBRTC_NOTICE_SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed)
}

// ---- browsecraft://flags ----------------------------------------------------

/// Applies the settings page's `perm-reset=<host>` / `perm-reset=<host> <kind>`
/// and `perm-reset-all=1` fields. Returns whether anything changed.
pub fn apply_form_pairs(pairs: &[(String, String)]) -> Result<bool, String> {
    let mut changed = false;
    for (k, v) in pairs {
        match k.as_str() {
            "perm-reset-all" => changed |= reset_all()?,
            "perm-reset" => match v.split_once(' ') {
                Some((host, kind)) => {
                    let kind = Kind::from_key(kind).ok_or("Not a site permission")?;
                    changed |= set(host, kind, None)?;
                }
                None => changed |= reset_host(v)?,
            },
            _ => {}
        }
    }
    Ok(changed)
}

/// The "Site permissions" card of the settings page. `to(key, value)` is a
/// link carrying the form token.
pub fn settings_card(to: &dyn Fn(&str, &str) -> String) -> String {
    use crate::internal::{encode, escape};
    let entries = store().entries();
    let mut rows = String::new();
    if entries.is_empty() {
        rows += "<div class=\"row first\"><div class=\"grow\"><div class=\"d\">No site has a saved permission. Sites ask when they need one.</div></div></div>";
    }
    let mut first = true;
    let mut last_host = "";
    for (host, kind, decision) in &entries {
        let reset_site = if host != last_host {
            format!(" <a class=\"btn ghost\" href=\"{}\">Reset site</a>", escape(&to("perm-reset", &encode(host))))
        } else {
            String::new()
        };
        last_host = host;
        rows += &format!(
            "<div class=\"row{}\"><div class=\"grow\"><div class=\"t\">{}</div><div class=\"d\">{} · {}</div></div>\
             <a class=\"btn ghost\" href=\"{}\">Reset</a>{reset_site}</div>",
            if std::mem::take(&mut first) { " first" } else { "" },
            escape(host),
            escape(kind.label()),
            decision.label(),
            escape(&to("perm-reset", &encode(&format!("{host} {}", kind.key())))),
        );
    }
    let all = if entries.is_empty() {
        String::new()
    } else {
        format!("<div class=\"row\"><div class=\"grow\"></div><a class=\"btn ghost\" href=\"{}\">Reset all</a></div>", escape(&to("perm-reset-all", "1")))
    };
    let error = SAVE_ERROR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .map(|e| format!("<div class=\"bad\" role=alert>{}</div>", escape(&e)))
        .unwrap_or_default();
    format!(
        "<div class=\"card\"><h2>Site permissions</h2>\
         <p class=\"d\">Camera, microphone, location and the others, as you answered a site's prompt with \
         \"Remember for this site\". Reset one and the site asks again. Also in the command palette: \
         Site permissions for this site.</p>{error}{rows}{all}</div>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_text_reads_like_chrome() {
        assert_eq!(
            prompt_text("teams.microsoft.com", &[Kind::Microphone, Kind::Camera]),
            "teams.microsoft.com wants to use your microphone and camera"
        );
        assert_eq!(prompt_text("a.test", &[Kind::Camera]), "a.test wants to use your camera");
        assert_eq!(prompt_text("a.test", &[Kind::Screen]), "a.test wants to share your screen");
        assert_eq!(
            prompt_text("a.test", &[Kind::Microphone, Kind::Screen]),
            "a.test wants to use your microphone and share your screen"
        );
        assert_eq!(prompt_text("a.test", &[Kind::Location]), "a.test wants to know your location");
    }

    #[test]
    fn nothing_is_allowed_without_a_saved_allow() {
        let mut s = Store::default();
        assert_eq!(s.decide("a.test", &[Kind::Camera]), None);
        assert_eq!(s.decide("a.test", &[]), Some(false));
        s.set("a.test", Kind::Camera, Some(Decision::Allow));
        assert_eq!(s.decide("a.test", &[Kind::Camera]), Some(true));
        assert_eq!(s.decide("a.test", &[Kind::Camera, Kind::Microphone]), None, "the microphone is still asked");
        assert_eq!(s.decide("b.test", &[Kind::Camera]), None, "per host");
        assert_eq!(s.decide("sub.a.test", &[Kind::Camera]), None, "not for subdomains");
        s.set("a.test", Kind::Microphone, Some(Decision::Block));
        assert_eq!(s.decide("a.test", &[Kind::Camera, Kind::Microphone]), Some(false));
    }

    #[test]
    fn device_info_follows_camera_and_microphone() {
        let mut s = Store::default();
        assert!(!s.device_info("a.test"));
        s.set("a.test", Kind::Microphone, Some(Decision::Block));
        assert!(!s.device_info("a.test"));
        s.set("a.test", Kind::Camera, Some(Decision::Allow));
        assert!(s.device_info("a.test"));
    }

    #[test]
    fn screen_sharing_allow_is_never_saved() {
        let mut s = Store::default();
        assert!(!s.set("a.test", Kind::Screen, Some(Decision::Allow)));
        assert!(s.set("a.test", Kind::Screen, Some(Decision::Block)));
        assert_eq!(s.get("a.test", Kind::Screen), Some(Decision::Block));
        assert!(!Prompt { id: 1, host: "a.test".into(), kinds: vec![Kind::Screen] }.rememberable());
        // Not even from a hand-edited file.
        let s = Store::parse(r#"{"sites":{"a.test":{"screen":"allow"}}}"#);
        assert_eq!(s.get("a.test", Kind::Screen), None);
    }

    #[test]
    fn file_round_trips_and_skips_junk() {
        let mut s = Store::default();
        s.set("Teams.Microsoft.com", Kind::Microphone, Some(Decision::Allow));
        s.set("teams.microsoft.com", Kind::Camera, Some(Decision::Block));
        s.set("maps.test", Kind::Location, Some(Decision::Allow));
        let text = s.serialize();
        assert!(text.contains("\"microphone\": \"allow\""), "{text}");
        assert_eq!(Store::parse(&text), s);
        let junk = Store::parse(r#"{"sites":{"a.test":{"camera":"maybe","warp":"allow","location":"block"},"b":7}}"#);
        assert_eq!(junk.entries(), vec![("a.test".to_string(), Kind::Location, Decision::Block)]);
        assert_eq!(Store::parse("not json"), Store::default());
        // Forgetting the last decision drops the host.
        s.set("maps.test", Kind::Location, None);
        assert!(s.entries().iter().all(|(h, _, _)| h != "maps.test"));
        assert!(s.reset_host("teams.microsoft.com"));
        assert!(s.entries().is_empty());
    }

    #[test]
    fn file_is_written_owner_only_and_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join(FILE_NAME);
        let mut s = Store::default();
        s.set("a.test", Kind::Camera, Some(Decision::Allow));
        crate::storage::write_private(&path, s.serialize().as_bytes()).unwrap();
        assert_eq!(load_from(&path), s);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        }
    }

    #[test]
    fn call_hosts_match_subdomains_only() {
        assert!(is_call_host("teams.microsoft.com"));
        assert!(is_call_host("us05web.zoom.us"));
        assert!(is_call_host("meet.google.com"));
        assert!(!is_call_host("notzoom.us"));
        assert!(!is_call_host("google.com"));
        assert!(!is_call_host("microsoft.com"));
    }

    #[test]
    fn kinds_have_stable_keys() {
        for k in Kind::ALL {
            assert_eq!(Kind::from_key(k.key()), Some(k));
        }
    }
}
