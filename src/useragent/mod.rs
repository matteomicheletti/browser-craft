//! The User-Agent the web views send, and per-site overrides.
//!
//! WebKitGTK's own default is `… Version/60.5 Safari/605.1.15`: WebKit
//! deliberately claims a Safari version no Safari ever had (see
//! `UserAgentGLib.cpp`), and some servers reject it. Outlook on the web
//! (`outlook.cloud.microsoft`, which `outlook.office.com` now redirects to)
//! classifies "Safari 60.5 on Linux" as unsupported and its boot fails with
//! `OwaBasicUnsupportedException`. BrowseCraft sends a realistic Safari
//! version instead ([`default_ua`]).
//!
//! On top of that, a site can get another browser's string:
//!
//! - built-in rules ([`BUILTIN`]): Microsoft 365 gets Safari on macOS, the
//!   string WebKit's own quirk already gives `outlook.office.com` and
//!   `outlook.office365.com`, extended to the hosts Microsoft moved to;
//! - the user's overrides (host pattern → preset or custom string), saved in
//!   `user-agents.txt` and listed on `browsecraft://flags`. They win over the
//!   built-in rules; choosing "Default" for a built-in host turns its rule off.
//!
//! WebKit's site-specific quirks (`enable-site-specific-quirks`) stay on:
//! they fix layout and behaviour on many sites and give a few hosts their
//! own User-Agent (Google Docs and Drive, WhatsApp, PayPal…). A WebKit UA
//! quirk replaces even a custom User-Agent, so while one of our overrides is
//! in effect the view's quirks are turned off (they come back on the next
//! page without an override). See `docs/usage.md`, "User agent".

use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

#[cfg(target_os = "linux")]
pub mod webkit;

/// The Safari version BrowseCraft claims (WebKit is Safari's engine, and
/// `AppleWebKit/605.1.15` is frozen in every Safari since 11).
pub const SAFARI_VERSION: &str = "26.0";
/// The Chrome, Edge and Firefox versions of the presets (reduced UA strings:
/// Chrome only says the major version).
pub const CHROME_VERSION: &str = "157";
pub const FIREFOX_VERSION: &str = "156.0";

/// A browser whose User-Agent a site can be given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    /// BrowseCraft's own string (Safari on Linux, with WebKit's quirks).
    Default,
    /// Safari on macOS.
    Safari,
    /// Chrome on Linux.
    Chrome,
    /// Firefox on Linux.
    Firefox,
    /// Microsoft Edge on Linux.
    Edge,
}

impl Preset {
    pub const ALL: [Preset; 5] = [Preset::Default, Preset::Safari, Preset::Chrome, Preset::Firefox, Preset::Edge];

    /// Stable name used in the settings file and the settings page.
    pub fn key(self) -> &'static str {
        match self {
            Preset::Default => "default",
            Preset::Safari => "safari",
            Preset::Chrome => "chrome",
            Preset::Firefox => "firefox",
            Preset::Edge => "edge",
        }
    }

    pub fn from_key(key: &str) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Preset::Default => "Default (BrowseCraft)",
            Preset::Safari => "Safari on macOS",
            Preset::Chrome => "Chrome on Linux",
            Preset::Firefox => "Firefox on Linux",
            Preset::Edge => "Edge on Linux",
        }
    }

    /// The User-Agent string.
    pub fn ua(self) -> String {
        match self {
            Preset::Default => default_ua(),
            Preset::Safari => format!(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/{SAFARI_VERSION} Safari/605.1.15"
            ),
            Preset::Chrome => chrome_ua(),
            Preset::Firefox => format!(
                "Mozilla/5.0 (X11; Linux x86_64; rv:{FIREFOX_VERSION}) Gecko/20100101 Firefox/{FIREFOX_VERSION}"
            ),
            Preset::Edge => format!("{} Edg/{CHROME_VERSION}.0.0.0", chrome_ua()),
        }
    }
}

/// Chrome's reduced User-Agent (the platform is always `x86_64`).
fn chrome_ua() -> String {
    format!(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{CHROME_VERSION}.0.0.0 Safari/537.36"
    )
}

/// BrowseCraft's default User-Agent: WebKitGTK's own shape (platform from
/// the machine, as WebKit's `uname` does) with a real Safari version.
pub fn default_ua() -> String {
    let arch = match std::env::consts::ARCH {
        "x86" => "i686",
        other => other,
    };
    format!("Mozilla/5.0 (X11; Linux {arch}) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/{SAFARI_VERSION} Safari/605.1.15")
}

/// What a site gets: a preset or the user's own string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Preset(Preset),
    Custom(String),
}

impl Choice {
    pub fn ua(&self) -> String {
        match self {
            Choice::Preset(p) => p.ua(),
            Choice::Custom(s) => s.clone(),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Choice::Preset(p) => p.label().to_string(),
            Choice::Custom(_) => "Custom".to_string(),
        }
    }

    /// The settings-file form: a preset key or `custom:<string>`.
    fn to_value(&self) -> String {
        match self {
            Choice::Preset(p) => p.key().to_string(),
            Choice::Custom(s) => format!("custom:{s}"),
        }
    }

    fn from_value(value: &str) -> Option<Choice> {
        match value.strip_prefix("custom:") {
            Some(s) => valid_custom(s).map(Choice::Custom),
            None => Preset::from_key(value).map(Choice::Preset),
        }
    }
}

/// Where a site's choice comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Nothing matches: the default.
    None,
    BuiltIn,
    User,
}

/// The built-in rules: Microsoft 365 (mail, calendar, Teams, Office, their
/// sign-in pages) gets Safari on macOS, a browser Microsoft supports with
/// exactly WebKit's features. WebKit already does this for
/// `outlook.office.com` and `outlook.office365.com`; Microsoft has since
/// moved Outlook to `outlook.cloud.microsoft`. Sign-in hosts are included so
/// the whole sign-in flow sees one browser.
pub const BUILTIN: &[(&str, Preset)] = &[
    ("*.cloud.microsoft", Preset::Safari),
    ("*.office.com", Preset::Safari),
    ("*.office365.com", Preset::Safari),
    ("*.microsoft365.com", Preset::Safari),
    ("outlook.live.com", Preset::Safari),
    ("teams.microsoft.com", Preset::Safari),
    ("teams.live.com", Preset::Safari),
    ("login.microsoftonline.com", Preset::Safari),
    ("login.live.com", Preset::Safari),
    ("*.sharepoint.com", Preset::Safari),
    ("onedrive.live.com", Preset::Safari),
    ("*.officeapps.live.com", Preset::Safari),
];

/// At most this many user overrides are kept.
pub const MAX_OVERRIDES: usize = 500;
/// The longest custom User-Agent accepted.
pub const MAX_CUSTOM_LEN: usize = 512;

/// A host pattern in canonical form, if `input` is one: a host name or IPv4
/// address (`mail.example.com`, `127.0.0.1`), or `*.` and a domain for the
/// domain and all its subdomains. A URL is reduced to its host.
pub fn normalize_pattern(input: &str) -> Option<String> {
    let mut s = input.trim().to_ascii_lowercase();
    if let Some((_, rest)) = s.split_once("://") {
        s = rest.to_string();
    }
    if let Some(end) = s.find(['/', '?', '#']) {
        s.truncate(end);
    }
    if let Some((host, port)) = s.rsplit_once(':')
        && !port.is_empty()
        && port.chars().all(|c| c.is_ascii_digit())
    {
        s = host.to_string();
    }
    let s = s.trim_end_matches('.').to_string();
    let host = s.strip_prefix("*.").unwrap_or(&s);
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    if host.is_empty() || host.len() > 253 || !host.split('.').all(label_ok) {
        return None;
    }
    Some(s)
}

/// Whether `pattern` (canonical) covers `host`.
pub fn pattern_matches(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(domain) => host == domain || host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.')),
        None => host == pattern,
    }
}

/// How specific a matching pattern is: an exact host beats `*.`, a longer
/// domain beats a shorter one.
fn specificity(pattern: &str) -> (bool, usize) {
    (!pattern.starts_with("*."), pattern.len())
}

/// A custom User-Agent, if `s` can be one: printable ASCII (it goes into an
/// HTTP header), not empty, not too long.
pub fn valid_custom(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s.len() <= MAX_CUSTOM_LEN && s.bytes().all(|b| (0x20..0x7f).contains(&b))).then(|| s.to_string())
}

/// The host of `url` (lower case, no trailing dot) for http(s) pages.
pub fn url_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// The best match for `host` among `rules`.
fn best<'a, T>(rules: impl Iterator<Item = (&'a str, T)>, host: &str) -> Option<(&'a str, T)> {
    rules
        .filter(|(p, _)| pattern_matches(p, host))
        .max_by_key(|(p, _)| specificity(p))
}

/// The user overrides (pattern, choice), in the order they were added.
static OVERRIDES: RwLock<Vec<(String, Choice)>> = RwLock::new(Vec::new());
static SAVE_ERROR: Mutex<Option<String>> = Mutex::new(None);
/// Headless `--user-agent`: one choice for every site.
static FORCED: RwLock<Option<Choice>> = RwLock::new(None);

/// What `host` gets and why, ignoring the headless `--user-agent`.
pub fn choice_for_host(host: &str) -> (Choice, Origin) {
    let overrides = OVERRIDES.read().unwrap_or_else(|p| p.into_inner());
    if let Some((_, c)) = best(overrides.iter().map(|(p, c)| (p.as_str(), c)), host) {
        return (c.clone(), Origin::User);
    }
    builtin_for_host(host).map_or((Choice::Preset(Preset::Default), Origin::None), |p| {
        (Choice::Preset(p), Origin::BuiltIn)
    })
}

/// The built-in rule's preset for `host`, if one matches.
pub fn builtin_for_host(host: &str) -> Option<Preset> {
    best(BUILTIN.iter().map(|(p, c)| (*p, *c)), host).map(|(_, c)| c)
}

/// The User-Agent a view should have for a page, and whether WebKit's
/// site-specific quirks stay on (they would replace an override's string).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub ua: String,
    pub quirks: bool,
}

impl Resolved {
    pub fn default_ua() -> Resolved {
        Resolved { ua: default_ua(), quirks: true }
    }

    fn from_choice(choice: &Choice) -> Resolved {
        match choice {
            Choice::Preset(Preset::Default) => Resolved::default_ua(),
            other => Resolved { ua: other.ua(), quirks: false },
        }
    }
}

/// What a view should send for `url` (any URL: non-web pages get the
/// default).
pub fn resolve_url(url: &str) -> Resolved {
    if let Some(forced) = FORCED.read().unwrap_or_else(|p| p.into_inner()).as_ref() {
        return Resolved::from_choice(forced);
    }
    match url_host(url) {
        Some(host) => Resolved::from_choice(&choice_for_host(&host).0),
        None => Resolved::default_ua(),
    }
}

/// Headless `--user-agent`: a preset key or a string, for every site.
pub fn force(value: &str) -> Result<(), String> {
    let choice = match Preset::from_key(&value.to_ascii_lowercase()) {
        Some(p) => Choice::Preset(p),
        None => Choice::Custom(valid_custom(value).ok_or("--user-agent needs a preset (default, safari, chrome, firefox, edge) or a printable string")?),
    };
    *FORCED.write().unwrap_or_else(|p| p.into_inner()) = Some(choice);
    Ok(())
}

/// The user overrides, most recently added last.
pub fn overrides() -> Vec<(String, Choice)> {
    OVERRIDES.read().unwrap_or_else(|p| p.into_inner()).clone()
}

const FILE_NAME: &str = "user-agents.txt";

fn file_path() -> Result<PathBuf, String> {
    crate::storage::config_file(FILE_NAME)
}

fn parse(text: &str) -> Vec<(String, Choice)> {
    let mut out: Vec<(String, Choice)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((pattern, value)) = line.split_once('=') else {
            continue;
        };
        let (Some(pattern), Some(choice)) = (normalize_pattern(pattern), Choice::from_value(value.trim())) else {
            continue;
        };
        out.retain(|(p, _)| *p != pattern);
        out.push((pattern, choice));
        if out.len() > MAX_OVERRIDES {
            out.remove(0);
        }
    }
    out
}

fn serialize(overrides: &[(String, Choice)]) -> String {
    let mut text = String::from("# BrowseCraft per-site User-Agent overrides: host pattern = preset or custom:<string>\n");
    for (pattern, choice) in overrides {
        text += &format!("{pattern} = {}\n", choice.to_value());
    }
    text
}

fn load_from(path: &Path) -> Vec<(String, Choice)> {
    crate::storage::read_text_limited(path, crate::storage::MAX_SETTINGS_FILE)
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// Loads the saved overrides (once, at start-up).
pub fn load() {
    if let Ok(path) = file_path() {
        *OVERRIDES.write().unwrap_or_else(|p| p.into_inner()) = load_from(&path);
    }
}

/// Saves `next` and makes it current; on failure nothing changes.
fn commit(next: Vec<(String, Choice)>) -> Result<(), String> {
    let mut current = OVERRIDES.write().unwrap_or_else(|p| p.into_inner());
    let result = file_path().and_then(|path| {
        crate::storage::write_private(&path, serialize(&next).as_bytes())
            .map_err(|e| format!("Could not save the site user agents to {}: {e}", path.display()))
    });
    *SAVE_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = result.as_ref().err().cloned();
    if result.is_ok() {
        *current = next;
    }
    result
}

/// Sets (or with `None` removes) the override for `pattern`.
pub fn set_override(pattern: &str, choice: Option<Choice>) -> Result<(), String> {
    let pattern = normalize_pattern(pattern).ok_or_else(|| format!("\"{pattern}\" is not a host name or *.domain pattern"))?;
    let mut next = overrides();
    next.retain(|(p, _)| *p != pattern);
    if let Some(choice) = choice {
        if next.len() >= MAX_OVERRIDES {
            return Err(format!("At most {MAX_OVERRIDES} site user agents can be saved"));
        }
        next.push((pattern, choice));
    }
    commit(next)
}

/// The palette's pick for the site `host`: `None` goes back to automatic
/// (the built-in rule or the default). A pick equal to what the site gets
/// automatically removes the override instead of saving a copy, except that
/// "Default" on a built-in host is kept (it turns the rule off).
pub fn choose_for_host(host: &str, choice: Option<Choice>) -> Result<(), String> {
    let automatic = builtin_for_host(host).map_or(Choice::Preset(Preset::Default), Choice::Preset);
    let keep = choice.filter(|c| *c != automatic);
    set_override(host, keep)
}

/// The error of the last save, for the settings page.
pub fn save_error() -> Option<String> {
    SAVE_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

// ---- browsecraft://flags ----------------------------------------------------

/// Applies the settings page's user-agent fields (`ua-remove=<pattern>`, or
/// `ua-pattern` + `ua-preset` [+ `ua-custom`]). Returns an error to show, or
/// whether anything changed.
pub fn apply_form_pairs(pairs: &[(String, String)]) -> Result<bool, String> {
    let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let mut changed = false;
    for (k, v) in pairs {
        if k == "ua-remove" {
            let pattern = normalize_pattern(v).ok_or("Not a site pattern")?;
            if overrides().iter().any(|(p, _)| *p == pattern) {
                set_override(&pattern, None)?;
                changed = true;
            }
        }
    }
    if let Some(pattern) = get("ua-pattern").filter(|p| !p.trim().is_empty()) {
        let choice = match get("ua-preset").unwrap_or("") {
            "custom" => Choice::Custom(
                valid_custom(get("ua-custom").unwrap_or(""))
                    .ok_or("A custom user agent is printable text of up to 512 characters")?,
            ),
            key => Choice::Preset(Preset::from_key(key).ok_or("Choose a browser for the site")?),
        };
        set_override(pattern, Some(choice))?;
        changed = true;
    }
    Ok(changed)
}

/// The "Site user agents" card of the settings page. `to(key, value)` is a
/// link carrying the form token; `token` goes into the add form.
pub fn settings_card(to: &dyn Fn(&str, &str) -> String, token: &str) -> String {
    use crate::internal::escape;
    let mut rows = String::new();
    let list = overrides();
    if list.is_empty() {
        rows += "<div class=\"row first\"><div class=\"grow\"><div class=\"d\">No sites have their own user agent.</div></div></div>";
    }
    for (i, (pattern, choice)) in list.iter().enumerate() {
        let detail = choice.ua();
        rows += &format!(
            "<div class=\"row{}\"><div class=\"grow\"><div class=\"t\">{}</div><div class=\"d\">{} · <code>{}</code></div></div>\
             <a class=\"btn ghost\" href=\"{}\">Remove</a></div>",
            if i == 0 { " first" } else { "" },
            escape(pattern),
            escape(&choice.label()),
            escape(&detail),
            escape(&to("ua-remove", &crate::internal::encode(pattern))),
        );
    }
    let builtin: Vec<&str> = BUILTIN.iter().map(|(p, _)| *p).collect();
    let options: String = Preset::ALL
        .iter()
        .map(|p| format!("<option value=\"{}\">{}</option>", p.key(), escape(p.label())))
        .collect();
    let error = save_error()
        .map(|e| format!("<div class=\"bad\" role=alert>{}</div>", escape(&e)))
        .unwrap_or_default();
    format!(
        "<div class=\"card\"><h2>Site user agents</h2>\
         <p class=\"d\">The browser identity each site sees. Every other site gets <code>{default}</code>. \
         Built in: {builtin} get Safari on macOS (choose Default for one of them to turn that off). \
         Also in the command palette: User agent for this site.</p>{error}{rows}\
         <form method=\"get\" action=\"browsecraft://flags\" class=\"row\">\
         <input type=\"hidden\" name=\"token\" value=\"{token}\">\
         <div class=\"grow\"><input class=\"field\" name=\"ua-pattern\" placeholder=\"example.com or *.example.com\" required>\
         <select class=\"field\" name=\"ua-preset\">{options}<option value=\"custom\">Custom…</option></select>\
         <input class=\"field\" name=\"ua-custom\" placeholder=\"Custom user agent (with Custom…)\"></div>\
         <button class=\"btn\" type=\"submit\">Add</button></form></div>",
        default = escape(&default_ua()),
        builtin = escape(&builtin.join(", ")),
    )
}

#[cfg(test)]
pub(crate) static TEST_SERIAL: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_a_real_safari_on_linux() {
        let ua = default_ua();
        assert!(ua.starts_with("Mozilla/5.0 (X11; Linux "), "{ua}");
        assert!(ua.ends_with(&format!("AppleWebKit/605.1.15 (KHTML, like Gecko) Version/{SAFARI_VERSION} Safari/605.1.15")));
        assert!(!ua.contains("60.5 "), "never WebKitGTK's made-up Safari version");
        let major: u32 = SAFARI_VERSION.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
        assert!((17..40).contains(&major), "a Safari version that exists or soon will");
    }

    #[test]
    fn presets_are_consistent() {
        assert!(Preset::Safari.ua().contains("Macintosh; Intel Mac OS X 10_15_7"));
        assert!(Preset::Chrome.ua().contains(&format!("Chrome/{CHROME_VERSION}.0.0.0 Safari/537.36")));
        assert!(!Preset::Chrome.ua().contains("Version/"), "Chrome has no Version/ token");
        assert!(Preset::Edge.ua().ends_with(&format!("Edg/{CHROME_VERSION}.0.0.0")));
        assert!(Preset::Firefox.ua().contains("Gecko/20100101 Firefox/"));
        for p in Preset::ALL {
            assert_eq!(Preset::from_key(p.key()), Some(p));
            assert!(valid_custom(&p.ua()).is_some(), "every preset is a valid header value");
        }
        assert_eq!(Preset::from_key("opera"), None);
    }

    #[test]
    fn patterns_normalize_and_reject() {
        assert_eq!(normalize_pattern(" Mail.Example.COM. ").as_deref(), Some("mail.example.com"));
        assert_eq!(normalize_pattern("https://outlook.cloud.microsoft/mail/?x=1").as_deref(), Some("outlook.cloud.microsoft"));
        assert_eq!(normalize_pattern("127.0.0.1:8795").as_deref(), Some("127.0.0.1"));
        assert_eq!(normalize_pattern("*.office.com").as_deref(), Some("*.office.com"));
        for bad in ["", "*.", "*", "a..b", "-a.com", "a_b.com", "*.*.com", "exa mple.com", "a<x>.com", "é.com"] {
            assert_eq!(normalize_pattern(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn patterns_match_hosts() {
        assert!(pattern_matches("*.office.com", "office.com"));
        assert!(pattern_matches("*.office.com", "outlook.office.com"));
        assert!(pattern_matches("*.office.com", "a.b.office.com"));
        assert!(!pattern_matches("*.office.com", "notoffice.com"));
        assert!(!pattern_matches("*.office.com", "office.com.evil.net"));
        assert!(pattern_matches("teams.microsoft.com", "teams.microsoft.com"));
        assert!(!pattern_matches("teams.microsoft.com", "x.teams.microsoft.com"));
        assert!(!pattern_matches("microsoft.com", "teams.microsoft.com"));
    }

    #[test]
    fn microsoft_365_gets_safari_on_macos() {
        for host in [
            "outlook.cloud.microsoft",
            "outlook.office.com",
            "outlook.office365.com",
            "outlook.live.com",
            "www.office.com",
            "teams.microsoft.com",
            "login.microsoftonline.com",
            "login.live.com",
            "contoso.sharepoint.com",
        ] {
            assert_eq!(builtin_for_host(host), Some(Preset::Safari), "{host}");
        }
        for host in ["www.microsoft.com", "docs.google.com", "example.com", "live.com"] {
            assert_eq!(builtin_for_host(host), None, "{host}");
        }
        for (pattern, _) in BUILTIN {
            assert_eq!(normalize_pattern(pattern).as_deref(), Some(*pattern), "built-in patterns are canonical");
        }
    }

    #[test]
    fn overrides_win_and_the_most_specific_one_applies() {
        let _guard = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let before = overrides();
        commit(Vec::new()).unwrap();
        assert_eq!(resolve_url("https://example.com/"), Resolved::default_ua());
        assert_eq!(resolve_url("browsecraft://flags"), Resolved::default_ua());
        let ms = resolve_url("https://outlook.cloud.microsoft/mail/");
        assert_eq!(ms, Resolved { ua: Preset::Safari.ua(), quirks: false }, "an override turns WebKit's UA quirks off");

        set_override("*.example.com", Some(Choice::Preset(Preset::Firefox))).unwrap();
        set_override("app.example.com", Some(Choice::Preset(Preset::Chrome))).unwrap();
        assert_eq!(resolve_url("https://www.example.com/").ua, Preset::Firefox.ua());
        assert_eq!(resolve_url("https://app.example.com/x").ua, Preset::Chrome.ua());
        assert_eq!(resolve_url("https://example.com/").ua, Preset::Firefox.ua());

        // "Default" on a built-in host turns the rule off.
        choose_for_host("outlook.cloud.microsoft", Some(Choice::Preset(Preset::Default))).unwrap();
        assert_eq!(resolve_url("https://outlook.cloud.microsoft/mail/"), Resolved::default_ua());
        assert_eq!(choice_for_host("outlook.cloud.microsoft").1, Origin::User);
        // Picking what the host gets anyway saves nothing.
        choose_for_host("outlook.cloud.microsoft", Some(Choice::Preset(Preset::Safari))).unwrap();
        assert_eq!(choice_for_host("outlook.cloud.microsoft"), (Choice::Preset(Preset::Safari), Origin::BuiltIn));
        choose_for_host("plain.org", Some(Choice::Preset(Preset::Default))).unwrap();
        assert!(!overrides().iter().any(|(p, _)| p == "plain.org"));
        commit(before).unwrap();
    }

    #[test]
    fn overrides_persist_and_bad_lines_are_skipped() {
        let _guard = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let before = overrides();
        commit(Vec::new()).unwrap();
        set_override("127.0.0.1", Some(Choice::Preset(Preset::Chrome))).unwrap();
        set_override("*.example.org", Some(Choice::Custom("MyAgent/1.0 (test)".into()))).unwrap();
        let path = file_path().unwrap();
        let loaded = load_from(&path);
        assert_eq!(
            loaded,
            vec![
                ("127.0.0.1".to_string(), Choice::Preset(Preset::Chrome)),
                ("*.example.org".to_string(), Choice::Custom("MyAgent/1.0 (test)".into())),
            ]
        );
        let parsed = parse("# c\nbad line\nx.com = opera\nbad host! = chrome\ny.com = custom:\nz.com = custom:A\u{1}B\nq.com = edge\nq.com = safari\n");
        assert_eq!(parsed, vec![("q.com".to_string(), Choice::Preset(Preset::Safari))], "the last line for a pattern wins");
        commit(before).unwrap();
    }

    #[test]
    fn settings_form_adds_and_removes() {
        let _guard = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let before = overrides();
        commit(Vec::new()).unwrap();
        let pairs = |v: &[(&str, &str)]| v.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        assert_eq!(apply_form_pairs(&pairs(&[("ua-pattern", "Teams.Microsoft.com"), ("ua-preset", "edge")])), Ok(true));
        assert_eq!(choice_for_host("teams.microsoft.com"), (Choice::Preset(Preset::Edge), Origin::User));
        assert!(apply_form_pairs(&pairs(&[("ua-pattern", "a.com"), ("ua-preset", "custom"), ("ua-custom", "")])).is_err());
        assert!(apply_form_pairs(&pairs(&[("ua-pattern", "a b"), ("ua-preset", "chrome")])).is_err());
        assert_eq!(apply_form_pairs(&pairs(&[("ua-pattern", "a.com"), ("ua-preset", "custom"), ("ua-custom", "X/1")])), Ok(true));
        let html = settings_card(&|k, v| format!("browsecraft://flags?token=T&{k}={v}"), "T");
        assert!(html.contains("teams.microsoft.com") && html.contains("ua-remove=a.com") && html.contains("X/1"));
        assert_eq!(apply_form_pairs(&pairs(&[("ua-remove", "teams.microsoft.com")])), Ok(true));
        assert_eq!(apply_form_pairs(&pairs(&[("ua-remove", "teams.microsoft.com")])), Ok(false));
        assert_eq!(choice_for_host("teams.microsoft.com").1, Origin::BuiltIn);
        assert_eq!(apply_form_pairs(&pairs(&[("theme", "light")])), Ok(false), "other fields are not ours");
        commit(before).unwrap();
    }

    #[test]
    fn custom_strings_are_header_safe() {
        assert_eq!(valid_custom("  A/1  ").as_deref(), Some("A/1"));
        assert_eq!(valid_custom(""), None);
        assert_eq!(valid_custom("a\r\nX-Evil: 1"), None);
        assert_eq!(valid_custom(&"a".repeat(MAX_CUSTOM_LEN + 1)), None);
    }
}
