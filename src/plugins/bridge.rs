//! Page <-> browser messages.
//!
//! Wire format, from `bc.*` in the page: `window.ipc.postMessage("bc-plugin:"
//! + JSON)` with `{n: nonce, p: plugin id, i: call id, m: method, a: args}`.
//! `parse_page_message` only checks shape and size; `dispatch` checks the
//! nonce, that the plugin is enabled and meant for this page, the rate and the
//! declared permissions, runs what the registry can do itself (storage) and
//! turns the rest into a `PluginEvent` for the browser. Answers go back as a
//! script for `evaluate_script` that calls `window.__bcDeliver`.

use super::permissions;
use super::registry::Registry;
use serde_json::Value;
use std::time::{Duration, Instant};

pub const PREFIX: &str = "bc-plugin:";
/// Whole IPC body; the page-side limit is a little lower.
pub const MAX_MESSAGE: usize = 64 * 1024;
const MAX_CALLS_PER_SECOND: u32 = 200;

/// A well-formed request, not yet checked against permissions.
#[derive(Debug, PartialEq)]
pub struct PageCall {
    pub nonce: String,
    pub plugin: String,
    /// 0 = fire and forget (no answer is delivered).
    pub call_id: u64,
    pub wire: String,
    pub args: Value,
}

/// Where to deliver the answer of a call.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub plugin: String,
    pub call_id: u64,
}

impl Reply {
    pub fn ok(&self, value: &Value) -> String {
        deliver_script(&self.plugin, self.call_id, true, value)
    }

    pub fn err(&self, message: &str) -> String {
        deliver_script(&self.plugin, self.call_id, false, &Value::String(message.into()))
    }
}

/// Script for `evaluate_script` that settles the page-side promise.
pub fn deliver_script(plugin: &str, call_id: u64, ok: bool, payload: &Value) -> String {
    use super::script::js_literal;
    // The payload travels as a JSON *string* that the page parses, so no
    // plugin-controlled text is ever spliced in as code.
    let json = Value::String(payload.to_string());
    format!("window.__bcDeliver&&window.__bcDeliver({},{call_id},{ok},{})", js_literal(&Value::String(plugin.into())), js_literal(&json))
}

/// What only the browser can do. `reply` (when present) must be answered with
/// `Reply::ok` / `Reply::err` once done; the registry already answered the
/// calls that need no browser help.
#[derive(Debug, PartialEq)]
pub enum PluginEvent {
    Notify { plugin: String, title: String, body: String },
    ClipboardWrite { plugin: String, text: String },
    /// Open an http(s) URL in a new tab.
    OpenTab { plugin: String, url: String },
    /// Answer with `[{id, title, url, active}]`.
    QueryTabs { plugin: String, reply: Reply },
    Log { plugin: String, text: String },
}

#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// Script to run in the calling tab now.
    pub reply: Option<String>,
    pub event: Option<PluginEvent>,
}

fn text_arg(a: &Value, max: usize) -> Result<String, String> {
    match a {
        Value::String(s) if s.len() <= max => Ok(s.clone()),
        _ => Err(format!("expected a string of at most {max} bytes")),
    }
}

/// Validates a request's arguments for its method.
fn check_args(wire: &str, a: &Value) -> Result<(), String> {
    match wire {
        "log" => text_arg(a, 2000).map(drop),
        "message" => Ok(()),
        "storage.get" | "storage.remove" => text_arg(a, super::registry::MAX_STORAGE_KEY).map(drop),
        "storage.set" => match a.get("key") {
            Some(k) if a.as_object().is_some_and(|o| o.len() == 2 && o.contains_key("value")) => text_arg(k, super::registry::MAX_STORAGE_KEY).map(drop),
            _ => Err("expected {key, value}".into()),
        },
        "notify" => {
            let ok = a.as_object().is_some_and(|o| {
                o.len() == 2 && o.get("title").is_some_and(|t| text_arg(t, 200).is_ok()) && o.get("body").is_some_and(|b| text_arg(b, 1000).is_ok())
            });
            ok.then_some(()).ok_or_else(|| "expected {title, body}".into())
        }
        "clipboard.write" => text_arg(a, 16 * 1024).map(drop),
        "tabs.query" => a.is_null().then_some(()).ok_or_else(|| "takes no arguments".into()),
        "tabs.open" => {
            let s = text_arg(a, 2048)?;
            match url::Url::parse(&s) {
                Ok(u) if matches!(u.scheme(), "http" | "https") => Ok(()),
                _ => Err("only http(s) addresses can be opened".into()),
            }
        }
        _ => Err(format!("unknown API method \"{wire}\"")),
    }
}

/// Parses an IPC body that starts with `bc-plugin:`. Anything malformed or
/// oversized is ignored: the page is untrusted.
pub fn parse_page_message(body: &str) -> Option<PageCall> {
    parse_checked(body).ok()
}

/// Like `parse_page_message` but says what is wrong (tests, debug logs).
pub fn parse_checked(body: &str) -> Result<PageCall, String> {
    if body.len() > MAX_MESSAGE {
        return Err("message too large".into());
    }
    let json = body.strip_prefix(PREFIX).ok_or("not a plugin message")?;
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let o = v.as_object().ok_or("expected an object")?;
    let nonce = o.get("n").and_then(Value::as_str).ok_or("missing n")?.to_string();
    let plugin = o.get("p").and_then(Value::as_str).ok_or("missing p")?.to_string();
    let call_id = o.get("i").and_then(Value::as_u64).filter(|i| *i < (1 << 53)).ok_or("bad i")?;
    let wire = o.get("m").and_then(Value::as_str).ok_or("missing m")?.to_string();
    let args = o.get("a").cloned().unwrap_or(Value::Null);
    if o.len() > 5 || nonce.len() > 64 || !super::manifest::valid_id(&plugin) {
        return Err("bad envelope".into());
    }
    if call_id == 0 && wire != "log" {
        return Err("only log may be fire-and-forget".into());
    }
    check_args(&wire, &args)?;
    Ok(PageCall { nonce, plugin, call_id, wire, args })
}

fn rate_ok(reg: &mut Registry, plugin: &str) -> bool {
    let now = Instant::now();
    let slot = reg.rate.entry(plugin.to_string()).or_insert((now, 0));
    if now.duration_since(slot.0) >= Duration::from_secs(1) {
        *slot = (now, 0);
    }
    slot.1 += 1;
    slot.1 <= MAX_CALLS_PER_SECOND
}

/// Handles a parsed call from the page at `page_url` (the tab's address, when
/// the browser knows it). Never panics, never trusts the plugin id: it is
/// only honoured together with the registry's secret nonce.
pub fn dispatch(reg: &mut Registry, call: PageCall, page_url: Option<&str>) -> Outcome {
    if call.nonce != reg.nonce() {
        return Outcome::default();
    }
    let reply = Reply { plugin: call.plugin.clone(), call_id: call.call_id };
    let fail = |msg: &str| Outcome { reply: (call.call_id != 0).then(|| reply.err(msg)), event: None };
    let Some(manifest) = reg.get(&call.plugin).and_then(|e| e.manifest()).filter(|_| reg.is_enabled(&call.plugin)).cloned() else {
        return fail("plugin is not enabled");
    };
    if let Some(url) = page_url
        && !manifest.applies_to(url)
    {
        return fail("plugin has no content script for this page");
    }
    if !rate_ok(reg, &call.plugin) {
        return fail("too many calls; slow down");
    }
    if let Err(e) = permissions::check(&manifest.permissions, &call.wire) {
        return fail(&e);
    }
    let id = call.plugin.as_str();
    let ok = |v: Value| Outcome { reply: (call.call_id != 0).then(|| reply.ok(&v)), event: None };
    let a = &call.args;
    let str_of = |v: &Value| v.as_str().unwrap_or_default().to_string();
    match call.wire.as_str() {
        "log" => Outcome { reply: None, event: Some(PluginEvent::Log { plugin: id.into(), text: str_of(a) }) },
        "storage.get" => ok(reg.storage_get(id, a.as_str().unwrap_or_default()).cloned().unwrap_or(Value::Null)),
        "storage.set" => {
            let key = str_of(&a["key"]);
            match reg.storage_set(id, &key, a["value"].clone()) {
                Ok(()) => ok(Value::Null),
                Err(e) => fail(&e),
            }
        }
        "storage.remove" => match reg.storage_remove(id, a.as_str().unwrap_or_default()) {
            Ok(()) => ok(Value::Null),
            Err(e) => fail(&e),
        },
        "notify" => Outcome {
            reply: ok(Value::Null).reply,
            event: Some(PluginEvent::Notify { plugin: id.into(), title: str_of(&a["title"]), body: str_of(&a["body"]) }),
        },
        "clipboard.write" => Outcome { reply: ok(Value::Null).reply, event: Some(PluginEvent::ClipboardWrite { plugin: id.into(), text: str_of(a) }) },
        "tabs.open" => Outcome { reply: ok(Value::Null).reply, event: Some(PluginEvent::OpenTab { plugin: id.into(), url: str_of(a) }) },
        "tabs.query" => Outcome { reply: None, event: Some(PluginEvent::QueryTabs { plugin: id.into(), reply }) },
        "message" => match super::wasm::handle_message(reg, id, a) {
            Ok(v) => ok(v),
            Err(e) => fail(&e),
        },
        _ => fail("unknown API method"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::registry::tests::write_plugin;
    use serde_json::json;

    fn msg(nonce: &str, plugin: &str, i: u64, m: &str, a: Value) -> String {
        format!("{PREFIX}{}", json!({"n": nonce, "p": plugin, "i": i, "m": m, "a": a}))
    }

    fn setup() -> (tempfile::TempDir, Registry) {
        let d = tempfile::tempdir().unwrap();
        write_plugin(&d.path().join("dev"), "com.t.b", ""); // permissions: page.modify, storage
        let mut r = Registry::open(d.path().join("cfg"));
        r.add_dev_folder(&d.path().join("dev")).unwrap();
        (d, r)
    }

    #[test]
    fn parsing_is_defensive() {
        let ok = msg("n", "com.t.b", 1, "storage.get", json!("k"));
        assert_eq!(parse_page_message(&ok).unwrap().wire, "storage.get");
        assert!(parse_page_message("bc:forms?x=1").is_none());
        assert!(parse_page_message("bc-plugin:{").is_none());
        assert!(parse_page_message("bc-plugin:[1]").is_none());
        assert!(parse_page_message(&msg("n", "NotAnId", 1, "log", json!("x"))).is_none());
        assert!(parse_page_message(&msg("n", "com.t.b", 1, "bogus", Value::Null)).is_none());
        assert!(parse_page_message(&msg("n", "com.t.b", 0, "storage.get", json!("k"))).is_none(), "only log is fire-and-forget");
        assert!(parse_page_message(&msg("n", "com.t.b", 1, "tabs.open", json!("javascript:alert(1)"))).is_none());
        assert!(parse_page_message(&msg("n", "com.t.b", 1, "tabs.open", json!("file:///etc/passwd"))).is_none());
        assert!(parse_page_message(&msg("n", "com.t.b", 1, "notify", json!({"title": "t"}))).is_none());
        assert!(parse_page_message(&msg("n", "com.t.b", 1, "storage.set", json!({"key": 1, "value": 2}))).is_none());
        assert!(parse_page_message(&msg("n", "com.t.b", 1, "log", json!("x".repeat(70_000)))).is_none());
        assert!(parse_page_message(&msg("n", "com.t.b", 1, "log", json!("x".repeat(3000)))).is_none());
        let extra = format!("{PREFIX}{}", json!({"n":"n","p":"com.t.b","i":1,"m":"log","a":"x","z":1}));
        assert!(parse_page_message(&extra).is_none());
        assert!(parse_page_message(&format!("{PREFIX}{}", json!({"n":"n","p":"com.t.b","i":-1,"m":"log","a":"x"}))).is_none());
    }

    #[test]
    fn nonce_enabled_page_and_permissions_are_enforced() {
        let (_d, mut r) = setup();
        let n = r.nonce().to_string();
        let call = |r: &mut Registry, nonce: &str, m: &str, a: Value, url: Option<&str>| {
            dispatch(r, parse_page_message(&msg(nonce, "com.t.b", 7, m, a)).unwrap(), url)
        };
        // Wrong nonce: dropped silently, nothing stored.
        assert_eq!(call(&mut r, "guess", "storage.set", json!({"key":"k","value":1}), None), Outcome::default());
        assert!(r.storage_get("com.t.b", "k").is_none());
        // Valid: storage works and answers.
        let o = call(&mut r, &n, "storage.set", json!({"key":"k","value":{"a":1}}), Some("https://a.test/x"));
        assert!(o.reply.unwrap().contains("__bcDeliver(\"com.t.b\",7,true"));
        let o = call(&mut r, &n, "storage.get", json!("k"), None);
        assert!(o.reply.unwrap().contains(r#"{\"a\":1}"#));
        // Permission missing: notifications / tabs are refused.
        let o = call(&mut r, &n, "notify", json!({"title":"t","body":"b"}), None);
        assert!(o.event.is_none() && o.reply.unwrap().contains("notifications"));
        let o = call(&mut r, &n, "tabs.query", Value::Null, None);
        assert!(o.event.is_none() && o.reply.unwrap().contains("tabs.read"));
        // Wrong page: the plugin only matches https://a.test/*.
        let o = call(&mut r, &n, "storage.get", json!("k"), Some("https://evil.test/"));
        assert!(o.reply.unwrap().contains("no content script"));
        // Disabled: refused.
        r.set_enabled("com.t.b", false).unwrap();
        assert!(call(&mut r, &n, "storage.get", json!("k"), None).reply.unwrap().contains("not enabled"));
        // Unknown plugin id with a right nonce.
        let c = parse_page_message(&msg(&n, "com.t.nope", 0, "log", json!("x"))).unwrap();
        assert_eq!(dispatch(&mut r, c, None), Outcome::default(), "fire-and-forget gets no answer");
        let c = parse_page_message(&msg(&n, "com.t.nope", 1, "storage.get", json!("k"))).unwrap();
        assert!(dispatch(&mut r, c, None).reply.unwrap().contains("not enabled"));
    }

    #[test]
    fn events_for_the_browser() {
        let d = tempfile::tempdir().unwrap();
        let dev = d.path().join("dev");
        write_plugin(&dev, "com.t.e", "");
        let t = std::fs::read_to_string(dev.join("plugin.toml")).unwrap().replace("\"storage\"", "\"notifications\", \"tabs.read\", \"tabs.open\", \"clipboard.write\"");
        std::fs::write(dev.join("plugin.toml"), t).unwrap();
        let mut r = Registry::open(d.path().join("cfg"));
        r.add_dev_folder(&dev).unwrap();
        let n = r.nonce().to_string();
        let mut run = |m: &str, a: Value| dispatch(&mut r, parse_page_message(&msg(&n, "com.t.e", 3, m, a)).unwrap(), None);
        let o = run("notify", json!({"title":"T","body":"B"}));
        assert_eq!(o.event, Some(PluginEvent::Notify { plugin: "com.t.e".into(), title: "T".into(), body: "B".into() }));
        assert!(o.reply.is_some());
        assert!(matches!(run("tabs.open", json!("https://x.test/")).event, Some(PluginEvent::OpenTab { .. })));
        assert!(matches!(run("clipboard.write", json!("hi")).event, Some(PluginEvent::ClipboardWrite { .. })));
        let o = run("tabs.query", Value::Null);
        assert!(o.reply.is_none());
        let Some(PluginEvent::QueryTabs { reply, .. }) = o.event else { panic!("no query event") };
        assert!(reply.ok(&json!([{"id":1}])).contains("__bcDeliver"));
        assert!(run("message", json!({"x":1})).reply.unwrap().contains("false"), "no wasm handler");
    }

    #[test]
    fn rate_limit() {
        let (_d, mut r) = setup();
        let n = r.nonce().to_string();
        let mut refused = 0;
        for _ in 0..300 {
            let c = parse_page_message(&msg(&n, "com.t.b", 1, "storage.get", json!("k"))).unwrap();
            if dispatch(&mut r, c, None).reply.is_some_and(|s| s.contains("too many")) {
                refused += 1;
            }
        }
        assert!(refused >= 90);
    }

    #[test]
    fn deliver_script_cannot_be_injected_through_payload() {
        let evil = json!("\"); alert(1); (\"</script>\u{2028}");
        let s = Reply { plugin: "com.t.b".into(), call_id: 2 }.ok(&evil);
        assert!(!s.contains('<') && !s.contains('\u{2028}'));
        assert!(s.starts_with("window.__bcDeliver&&window.__bcDeliver(\"com.t.b\",2,true,\""));
    }
}
