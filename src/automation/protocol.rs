//! The control protocol: JSON-RPC 2.0, one message per line, over the
//! control socket. This file is the reference for every method: its name,
//! its parameters (deserialised strictly: unknown fields are refused, so a
//! typo is an error rather than a silently ignored option) and the shape of
//! its result.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Version of the method set, bumped when a method changes incompatibly.
pub const API_VERSION: u32 = 1;

/// Longest request line the server reads (eval scripts, typed text).
pub const MAX_REQUEST_BYTES: usize = 1 << 20;
/// Longest response line the clients read (screenshots, PDFs, page HTML).
pub const MAX_RESPONSE_BYTES: usize = 64 << 20;

/// Default and maximum wait for page operations, in milliseconds.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub const MAX_TIMEOUT_MS: u64 = 120_000;
/// Default and maximum characters returned by `page.text` / `page.html`.
pub const DEFAULT_MAX_CHARS: usize = 50_000;
pub const MAX_MAX_CHARS: usize = 2_000_000;
/// Default and maximum elements in a `page.snapshot`.
pub const DEFAULT_MAX_ELEMENTS: usize = 300;
pub const MAX_MAX_ELEMENTS: usize = 5_000;

// ---- errors ----

/// A JSON-RPC error object. Codes from -32768 to -32000 are JSON-RPC's;
/// the browser's own are in the -32000..-32099 "server error" range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL: i64 = -32603;
    /// The browser could not do it (no such tab, page error, …).
    pub const BROWSER: i64 = -32000;
    /// Refused by the automation permissions in settings.
    pub const PERMISSION_DENIED: i64 = -32001;
    /// The operation (or the browser's answer) took too long.
    pub const TIMEOUT: i64 = -32002;
    /// No element matches the selector or ref.
    pub const NO_ELEMENT: i64 = -32003;
    /// The server is busy (too many connections).
    pub const BUSY: i64 = -32004;
    /// A script running in the page threw.
    pub const SCRIPT: i64 = -32005;

    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), data: None }
    }
    pub fn browser(message: impl Into<String>) -> Self {
        Self::new(Self::BROWSER, message)
    }
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(Self::INVALID_PARAMS, message)
    }
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(Self::PERMISSION_DENIED, message)
    }
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(Self::TIMEOUT, message)
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

// ---- JSON-RPC envelopes ----

/// An incoming JSON-RPC request. `id` is absent for notifications.
#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// An outgoing JSON-RPC response.
#[derive(Debug, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    pub fn new(id: Value, outcome: Result<Value, RpcError>) -> Self {
        let (result, error) = match outcome {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e)),
        };
        Self { jsonrpc: "2.0".into(), id, result, error }
    }

    /// The response as one line (no trailing newline).
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"unserialisable response"}}"#.into()
        })
    }
}

// ---- methods ----

macro_rules! methods {
    ($($variant:ident = $name:literal => $params:ty : $doc:literal,)*) => {
        /// Every method of the control API.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum Method {
            $(#[doc = $doc] #[serde(rename = $name)] $variant,)*
        }

        impl Method {
            pub const ALL: &'static [Method] = &[$(Method::$variant,)*];

            /// The wire name, such as `page.navigate`.
            pub fn name(self) -> &'static str {
                match self { $(Method::$variant => $name,)* }
            }

            pub fn from_name(name: &str) -> Option<Method> {
                match name { $($name => Some(Method::$variant),)* _ => None }
            }

            /// One-line description (also used by `ctl --help`).
            pub fn doc(self) -> &'static str {
                match self { $(Method::$variant => $doc,)* }
            }

            /// Checks `params` against the method's parameter type.
            pub fn validate(self, params: &Value) -> Result<(), RpcError> {
                match self {
                    $(Method::$variant => parse_params::<$params>(params).map(|_| ()),)*
                }
            }
        }
    };
}

methods! {
    TabsList = "tabs.list" => NoParams : "List the open tabs.",
    TabsOpen = "tabs.open" => TabsOpenParams : "Open a URL in a new tab, optionally in a container.",
    TabsActivate = "tabs.activate" => TabParams : "Switch to a tab.",
    TabsClose = "tabs.close" => OptTabParams : "Close a tab (the active one by default).",
    PageNavigate = "page.navigate" => NavigateParams : "Load a URL in a tab and wait for it to load.",
    PageBack = "page.back" => HistoryParams : "Go back in the tab's history.",
    PageForward = "page.forward" => HistoryParams : "Go forward in the tab's history.",
    PageReload = "page.reload" => HistoryParams : "Reload the page.",
    PageText = "page.text" => ReadParams : "The page's readable text (innerText).",
    PageHtml = "page.html" => ReadParams : "The page's HTML (outerHTML).",
    PageSnapshot = "page.snapshot" => SnapshotParams : "Interactive elements with stable refs, for clicking and typing.",
    PageClick = "page.click" => ClickParams : "Click an element by selector or ref.",
    PageType = "page.type" => TypeParams : "Type text into a field.",
    PagePress = "page.press" => PressParams : "Press a key (Enter, Tab, Escape, ArrowDown…).",
    PageScroll = "page.scroll" => ScrollParams : "Scroll the page or an element into view.",
    PageEval = "page.eval" => EvalParams : "Run JavaScript in the page (off unless allowed in settings).",
    PageWait = "page.wait" => WaitParams : "Wait for a selector, text, or the end of loading.",
    PageScreenshot = "page.screenshot" => ScreenshotParams : "PNG screenshot of the page, base64.",
    PagePdf = "page.pdf" => OptTabParams : "PDF of the page, base64.",
    ConsoleRead = "console.read" => ConsoleReadParams : "Messages from the page's console and errors.",
    ContainersList = "containers.list" => NoParams : "List the containers.",
    BookmarksList = "bookmarks.list" => BookmarksParams : "List or search bookmarks.",
    HistorySearch = "history.search" => HistorySearchParams : "Search the browsing history.",
    BrowserVersion = "browser.version" => NoParams : "Browser name, version and API version.",
}

/// Parses `params` (null or absent means `{}`) into `T`.
pub fn parse_params<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, RpcError> {
    let params = if params.is_null() { Value::Object(Default::default()) } else { params.clone() };
    serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

// ---- parameters ----

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

/// A tab: the `id` from `tabs.list`.
pub type TabId = u64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabsOpenParams {
    pub url: String,
    /// Container id (`containers.list`); the default profile when absent.
    #[serde(default)]
    pub container: Option<String>,
    /// Open without switching to the tab.
    #[serde(default)]
    pub background: bool,
    /// The window to open it in (`tabs.list`'s `windows[].id`); the window
    /// last used when absent.
    #[serde(default)]
    pub window: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabParams {
    pub tab: TabId,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptTabParams {
    /// The active tab when absent.
    #[serde(default)]
    pub tab: Option<TabId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NavigateParams {
    pub url: String,
    #[serde(default)]
    pub tab: Option<TabId>,
    /// Answer after the page has loaded (default) or right away.
    #[serde(default = "yes")]
    pub wait: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryParams {
    #[serde(default)]
    pub tab: Option<TabId>,
    #[serde(default = "yes")]
    pub wait: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl Default for HistoryParams {
    fn default() -> Self {
        Self { tab: None, wait: true, timeout_ms: None }
    }
}

/// `page.text` and `page.html`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadParams {
    #[serde(default)]
    pub tab: Option<TabId>,
    /// Only this element (the whole document when absent).
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub max_chars: Option<usize>,
}

impl ReadParams {
    pub fn max_chars(&self) -> usize {
        self.max_chars.unwrap_or(DEFAULT_MAX_CHARS).clamp(1, MAX_MAX_CHARS)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotParams {
    #[serde(default)]
    pub tab: Option<TabId>,
    #[serde(default)]
    pub max_elements: Option<usize>,
    /// Only elements inside the viewport.
    #[serde(default)]
    pub viewport_only: bool,
}

impl SnapshotParams {
    pub fn max_elements(&self) -> usize {
        self.max_elements.unwrap_or(DEFAULT_MAX_ELEMENTS).clamp(1, MAX_MAX_ELEMENTS)
    }
}

/// Which element an action targets: a CSS selector, or a `ref` from
/// `page.snapshot` (such as `e12`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Selector(String),
    Ref(String),
}

fn target(selector: &Option<String>, reference: &Option<String>) -> Result<Target, RpcError> {
    match (selector, reference) {
        (Some(s), None) if !s.trim().is_empty() => Ok(Target::Selector(s.clone())),
        (None, Some(r)) if is_valid_ref(r) => Ok(Target::Ref(r.clone())),
        (None, Some(_)) => Err(RpcError::invalid_params("ref must look like e12 (from page.snapshot)")),
        (Some(_), Some(_)) => Err(RpcError::invalid_params("give selector or ref, not both")),
        _ => Err(RpcError::invalid_params("selector or ref is required")),
    }
}

/// A snapshot ref: `e` and 1–9 digits.
pub fn is_valid_ref(r: &str) -> bool {
    r.len() > 1 && r.len() <= 10 && r.starts_with('e') && r[1..].bytes().all(|b| b.is_ascii_digit())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClickParams {
    #[serde(default)]
    pub tab: Option<TabId>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
}

impl ClickParams {
    pub fn target(&self) -> Result<Target, RpcError> {
        target(&self.selector, &self.reference)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeParams {
    #[serde(default)]
    pub tab: Option<TabId>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    pub text: String,
    /// Replace the field's value (default) or append to it.
    #[serde(default = "yes")]
    pub clear: bool,
    /// Submit the field's form afterwards.
    #[serde(default)]
    pub submit: bool,
}

impl TypeParams {
    pub fn target(&self) -> Result<Target, RpcError> {
        target(&self.selector, &self.reference)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PressParams {
    #[serde(default)]
    pub tab: Option<TabId>,
    /// A `KeyboardEvent.key` value: `Enter`, `Tab`, `Escape`, `ArrowDown`, `a`…
    pub key: String,
    /// The element that receives the key; the focused element when absent.
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
}

impl PressParams {
    /// The target element, or None for the focused element.
    pub fn target(&self) -> Result<Option<Target>, RpcError> {
        if self.selector.is_none() && self.reference.is_none() {
            return Ok(None);
        }
        target(&self.selector, &self.reference).map(Some)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrollTo {
    Top,
    Bottom,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScrollParams {
    #[serde(default)]
    pub tab: Option<TabId>,
    /// Scroll this element into view instead of scrolling by an amount.
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub dx: f64,
    #[serde(default)]
    pub dy: f64,
    #[serde(default)]
    pub to: Option<ScrollTo>,
}

impl ScrollParams {
    pub fn target(&self) -> Result<Option<Target>, RpcError> {
        if self.selector.is_none() && self.reference.is_none() {
            return Ok(None);
        }
        target(&self.selector, &self.reference).map(Some)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalParams {
    #[serde(default)]
    pub tab: Option<TabId>,
    /// A JavaScript expression or statements; a returned promise is awaited.
    pub script: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitParams {
    #[serde(default)]
    pub tab: Option<TabId>,
    /// Wait until an element matches.
    #[serde(default)]
    pub selector: Option<String>,
    /// Wait until the page's text contains this.
    #[serde(default)]
    pub text: Option<String>,
    /// Wait until the page has finished loading.
    #[serde(default)]
    pub load: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotParams {
    #[serde(default)]
    pub tab: Option<TabId>,
    /// The whole document instead of the visible part.
    #[serde(default)]
    pub full_page: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsoleReadParams {
    #[serde(default)]
    pub tab: Option<TabId>,
    /// Only entries with a sequence number at or after this (from `next`).
    #[serde(default)]
    pub since: Option<u64>,
    /// Only this level and more severe.
    #[serde(default)]
    pub level: Option<crate::automation::console::Level>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookmarksParams {
    /// Case-insensitive match on title or URL; all bookmarks when absent.
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistorySearchParams {
    /// Case-insensitive match on title or URL; the most recent when empty.
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// The timeout a request asks for, clamped; the default when absent.
pub fn timeout_ms(requested: Option<u64>) -> u64 {
    requested.unwrap_or(DEFAULT_TIMEOUT_MS).clamp(1, MAX_TIMEOUT_MS)
}

// ---- results ----

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: TabId,
    pub index: usize,
    pub url: String,
    pub title: String,
    pub active: bool,
    pub loading: bool,
    #[serde(default)]
    pub container: Option<String>,
    /// The tab group the tab is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<TabGroupInfo>,
    /// The tab is shown in split view: `"left"` or `"right"` pane, with the
    /// other tab's id. The active tab is the focused pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<SplitInfo>,
    /// The browser window the tab is in. `index` counts within it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<TabWindow>,
}

/// A tab's window, in `TabInfo`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabWindow {
    pub id: u64,
    /// The name the user gave it; none when unnamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A browser window, in `tabs.list`'s `windows`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The name, or "Window N" (its position).
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub tabs: usize,
    /// The window the user last used.
    #[serde(default)]
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabGroupInfo {
    pub id: u64,
    #[serde(default)]
    pub name: String,
    /// One of grey, blue, red, yellow, green, pink, purple, cyan, orange.
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SplitInfo {
    pub side: String,
    pub with: TabId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabsListResult {
    pub tabs: Vec<TabInfo>,
    /// Every browser window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<WindowInfo>,
}

/// `tabs.close`, `page.click`, `page.type`, `page.press`, `page.scroll`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionResult {
    pub ok: bool,
    /// What was acted on, such as `button "Sign in"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `page.navigate`, `page.back`, `page.forward`, `page.reload`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageInfo {
    pub tab: TabId,
    pub url: String,
    pub title: String,
    pub loading: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextResult {
    pub url: String,
    pub title: String,
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HtmlResult {
    pub url: String,
    pub html: String,
    pub truncated: bool,
}

/// One element of a `page.snapshot`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotElement {
    /// Stable id, valid as `ref` in actions until the page navigates.
    #[serde(rename = "ref")]
    pub reference: String,
    /// ARIA role (explicit or implied): link, button, textbox, checkbox…
    pub role: String,
    /// Accessible name: label, aria-label, text, placeholder, alt.
    pub name: String,
    pub tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_viewport: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotResult {
    pub url: String,
    pub title: String,
    /// Headings outline (`h1`–`h3`), for orientation.
    pub headings: Vec<String>,
    pub elements: Vec<SnapshotElement>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvalResult {
    /// The result, as JSON (undefined and functions become null).
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaitResult {
    pub found: bool,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotResult {
    pub png_base64: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PdfResult {
    pub pdf_base64: String,
    pub bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConsoleReadResult {
    pub entries: Vec<crate::automation::console::ConsoleEntry>,
    /// Pass as `since` to read only newer entries.
    pub next: u64,
    /// Entries dropped by the page-side rate limit or the ring buffer.
    pub dropped: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub color: String,
    pub icon: String,
    pub temporary: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainersListResult {
    pub containers: Vec<ContainerInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BookmarkInfo {
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BookmarksListResult {
    pub bookmarks: Vec<BookmarkInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub title: String,
    pub url: String,
    /// Last visit, seconds since the Unix epoch, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visited: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistorySearchResult {
    pub entries: Vec<HistoryEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VersionResult {
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub methods: Vec<String>,
}

impl VersionResult {
    pub fn current() -> Self {
        Self {
            name: "BrowseCraft".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            api_version: API_VERSION,
            methods: Method::ALL.iter().map(|m| m.name().to_string()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn automation_method_names_round_trip() {
        for &m in Method::ALL {
            assert_eq!(Method::from_name(m.name()), Some(m));
            let wire = serde_json::to_value(m).unwrap();
            assert_eq!(wire, json!(m.name()));
            assert_eq!(serde_json::from_value::<Method>(wire).unwrap(), m);
            assert!(!m.doc().is_empty());
        }
        assert_eq!(Method::ALL.len(), 24);
        assert_eq!(Method::from_name("page.passwords"), None);
    }

    #[test]
    fn automation_params_are_strict() {
        assert!(Method::TabsList.validate(&Value::Null).is_ok());
        assert!(Method::TabsList.validate(&json!({})).is_ok());
        assert!(Method::TabsList.validate(&json!({"x": 1})).is_err());
        assert!(Method::PageNavigate.validate(&json!({})).is_err(), "url is required");
        let err = Method::PageNavigate.validate(&json!({"url": "https://a.test", "wiat": false})).unwrap_err();
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(err.message.contains("wiat"));
        let p: NavigateParams = parse_params(&json!({"url": "https://a.test"})).unwrap();
        assert!(p.wait && p.tab.is_none());
        let p: TabsOpenParams = parse_params(&json!({"url": "u", "container": "work", "background": true})).unwrap();
        assert_eq!((p.container.as_deref(), p.background), (Some("work"), true));
        let p: TypeParams = parse_params(&json!({"ref": "e3", "text": "hi"})).unwrap();
        assert_eq!(p.target().unwrap(), Target::Ref("e3".into()));
        assert!(p.clear && !p.submit);
        let p: ScrollParams = parse_params(&json!({"to": "bottom"})).unwrap();
        assert_eq!((p.to, p.target().unwrap()), (Some(ScrollTo::Bottom), None));
        assert!(Method::PageScroll.validate(&json!({"to": "middle"})).is_err());
        let p: ConsoleReadParams = parse_params(&json!({"level": "warn", "since": 4})).unwrap();
        assert_eq!(p.since, Some(4));
    }

    #[test]
    fn automation_targets() {
        let click = |v: Value| parse_params::<ClickParams>(&v).unwrap().target();
        assert_eq!(click(json!({"selector": "#a"})).unwrap(), Target::Selector("#a".into()));
        assert!(click(json!({})).is_err());
        assert!(click(json!({"selector": "  "})).is_err());
        assert!(click(json!({"selector": "#a", "ref": "e1"})).is_err());
        assert!(click(json!({"ref": "e1\"]"})).is_err(), "refs go into selectors: strict");
        assert!(click(json!({"ref": "x1"})).is_err());
        assert!(is_valid_ref("e123") && !is_valid_ref("e") && !is_valid_ref("e12345678901"));
    }

    #[test]
    fn automation_limits_are_clamped() {
        assert_eq!(timeout_ms(None), DEFAULT_TIMEOUT_MS);
        assert_eq!(timeout_ms(Some(u64::MAX)), MAX_TIMEOUT_MS);
        assert_eq!(timeout_ms(Some(0)), 1);
        let r = ReadParams { max_chars: Some(usize::MAX), ..Default::default() };
        assert_eq!(r.max_chars(), MAX_MAX_CHARS);
        assert_eq!(SnapshotParams::default().max_elements(), DEFAULT_MAX_ELEMENTS);
    }

    #[test]
    fn automation_envelopes() {
        let req: RpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"method":"tabs.list"}"#).unwrap();
        assert_eq!((req.id, req.method.as_str(), req.params), (Some(json!(7)), "tabs.list", None));
        let ok = RpcResponse::new(json!(1), Ok(json!({"a": 1}))).to_line();
        assert_eq!(ok, r#"{"jsonrpc":"2.0","id":1,"result":{"a":1}}"#);
        let err = RpcResponse::new(json!("x"), Err(RpcError::denied("no"))).to_line();
        assert_eq!(err, r#"{"jsonrpc":"2.0","id":"x","error":{"code":-32001,"message":"no"}}"#);
        let back: RpcResponse = serde_json::from_str(&err).unwrap();
        assert_eq!(back.error.unwrap().code, RpcError::PERMISSION_DENIED);
    }

    #[test]
    fn automation_results_serialise() {
        let el = SnapshotElement {
            reference: "e1".into(),
            role: "button".into(),
            name: "Go".into(),
            tag: "button".into(),
            value: None,
            href: None,
            checked: None,
            disabled: false,
            in_viewport: true,
        };
        assert_eq!(
            serde_json::to_value(&el).unwrap(),
            json!({"ref": "e1", "role": "button", "name": "Go", "tag": "button", "in_viewport": true})
        );
        let v = VersionResult::current();
        assert_eq!(v.api_version, API_VERSION);
        assert!(v.methods.iter().any(|m| m == "page.snapshot"));
    }

    #[test]
    fn tab_info_group_and_split_are_optional() {
        let old: TabInfo = serde_json::from_value(json!({
            "id": 1, "index": 0, "url": "https://a.example/", "title": "A", "active": true, "loading": false
        }))
        .unwrap();
        assert_eq!((&old.group, &old.split), (&None, &None));
        // Absent fields are not written either.
        let out = serde_json::to_value(&old).unwrap();
        assert!(out.get("group").is_none() && out.get("split").is_none());
        let new: TabInfo = serde_json::from_value(json!({
            "id": 2, "index": 1, "url": "u", "title": "t", "active": false, "loading": false,
            "group": {"id": 4, "name": "Work", "color": "blue", "collapsed": true},
            "split": {"side": "right", "with": 1}
        }))
        .unwrap();
        assert_eq!(new.group.map(|g| (g.name, g.collapsed)), Some(("Work".to_string(), true)));
        assert_eq!(new.split.map(|s| (s.side, s.with)), Some(("right".to_string(), 1)));
    }
}
