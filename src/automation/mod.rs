//! AI-first automation: a local control API (JSON-RPC over a Unix socket)
//! that scripts and agents use to drive the browser, the `browsecraft ctl`
//! and `browsecraft mcp` clients built on it, the page scripts the methods
//! run, and the console capture. See docs/AUTOMATION.md.
//!
//! The server never touches browser state: it validates each request,
//! checks it against the permissions in [`Settings`], and hands a
//! [`Request`] to the callback given to [`start`]. The browser posts it to
//! its event loop and answers through `Request::reply`.
//!
//! There are no methods for passwords, the vault or the authenticator, and
//! there never will be (docs/CONTRACTS.md).

// The browser wires the server, the page jobs and the console log in a
// later step (docs/AUTOMATION.md, "Integration"); until then parts of the
// API are only used by tests and the CLI.
#![allow(dead_code)]

pub mod console;
pub mod ctl;
pub mod mcp;
pub mod panel;
pub mod protocol;
pub mod scripts;
#[cfg(unix)]
pub mod server;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use protocol::{Method, RpcError};

/// A request for the browser. Answer it exactly once with `respond` (or by
/// sending on `reply`); dropping it answers "the browser dropped the
/// request". The server stops waiting after a method-specific timeout.
#[derive(Debug)]
pub struct Request {
    pub method: Method,
    /// Already validated against the method's parameter type.
    pub params: Value,
    pub reply: Sender<Result<Value, RpcError>>,
}

impl Request {
    /// The parameters as the method's type (see `protocol`).
    pub fn params<T: serde::de::DeserializeOwned>(&self) -> Result<T, RpcError> {
        protocol::parse_params(&self.params)
    }

    pub fn respond(&self, result: Result<Value, RpcError>) {
        let _ = self.reply.send(result);
    }

    /// Replies with `value` serialised.
    pub fn respond_with<T: Serialize>(&self, value: &T) {
        self.respond(serde_json::to_value(value).map_err(|e| RpcError::new(RpcError::INTERNAL, e.to_string())));
    }
}

#[cfg(unix)]
pub use server::Handle;

/// Starts the control server with the user's settings (`automation.toml`),
/// enabled for this run by `--automation`. Fails when automation is off,
/// another browser already listens, or the socket cannot be made private.
#[cfg(unix)]
pub fn start(callback: impl Fn(Request) + Send + Sync + 'static) -> Result<Handle, String> {
    let mut settings = Settings::load();
    if requested() {
        settings.enabled = true;
    }
    if !settings.enabled {
        return Err("automation is disabled (settings, or start with --automation)".into());
    }
    server::start_at(&socket_path()?, settings, callback)
}

#[cfg(not(unix))]
pub fn start(_callback: impl Fn(Request) + Send + Sync + 'static) -> Result<(), String> {
    Err("the control socket is only available on Linux and macOS for now".into())
}

// ---- the --automation flag ----

static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Removes `--automation` from the command line; remembers it was given.
pub fn take_cli_flag(args: &mut Vec<String>) -> bool {
    let before = args.len();
    args.retain(|a| a != "--automation");
    let found = args.len() != before;
    if found {
        REQUESTED.store(true, Ordering::SeqCst);
    }
    found
}

/// True when the browser was started with `--automation`.
pub fn requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}

/// Runs `ctl`, `mcp` or `headless` when the command line asks for one;
/// returns the exit code, or None to start the browser normally.
pub fn run_cli(args: &[String]) -> Option<i32> {
    let rest = args.get(2..).unwrap_or(&[]);
    match args.get(1).map(String::as_str)? {
        "ctl" => Some(ctl::main(rest)),
        "mcp" => Some(mcp::main(rest)),
        #[cfg(target_os = "linux")]
        "headless" => Some(crate::headless_webkit::main(rest)),
        #[cfg(not(target_os = "linux"))]
        "headless" => {
            eprintln!("browsecraft: headless WebKit is only available on Linux for now");
            Some(2)
        }
        _ => None,
    }
}

// ---- socket location ----

/// The control socket: `$BROWSECRAFT_CONTROL_SOCKET`, or
/// `$XDG_RUNTIME_DIR/browsecraft/control.sock`, or (without a runtime
/// directory) `<tmp>/browsecraft-<uid>/control.sock`.
pub fn socket_path() -> Result<PathBuf, String> {
    let abs = |key: &str| std::env::var_os(key).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(p) = abs("BROWSECRAFT_CONTROL_SOCKET") {
        return Ok(p);
    }
    if let Some(dir) = abs("XDG_RUNTIME_DIR") {
        return Ok(dir.join("browsecraft").join("control.sock"));
    }
    #[cfg(unix)]
    {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        Ok(std::env::temp_dir().join(format!("browsecraft-{uid}")).join("control.sock"))
    }
    #[cfg(not(unix))]
    Err("no runtime directory for the control socket".into())
}

// ---- permissions ----

/// What the automation API may do, from `automation.toml` in the config
/// folder. Off by default; when on, scripts in pages (`page.eval`) stay off
/// until allowed, and local files are not opened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Start the control server with the browser.
    pub enabled: bool,
    /// Allow `page.eval` (arbitrary JavaScript in the page).
    pub allow_eval: bool,
    /// Allow `tabs.open`, `page.navigate`, back, forward and reload.
    pub allow_navigation: bool,
    /// Allow `file://` URLs in `tabs.open` and `page.navigate`.
    pub allow_file_urls: bool,
    /// When set, only these hosts (and their subdomains) can be opened or
    /// read: navigation and `page.*` on other sites are refused.
    pub allowed_hosts: Option<Vec<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_eval: false,
            allow_navigation: true,
            allow_file_urls: false,
            allowed_hosts: None,
        }
    }
}

const SETTINGS_FILE: &str = "automation.toml";

impl Settings {
    /// The user's settings; the defaults when the file is missing or invalid
    /// (an invalid file is reported, never half-applied).
    pub fn load() -> Settings {
        let Ok(path) = crate::storage::config_file(SETTINGS_FILE) else {
            return Settings::default();
        };
        match crate::storage::read_text_limited(&path, crate::storage::MAX_SETTINGS_FILE) {
            Ok(text) => Self::parse(&text).unwrap_or_else(|e| {
                eprintln!("browsecraft: {}: {e}; automation uses the defaults", path.display());
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }

    pub fn parse(text: &str) -> Result<Settings, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn save(&self) -> Result<(), String> {
        let path = crate::storage::config_file(SETTINGS_FILE)?;
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::storage::write_private(&path, text.as_bytes()).map_err(|e| e.to_string())
    }

    /// Checks a validated request against the permissions. The browser
    /// must also call `page_allowed` with the tab's URL for `page.*` methods
    /// when `allowed_hosts` is set (only it knows which page a tab shows).
    pub fn check(&self, method: Method, params: &Value) -> Result<(), RpcError> {
        if method == Method::BrowserVersion {
            return Ok(());
        }
        if !self.enabled {
            return Err(RpcError::denied("automation is disabled in settings"));
        }
        match method {
            Method::PageEval if !self.allow_eval => Err(RpcError::denied(
                "page.eval is disabled: set allow_eval = true in automation.toml to allow scripts",
            )),
            Method::TabsOpen | Method::PageNavigate => {
                if !self.allow_navigation {
                    return Err(RpcError::denied("navigation is disabled in automation settings"));
                }
                let url = params.get("url").and_then(Value::as_str).unwrap_or_default();
                self.url_allowed(url)
            }
            Method::PageBack | Method::PageForward | Method::PageReload if !self.allow_navigation => {
                Err(RpcError::denied("navigation is disabled in automation settings"))
            }
            _ => Ok(()),
        }
    }

    /// Whether automation may open `url`. Only web pages (and
    /// `about:blank`): never `javascript:` (it would bypass `allow_eval`),
    /// `data:`, internal or view-source pages; `file:` only when allowed.
    pub fn url_allowed(&self, url: &str) -> Result<(), RpcError> {
        if url == "about:blank" {
            return Ok(());
        }
        let parsed = url::Url::parse(url).map_err(|e| RpcError::invalid_params(format!("invalid URL {url:?}: {e}")))?;
        match parsed.scheme() {
            "http" | "https" => self.host_allowed(parsed.host_str().unwrap_or_default()),
            "file" if self.allow_file_urls => Ok(()),
            "file" => Err(RpcError::denied("file: URLs are disabled (allow_file_urls in automation.toml)")),
            other => Err(RpcError::denied(format!("{other}: URLs cannot be opened by automation"))),
        }
    }

    /// Whether `page.*` methods may read or act on a tab showing `url`.
    pub fn page_allowed(&self, url: &str) -> Result<(), RpcError> {
        if self.allowed_hosts.is_none() {
            return Ok(());
        }
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        if url == "about:blank" || url.is_empty() {
            return Ok(());
        }
        self.host_allowed(&host)
    }

    fn host_allowed(&self, host: &str) -> Result<(), RpcError> {
        let Some(allowed) = &self.allowed_hosts else {
            return Ok(());
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let ok = !host.is_empty()
            && allowed.iter().any(|entry| {
                let entry = entry.trim().trim_start_matches("*.").trim_start_matches('.').to_ascii_lowercase();
                !entry.is_empty() && (host == entry || host.ends_with(&format!(".{entry}")))
            });
        if ok { Ok(()) } else { Err(RpcError::denied(format!("{host} is not in allowed_hosts"))) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn on() -> Settings {
        Settings { enabled: true, ..Settings::default() }
    }

    #[test]
    fn automation_settings_defaults_deny() {
        let s = Settings::default();
        assert!(!s.enabled && !s.allow_eval && s.allow_navigation && !s.allow_file_urls);
        assert_eq!(s.check(Method::TabsList, &json!({})).unwrap_err().code, RpcError::PERMISSION_DENIED);
        assert!(s.check(Method::BrowserVersion, &json!({})).is_ok());
        assert_eq!(on().check(Method::PageEval, &json!({"script": "1"})).unwrap_err().code, RpcError::PERMISSION_DENIED);
        let eval = Settings { allow_eval: true, ..on() };
        assert!(eval.check(Method::PageEval, &json!({"script": "1"})).is_ok());
    }

    #[test]
    fn automation_settings_parse() {
        let s = Settings::parse("enabled = true\nallow_eval = true\nallowed_hosts = [\"example.com\"]\n").unwrap();
        assert!(s.enabled && s.allow_eval && s.allow_navigation);
        assert!(Settings::parse("enabled = true\nallow_passwords = true\n").is_err(), "unknown keys refused");
        assert_eq!(Settings::parse("").unwrap(), Settings::default());
        let text = toml::to_string_pretty(&s).unwrap();
        assert_eq!(Settings::parse(&text).unwrap(), s);
    }

    #[test]
    fn automation_urls_and_hosts() {
        let s = on();
        let nav = |s: &Settings, url: &str| s.check(Method::PageNavigate, &json!({ "url": url }));
        assert!(nav(&s, "https://example.com/a").is_ok());
        assert!(nav(&s, "about:blank").is_ok());
        assert!(nav(&s, "javascript:alert(1)").is_err());
        assert!(nav(&s, "data:text/html,<script>1</script>").is_err());
        assert!(nav(&s, "browsecraft://settings").is_err());
        assert!(nav(&s, "view-source:https://a.test").is_err());
        assert!(nav(&s, "file:///etc/passwd").is_err());
        assert!(nav(&Settings { allow_file_urls: true, ..on() }, "file:///tmp/a.html").is_ok());
        assert_eq!(nav(&s, "not a url").unwrap_err().code, RpcError::INVALID_PARAMS);
        let no_nav = Settings { allow_navigation: false, ..on() };
        assert!(nav(&no_nav, "https://example.com").is_err());
        assert!(no_nav.check(Method::PageReload, &json!({})).is_err());
        assert!(no_nav.check(Method::PageText, &json!({})).is_ok());

        let hosts = Settings { allowed_hosts: Some(vec!["example.com".into(), "*.test.org".into()]), ..on() };
        assert!(nav(&hosts, "https://example.com/").is_ok());
        assert!(nav(&hosts, "https://www.EXAMPLE.com./").is_ok());
        assert!(nav(&hosts, "https://a.test.org/").is_ok());
        assert!(nav(&hosts, "https://badexample.com/").is_err());
        assert!(nav(&hosts, "https://example.com.evil.net/").is_err());
        assert!(hosts.page_allowed("https://example.com/x").is_ok());
        assert!(hosts.page_allowed("https://other.net/").is_err());
        assert!(hosts.page_allowed("about:blank").is_ok());
        assert!(on().page_allowed("https://other.net/").is_ok());
    }

    #[test]
    fn automation_cli_flag() {
        let mut args: Vec<String> = ["browsecraft", "--automation", "https://a.test"].map(String::from).into();
        assert!(take_cli_flag(&mut args));
        assert_eq!(args, ["browsecraft", "https://a.test"]);
        assert!(requested());
        assert!(!take_cli_flag(&mut args));
        assert_eq!(run_cli(&args), None);
    }
}
