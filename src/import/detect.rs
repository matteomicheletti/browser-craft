//! Finding other browsers' profiles: Firefox and its forks through their
//! `profiles.ini`, the Chromium family through their profile folders
//! ("Default", "Profile 1", ...) and the names in their `Local State`.
//! Native, Flatpak and Snap installs on Linux; the usual folders on
//! Windows and macOS.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// The largest `Local State` / `profiles.ini` read.
const MAX_STATE_FILE: u64 = 16 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Firefox,
    Chromium,
}

/// One profile of another browser.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Profile {
    /// "Firefox", "Google Chrome (Flatpak)", ...
    pub browser: String,
    /// The command-line name: firefox, chrome, chromium, brave, edge, ...
    pub key: String,
    pub family: Family,
    /// The profile's name as the browser shows it.
    pub name: String,
    pub dir: PathBuf,
    /// The browser's default profile.
    pub default: bool,
}

impl Profile {
    /// "Firefox – default-release".
    pub fn label(&self) -> String {
        format!("{} \u{2013} {}", self.browser, self.name)
    }
}

/// Where Firefox-family browsers keep `profiles.ini`, relative to the home
/// folder: (key, browser name, folder).
fn firefox_roots() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut roots = vec![
        ("firefox", "Firefox", ".mozilla/firefox"),
        ("firefox", "Firefox (Flatpak)", ".var/app/org.mozilla.firefox/.mozilla/firefox"),
        ("firefox", "Firefox (Snap)", "snap/firefox/common/.mozilla/firefox"),
        ("librewolf", "LibreWolf", ".librewolf"),
        ("librewolf", "LibreWolf (Flatpak)", ".var/app/io.gitlab.librewolf-community/.librewolf"),
        ("waterfox", "Waterfox", ".waterfox"),
    ];
    if cfg!(target_os = "macos") {
        roots.push(("firefox", "Firefox", "Library/Application Support/Firefox"));
    }
    if cfg!(windows) {
        roots.push(("firefox", "Firefox", "AppData/Roaming/Mozilla/Firefox"));
    }
    roots
}

/// Where Chromium-family browsers keep their profiles, relative to the
/// config folder (`~/.config`) or, with a leading `~/`, to the home folder.
fn chromium_roots() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut roots = vec![
        ("chrome", "Google Chrome", "google-chrome"),
        ("chrome-beta", "Google Chrome Beta", "google-chrome-beta"),
        ("chrome-dev", "Google Chrome Dev", "google-chrome-unstable"),
        ("chromium", "Chromium", "chromium"),
        ("brave", "Brave", "BraveSoftware/Brave-Browser"),
        ("edge", "Microsoft Edge", "microsoft-edge"),
        ("vivaldi", "Vivaldi", "vivaldi"),
        ("opera", "Opera", "opera"),
        ("chrome", "Google Chrome (Flatpak)", "~/.var/app/com.google.Chrome/config/google-chrome"),
        ("chromium", "Chromium (Flatpak)", "~/.var/app/org.chromium.Chromium/config/chromium"),
        ("brave", "Brave (Flatpak)", "~/.var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser"),
        ("edge", "Microsoft Edge (Flatpak)", "~/.var/app/com.microsoft.Edge/config/microsoft-edge"),
        ("vivaldi", "Vivaldi (Flatpak)", "~/.var/app/com.vivaldi.Vivaldi/config/vivaldi"),
        ("opera", "Opera (Flatpak)", "~/.var/app/com.opera.Opera/config/opera"),
        ("chromium", "Chromium (Snap)", "~/snap/chromium/common/chromium"),
    ];
    if cfg!(target_os = "macos") {
        roots.extend([
            ("chrome", "Google Chrome", "~/Library/Application Support/Google/Chrome"),
            ("chromium", "Chromium", "~/Library/Application Support/Chromium"),
            ("brave", "Brave", "~/Library/Application Support/BraveSoftware/Brave-Browser"),
            ("edge", "Microsoft Edge", "~/Library/Application Support/Microsoft Edge"),
        ]);
    }
    if cfg!(windows) {
        roots.extend([
            ("chrome", "Google Chrome", "~/AppData/Local/Google/Chrome/User Data"),
            ("chromium", "Chromium", "~/AppData/Local/Chromium/User Data"),
            ("brave", "Brave", "~/AppData/Local/BraveSoftware/Brave-Browser/User Data"),
            ("edge", "Microsoft Edge", "~/AppData/Local/Microsoft/Edge/User Data"),
        ]);
    }
    roots
}

/// Every profile found under `home` and the config folders `configs`
/// (`$XDG_CONFIG_HOME` and `~/.config`; duplicates are listed once).
pub fn detect_in(home: &Path, configs: &[PathBuf]) -> Vec<Profile> {
    let mut out: Vec<Profile> = Vec::new();
    for (key, browser, rel) in firefox_roots() {
        out.extend(firefox_profiles(&home.join(rel), key, browser));
    }
    for (key, browser, rel) in chromium_roots() {
        let bases: Vec<PathBuf> = match rel.strip_prefix("~/") {
            Some(rest) => vec![home.join(rest)],
            None => configs.iter().map(|c| c.join(rel)).collect(),
        };
        for base in bases {
            out.extend(chromium_profiles(&base, key, browser));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(std::fs::canonicalize(&p.dir).unwrap_or_else(|_| p.dir.clone())));
    out
}

/// The profiles of this user.
pub fn detect() -> Vec<Profile> {
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from) else {
        return Vec::new();
    };
    let mut configs = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        configs.push(xdg);
    }
    configs.push(home.join(".config"));
    detect_in(&home, &configs)
}

/// The sections of an ini file: (name, [(key, value)]).
fn parse_ini(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.push((name.to_string(), Vec::new()));
        } else if let (Some((k, v)), Some(last)) = (line.split_once('='), sections.last_mut()) {
            last.1.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    sections
}

/// The Firefox profiles listed in `root/profiles.ini` that have a
/// places.sqlite.
pub fn firefox_profiles(root: &Path, key: &str, browser: &str) -> Vec<Profile> {
    let Ok(text) = crate::storage::read_text_limited(&root.join("profiles.ini"), MAX_STATE_FILE) else {
        return Vec::new();
    };
    let sections = parse_ini(&text);
    let get = |fields: &[(String, String)], k: &str| fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.clone());
    // Newer Firefox marks the default per installation ([Install...]).
    let install_defaults: Vec<String> =
        sections.iter().filter(|(n, _)| n.starts_with("Install")).filter_map(|(_, f)| get(f, "Default")).collect();
    let mut out = Vec::new();
    for (name, fields) in &sections {
        if !name.starts_with("Profile") {
            continue;
        }
        let Some(path) = get(fields, "Path") else { continue };
        let relative = get(fields, "IsRelative").is_none_or(|v| v == "1");
        let dir = if relative { root.join(&path) } else { PathBuf::from(&path) };
        if !dir.join("places.sqlite").is_file() {
            continue;
        }
        let default = install_defaults.contains(&path) || (install_defaults.is_empty() && get(fields, "Default").as_deref() == Some("1"));
        out.push(Profile {
            browser: browser.to_string(),
            key: key.to_string(),
            family: Family::Firefox,
            name: get(fields, "Name").unwrap_or(path),
            dir,
            default,
        });
    }
    out
}

/// The Chromium profiles in `root` ("Default", "Profile 3", ...) that have
/// bookmarks or history, named from `Local State`.
pub fn chromium_profiles(root: &Path, key: &str, browser: &str) -> Vec<Profile> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let state: serde_json::Value = crate::storage::read_text_limited(&root.join("Local State"), MAX_STATE_FILE)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let cache = &state["profile"]["info_cache"];
    let last_used = state["profile"]["last_used"].as_str().unwrap_or("Default").to_string();
    let has_data = |dir: &Path| dir.join("Bookmarks").is_file() || dir.join("History").is_file();
    let mut dirs: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n == "Default" || n.starts_with("Profile "))
        .filter(|n| has_data(&root.join(n)))
        .collect();
    dirs.sort_by_key(|n| (n != "Default", n.trim_start_matches("Profile ").parse::<u32>().unwrap_or(u32::MAX)));
    let mut out: Vec<Profile> = dirs
        .into_iter()
        .map(|d| Profile {
            browser: browser.to_string(),
            key: key.to_string(),
            family: Family::Chromium,
            name: cache[&d]["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(&d).to_string(),
            default: d == last_used,
            dir: root.join(d),
        })
        .collect();
    // Opera keeps its one profile in the root folder itself.
    if out.is_empty() && has_data(root) && state.is_object() {
        out.push(Profile {
            browser: browser.to_string(),
            key: key.to_string(),
            family: Family::Chromium,
            name: "Default".into(),
            dir: root.to_path_buf(),
            default: true,
        });
    }
    out
}

/// The profile `wanted` (a profile name, folder name or path; the default
/// one when `None`) of browser `key` among `profiles`.
pub fn pick<'a>(profiles: &'a [Profile], key: &str, wanted: Option<&str>) -> Result<&'a Profile, String> {
    let of_browser: Vec<&Profile> = profiles.iter().filter(|p| p.key == key).collect();
    if of_browser.is_empty() {
        return Err(format!("no {key} profile found (see `browsecraft import --list`)"));
    }
    match wanted {
        None => Ok(of_browser.iter().find(|p| p.default).copied().unwrap_or(of_browser[0])),
        Some(w) => of_browser
            .iter()
            .find(|p| {
                p.name == w
                    || p.dir == Path::new(w)
                    || p.dir.file_name().is_some_and(|f| f.to_string_lossy() == w)
            })
            .copied()
            .ok_or_else(|| {
                let names: Vec<&str> = of_browser.iter().map(|p| p.name.as_str()).collect();
                format!("no {key} profile named {w:?} (profiles: {})", names.join(", "))
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn finds_firefox_profiles_from_profiles_ini() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".mozilla/firefox");
        touch(&root.join("aaa.default/places.sqlite"));
        touch(&root.join("bbb.default-release/places.sqlite"));
        std::fs::create_dir_all(root.join("ccc.empty")).unwrap();
        std::fs::write(
            root.join("profiles.ini"),
            "[Install4F96D1932A9F858E]\nDefault=bbb.default-release\nLocked=1\n\n\
             [Profile1]\nName=default\nIsRelative=1\nPath=aaa.default\nDefault=1\n\n\
             [Profile0]\nName=default-release\nIsRelative=1\nPath=bbb.default-release\n\n\
             [Profile2]\nName=empty\nIsRelative=1\nPath=ccc.empty\n\n[General]\nVersion=2\n",
        )
        .unwrap();
        // A Flatpak Firefox too.
        let flat = home.path().join(".var/app/org.mozilla.firefox/.mozilla/firefox");
        touch(&flat.join("x.default/places.sqlite"));
        std::fs::write(flat.join("profiles.ini"), "[Profile0]\nName=flat\nPath=x.default\nDefault=1\n").unwrap();
        let found = detect_in(home.path(), &[home.path().join(".config")]);
        let names: Vec<(&str, &str, bool)> = found.iter().map(|p| (p.browser.as_str(), p.name.as_str(), p.default)).collect();
        assert_eq!(
            names,
            [("Firefox", "default", false), ("Firefox", "default-release", true), ("Firefox (Flatpak)", "flat", true)]
        );
        assert_eq!(pick(&found, "firefox", None).unwrap().name, "default-release");
        assert_eq!(pick(&found, "firefox", Some("aaa.default")).unwrap().name, "default");
        assert_eq!(pick(&found, "firefox", Some("flat")).unwrap().browser, "Firefox (Flatpak)");
        assert!(pick(&found, "firefox", Some("nope")).is_err());
        assert!(pick(&found, "chrome", None).is_err());
    }

    #[test]
    fn finds_chromium_profiles_and_names() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join(".config");
        let chrome = config.join("google-chrome");
        touch(&chrome.join("Default/History"));
        touch(&chrome.join("Profile 2/Bookmarks"));
        touch(&chrome.join("Profile 10/History"));
        std::fs::create_dir_all(chrome.join("Profile 3")).unwrap(); // no data
        touch(&chrome.join("System Profile/History"));
        std::fs::write(
            chrome.join("Local State"),
            r#"{"profile":{"last_used":"Profile 2","info_cache":{"Default":{"name":"Person 1"},"Profile 2":{"name":"Work"}}}}"#,
        )
        .unwrap();
        // Chromium with only crash reports: nothing.
        std::fs::create_dir_all(config.join("chromium/Crash Reports")).unwrap();
        touch(&home.path().join(".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser/Default/Bookmarks"));
        touch(&config.join("opera/Bookmarks"));
        std::fs::write(config.join("opera/Local State"), "{}").unwrap();
        let found = detect_in(home.path(), &[config.clone(), config.clone()]);
        let names: Vec<(&str, &str, bool)> = found.iter().map(|p| (p.key.as_str(), p.name.as_str(), p.default)).collect();
        assert_eq!(
            names,
            [
                ("chrome", "Person 1", false),
                ("chrome", "Work", true),
                ("chrome", "Profile 10", false),
                ("opera", "Default", true),
                ("brave", "Default", true),
            ],
            "listed once even with the same config folder twice"
        );
        assert_eq!(pick(&found, "chrome", None).unwrap().name, "Work");
        assert_eq!(pick(&found, "chrome", Some("Profile 10")).unwrap().name, "Profile 10");
        assert!(pick(&found, "chromium", None).is_err());
    }

    #[test]
    fn malformed_state_files_are_tolerated() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".mozilla/firefox");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("profiles.ini"), "garbage\n[Profile0\nPath\n=\n[Profile1]\nPath=missing\n").unwrap();
        let chrome = home.path().join(".config/google-chrome");
        touch(&chrome.join("Default/History"));
        std::fs::write(chrome.join("Local State"), "{not json").unwrap();
        let found = detect_in(home.path(), &[home.path().join(".config")]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Default");
    }
}
