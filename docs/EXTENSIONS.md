# Plugins and extensions

BrowseCraft has its own plugin system (API version **1.0**). A plugin is a
folder with a `plugin.toml` manifest, optional scripts and styles that run on
pages you choose, and an optional WebAssembly module for logic that runs in
the browser process under a sandbox. The manager in the browser's drawer lists
every plugin, what it may do and which API calls exist.

This is **not** a WebExtensions implementation. See
[WebExtensions comparison](#webextensions-comparison).

Source: `src/plugins/`. Examples: `examples/plugins/`.

## Quick start

1. Copy `examples/plugins/hello-world` somewhere, or write your own folder:

   ```toml
   # plugin.toml
   id = "com.example.hello"
   name = "Hello"
   version = "0.1.0"
   api_version = "1.0"
   permissions = ["page.modify"]

   [[content_scripts]]
   matches = ["*://example.com/*"]
   js = ["hello.js"]
   css = ["hello.css"]
   ```

2. Open the plugin manager, paste the folder's path into **Development
   folder** and press **Add**. The plugin is used in place and reloaded when
   its files change (the manager checks about once a second; a tab needs a
   reload to run the new script).
3. To install a copy instead, use **Install**: the manager shows the
   permissions first and installs into `~/.config/BrowseCraft/plugins/<id>/`.

## Plugin folders

| Kind | Where | Behaviour |
|------|-------|-----------|
| Built-in | the browser itself | Containers, Authenticator and Dark mode are listed with their permissions and an enable switch (`plugins::is_enabled("builtin.darkmode")`). They are implemented elsewhere and cannot be removed. |
| Installed | `~/.config/BrowseCraft/plugins/<id>/` | A validated copy. Installing again replaces it and keeps its settings and storage. |
| Development | any folder you point at | Referenced, never copied. The folder may be a plugin itself or contain plugin subfolders (up to 64). Edits are picked up automatically; a broken manifest shows its errors in the manager instead of failing silently. Removing it only forgets the folder. |

If two folders declare the same id, the development copy wins and the other is
shown with an error. Ids starting with `builtin.` are reserved.

State files (all in `~/.config/BrowseCraft/`, written atomically, owner only):
`plugins.json` (dev folders, disabled ids, settings) and `plugin-storage.json`
(what plugins keep with `bc.storage`).

## Manifest reference (`plugin.toml`)

Unknown keys are errors (they are almost always typos). All problems are
reported together.

| Key | Required | Description |
|-----|----------|-------------|
| `id` | yes | Reverse-DNS, lowercase: `[a-z0-9_-]` segments separated by dots, at least two (`com.example.my-plugin`). Not `builtin.*`. |
| `name` | yes | Display name, at most 80 characters. |
| `version` | yes | Semantic version `MAJOR.MINOR.PATCH[-pre][+build]`. |
| `api_version` | yes | The plugin API it was written for, `"1"` or `"1.0"`. Same major as the browser, and not a newer minor. |
| `description`, `author` | no | Shown in the manager (500 / 120 characters). |
| `homepage` | no | `http(s)` address; the manager offers it as a link. |
| `permissions` | no | List of permission names and host patterns, see below. |
| `wasm` | no | Path of a `.wasm` module (see [WASM](#wasm-modules)). |
| `[[content_scripts]]` | no | Up to 16 entries, see below. |
| `[[options]]` | no | Up to 32 settings, see below. |

### Content scripts

| Key | Description |
|-----|-------------|
| `matches` | Required. [Match patterns](#match-patterns) of pages to run on. |
| `exclude_matches` | Patterns that cancel a match. |
| `js`, `css` | Files, relative to the plugin folder (at most 16 each, 1 MB per file). At least one of the two. |
| `run_at` | `document_start`, `document_end` (default) or `idle` (after `load`, when the browser is idle). |
| `all_frames` | `false` (default): top frame only. |

A content script with `js` needs `page.read` or `page.modify`; with `css` it
needs `page.modify`. Files must stay inside the plugin folder: absolute paths,
`..`, backslashes and symlinks that resolve outside are refused.

### Options (settings)

```toml
[[options]]
key = "color"            # identifier; becomes bc.settings.color
type = "choice"          # bool | string | number | choice
label = "Highlight color"
default = "yellow"
choices = ["yellow", "green"]   # choice only
# min = 1, max = 20              # number only
```

The manager draws a form from this schema (checkbox, text field, number, drop
down) and validates every value. Settings reach scripts as the frozen object
`bc.settings`, fixed when the page script was built: after changing a setting,
reload the tab.

### Match patterns

WebExtension syntax: `<scheme>://<host><path>` or `<all_urls>`.

- Scheme: `*` (http and https), `http`, `https`, `file`, `ftp`.
- Host: `*`, `*.example.com` (the domain and all subdomains) or an exact
  name. The wildcard is only allowed as the whole host or a leading `*.`.
  Ports are not allowed in patterns and are ignored when matching.
  Empty host only for `file:///...`.
- Path: must start with `/`; `*` matches any run of characters. It is
  matched against path plus query, never the fragment.
- `<all_urls>` covers http, https, file and ftp. Internal pages (`about:`,
  `data:`, `browsecraft:`) are never matched.

The same compiled patterns are used by the Rust side (permission checks) and
by the page script (injection), so both agree.

## Permissions

Permissions are declared in the manifest and granted by installing or adding
the plugin after seeing the list; there is no separate per-permission prompt
yet. Every bridge call is checked against the plugin's own list.

| Permission | Risk | Lets the plugin |
|------------|------|-----------------|
| `page.read` | high | Run JavaScript on pages its content scripts match and read them, including what you type there. |
| `page.modify` | high | Inject styles and scripts and edit those pages. |
| `tabs.read` | medium | List open tabs (`bc.tabs.query`). |
| `tabs.open` | medium | Open http(s) addresses in new tabs. |
| `clipboard.write` | medium | Put text on the clipboard. |
| `storage` | low | Keep up to 256 KB / 256 keys privately (64 KB per value). |
| `notifications` | low | Show browser notifications. |
| host pattern (e.g. `*://*.example.com/*`) | medium | Reserved. Accepted and displayed, but no API uses host permissions yet; injection is controlled by `content_scripts.matches`. |

Passwords, TOTP seeds and vault keys are never reachable through any API.

## JavaScript API (`bc`)

Each plugin script receives its own frozen `bc` object (it is a parameter of
the function that wraps the script, not a global). Calls that talk to the
browser return Promises and are only answered in the **top frame**; in
sub-frames (`all_frames = true`) they reject.

| Call | Needs | Description |
|------|-------|-------------|
| `bc.id`, `bc.version`, `bc.apiVersion`, `bc.permissions`, `bc.settings` | | Plain data. |
| `bc.log(...args)` | | Writes to the plugin log. Fire and forget. |
| `bc.sendMessage(obj)` | | Sends JSON to the plugin's WASM module and resolves with its answer; rejects if there is none. |
| `bc.storage.get(key)` / `.set(key, value)` / `.remove(key)` | `storage` | JSON values. |
| `bc.notify(title, body)` | `notifications` | |
| `bc.clipboard.writeText(text)` | `clipboard.write` | At most 16 KB. |
| `bc.tabs.query()` | `tabs.read` | Resolves with `[{id, title, url, active}]`. |
| `bc.tabs.open(url)` | `tabs.open` | `http(s)` only. |

Limits: a message is at most 64 KB, a plugin may make 200 calls per second,
`log` lines are 2000 bytes. Errors reject with a message such as
`permission "notifications" is required for bc.notify(title, body)`.

### Wire format (for browser integrators)

The page posts `window.ipc.postMessage("bc-plugin:" + JSON)` with
`{"n": nonce, "p": plugin id, "i": call id, "m": method, "a": args}`. In Rust:

1. `plugins::parse_page_message(body) -> Option<PageCall>` checks prefix,
   size, envelope, argument shapes (unknown methods, non-http `tabs.open`
   URLs and oversized arguments are dropped).
2. `plugins::dispatch(&mut registry, call, Some(tab_url)) -> Outcome`
   checks the nonce, that the plugin is enabled and has a content script for
   the page's address, the rate limit and the permission; it performs
   storage and WASM calls itself and returns a `reply` script (run it in the
   calling tab with `evaluate_script`) and/or a `PluginEvent`.
3. `PluginEvent`: `Notify`, `ClipboardWrite`, `OpenTab`, `QueryTabs { reply }`,
   `Log`. For `QueryTabs`, answer later with `reply.ok(&json)` or
   `reply.err("...")` and evaluate the returned script.

The reply script calls `window.__bcDeliver(plugin, id, ok, json)`.

## WASM modules

Logic that should not run in the page (and Rust plugins) compiles to
WebAssembly and runs inside the browser process under
[`wasmi`](https://github.com/wasmi-labs/wasmi), a pure-Rust interpreter. It is
behind the cargo feature `wasm`, which is **on by default** (wasmi is pure Rust
and builds cleanly everywhere). A build made with `--no-default-features`
still loads a manifest with `wasm`, but `bc.sendMessage` reports that WASM is
not compiled in.

**Why wasmi and not wasmtime.** Plugin logic is glue code, so interpreter
speed is enough; wasmi adds seconds, not minutes, to the build and a small
amount to the binary, needs no JIT and no executable memory, and exposes a
small surface. wasmtime is the better choice if heavy computation in plugins
ever matters; the host ABI below is engine-independent.

**Never native.** BrowseCraft does not `dlopen` plugin libraries. A Rust
plugin is compiled with `--target wasm32-unknown-unknown`.

Host ABI (module `bc`, UTF-8 strings in the exported `memory`):

| Import | Description |
|--------|-------------|
| `log(ptr, len)` | Plugin log line (64 per call). |
| `input_len() -> i32`, `input_read(ptr, cap) -> i32` | The JSON passed to `bc.sendMessage`. |
| `output(ptr, len)` | The answer (JSON, or text); the last call wins. |
| `storage_get(kptr, klen, optr, ocap) -> i32` | Value length, `-1` if absent. Needs `storage`. |
| `storage_set(kptr, klen, vptr, vlen) -> i32` | `0` or `-1`. Needs `storage`. |

Export `run()` and `memory`. It is called once per `bc.sendMessage`, on its
own thread, with a fuel budget of 5 million instructions, 16 MiB of memory, and
64 KB of input and output. Running out stops the module and rejects the call.
A module that imports anything else (or `storage_*` without the permission)
does not link. The module is validated; the text format is not accepted.
Modules are limited to 4 MB.

## Isolation limits (read this)

- **Page scripts are not sandboxed from the page.** WebKit runs user scripts
  in the page's JavaScript world. Each plugin file runs in its own function
  scope with its own `bc`, so plugins do not see each other's variables, but
  they share the DOM, globals and built-ins with the page. A plugin can
  change the page, and a hostile page can tamper with built-ins (`Promise`,
  `JSON`, `Function.prototype`) or enumerate `window` to find
  `window.__bcDeliver`, spy on or spoof answers to the plugin. Do not put
  secrets in page-side plugin code; put logic that needs protection in the WASM
  module.
- **Requests are authenticated, answers are not.** Messages to the browser
  carry a per-process random nonce that lives in a closure, so a page cannot
  forge a plugin's requests. Answers travel by `evaluate_script` to a
  fixed global and can be spoofed by the page.
- **Strict CSP.** Plugin JavaScript is compiled with `new Function`, so pages
  whose Content-Security-Policy forbids `unsafe-eval` stop plugin JS (an error
  is logged). Styles still apply: they use constructable stylesheets, which
  are not subject to the page's `style-src`. A future option is one init script
  per plugin file, which avoids `eval` but loses per-file syntax-error
  isolation.
- **Same-process limits.** Plugin JS runs in the web process (WebKit's own
  sandbox applies). Rust-side handling and WASM run in the browser process:
  there, only the permission checks, the nonce, size limits and the WASM
  sandbox protect it. There is no rate limit on WASM CPU beyond its fuel.
- Dev folders run whatever is in them; only add folders you trust.
- Installing trusts the manifest's permission list; nothing prevents a
  plugin with `page.read` from reading what you type on matching pages.

## Versioning policy

- The plugin API has a version `MAJOR.MINOR` (`1.0` now), separate from the
  browser's version, exposed as `bc.apiVersion` and shown in the manager.
- **Minor** releases only add (methods, permissions, manifest keys); a plugin
  written for `1.0` keeps working on `1.x`. A plugin declaring a newer minor
  than the browser is refused with a message asking to update.
- **Major** releases may remove or change things; the browser supports the
  current major, and any earlier major only for a documented deprecation
  period. A plugin for another major is refused with a clear error.
- A new permission is never added to an existing one: access that used to be
  implied by `storage` stays `storage`.
- Plugin versions are their own business (semver); installing a newer version
  of an installed id replaces it.

## WebExtensions comparison

BrowseCraft does not run WebExtensions and does not claim to. Evaluation:

| Area | WebExtensions | BrowseCraft plugins |
|------|---------------|---------------------|
| Manifest | `manifest.json` v2/v3 | `plugin.toml`; a converter for the content-scripts-only subset is possible. |
| Content scripts, match patterns | yes | yes, same pattern syntax; isolated worlds are **not** available. |
| `browser.*` / `chrome.*` APIs | large | a handful of `bc.*` calls. |
| Background pages / service workers | yes | none; WASM module on demand instead. |
| `webRequest`, `declarativeNetRequest` | yes | none (ad blocking is a built-in component). |
| Popups, options pages, devtools panels | yes | settings form generated from the manifest; no custom UI. |
| Store, signing, updates | yes | none; install from folder. |

A realistic path to partial compatibility is to translate simple
content-script-only extensions (`manifest.json` with `content_scripts`, no
background script) into plugins at install time, with a shim for
`chrome.storage.local` and `runtime.sendMessage`. It is not implemented, and
anything beyond that subset (background pages, `webRequest`, `tabs` events,
isolated worlds) needs engine support WebKitGTK does not expose through wry.
Until then: not compatible.

## Developing the runtime

- `cargo test plugins` runs the unit tests, the WASM ones included (the
  `wasm` feature is on by default).
- `cargo test plugins::script::live -- --ignored --nocapture` loads the
  generated script into a real WebKitGTK 4.1 (offscreen, needs `python3-gi`;
  see `tests/plugins_live.py`), serves test pages, answers bridge calls with
  the real dispatcher and writes PNG screenshots to `$BC_LIVE_OUT`.
