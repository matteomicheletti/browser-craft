# Automation: scripting, headless and MCP

BrowseCraft is meant to be driven by scripts and AI agents as well as by
people. There are three entry points, all in the same binary:

| Command | What it does | Needs a running browser |
| --- | --- | --- |
| `browsecraft headless …` | Loads one page in an offscreen WebKit view, does one thing (screenshot, PDF, text, snapshot, script, a list of steps), prints JSON | No |
| `browsecraft ctl <method> [json]` | One call to the control API of the running browser, result printed as JSON | Yes |
| `browsecraft mcp` | MCP server on stdio for agents (Claude Code, Claude Desktop, other MCP clients), forwarding tool calls to the control API | Starts one if needed |

The control API is a local JSON-RPC 2.0 server on a Unix socket that only
the user can reach. It is **off by default**: start the browser with
`browsecraft --automation`, or set `enabled = true` in `automation.toml`.

Code: `src/automation/` (protocol, server, scripts, console, ctl, mcp) and
`src/headless_webkit.rs`.

> Status (wave 1): the server, clients, scripts and headless mode are done
> and tested. The browser side (answering the requests with its tabs and
> web views) is the integration step described at the end of this page;
> until it lands, `ctl` and `mcp` report that no browser is listening.

## Headless mode

```sh
browsecraft headless screenshot https://example.com shot.png --width 1280 --height 800
browsecraft headless screenshot https://example.com full.png --full-page
browsecraft headless pdf https://example.com page.pdf
browsecraft headless text https://example.com
browsecraft headless snapshot https://example.com
browsecraft headless eval https://example.com 'document.title'
browsecraft headless eval https://github.com 'const r = await fetch(location.href); return r.status'
browsecraft headless run http://localhost:8000/form.html \
  '[{"method":"page.type","params":{"selector":"#q","text":"hello","submit":true}},
    {"method":"page.wait","params":{"text":"Results"}},
    {"method":"page.text","params":{"selector":"main"}}]'
```

Options: `--timeout SECONDS` (load and work, default 30), `--delay MS` (wait
after the load event, for pages that keep working after it), `--width`,
`--height` (viewport, default 1280×800), `--full-page` (screenshots). A
path to an existing local file is opened as a `file://` URL; anything else
is resolved like address-bar input.

Output is one JSON object on stdout:

```json
{"ok":true,"url":"https://example.com/","title":"Example Domain","status":200,
 "path":"/home/me/shot.png","width":1280,"height":800,"bytes":156393,"console":[]}
```

- `status` is the HTTP status of the main document (null for `file:`).
- `console` lists what the page logged (`console.*`, uncaught errors,
  unhandled rejections), the same entries as `console.read`.
- `text` adds `text` and `truncated`; `snapshot` adds `headings` and
  `elements` (see `page.snapshot`); `eval` adds `value`; `run` adds
  `results`, one `{method, result}` per step, and stops at the first
  failing step.
- On failure: `{"ok":false,"url":…,"error":"…"}`, exit code 1. Bad usage
  exits with 2.

How it works: GTK `OffscreenWindow` holding a `webkit2gtk::WebView` with an
**ephemeral** data context (nothing is kept between runs), WebKit's
bubblewrap sandbox (`WEBKIT_FORCE_SANDBOX=1`, as the browser) and the
DMA-BUF renderer off. It needs a display (X11 or Wayland); on a machine
without one, run it under `xvfb-run`. Page scripts in headless mode are not
subject to `automation.toml`: the user runs them explicitly on a throwaway
profile.

The old own-engine renderer (`browsecraft --screenshot out.png <url>`) is
still there for pages without JavaScript.

## The control socket

- Path: `$XDG_RUNTIME_DIR/browsecraft/control.sock` (without a runtime
  directory: `<tmp>/browsecraft-<uid>/control.sock`). Override with
  `BROWSECRAFT_CONTROL_SOCKET=/abs/path.sock` (its directory must belong to
  the user).
- The directory is created `0700` and must be owned by the user; the
  socket is `0600`. Every connection's peer uid is read with `SO_PEERCRED`
  and connections from other users are closed without an answer.
- One JSON-RPC 2.0 request per line, one response per line. Requests over
  1 MiB are refused and the connection is closed. At most 16 connections
  at once, idle connections are closed after 5 minutes. No batches.
- A stale socket is replaced at start; if another browser answers on it,
  the second one does not start its server.

```sh
$ printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"browser.version"}' \
    | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/browsecraft/control.sock
{"jsonrpc":"2.0","id":1,"result":{"name":"BrowseCraft","version":"0.1.0","api_version":1,"methods":[…]}}
```

### Errors

| Code | Meaning |
| --- | --- |
| -32700 / -32600 / -32601 / -32602 / -32603 | JSON-RPC: parse error, invalid request, unknown method, invalid params (unknown fields are refused), internal |
| -32000 | The browser could not do it (no such tab, page error, request dropped) |
| -32001 | Refused by the permissions in `automation.toml` |
| -32002 | Timed out (the operation, or the browser's answer) |
| -32003 | No element matches the selector or ref |
| -32004 | Too many connections |
| -32005 | The script in the page threw |

### Methods

Tab ids are unique across all browser windows: a request naming a `tab` goes
to whatever window has it. `tab` is always optional and means the active tab
of the window last used when omitted. Timeouts
are in milliseconds (default 10 000, at most 120 000). Element targets are
either `selector` (CSS, searched through open shadow roots) or `ref` (from
`page.snapshot`, such as `e12`), never both.

| Method | Params | Result |
| --- | --- | --- |
| `tabs.list` | – | `{tabs: [{id, index, url, title, active, loading, container, group?, split?, window}], windows: [{id, name?, label, tabs, active}]}`; `group` = `{id, name, color, collapsed}` for a tab in a tab group, `split` = `{side: "left"\|"right", with: <tab id>}` for a tab in split view (the active tab is the focused pane); `window` = `{id, name?}`, the browser window the tab is in (`index` and `active` count within that window); `windows` lists every window (`label` is its name or "Window N", `active` the window last used) |
| `tabs.open` | `url`, `container?`, `background?`, `window?` | the new tab (`TabInfo`); `window` (a `windows[].id`) picks the window, the one last used by default; an unknown id is an error |
| `tabs.activate` | `tab` (required) | `TabInfo` |
| `tabs.close` | `tab?` | `{ok}` |
| `page.navigate` | `url`, `tab?`, `wait?` (default true), `timeout_ms?` | `{tab, url, title, loading}` |
| `page.back`, `page.forward`, `page.reload` | `tab?`, `wait?`, `timeout_ms?` | `{tab, url, title, loading}` |
| `page.text` | `selector?`, `max_chars?` (default 50 000) | `{url, title, text, truncated}` |
| `page.html` | `selector?`, `max_chars?` | `{url, html, truncated}` |
| `page.snapshot` | `max_elements?` (default 300), `viewport_only?` | `{url, title, headings, elements: [{ref, role, name, tag, value?, href?, checked?, disabled?, in_viewport?}], truncated}` |
| `page.click` | `selector` or `ref` | `{ok, description}` |
| `page.type` | `selector` or `ref`, `text`, `clear?` (default true), `submit?` | `{ok, description}` |
| `page.press` | `key` (`Enter`, `Tab`, `Escape`, `ArrowDown`, `a`…), `selector?`/`ref?` | `{ok, description}` |
| `page.scroll` | `selector`/`ref`, or `to` (`top`/`bottom`), or `dx`/`dy` | `{ok, description}` |
| `page.eval` | `script`, `timeout_ms?` | `{value}` (needs `allow_eval`) |
| `page.wait` | `selector?`, `text?`, `load?`, `timeout_ms?` | `{found: true, elapsed_ms}` or error -32002 |
| `page.screenshot` | `full_page?` | `{png_base64, width, height}` |
| `page.pdf` | – | `{pdf_base64, bytes}` |
| `console.read` | `since?`, `level?` (`debug`…`error`, minimum), `limit?` (default 200) | `{entries: [{seq, time_ms, level, kind, text, url, source?, line?, column?}], next, dropped}` |
| `containers.list` | – | `{containers: [{id, name, color, icon, temporary}]}` |
| `bookmarks.list` | `query?`, `limit?` | `{bookmarks: [{title, url, folder?}]}` |
| `history.search` | `query?`, `limit?` | `{entries: [{title, url, visited?}]}` |
| `browser.version` | – | `{name, version, api_version, methods}` (answered by the server itself) |
| `sessions.list` | – | `{sessions: [{name, windows, tabs, created, updated}]}`: the session presets, in the user's order (times in Unix seconds) |
| `sessions.open` | `name` (case-insensitive), `window?` (`"new"`, the default, or `"current"`) | `{session: {name, windows, tabs, created, updated}, notice?}`: `new` opens each of the preset's windows as a new window, `current` adds its tabs after those of the window last used; tabs load when first shown; `notice` mentions tabs whose container no longer exists. Needs `allow_navigation`, refused while `allowed_hosts` is set |
| `sessions.save` | `name`, `scope?` (`"window"`, the default: the window last used, or `"all"`) | `{session: {…}}`; an existing name is an error (update a preset on `browsecraft://sessions`); tabs in temporary containers are left out. Needs `allow_navigation` |

The exact serde types are in `src/automation/protocol.rs`.

Notes on behaviour:

- `page.snapshot` lists visible links, buttons, fields, selects,
  content-editables and ARIA widgets. Each gets a `data-bc-ref` attribute,
  so refs stay the same across snapshots of the same document and can be
  used in later calls until the page navigates. Password fields never
  report their value.
- `page.click` scrolls the element into view, sends pointer and mouse
  events at its centre, focuses it and calls `click()`.
- `page.type` sets the value through the native setter and fires `input`
  and `change` (so React and similar frameworks see it); on a `<select>`
  it picks the option whose value or label is `text`; on a content-editable
  element it uses `insertText`.
- `page.press` events are synthetic (`isTrusted` is false), so the browser's
  own default actions do not run. Enter in a form field submits the form,
  Tab moves the focus and printable keys are inserted into text fields
  explicitly; other defaults (opening a `<select>`, keyboard shortcuts of
  the browser) do not happen.
- `page.eval` takes an expression (`document.title`,
  `fetch(u).then(r => r.status)`) or a function body that uses `return`
  (`const r = await fetch(u); return r.status`). The script is inlined into
  the evaluated code, not passed to `eval()`, so pages whose
  Content-Security-Policy forbids `eval` still work. Promises are awaited;
  the value is returned as JSON (undefined and functions become null).
- `page.wait` checks every 100 ms; it keeps waiting while the page is
  navigating.

## `browsecraft ctl`

```sh
browsecraft ctl --help                       # every method with a one-line description
browsecraft ctl tabs.list
browsecraft ctl page.navigate '{"url":"https://example.com"}'
browsecraft ctl page.snapshot '{"viewport_only":true}'
browsecraft ctl page.click '{"ref":"e3"}'
browsecraft ctl page.screenshot '{"full_page":true}' --out page.png
browsecraft ctl console.read '{"level":"warn"}'
```

The result is printed as pretty JSON on stdout. With `--out FILE`, a
`png_base64` or `pdf_base64` in the result is written to `FILE` and replaced
by `path` and `bytes`. `--socket PATH` talks to another socket.

Exit codes: 0 success, 1 the browser answered an error (also printed as
`{"error":…}` on stdout), 2 bad usage, 3 no browser listening.

## MCP server

`browsecraft mcp` speaks the Model Context Protocol over stdio
(newline-delimited JSON-RPC; protocol versions 2025-06-18, 2025-03-26 and
2024-11-05, negotiated in `initialize`). It implements `initialize`,
`notifications/initialized`, `ping`, `tools/list` and `tools/call`. It is
hand-written (≈300 lines plus tests): the official `rmcp` SDK would add an
async runtime for four methods.

Register it:

```sh
# Claude Code (user scope, every project)
claude mcp add --scope user browsecraft -- browsecraft mcp
# or with an absolute path when browsecraft is not on PATH
claude mcp add browsecraft -- /home/me/.local/bin/browsecraft mcp
```

Claude Desktop, Cursor and other clients that read an `mcpServers` JSON:

```json
{
  "mcpServers": {
    "browsecraft": { "command": "browsecraft", "args": ["mcp"] }
  }
}
```

When no browser is listening, the first tool call starts
`browsecraft --automation` (detached, in its own process group) and waits up
to 20 s for the socket (`--launch-timeout SECONDS`). `--no-launch` only
connects. Diagnostics go to stderr; stdout carries only protocol messages.

### Tools

One tool per control method, with JSON Schemas (`additionalProperties:
false`) and annotations (`readOnlyHint`, `destructiveHint`):

| Tool | Method | Result for the model |
| --- | --- | --- |
| `browser_tabs` | `tabs.list` | JSON |
| `browser_tab_open` / `_activate` / `_close` | `tabs.*` | JSON |
| `browser_navigate`, `browser_back`, `browser_forward`, `browser_reload` | `page.navigate` … | JSON |
| `browser_text` | `page.text` | `URL:`/`Title:` header and the text |
| `browser_html` | `page.html` | the HTML |
| `browser_snapshot` | `page.snapshot` | one line per element: `[e3] button "Sign in"`, `-> href`, `value=`, `checked`, `(off-screen)` |
| `browser_click`, `browser_type`, `browser_press`, `browser_scroll` | `page.*` | JSON |
| `browser_eval` | `page.eval` | JSON |
| `browser_wait` | `page.wait` | JSON |
| `browser_screenshot` | `page.screenshot` | **image** content (`image/png`) and its size |
| `browser_pdf` | `page.pdf` | embedded `application/pdf` resource |
| `browser_console` | `console.read` | JSON |
| `browser_containers`, `browser_bookmarks`, `browser_history`, `browser_version` | … | JSON |
| `browser_sessions_list`, `browser_sessions_open`, `browser_sessions_save` | `sessions.*` | JSON |

Errors from the browser (permission denied, no element, bad arguments) are
returned as tool results with `isError: true`, so the model can read and
correct them; an unknown tool name is a JSON-RPC error.

A typical agent loop: `browser_navigate` → `browser_snapshot` →
`browser_click`/`browser_type` with refs → `browser_wait` →
`browser_text` or `browser_screenshot`.

## Security model

- **Off by default.** The server only starts with `--automation` or
  `enabled = true`.
- **Owner only.** Socket in a `0700` directory owned by the user, socket
  `0600`, and a peer-uid check on every connection. Anything the user's own
  processes can do, they could already do with the user's files; the API
  does not widen that to other users or to the network (there is no TCP
  listener).
- **Permissions** in `~/.config/BrowseCraft/automation.toml`:

  ```toml
  enabled = false          # start the control server with the browser
  allow_eval = false       # page.eval (arbitrary JavaScript in pages)
  allow_navigation = true  # tabs.open, page.navigate, back/forward/reload, sessions.open/save
  allow_file_urls = false  # file:// in tabs.open / page.navigate
  # allowed_hosts = ["example.com", "intranet.local"]  # and their subdomains
  ```

  Unknown keys make the file invalid (it is reported and the defaults are
  used), so a typo never silently enables something.
- **URLs**: automation only opens `http`, `https` and `about:blank`
  (`file:` when allowed). `javascript:` is refused because it would bypass
  `allow_eval`; `data:`, `view-source:` and internal `browsecraft:` pages
  are refused too.
- **`allowed_hosts`** limits navigation and, enforced by the browser
  (`Settings::page_allowed`), every `page.*` method to those sites.
- **No secrets.** There are no methods for passwords, the vault or the
  authenticator, and `page.snapshot` omits password values. `page.eval`
  could read a field the password manager filled in, which is one reason
  it is off by default.
- Page content (text, console messages, element names) is untrusted data
  for the agent reading it, never instructions. The console channel
  (`bc-console:`) can be spoofed by the page; its entries are only shown or
  returned, never acted on.
- Headless runs use an ephemeral profile, so they never see the user's
  cookies or logins.

## Integration in the browser (core)

Done in `src/browser/automation_rpc.rs` (requests) and
`src/browser/features.rs` (server start, settings, console log per tab,
the Automation drawer). Two differences from the outline below: page
scripts run through WebKitGTK's `run_javascript` directly, because wry
queues scripts sent before a new web view's first commit and drops their
callbacks; and `UserMessage` lost its `Clone` derive so it can carry the
`Request`. Closing the last tab with `tabs.close` opens a blank tab first
(the browser would quit otherwise); `page.navigate`, back, forward and
reload switch to the named tab, while page scripts, screenshots and PDFs
also work on live background tabs.

The outline as originally specified:

1. **Start the server** when automation is wanted (after the event loop and
   proxy exist):

   ```rust
   // UserMessage gains: Automation(crate::automation::Request)
   let proxy = self.proxy.clone();
   match crate::automation::start(move |req| {
       // Called on a server thread: hand the request to the UI thread.
       let _ = proxy.send_event(UserMessage::Automation(req));
   }) {
       Ok(handle) => self.automation = Some(handle), // keep it: drop stops the server
       Err(e) => if crate::automation::requested() { eprintln!("browsecraft: {e}") },
   }
   ```

   `UserMessage` derives `Clone`; `Request` holds an `mpsc::Sender` and is
   not `Clone`, so wrap it (`Automation(Arc<Mutex<Option<Request>>>)`) or
   drop the `Clone` derive. A settings page can call
   `handle.set_settings(..)` to apply changes without a restart.

2. **Answer** in `handle_message`. Every request must be answered once
   (`req.respond(Ok(value))` / `Err(RpcError)`); a dropped request is
   reported to the client. The server already validated the parameters
   and checked `Settings::check`; when `allowed_hosts` is set, call
   `settings.page_allowed(&tab_url)?` before any `page.*` method.

   - `tabs.*`, `page.navigate/back/forward/reload`: browser state. For
     `wait: true`, keep the request and answer from `web_loaded` (or after
     `timeout_ms` with `RpcError::timeout`). Results: `TabInfo`, `PageInfo`.
   - Page-script methods (`text`, `html`, `snapshot`, `click`, `type`,
     `press`, `scroll`, `eval`, `wait`):

     ```rust
     use crate::automation::scripts::{PageJob, Step};
     if let Some((job, script)) = PageJob::start(req.method, &req.params)? {
         run_job(webview, job, script, req.reply.clone());
     }
     fn run_job(wv: &wry::WebView, mut job: PageJob, script: String, reply: Sender<…>) {
         let proxy = /* event loop proxy */;
         wv.evaluate_script_with_callback(&script, move |json| match job.on_result(&json) {
             Step::Done(result) => { let _ = reply.send(result); }
             // Run again on the UI thread (now, or after POLL_INTERVAL):
             Step::Run(next) | Step::RunLater(next) => {
                 let _ = proxy.send_event(UserMessage::AutomationScript { job, script: next, reply });
             }
         });
     }
     ```

     The callback runs on the GTK main thread during the GLib pump; it is
     `Fn`, so move the job through a `Cell<Option<_>>` or post it back to
     the event loop as above. For `RunLater`, wait `POLL_INTERVAL` (a
     timer in `about_to_wait`, or `glib::timeout_add_local_once`).
   - `page.screenshot`: on Linux, `wry::WebViewExtUnix::webview(&wv)` gives
     the `webkit2gtk::WebView`; call
     `crate::headless_webkit::snapshot_png(&wk, full_page, |r| …)` and
     answer `ScreenshotResult { png_base64, width, height }`.
   - `page.pdf`: `crate::headless_webkit::print_pdf(&wk, &tmp_path, |r| …)`,
     then `file_base64(&tmp_path)`, delete the file, answer `PdfResult`.
   - `console.read`: `tab.console.read_result(&req.params()?)`.
   - `containers.list`, `bookmarks.list`, `history.search`: from the
     browser's stores, shaped as in `protocol.rs`.

3. **Console capture**: add
   `.with_initialization_script(&crate::automation::console::console_capture_script())`
   to every web view, and in the IPC handler:

   ```rust
   if let Some(msg) = crate::automation::console::parse_page_message(request.body()) {
       let _ = ipc_proxy.send_event(UserMessage::Console { url: request.uri().to_string(), msg });
   }
   ```

   Keep a `ConsoleLog` per tab (`log.push(msg, &url)`), clear it or keep
   it across navigations as the dev console prefers; the same entries feed
   the developer JS console and `console.read`.

## Tests

- `cargo test automation`: protocol (de)serialisation and strict
  parameters, permissions and URL rules, socket permissions and stale
  socket handling, peer uid, request size limit, connection limit, server
  dispatch, `ctl` round trip and exit codes against a server with a fake
  browser, MCP `initialize`/`tools/list`/`tools/call` and the stdio loop,
  tool schemas checked against the method parameter types, script
  escaping, structure and `PageJob` state machine, console ring buffer.
- Live (manual): `python3 -m http.server -d tests/web 8000`, then
  `browsecraft headless eval http://127.0.0.1:8000/features.html 'new Promise(r => setTimeout(() => r(document.title), 3000))'`
  must print `"value":"probe 17/17"`, and
  `browsecraft headless run http://127.0.0.1:8000/automation.html '[…]'`
  exercises type, select, checkbox, click, submit, keys, content-editable,
  wait, shadow DOM, scroll and eval (see `docs/MANUAL_TESTS.md`).
