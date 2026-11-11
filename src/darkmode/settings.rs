//! Dark mode settings: the data model, host matching, normalisation and
//! persistence (`darkmode.json` in the per-user config folder).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "darkmode.json";
const MAX_SITES: usize = 500;
const MAX_EXCLUDED: usize = 1000;
/// Per-site custom CSS, in bytes.
pub const MAX_CSS: usize = 64 * 1024;
const MAX_FONT: usize = 200;

/// When the page gets the dark theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Dark while the system prefers dark.
    #[default]
    System,
    Dark,
    /// Never, except on sites set to "on".
    Light,
    /// Same as `Light`; kept apart so the UI can say "off".
    Off,
}

/// A per-site override of the global mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteMode {
    #[default]
    Default,
    On,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteSettings {
    pub mode: SiteMode,
    /// Extra CSS applied on top of the dark theme (Dark Reader `fixes.css`).
    pub css: String,
}

/// A daily window in which dark mode may apply, minutes after midnight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub enabled: bool,
    pub from: u16,
    pub to: u16,
}

impl Default for Schedule {
    fn default() -> Self {
        Self { enabled: false, from: 19 * 60, to: 7 * 60 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DarkSettings {
    /// Master switch.
    pub enabled: bool,
    pub mode: Mode,
    /// Percent, 50..=150.
    pub brightness: u16,
    /// Percent, 50..=150.
    pub contrast: u16,
    /// Percent, 0..=100.
    pub sepia: u16,
    /// Percent, 0..=100.
    pub grayscale: u16,
    pub use_font: bool,
    pub font_family: String,
    /// Text stroke in 1/10 px, 0..=10.
    pub text_stroke: u16,
    /// Leave pages alone that are already dark.
    pub respect_native: bool,
    /// Hosts that never get the dark theme (a host covers its subdomains).
    pub excluded: Vec<String>,
    pub sites: BTreeMap<String, SiteSettings>,
    pub schedule: Schedule,
}

impl Default for DarkSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: Mode::System,
            brightness: 100,
            contrast: 100,
            sepia: 0,
            grayscale: 0,
            use_font: false,
            font_family: String::new(),
            text_stroke: 0,
            respect_native: true,
            excluded: Vec::new(),
            sites: BTreeMap::new(),
            schedule: Schedule::default(),
        }
    }
}

/// Lower-cases and strips what is not part of the host: scheme, credentials,
/// port, path, a leading `*.` or `www.` and a trailing dot. `""` for junk.
pub fn normalize_host(input: &str) -> String {
    let mut s = input.trim().to_ascii_lowercase();
    if let Some(i) = s.find("://") {
        s = s[i + 3..].to_string();
    }
    if let Some(i) = s.find(['/', '?', '#']) {
        s.truncate(i);
    }
    if let Some(i) = s.rfind('@') {
        s = s[i + 1..].to_string();
    }
    // Keep IPv6 literals whole, drop a port otherwise.
    if !s.starts_with('[') {
        if let Some(i) = s.rfind(':') {
            s.truncate(i);
        }
    }
    let s = s.trim_end_matches('.');
    let s = s.strip_prefix("*.").unwrap_or(s);
    let s = s.strip_prefix("www.").unwrap_or(s);
    let ok = !s.is_empty()
        && s.len() <= 253
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '[' | ']' | ':'));
    if ok { s.to_string() } else { String::new() }
}

/// True when `rule` is `host` or a parent domain of it. Both are normalised.
pub fn host_matches(rule: &str, host: &str) -> bool {
    let rule = normalize_host(rule);
    let host = normalize_host(host);
    !rule.is_empty()
        && !host.is_empty()
        && (host == rule || host.strip_suffix(rule.as_str()).is_some_and(|p| p.ends_with('.')))
}

/// What dark mode does on a page, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Dark,
    Light(&'static str),
}

impl DarkSettings {
    /// Clamps every value and drops junk; run after loading untrusted files
    /// and after UI edits.
    pub fn normalize(&mut self) {
        self.brightness = self.brightness.clamp(50, 150);
        self.contrast = self.contrast.clamp(50, 150);
        self.sepia = self.sepia.min(100);
        self.grayscale = self.grayscale.min(100);
        self.text_stroke = self.text_stroke.min(10);
        self.font_family = self.font_family.chars().filter(|c| !c.is_control()).take(MAX_FONT).collect();
        self.schedule.from %= 24 * 60;
        self.schedule.to %= 24 * 60;
        let mut ex: Vec<String> = self.excluded.iter().map(|h| normalize_host(h)).filter(|h| !h.is_empty()).collect();
        ex.sort();
        ex.dedup();
        ex.truncate(MAX_EXCLUDED);
        self.excluded = ex;
        let sites = std::mem::take(&mut self.sites);
        for (k, mut v) in sites {
            let k = normalize_host(&k);
            if k.is_empty() || self.sites.len() >= MAX_SITES {
                continue;
            }
            if v.css.len() > MAX_CSS {
                let mut cut = MAX_CSS;
                while !v.css.is_char_boundary(cut) {
                    cut -= 1;
                }
                v.css.truncate(cut);
            }
            if v.mode == SiteMode::Default && v.css.is_empty() {
                continue;
            }
            self.sites.insert(k, v);
        }
    }

    /// The most specific per-site entry covering `host`.
    pub fn site_for(&self, host: &str) -> Option<&SiteSettings> {
        self.sites
            .iter()
            .filter(|(k, _)| host_matches(k, host))
            .max_by_key(|(k, _)| k.len())
            .map(|(_, v)| v)
    }

    pub fn is_excluded(&self, host: &str) -> bool {
        self.excluded.iter().any(|r| host_matches(r, host))
    }

    /// Mirrors the page script's decision, ignoring the native-dark check
    /// (which only the page can make). `minutes` is the local time of day.
    pub fn decide(&self, host: &str, system_dark: bool, minutes: u16) -> Decision {
        if !self.enabled {
            return Decision::Light("off");
        }
        match self.site_for(host).map(|s| s.mode) {
            Some(SiteMode::On) => return Decision::Dark,
            Some(SiteMode::Off) => return Decision::Light("site"),
            _ => {}
        }
        if self.is_excluded(host) {
            return Decision::Light("excluded");
        }
        let want = match self.mode {
            Mode::Dark => true,
            Mode::System => system_dark,
            Mode::Light | Mode::Off => false,
        };
        if !want {
            return Decision::Light("mode");
        }
        let s = &self.schedule;
        if s.enabled {
            let inside = if s.from <= s.to { minutes >= s.from && minutes < s.to } else { minutes >= s.from || minutes < s.to };
            if !inside {
                return Decision::Light("schedule");
            }
        }
        Decision::Dark
    }

    pub fn site_mode(&self, host: &str) -> SiteMode {
        self.site_for(host).map(|s| s.mode).unwrap_or_default()
    }

    /// Sets the override of exactly `host`'s entry (keeping its CSS).
    pub fn set_site_mode(&mut self, host: &str, mode: SiteMode) {
        let key = normalize_host(host);
        if key.is_empty() {
            return;
        }
        self.sites.entry(key).or_default().mode = mode;
        self.normalize();
    }

    pub fn set_site_css(&mut self, host: &str, css: &str) {
        let key = normalize_host(host);
        if key.is_empty() {
            return;
        }
        self.sites.entry(key).or_default().css = css.to_string();
        self.normalize();
    }

    pub fn set_excluded(&mut self, host: &str, excluded: bool) {
        let key = normalize_host(host);
        if key.is_empty() {
            return;
        }
        self.excluded.retain(|h| *h != key);
        if excluded {
            self.excluded.push(key);
        }
        self.normalize();
    }
}

/// Loads the saved settings; defaults when the file is missing or unreadable.
pub fn load() -> DarkSettings {
    let mut s = crate::storage::config_file(FILE_NAME)
        .ok()
        .and_then(|p| crate::storage::read_text_limited(&p, crate::storage::MAX_SETTINGS_FILE).ok())
        .and_then(|t| serde_json::from_str::<DarkSettings>(&t).ok())
        .unwrap_or_default();
    s.normalize();
    s
}

pub fn save(settings: &DarkSettings) -> Result<(), String> {
    let path = crate::storage::config_file(FILE_NAME)?;
    let json = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    crate::storage::write_private(&path, &json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_normalised() {
        assert_eq!(normalize_host("https://User@WWW.Example.COM:8080/a?b#c"), "example.com");
        assert_eq!(normalize_host("*.example.com."), "example.com");
        assert_eq!(normalize_host("a b"), "");
        assert_eq!(normalize_host("</style>"), "");
        assert_eq!(normalize_host(""), "");
        assert_eq!(normalize_host("localhost:3000"), "localhost");
    }

    #[test]
    fn host_matching_respects_label_boundaries() {
        assert!(host_matches("example.com", "example.com"));
        assert!(host_matches("example.com", "www.example.com"));
        assert!(host_matches("example.com", "a.b.example.com"));
        assert!(host_matches("*.example.com", "a.example.com"));
        assert!(!host_matches("example.com", "badexample.com"));
        assert!(!host_matches("example.com", "example.com.evil.org"));
        assert!(!host_matches("", "example.com"));
        assert!(!host_matches("example.com", ""));
    }

    #[test]
    fn decisions_follow_the_precedence() {
        let mut s = DarkSettings::default();
        assert_eq!(s.decide("a.com", false, 0), Decision::Light("mode"));
        assert_eq!(s.decide("a.com", true, 0), Decision::Dark);
        s.mode = Mode::Dark;
        s.set_excluded("a.com", true);
        assert_eq!(s.decide("x.a.com", false, 0), Decision::Light("excluded"));
        s.set_site_mode("x.a.com", SiteMode::On);
        assert_eq!(s.decide("x.a.com", false, 0), Decision::Dark, "site override beats exclusion");
        assert_eq!(s.decide("y.a.com", false, 0), Decision::Light("excluded"));
        s.set_site_mode("b.com", SiteMode::Off);
        assert_eq!(s.decide("b.com", true, 0), Decision::Light("site"));
        s.mode = Mode::Off;
        assert_eq!(s.decide("c.com", true, 0), Decision::Light("mode"));
        s.enabled = false;
        assert_eq!(s.decide("x.a.com", true, 0), Decision::Light("off"));
    }

    #[test]
    fn the_most_specific_site_wins() {
        let mut s = DarkSettings::default();
        s.set_site_mode("example.com", SiteMode::Off);
        s.set_site_mode("docs.example.com", SiteMode::On);
        assert_eq!(s.site_mode("a.docs.example.com"), SiteMode::On);
        assert_eq!(s.site_mode("example.com"), SiteMode::Off);
        assert_eq!(s.site_mode("other.org"), SiteMode::Default);
    }

    #[test]
    fn schedule_wraps_midnight() {
        let mut s = DarkSettings { mode: Mode::Dark, ..Default::default() };
        s.schedule = Schedule { enabled: true, from: 19 * 60, to: 7 * 60 };
        assert_eq!(s.decide("a.com", false, 22 * 60), Decision::Dark);
        assert_eq!(s.decide("a.com", false, 3 * 60), Decision::Dark);
        assert_eq!(s.decide("a.com", false, 12 * 60), Decision::Light("schedule"));
        s.set_site_mode("a.com", SiteMode::On);
        assert_eq!(s.decide("a.com", false, 12 * 60), Decision::Dark, "forced sites ignore the schedule");
    }

    #[test]
    fn normalize_clamps_and_cleans() {
        let mut s = DarkSettings {
            brightness: 900,
            contrast: 0,
            sepia: 500,
            text_stroke: 99,
            excluded: vec!["WWW.B.com".into(), "b.com".into(), "bad host".into()],
            ..Default::default()
        };
        s.sites.insert("A.com".into(), SiteSettings { mode: SiteMode::Default, css: "é".repeat(MAX_CSS) });
        s.sites.insert("empty.com".into(), SiteSettings::default());
        s.normalize();
        assert_eq!((s.brightness, s.contrast, s.sepia, s.text_stroke), (150, 50, 100, 10));
        assert_eq!(s.excluded, vec!["b.com"]);
        assert_eq!(s.sites.len(), 1);
        assert!(s.sites["a.com"].css.len() <= MAX_CSS);
    }

    #[test]
    fn settings_round_trip_through_disk_and_tolerate_junk() {
        let mut s = DarkSettings { mode: Mode::Dark, sepia: 20, ..Default::default() };
        s.set_site_css("x.com", "body{color:red}");
        save(&s).unwrap();
        assert_eq!(load(), s);
        let path = crate::storage::config_file(FILE_NAME).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(), DarkSettings::default());
        std::fs::write(&path, r#"{"mode":"dark","brightness":9999,"unknown":1}"#).unwrap();
        let l = load();
        assert_eq!((l.mode, l.brightness), (Mode::Dark, 150));
    }
}
