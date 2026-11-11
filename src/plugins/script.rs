//! The generated init script: runs enabled plugins' scripts and styles where
//! their match patterns say so.
//!
//! Isolation (honest version): every plugin file runs inside its own function
//! with its own frozen `bc` object, created with `new Function` so a syntax
//! error in one plugin cannot break the others or the runtime. But WebKit
//! user scripts share the page's JavaScript world: a plugin script can see
//! and change the page's globals, and a hostile page could tamper with
//! built-ins (`Function.prototype`, `JSON`, `Promise`) to spy on a plugin. Only
//! the bridge's secret nonce and the closure-held `bc` objects are out of the
//! page's reach. Untrusted logic belongs in the WASM module, not in page JS.

use super::pattern::{HostMatch, MatchPattern, SchemeMatch};
use super::registry::Registry;
use serde_json::{json, Value};

const RUNTIME: &str = include_str!("page.js");

/// Serializes `v` so the text is safe to paste into a JavaScript source file
/// (and into HTML, should it ever be): no raw `<`, U+2028 or U+2029.
pub fn js_literal(v: &Value) -> String {
    v.to_string().replace('<', "\\u003c").replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029")
}

fn pattern_json(p: &MatchPattern) -> Value {
    let scheme = match &p.scheme {
        SchemeMatch::WebAny => "*web".to_string(),
        SchemeMatch::AllUrls => "<all>".to_string(),
        SchemeMatch::Exact(s) => s.clone(),
    };
    let (mode, host) = match &p.host {
        HostMatch::Any => (0, ""),
        HostMatch::Suffix(h) => (1, h.as_str()),
        HostMatch::Exact(h) => (2, h.as_str()),
    };
    json!([scheme, mode, host, p.path])
}

/// The single init script for a web view. Empty-ish (still valid) when no
/// enabled plugin has content scripts.
pub fn page_script(reg: &Registry) -> String {
    let plugins: Vec<Value> = reg
        .enabled()
        .filter(|p| !p.scripts.is_empty())
        .map(|p| {
            let m = &p.manifest;
            let scripts: Vec<Value> = p
                .scripts
                .iter()
                .map(|s| {
                    json!({
                        "inc": s.matches.iter().map(pattern_json).collect::<Vec<_>>(),
                        "exc": s.exclude.iter().map(pattern_json).collect::<Vec<_>>(),
                        "at": s.run_at.as_str(),
                        "frames": s.all_frames,
                        "css": s.css,
                        "js": s.js,
                    })
                })
                .collect();
            json!({
                "id": m.id,
                "v": m.version.to_string(),
                "api": format!("{}.{}", m.api.0, m.api.1),
                "perms": m.permissions.iter().map(|r| r.label()).collect::<Vec<_>>(),
                "settings": reg.settings(&m.id),
                "scripts": scripts,
            })
        })
        .collect();
    let data = json!({ "nonce": reg.nonce(), "plugins": plugins });
    RUNTIME.replace("__BC_DATA__", &js_literal(&data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::registry::tests::write_plugin;

    #[test]
    fn literal_escaping() {
        let s = js_literal(&json!("</script><!-- \u{2028}\u{2029}\"'\\\n"));
        assert!(!s.contains('<') && !s.contains('\u{2028}') && !s.contains('\u{2029}'));
        assert_eq!(serde_json::from_str::<Value>(&s).unwrap(), json!("</script><!-- \u{2028}\u{2029}\"'\\\n"));
    }

    #[test]
    fn script_embeds_only_enabled_plugins_with_escaped_code() {
        let d = tempfile::tempdir().unwrap();
        let dev = d.path().join("dev");
        write_plugin(&dev.join("a"), "com.t.a", "var evil = '</script>'; /* \u{2028} */");
        let mut r = Registry::open(d.path().join("cfg"));
        r.add_dev_folder(&dev).unwrap();
        let js = page_script(&r);
        assert!(js.contains("com.t.a") && js.contains(r.nonce()));
        assert!(!js.contains("__BC_DATA__"));
        assert!(!js.contains("</script>") && !js.contains('\u{2028}'));
        assert!(js.contains("body{color:red}"));
        r.set_enabled("com.t.a", false).unwrap();
        let off = page_script(&r);
        assert!(!off.contains("com.t.a") && !off.contains("body{color:red}"));
    }

    #[test]
    fn settings_reach_the_script() {
        let d = tempfile::tempdir().unwrap();
        let dev = d.path().join("dev");
        write_plugin(&dev, "com.t.s", "");
        let mut r = Registry::open(d.path().join("cfg"));
        r.add_dev_folder(&dev).unwrap();
        r.set_setting("com.t.s", "on", Value::Bool(false)).unwrap();
        assert!(page_script(&r).contains(r#""settings":{"on":false}"#));
    }

    #[test]
    fn the_runtime_parses_as_javascript() {
        let d = tempfile::tempdir().unwrap();
        let dev = d.path().join("dev");
        write_plugin(&dev, "com.t.js", "");
        let mut r = Registry::open(d.path().join("cfg"));
        r.add_dev_folder(&dev).unwrap();
        let out = d.path().join("s.js");
        std::fs::write(&out, page_script(&r)).unwrap();
        // Syntax check with node when it is installed (skipped otherwise).
        if let Ok(o) = std::process::Command::new("node").arg("--check").arg(&out).output() {
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        }
    }
}
