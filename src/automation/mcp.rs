//! `browsecraft mcp`: a Model Context Protocol server on stdio
//! (newline-delimited JSON-RPC 2.0) whose tools forward to the control
//! socket, one tool per control method. Registered in Claude Code with
//! `claude mcp add browsecraft -- browsecraft mcp`.
//!
//! Hand-written rather than built on the `rmcp` SDK: the server needs four
//! methods over blocking stdio, while the SDK brings an async runtime
//! (tokio) and a schema generator into a browser that aims at low memory.
//!
//! When no browser is listening, the first tool call starts one
//! (`browsecraft --automation`) and waits for its socket.

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use super::ctl::{self, CallError};
use super::protocol::{Method, RpcError, RpcResponse};

/// Protocol versions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
/// Longest message read from the client.
const MAX_MESSAGE_BYTES: usize = 4 << 20;

const INSTRUCTIONS: &str = "Drives the BrowseCraft web browser (WebKit) the user is running. \
Typical loop: browser_navigate to a URL, browser_snapshot to list the interactive elements with \
refs such as e12, then browser_click / browser_type with that ref, and browser_text to read the \
result. browser_screenshot shows the page. Refs stay valid until the page navigates; take a new \
snapshot after navigation. Tabs are numbered by id (browser_tabs). browser_eval runs JavaScript \
only if the user allowed it. Passwords and the authenticator are never accessible.";

/// Calls control methods; the socket in production, a fake in tests.
pub trait Backend {
    fn call(&mut self, method: Method, params: Value) -> Result<Value, CallError>;
}

/// One MCP tool: the control method it calls and how it is described.
struct Tool {
    name: &'static str,
    method: Method,
    title: &'static str,
    description: &'static str,
    read_only: bool,
    schema: fn() -> Value,
}

fn tab_prop() -> Value {
    json!({ "type": "integer", "minimum": 0, "description": "Tab id from browser_tabs; the active tab when omitted." })
}

fn timeout_prop() -> Value {
    json!({ "type": "integer", "minimum": 1, "maximum": 120000, "description": "Milliseconds to wait (default 10000)." })
}

fn target_props(what: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("ref".into(), json!({ "type": "string", "pattern": "^e[0-9]{1,9}$", "description": format!("Ref of the {what} from browser_snapshot, such as e12. Preferred over selector.") }));
    m.insert("selector".into(), json!({ "type": "string", "description": format!("CSS selector of the {what}, if no ref is known.") }));
    m.insert("tab".into(), tab_prop());
    m
}

fn object(props: Map<String, Value>, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
}

fn props(list: &[(&str, Value)]) -> Map<String, Value> {
    list.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn no_args() -> Value {
    object(Map::new(), &[])
}

fn tab_only() -> Value {
    object(props(&[("tab", tab_prop())]), &[])
}

fn history_args() -> Value {
    object(
        props(&[
            ("tab", tab_prop()),
            ("wait", json!({ "type": "boolean", "description": "Wait for the page to load (default true)." })),
            ("timeout_ms", timeout_prop()),
        ]),
        &[],
    )
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "browser_tabs",
        method: Method::TabsList,
        title: "List tabs",
        description: "List the open tabs with their id, URL, title, container and which one is active.",
        read_only: true,
        schema: no_args,
    },
    Tool {
        name: "browser_tab_open",
        method: Method::TabsOpen,
        title: "Open tab",
        description: "Open a URL in a new tab, optionally inside a container (separate cookies and logins; ids from browser_containers).",
        read_only: false,
        schema: || {
            object(
                props(&[
                    ("url", json!({ "type": "string", "description": "http(s) URL to open." })),
                    ("container", json!({ "type": "string", "description": "Container id; the default profile when omitted." })),
                    ("background", json!({ "type": "boolean", "description": "Open without switching to it (default false)." })),
                ]),
                &["url"],
            )
        },
    },
    Tool {
        name: "browser_tab_activate",
        method: Method::TabsActivate,
        title: "Switch tab",
        description: "Switch to the tab with this id.",
        read_only: false,
        schema: || object(props(&[("tab", json!({ "type": "integer", "minimum": 0, "description": "Tab id from browser_tabs." }))]), &["tab"]),
    },
    Tool {
        name: "browser_tab_close",
        method: Method::TabsClose,
        title: "Close tab",
        description: "Close a tab (the active one when no id is given).",
        read_only: false,
        schema: tab_only,
    },
    Tool {
        name: "browser_navigate",
        method: Method::PageNavigate,
        title: "Navigate",
        description: "Load a URL in the active tab (or the given one) and wait until it has loaded. Returns the final URL and title.",
        read_only: false,
        schema: || {
            object(
                props(&[
                    ("url", json!({ "type": "string", "description": "http(s) URL to load." })),
                    ("tab", tab_prop()),
                    ("wait", json!({ "type": "boolean", "description": "Wait for the load to finish (default true)." })),
                    ("timeout_ms", timeout_prop()),
                ]),
                &["url"],
            )
        },
    },
    Tool { name: "browser_back", method: Method::PageBack, title: "Back", description: "Go back one page in the tab's history and wait for it to load.", read_only: false, schema: history_args },
    Tool { name: "browser_forward", method: Method::PageForward, title: "Forward", description: "Go forward one page in the tab's history and wait for it to load.", read_only: false, schema: history_args },
    Tool { name: "browser_reload", method: Method::PageReload, title: "Reload", description: "Reload the current page and wait for it to load.", read_only: false, schema: history_args },
    Tool {
        name: "browser_text",
        method: Method::PageText,
        title: "Read page text",
        description: "The page's visible text (innerText), or one element's with a selector. The cheapest way to read a page.",
        read_only: true,
        schema: || {
            object(
                props(&[
                    ("selector", json!({ "type": "string", "description": "CSS selector of the part to read, such as main or article." })),
                    ("max_chars", json!({ "type": "integer", "minimum": 1, "description": "Truncate after this many characters (default 50000)." })),
                    ("tab", tab_prop()),
                ]),
                &[],
            )
        },
    },
    Tool {
        name: "browser_html",
        method: Method::PageHtml,
        title: "Read page HTML",
        description: "The page's HTML (outerHTML), or one element's. Prefer browser_text or browser_snapshot: HTML is large.",
        read_only: true,
        schema: || {
            object(
                props(&[
                    ("selector", json!({ "type": "string", "description": "CSS selector of the element." })),
                    ("max_chars", json!({ "type": "integer", "minimum": 1, "description": "Truncate after this many characters (default 50000)." })),
                    ("tab", tab_prop()),
                ]),
                &[],
            )
        },
    },
    Tool {
        name: "browser_snapshot",
        method: Method::PageSnapshot,
        title: "Snapshot interactive elements",
        description: "List the visible links, buttons, fields and other interactive elements, each with a ref (e12), role, name and value, plus the page's headings. Use the refs with browser_click, browser_type, browser_press and browser_scroll.",
        read_only: true,
        schema: || {
            object(
                props(&[
                    ("max_elements", json!({ "type": "integer", "minimum": 1, "maximum": 5000, "description": "At most this many elements (default 300)." })),
                    ("viewport_only", json!({ "type": "boolean", "description": "Only elements currently on screen." })),
                    ("tab", tab_prop()),
                ]),
                &[],
            )
        },
    },
    Tool {
        name: "browser_click",
        method: Method::PageClick,
        title: "Click",
        description: "Click an element (scrolled into view first). Give its ref from browser_snapshot, or a CSS selector.",
        read_only: false,
        schema: || object(target_props("element"), &[]),
    },
    Tool {
        name: "browser_type",
        method: Method::PageType,
        title: "Type text",
        description: "Type text into a text field, text area, content-editable element, or choose a <select> option by value or label. Replaces the current value unless clear is false; submit sends the form.",
        read_only: false,
        schema: || {
            let mut p = target_props("field");
            p.insert("text".into(), json!({ "type": "string", "description": "Text to enter." }));
            p.insert("clear".into(), json!({ "type": "boolean", "description": "Replace the current value (default true)." }));
            p.insert("submit".into(), json!({ "type": "boolean", "description": "Submit the form afterwards (default false)." }));
            object(p, &["text"])
        },
    },
    Tool {
        name: "browser_press",
        method: Method::PagePress,
        title: "Press key",
        description: "Press a key on an element or the focused one: Enter, Tab, Escape, Backspace, ArrowDown, a single character… Enter in a form field submits the form.",
        read_only: false,
        schema: || {
            let mut p = target_props("element");
            p.insert("key".into(), json!({ "type": "string", "description": "A KeyboardEvent key value." }));
            object(p, &["key"])
        },
    },
    Tool {
        name: "browser_scroll",
        method: Method::PageScroll,
        title: "Scroll",
        description: "Scroll an element into view (ref or selector), to the top or bottom of the page, or by dx/dy pixels.",
        read_only: false,
        schema: || {
            let mut p = target_props("element");
            p.insert("to".into(), json!({ "type": "string", "enum": ["top", "bottom"] }));
            p.insert("dx".into(), json!({ "type": "number", "description": "Horizontal pixels." }));
            p.insert("dy".into(), json!({ "type": "number", "description": "Vertical pixels (positive scrolls down)." }));
            object(p, &[])
        },
    },
    Tool {
        name: "browser_eval",
        method: Method::PageEval,
        title: "Run JavaScript",
        description: "Run JavaScript in the page and return its JSON value. Give an expression (document.title, fetch(u).then(r => r.status)) or a function body that uses return (const r = await fetch(u); return r.status). Promises are awaited. Disabled unless the user set allow_eval in automation.toml.",
        read_only: false,
        schema: || {
            object(
                props(&[
                    ("script", json!({ "type": "string", "description": "JavaScript expression or function body." })),
                    ("timeout_ms", timeout_prop()),
                    ("tab", tab_prop()),
                ]),
                &["script"],
            )
        },
    },
    Tool {
        name: "browser_wait",
        method: Method::PageWait,
        title: "Wait",
        description: "Wait until an element matching selector is visible, the page text contains text, and/or (load: true) the page finished loading. Errors after timeout_ms.",
        read_only: true,
        schema: || {
            object(
                props(&[
                    ("selector", json!({ "type": "string", "description": "CSS selector to wait for." })),
                    ("text", json!({ "type": "string", "description": "Text to wait for." })),
                    ("load", json!({ "type": "boolean", "description": "Wait for the end of loading." })),
                    ("timeout_ms", timeout_prop()),
                    ("tab", tab_prop()),
                ]),
                &[],
            )
        },
    },
    Tool {
        name: "browser_screenshot",
        method: Method::PageScreenshot,
        title: "Screenshot",
        description: "A PNG screenshot of the visible page, or of the whole page with full_page.",
        read_only: true,
        schema: || object(props(&[("full_page", json!({ "type": "boolean", "description": "Capture the whole document." })), ("tab", tab_prop())]), &[]),
    },
    Tool {
        name: "browser_pdf",
        method: Method::PagePdf,
        title: "Save as PDF",
        description: "Print the page to PDF; returned as an embedded application/pdf resource.",
        read_only: true,
        schema: tab_only,
    },
    Tool {
        name: "browser_console",
        method: Method::ConsoleRead,
        title: "Read console",
        description: "Messages from the page's console, uncaught errors and unhandled promise rejections, oldest first. Pass the returned next as since to get only newer ones.",
        read_only: true,
        schema: || {
            object(
                props(&[
                    ("since", json!({ "type": "integer", "minimum": 0, "description": "Only entries from this sequence number (the previous call's next)." })),
                    ("level", json!({ "type": "string", "enum": ["debug", "log", "info", "warn", "error"], "description": "Minimum level." })),
                    ("limit", json!({ "type": "integer", "minimum": 1, "description": "At most this many entries (default 200)." })),
                    ("tab", tab_prop()),
                ]),
                &[],
            )
        },
    },
    Tool {
        name: "browser_containers",
        method: Method::ContainersList,
        title: "List containers",
        description: "List the containers (isolated cookie and storage profiles) with their ids, for browser_tab_open.",
        read_only: true,
        schema: no_args,
    },
    Tool {
        name: "browser_bookmarks",
        method: Method::BookmarksList,
        title: "Bookmarks",
        description: "List bookmarks, or those whose title or URL contains query.",
        read_only: true,
        schema: || {
            object(
                props(&[
                    ("query", json!({ "type": "string" })),
                    ("limit", json!({ "type": "integer", "minimum": 1 })),
                ]),
                &[],
            )
        },
    },
    Tool {
        name: "browser_history",
        method: Method::HistorySearch,
        title: "Search history",
        description: "Search the browsing history by title or URL; the most recent pages when query is empty.",
        read_only: true,
        schema: || {
            object(
                props(&[
                    ("query", json!({ "type": "string" })),
                    ("limit", json!({ "type": "integer", "minimum": 1 })),
                ]),
                &[],
            )
        },
    },
    Tool {
        name: "browser_version",
        method: Method::BrowserVersion,
        title: "Browser version",
        description: "The browser's name, version and automation API version; also checks the browser is reachable.",
        read_only: true,
        schema: no_args,
    },
];

fn tool_json(t: &Tool) -> Value {
    json!({
        "name": t.name,
        "title": t.title,
        "description": t.description,
        "inputSchema": (t.schema)(),
        "annotations": {
            "title": t.title,
            "readOnlyHint": t.read_only,
            "destructiveHint": matches!(t.method, Method::TabsClose),
            "idempotentHint": t.read_only,
            "openWorldHint": true,
        },
    })
}

/// The MCP session over one client.
pub struct Server<B: Backend> {
    backend: B,
}

impl<B: Backend> Server<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// Answers one message; None for notifications and client responses.
    pub fn handle(&mut self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let err = RpcError::new(RpcError::PARSE_ERROR, format!("invalid JSON: {e}"));
                return Some(RpcResponse::new(Value::Null, Err(err)).to_line());
            }
        };
        let method = msg.get("method").and_then(Value::as_str)?;
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let outcome = match method {
            "initialize" => Ok(Self::initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": TOOLS.iter().map(tool_json).collect::<Vec<_>>() })),
            "tools/call" => self.call_tool(&params),
            // notifications/initialized, notifications/cancelled, …
            _ if id.is_none() => return None,
            other => Err(RpcError::new(RpcError::METHOD_NOT_FOUND, format!("unknown method {other}"))),
        };
        id.map(|id| RpcResponse::new(id, outcome).to_line())
    }

    fn initialize(params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
        let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSIONS[0]);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "browsecraft", "title": "BrowseCraft", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        })
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, RpcError> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
        let tool = TOOLS
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| RpcError::invalid_params(format!("unknown tool {name}")))?;
        let args = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(v @ Value::Object(_)) => v.clone(),
            Some(_) => return Err(RpcError::invalid_params("arguments must be an object")),
        };
        // Errors of the call itself are tool results (isError), so the
        // model sees them and can correct its arguments.
        Ok(match self.backend.call(tool.method, args) {
            Ok(result) => tool_result(tool.method, result),
            Err(e) => json!({ "content": [{ "type": "text", "text": format!("Error: {e}") }], "isError": true }),
        })
    }
}

fn text(s: impl Into<String>) -> Value {
    json!({ "type": "text", "text": s.into() })
}

/// The tool result for a method's result: compact text for the model,
/// images for screenshots.
fn tool_result(method: Method, result: Value) -> Value {
    let pretty = || serde_json::to_string_pretty(&result).unwrap_or_default();
    let s = |k: &str| result.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let content = match method {
        Method::PageScreenshot => {
            let size = format!("{}x{} PNG", result["width"], result["height"]);
            vec![json!({ "type": "image", "data": s("png_base64"), "mimeType": "image/png" }), text(size)]
        }
        Method::PagePdf => vec![
            json!({ "type": "resource", "resource": { "uri": "browsecraft://page.pdf", "mimeType": "application/pdf", "blob": s("pdf_base64") } }),
            text(format!("PDF, {} bytes", result["bytes"])),
        ],
        Method::PageText => {
            let more = if result["truncated"] == true { "\n\n[truncated: pass max_chars or a selector]" } else { "" };
            vec![text(format!("URL: {}\nTitle: {}\n\n{}{more}", s("url"), s("title"), s("text")))]
        }
        Method::PageHtml => vec![text(s("html"))],
        Method::PageSnapshot => vec![text(snapshot_text(&result))],
        _ => vec![text(pretty())],
    };
    json!({ "content": content, "isError": false })
}

/// A snapshot as one line per element: `[e3] button "Sign in"`.
fn snapshot_text(r: &Value) -> String {
    let mut out = format!(
        "URL: {}\nTitle: {}\n",
        r["url"].as_str().unwrap_or_default(),
        r["title"].as_str().unwrap_or_default()
    );
    if let Some(h) = r["headings"].as_array().filter(|h| !h.is_empty()) {
        out.push_str("Headings:\n");
        for x in h {
            out.push_str(&format!("  {}\n", x.as_str().unwrap_or_default()));
        }
    }
    out.push_str("Elements:\n");
    for e in r["elements"].as_array().into_iter().flatten() {
        let g = |k: &str| e[k].as_str().unwrap_or_default();
        out.push_str(&format!("[{}] {} {:?}", g("ref"), g("role"), g("name")));
        if !g("value").is_empty() {
            out.push_str(&format!(" value={:?}", g("value")));
        }
        if let Some(c) = e["checked"].as_bool() {
            out.push_str(if c { " checked" } else { " unchecked" });
        }
        if e["disabled"] == true {
            out.push_str(" disabled");
        }
        if !g("href").is_empty() {
            out.push_str(&format!(" -> {}", g("href")));
        }
        if e["in_viewport"] != true {
            out.push_str(" (off-screen)");
        }
        out.push('\n');
    }
    if r["truncated"] == true {
        out.push_str("[more elements: raise max_elements or use viewport_only]\n");
    }
    out
}

// ---- the socket backend ----

/// Forwards to the control socket, starting the browser when none listens.
pub struct SocketBackend {
    launch: bool,
    launch_timeout: Duration,
    child: Option<std::process::Child>,
}

impl SocketBackend {
    fn call_once(method: Method, params: &Value) -> Result<Value, CallError> {
        ctl::call(method.name(), params.clone())
    }

    /// Starts `browsecraft --automation` and waits for its socket.
    fn launch_browser(&mut self) -> Result<(), CallError> {
        if let Some(child) = self.child.as_mut()
            && matches!(child.try_wait(), Ok(None))
        {
            // Still starting (or running without automation): only wait.
            return self.wait_for_socket();
        }
        let exe = std::env::current_exe().map_err(|e| CallError::Connect(e.to_string()))?;
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--automation")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Its own process group: the client stopping us must not stop it.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let child = cmd.spawn().map_err(|e| CallError::Connect(format!("cannot start the browser: {e}")))?;
        eprintln!("browsecraft mcp: started the browser (pid {})", child.id());
        self.child = Some(child);
        self.wait_for_socket()
    }

    fn wait_for_socket(&self) -> Result<(), CallError> {
        let deadline = Instant::now() + self.launch_timeout;
        loop {
            if Self::call_once(Method::BrowserVersion, &json!({})).is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(CallError::Connect(format!(
                    "the browser did not open its control socket within {} s (is automation supported by this build?)",
                    self.launch_timeout.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl Backend for SocketBackend {
    fn call(&mut self, method: Method, params: Value) -> Result<Value, CallError> {
        match Self::call_once(method, &params) {
            Err(CallError::Connect(e)) if self.launch => {
                eprintln!("browsecraft mcp: {e}; starting the browser");
                self.launch_browser()?;
                Self::call_once(method, &params)
            }
            other => other,
        }
    }
}

fn usage() -> &'static str {
    "usage: browsecraft mcp [--no-launch] [--launch-timeout SECONDS]\n\n\
     MCP server on stdio for AI agents. Register it with\n  claude mcp add browsecraft -- browsecraft mcp\n\
     Starts `browsecraft --automation` on the first tool call when no browser listens\n\
     (unless --no-launch). See docs/AUTOMATION.md.\n"
}

/// The `mcp` command; returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let mut backend = SocketBackend { launch: true, launch_timeout: Duration::from_secs(20), child: None };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--no-launch" => backend.launch = false,
            "--launch-timeout" => match it.next().and_then(|s| s.parse::<u64>().ok()) {
                Some(s) if (1..=600).contains(&s) => backend.launch_timeout = Duration::from_secs(s),
                _ => {
                    eprint!("{}", usage());
                    return 2;
                }
            },
            "-h" | "--help" => {
                print!("{}", usage());
                return 0;
            }
            _ => {
                eprint!("{}", usage());
                return 2;
            }
        }
    }
    serve(Server::new(backend), std::io::stdin().lock(), std::io::stdout().lock())
}

/// Serves messages from `input` until it closes.
pub fn serve<B: Backend>(mut server: Server<B>, mut input: impl BufRead, mut output: impl Write) -> i32 {
    loop {
        let line = match read_line(&mut input) {
            Ok(Some(line)) => line,
            Ok(None) => return 0,
            Err(e) => {
                eprintln!("browsecraft mcp: {e}");
                let err = RpcError::new(RpcError::INVALID_REQUEST, format!("message larger than {MAX_MESSAGE_BYTES} bytes"));
                let _ = writeln!(output, "{}", RpcResponse::new(Value::Null, Err(err)).to_line());
                return 1;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = server.handle(&line)
            && writeln!(output, "{response}").and_then(|_| output.flush()).is_err()
        {
            return 0;
        }
    }
}

fn read_line(input: &mut impl BufRead) -> std::io::Result<Option<String>> {
    use std::io::Read;
    let mut buf = Vec::new();
    let n = input.by_ref().take(MAX_MESSAGE_BYTES as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') && buf.len() > MAX_MESSAGE_BYTES {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "message too long"));
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake browser behind the tools.
    struct Fake(Vec<(Method, Value)>);

    impl Backend for Fake {
        fn call(&mut self, method: Method, params: Value) -> Result<Value, CallError> {
            self.0.push((method, params.clone()));
            method.validate(&params).map_err(CallError::Rpc)?;
            match method {
                Method::PageScreenshot => Ok(json!({"png_base64": "iVBORw==", "width": 2, "height": 3})),
                Method::PageText => Ok(json!({"url": "https://a.test/", "title": "A", "text": "Hello", "truncated": false})),
                Method::PageSnapshot => Ok(json!({"url": "https://a.test/", "title": "A", "headings": ["h1 Welcome"], "truncated": false,
                    "elements": [{"ref": "e1", "role": "link", "name": "Docs", "tag": "a", "href": "https://a.test/docs", "in_viewport": true},
                                 {"ref": "e2", "role": "checkbox", "name": "Remember", "tag": "input", "checked": false}]})),
                Method::PageEval => Err(CallError::Rpc(RpcError::denied("page.eval is disabled"))),
                _ => Ok(json!({"ok": true})),
            }
        }
    }

    fn rpc(server: &mut Server<Fake>, msg: Value) -> Value {
        serde_json::from_str(&server.handle(&msg.to_string()).unwrap()).unwrap()
    }

    #[test]
    fn automation_mcp_initialize_and_list() {
        let mut s = Server::new(Fake(Vec::new()));
        let r = rpc(&mut s, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}));
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "browsecraft");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        let r = rpc(&mut s, json!({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {"protocolVersion": "2099-01-01"}}));
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
        assert!(s.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        assert_eq!(rpc(&mut s, json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}))["result"], json!({}));
        assert_eq!(rpc(&mut s, json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}))["error"]["code"], RpcError::METHOD_NOT_FOUND);

        let r = rpc(&mut s, json!({"jsonrpc": "2.0", "id": 5, "method": "tools/list"}));
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), Method::ALL.len(), "one tool per control method");
        for &m in Method::ALL {
            assert!(TOOLS.iter().any(|t| t.method == m), "no tool for {m}");
        }
        let mut names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), tools.len());
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
            assert!(t["description"].as_str().unwrap().len() > 20);
        }
    }

    #[test]
    fn automation_mcp_schemas_match_params() {
        // Every property a schema advertises is accepted by the method,
        // and every required one is required.
        for t in TOOLS {
            let schema = (t.schema)();
            let props = schema["properties"].as_object().unwrap();
            let required: Vec<&str> = schema["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
            let sample = |k: &str, p: &Value| match p["type"].as_str().unwrap() {
                "string" if k == "ref" => json!("e1"),
                "string" if k == "to" => json!("top"),
                "string" if k == "level" => json!("warn"),
                "string" if k == "url" => json!("https://a.test/"),
                "string" => json!("x"),
                "integer" => json!(1),
                "number" => json!(1.5),
                "boolean" => json!(true),
                other => panic!("{other}"),
            };
            let mut base = Map::new();
            for r in &required {
                base.insert(r.to_string(), sample(r, &props[*r]));
            }
            assert!(t.method.validate(&Value::Object(base.clone())).is_ok() || matches!(t.method, Method::PageClick | Method::PageWait), "{}", t.name);
            for (k, p) in props {
                if k == "ref" || k == "selector" {
                    continue;
                }
                let mut args = base.clone();
                args.insert(k.clone(), sample(k, p));
                let r = crate::automation::protocol::parse_params::<Value>(&Value::Object(args.clone()));
                assert!(r.is_ok());
                // Type-level check through the method's own parameter struct.
                let ok = t.method.validate(&Value::Object(args)).is_ok();
                assert!(ok || matches!(t.method, Method::PageWait | Method::PageClick), "{}: {k}", t.name);
            }
            for r in &required {
                let mut args = base.clone();
                args.remove(*r);
                assert!(t.method.validate(&Value::Object(args)).is_err(), "{}: {r} should be required", t.name);
            }
        }
    }

    #[test]
    fn automation_mcp_tools_call() {
        let mut s = Server::new(Fake(Vec::new()));
        let call = |s: &mut Server<Fake>, name: &str, args: Value| {
            rpc(s, json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": name, "arguments": args}}))
        };
        let r = call(&mut s, "browser_navigate", json!({"url": "https://a.test/"}));
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(s.backend.0.last().unwrap(), &(Method::PageNavigate, json!({"url": "https://a.test/"})));

        let r = call(&mut s, "browser_screenshot", json!({}));
        assert_eq!(r["result"]["content"][0], json!({"type": "image", "data": "iVBORw==", "mimeType": "image/png"}));

        let r = call(&mut s, "browser_text", json!({}));
        assert_eq!(r["result"]["content"][0]["text"], "URL: https://a.test/\nTitle: A\n\nHello");

        let r = call(&mut s, "browser_snapshot", json!({}));
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("[e1] link \"Docs\" -> https://a.test/docs\n"), "{text}");
        assert!(text.contains("[e2] checkbox \"Remember\" unchecked (off-screen)"), "{text}");
        assert!(text.contains("h1 Welcome"));

        let r = call(&mut s, "browser_eval", json!({"script": "1"}));
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("disabled"));

        let r = call(&mut s, "browser_click", json!({"selector": "#a", "bogus": 1}));
        assert_eq!(r["result"]["isError"], true, "bad arguments are tool errors");

        let r = call(&mut s, "browser_nope", json!({}));
        assert_eq!(r["error"]["code"], RpcError::INVALID_PARAMS);
        let r = rpc(&mut s, json!({"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {"name": "browser_tabs", "arguments": [1]}}));
        assert_eq!(r["error"]["code"], RpcError::INVALID_PARAMS);
        let r = rpc(&mut s, json!({"jsonrpc": "2.0", "id": 11, "method": "tools/call", "params": {"name": "browser_tabs"}}));
        assert_eq!(r["result"]["isError"], false, "arguments are optional");
    }

    #[test]
    fn automation_mcp_stdio_loop() {
        let input = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}).to_string(),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
            String::new(),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "browser_tabs"}}).to_string(),
            "not json".into(),
        ]
        .join("\n");
        let mut out = Vec::new();
        assert_eq!(serve(Server::new(Fake(Vec::new())), input.as_bytes(), &mut out), 0);
        let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[1]["id"], 2);
        assert_eq!(lines[2]["error"]["code"], RpcError::PARSE_ERROR);
    }

    #[cfg(unix)]
    #[test]
    fn automation_mcp_over_the_socket() {
        // The real backend path against a server with a fake browser.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("m").join("control.sock");
        let _h = crate::automation::server::start_at(
            &path,
            crate::automation::Settings { enabled: true, ..Default::default() },
            crate::automation::server::tests::fake_browser,
        )
        .unwrap();
        struct At(std::path::PathBuf);
        impl Backend for At {
            fn call(&mut self, method: Method, params: Value) -> Result<Value, CallError> {
                ctl::call_at(&self.0, method.name(), params)
            }
        }
        let mut s = Server::new(At(path));
        let r: Value = serde_json::from_str(&s.handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "browser_tabs"}}).to_string()).unwrap()).unwrap();
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("https://a.test/"));
    }
}
