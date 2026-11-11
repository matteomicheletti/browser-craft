//! Smart dark mode (roadmap M5).
//!
//! The colour engine is the MIT-licensed Dark Reader dynamic theme (vendored
//! in `vendor/`, see `docs/DARKMODE.md`). This module wraps it: settings,
//! the per-page controller (`controller.js`: host rules, no white flash,
//! native-dark detection, schedule), the `bc-dark:` IPC message and the
//! drawer panel.
//!
//! Wiring (core): put `page_script(..)` in every web view's init scripts,
//! call `update_script(..)` through `evaluate_script` on every tab when a
//! `DarkEvent::SettingsChanged` arrives, and route IPC bodies starting with
//! `bc-dark:` to `parse_page_message`.

// The browser does not call this API yet (integration step).
#![allow(dead_code)]

mod panel;
mod settings;

pub use panel::{DarkEvent, DarkModePanel};
pub use settings::{
    DarkSettings, Decision, Mode, Schedule, SiteMode, SiteSettings, host_matches, load, normalize_host, save,
};

use serde::{Deserialize, Serialize};

const ENGINE: &str = include_str!("vendor/darkreader.js");
const CONTROLLER: &str = include_str!("controller.js");
/// IPC prefix of messages from the page script.
pub const IPC_PREFIX: &str = "bc-dark:";

/// The part of the settings the page needs, as JSON for the script.
#[derive(Serialize)]
struct PageConfig<'a> {
    enabled: bool,
    mode: Mode,
    brightness: u16,
    contrast: u16,
    sepia: u16,
    grayscale: u16,
    #[serde(rename = "useFont")]
    use_font: bool,
    #[serde(rename = "fontFamily")]
    font_family: &'a str,
    #[serde(rename = "textStroke")]
    text_stroke: f32,
    #[serde(rename = "respectNative")]
    respect_native: bool,
    excluded: &'a [String],
    sites: std::collections::BTreeMap<&'a str, PageSite<'a>>,
    schedule: Schedule,
}

#[derive(Serialize)]
struct PageSite<'a> {
    mode: SiteMode,
    css: &'a str,
}

/// JSON that is also safe to paste into a script: `<`, `>`, `&` and the
/// line separators never appear raw, so no value (custom CSS, host names)
/// can close a `<script>` or a string literal.
fn js_json<T: Serialize>(v: &T) -> String {
    let json = serde_json::to_string(v).unwrap_or_else(|_| "null".into());
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn config_json(settings: &DarkSettings) -> String {
    let mut s = settings.clone();
    s.normalize();
    js_json(&PageConfig {
        enabled: s.enabled,
        mode: s.mode,
        brightness: s.brightness,
        contrast: s.contrast,
        sepia: s.sepia,
        grayscale: s.grayscale,
        use_font: s.use_font,
        font_family: &s.font_family,
        text_stroke: f32::from(s.text_stroke) / 10.0,
        respect_native: s.respect_native,
        excluded: &s.excluded,
        sites: s.sites.iter().map(|(k, v)| (k.as_str(), PageSite { mode: v.mode, css: &v.css })).collect(),
        schedule: s.schedule,
    })
}

fn system_json(system_dark: Option<bool>) -> &'static str {
    match system_dark {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    }
}

/// The init script for every page: the controller plus the engine (only
/// executed when dark mode applies). `system_dark` is the system's colour
/// preference when the browser knows it; `None` lets the page ask
/// `matchMedia`.
pub fn page_script(settings: &DarkSettings, system_dark: Option<bool>) -> String {
    // Fill the three tokens in one pass over the template, so text inserted
    // for one (custom CSS in the config) is never scanned for another.
    let config = config_json(settings);
    let system = system_json(system_dark);
    let mut out = String::with_capacity(CONTROLLER.len() + ENGINE.len() + config.len());
    let mut rest = CONTROLLER;
    while let Some(i) = rest.find("__BC_") {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let (token, value) = if rest.starts_with("__BC_ENGINE__") {
            ("__BC_ENGINE__", ENGINE)
        } else if rest.starts_with("__BC_CONFIG__") {
            ("__BC_CONFIG__", config.as_str())
        } else if rest.starts_with("__BC_SYSTEM_DARK__") {
            ("__BC_SYSTEM_DARK__", system)
        } else {
            ("__BC_", "__BC_")
        };
        out.push_str(value);
        rest = &rest[token.len()..];
    }
    out.push_str(rest);
    out
}

/// Re-applies the settings on a page that already runs `page_script`
/// (through `evaluate_script`, no reload).
pub fn update_script(settings: &DarkSettings, system_dark: Option<bool>) -> String {
    format!(
        "window.__bcDark&&window.__bcDark.apply({},{});",
        config_json(settings),
        system_json(system_dark)
    )
}

/// A message from the page script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageMessage {
    /// The page at `host` was judged natively dark (or not).
    NativeDark { host: String, dark: bool },
}

/// Parses a `bc-dark:` IPC body. The page is untrusted: anything malformed
/// or oversized is ignored.
pub fn parse_page_message(body: &str) -> Option<PageMessage> {
    #[derive(Deserialize)]
    struct Raw {
        host: String,
        #[serde(rename = "nativeDark")]
        native_dark: bool,
    }
    if body.len() > 1024 {
        return None;
    }
    let raw: Raw = serde_json::from_str(body.strip_prefix(IPC_PREFIX)?).ok()?;
    let host = normalize_host(&raw.host);
    if host.is_empty() {
        return None;
    }
    Some(PageMessage::NativeDark { host, dark: raw.native_dark })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nasty() -> DarkSettings {
        let mut s = DarkSettings::default();
        s.set_site_css("evil.com", "</script><script>alert(1)</script>\"; alert(2); //\u{2028}\\");
        s.font_family = "Foo\"</script>".into();
        s.excluded.push("a.com".into());
        s
    }

    #[test]
    fn config_json_cannot_break_out_of_the_script() {
        let s = nasty();
        let json = config_json(&s);
        for bad in ["</", "<script", "\u{2028}", "<", ">"] {
            assert!(!json.contains(bad), "{bad:?} leaked into {json}");
        }
        // It is still the same data once parsed.
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["sites"]["evil.com"]["css"], s.sites["evil.com"].css.as_str());
        assert_eq!(v["fontFamily"], "Foo\"</script>");
        assert_eq!(v["mode"], "system");
        assert_eq!(v["excluded"][0], "a.com");
    }

    #[test]
    fn host_names_are_sanitised_before_they_reach_the_script() {
        let mut s = DarkSettings::default();
        s.set_site_css("x.com\"});alert(1);//", "a{}");
        s.set_excluded("</script>.com", true);
        assert!(s.sites.is_empty() && s.excluded.is_empty());
    }

    #[test]
    fn page_script_has_the_config_and_the_engine() {
        let script = page_script(&nasty(), Some(true));
        assert!(script.contains("__bcDark") && script.contains("DarkReader") && script.contains("dynamicTheme"));
        assert!(!script.contains("__BC_"), "unreplaced token");
        assert!(script.contains("\\u003c/script\\u003e"));
        assert!(!script.contains("window.chrome = {}"), "no global chrome shim");
        assert!(update_script(&nasty(), None).starts_with("window.__bcDark&&"));
        assert!(update_script(&nasty(), None).ends_with(",null);"));
    }

    #[test]
    fn scripts_are_valid_javascript() {
        let Ok(node) = std::process::Command::new("node").arg("--version").output() else {
            return; // node is only a test tool
        };
        if !node.status.success() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.js");
        let body = format!("{}\n;{}", page_script(&nasty(), None), update_script(&nasty(), Some(false)));
        // `new Function` parses without running (no DOM in node).
        let wrapper = format!("new Function({});", serde_json::to_string(&body).unwrap());
        std::fs::write(&file, wrapper).unwrap();
        let out = std::process::Command::new("node").arg(&file).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn page_messages_are_parsed_defensively() {
        assert_eq!(
            parse_page_message(r#"bc-dark:{"host":"WWW.Example.com","nativeDark":true}"#),
            Some(PageMessage::NativeDark { host: "example.com".into(), dark: true })
        );
        assert_eq!(parse_page_message(r#"bc-dark:{"host":"","nativeDark":true}"#), None);
        assert_eq!(parse_page_message(r#"bc-dark:{"host":"a b","nativeDark":true}"#), None);
        assert_eq!(parse_page_message(r#"bc-dark:{"host":"a.com"}"#), None);
        assert_eq!(parse_page_message("bc-dark:nope"), None);
        assert_eq!(parse_page_message(r#"bc:{"host":"a.com","nativeDark":true}"#), None);
        let big = format!(r#"bc-dark:{{"host":"{}.com","nativeDark":true}}"#, "a".repeat(2000));
        assert_eq!(parse_page_message(&big), None);
    }
}
