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

    #[test]
    fn shipped_examples_are_valid() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/plugins");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                crate::plugins::manifest::load_dir(&e.path()).unwrap_or_else(|err| panic!("{}: {err:?}", e.path().display()));
                n += 1;
            }
        }
        assert!(n >= 3);
    }
}

/// Real-WebKit check, run by hand: needs python3-gi with WebKit2 4.1.
#[cfg(test)]
mod live {
    use super::*;
    use crate::plugins::{bridge, manifest};
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};

    fn copy_with(src: &str, dest: &std::path::Path, replace: &[(&str, &str)]) {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/plugins").join(src);
        std::fs::create_dir_all(dest).unwrap();
        for e in std::fs::read_dir(src).unwrap().flatten() {
            let mut t = std::fs::read_to_string(e.path()).unwrap();
            for (a, b) in replace {
                t = t.replace(a, b);
            }
            std::fs::write(dest.join(e.file_name()), t).unwrap();
        }
    }

    #[test]
    #[ignore = "needs a display-less WebKitGTK 4.1 via python3-gi; run by hand"]
    fn plugin_script_in_webkit() {
        let _ = manifest::MANIFEST_FILE;
        let out = std::env::var("BC_LIVE_OUT").unwrap_or_else(|_| "/tmp".into());
        let d = tempfile::tempdir().unwrap();
        let dev = d.path().join("dev");
        // The examples, pointed at the harness' local server.
        copy_with("hello-world", &dev.join("hello"), &[("*://example.com/*", "*://127.0.0.1/*"), ("\"*://*.example.com/*\"", "\"*://127.0.0.2/*\"")]);
        copy_with("word-counter", &dev.join("counter"), &[]);
        copy_with("site-css-tweak", &dev.join("css"), &[("*://*.wikipedia.org/wiki/*", "*://127.0.0.1/wiki/*"), ("*://*.wikipedia.org/wiki/Special:*", "*://127.0.0.1/wiki/Special:*")]);
        let mut reg = Registry::open(d.path().join("cfg"));
        reg.add_dev_folder(&dev).unwrap();
        let script = d.path().join("page_script.js");
        std::fs::write(&script, page_script(&reg)).unwrap();
        let harness = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plugins_live.py");
        let targets = ["127.0.0.1:/", "localhost:/", "127.0.0.1:/csp", "127.0.0.1:/wiki/Test", "127.0.0.1:/wiki/csp"];
        let mut child = Command::new("python3")
            .env("GDK_BACKEND", "x11").env("WEBKIT_DISABLE_COMPOSITING_MODE", "1").env("WEBKIT_DISABLE_DMABUF_RENDERER", "1")
            .arg("-I").arg(&harness).arg(&script).arg(&out).args(targets)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let (mut results, mut bodies) = (Vec::new(), Vec::new());
        while let Some(Ok(line)) = lines.next() {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if let Some(body) = v["msg"].as_str() {
                bodies.push(body.to_string());
                let reply = bridge::parse_page_message(body).and_then(|c| bridge::dispatch(&mut reg, c, Some("http://127.0.0.1/")).reply);
                writeln!(stdin, "{}", json!({ "reply": reply })).unwrap();
            } else if v.get("result").is_some() {
                results.push(v["result"].clone());
                if results.len() == targets.len() {
                    break;
                }
            } else if v.get("error").is_some() {
                panic!("harness: {v}");
            }
        }
        drop(stdin);
        let _ = child.wait();
        println!("{results:#?}\n{bodies:#?}");
        assert_eq!(results.len(), targets.len());
        let f = |i: usize, k: &str| results[i]["facts"][k].clone();
        assert!(f(0, "banner").as_str().unwrap().starts_with("Hello from a BrowseCraft plugin"), "hello ran");
        assert!(f(0, "badge").as_str().is_some_and(|b| b.ends_with(" words")), "counter ran");
        assert_eq!(f(0, "leaked"), json!("undefined"), "bc is not a global");
        assert_eq!(f(1, "banner"), Value::Null, "host mismatch: hello must not run on localhost");
        assert!(f(1, "badge").is_string(), "counter matches every http host");
        assert_eq!(f(2, "banner"), Value::Null, "strict CSP (no unsafe-eval) blocks plugin JS: documented limitation");
        assert!(f(4, "lineHeight").as_str().unwrap().starts_with("27.2"), "CSS still applies under a strict CSP");
        assert!(f(3, "lineHeight").as_str().unwrap().starts_with("27.2"), "CSS-only plugin applied");
        assert_eq!(f(3, "banner").is_string(), true);
        assert!(reg.storage_get("dev.browsecraft.examples.word-counter", "largest").unwrap()["words"].as_u64().unwrap() > 50, "storage via the bridge");
        assert!(bodies.iter().any(|b| b.contains("\"m\":\"storage.set\"")));
    }
}
