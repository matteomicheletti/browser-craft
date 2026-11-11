# Plugins and extensions

BrowseCraft has its own plugin system (API version **1.1**). A plugin is a
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
The clipboard history (see [`clipboard.history`](#clipboard-history)) is kept
in `~/.local/share/BrowseCraft/profiles/clipboard-history/` (owner only).

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
| `[[pages]]` | no | API 1.1. Up to 16 pages of the plugin's own UI, see [Plugin pages](#plugin-pages-api-11). |
| `[[commands]]` | no | API 1.1. Up to 32 commands (palette entries and shortcuts), see [Commands and shortcuts](#commands-and-shortcuts-api-11). |
| `screenshot_editor` | no | API 1.1. The page id that edits the browser's screenshots; needs `screenshot.edit`. |
| `remote_sources` | no | API 1.1. Up to 8 `https://` origins the pages may load scripts, styles, images and data from (added to their CSP). Default: none. |

Using any API 1.1 key or permission needs `api_version = "1.1"`.

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
| `clipboard.history` | **high** | API 1.1, plugin pages only. Record everything copied (in the browser and in other apps: text, HTML, images, file lists) and paste it back. See [Clipboard history](#clipboard-history). |
| `screenshot.edit` | medium | API 1.1. Receive the screenshots the user takes, put images on the clipboard, save PNG files to `~/Pictures/BrowseCraft`. |
| `shortcuts.override` | medium | API 1.1. Bind keys the browser or text editing already uses (Ctrl+V, Ctrl+Shift+S, ...). Shown under the plugin's name in the drawer. |
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

## Plugin pages (API 1.1)

A plugin can ship its own UI as HTML pages, served from its folder:

```toml
[[pages]]
id = "manager"            # [a-z0-9-], unique
path = "manager.html"     # an .html file in the plugin folder
title = "Clipboard"       # default: the plugin's name
open_as = "modal"         # "tab" (default) or "modal"
width = 860               # modal size in CSS px (200..4000, default 720)
height = 560              # (150..4000, default 520)
```

- **Address**: `bc-plugin://<plugin-id>/<path>`, a custom scheme registered
  once per web data store. Only an enabled plugin with pages is served, only
  files inside its folder (no `..`, no absolute paths, no symlink leading
  out; 8 MB per file), and only to its own pages: a request whose `Origin` or
  `Referer` is another origin gets 403, so web pages cannot load plugin files
  (or probe which plugins exist).
- **CSP** on every response: `default-src 'none'`; scripts, styles, fonts,
  images and `connect-src` from `'self'` (images also `data:` and `blob:`,
  styles `'unsafe-inline'`), plus the manifest's `remote_sources`; no frames,
  no objects, no forms, `frame-ancestors 'none'`. Inline scripts do not run:
  put code in `.js` files.
- **Tab**: a normal tab (default data store) showing the page. Web pages
  cannot navigate a tab or a frame to `bc-plugin:`; the browser grants each
  address it opens itself, and a granted tab may move between that plugin's
  pages until it leaves for something else. Plugin pages keep their own
  editing keys (Ctrl+C, Ctrl+S, Ctrl+Z, ...); Ctrl+T/W/Tab/PageUp/PageDown,
  Ctrl+1..9, Ctrl+L, Ctrl+P, Ctrl+Q and the function keys stay the browser's.
- **Modal**: a dedicated web view centred over the page area, whose picture
  is shown dimmed behind it (the page's own view hides meanwhile, as under
  menus). Esc (when the page does not handle it), a click on the dimmed area
  or `bc.host.close()` closes it, and the keyboard goes back where it was.
  Switching tabs closes it. Modal pages have their own data store
  (`profiles/plugin-pages`).
- **Theme**: pages get the browser theme as CSS variables on `:root`, live:
  `--bc-background`, `--bc-surface`, `--bc-surface2`, `--bc-border`,
  `--bc-text`, `--bc-text-muted`, `--bc-accent`, `--bc-accent-text`,
  `--bc-danger`, `--bc-danger-text`, `--bc-warning`, `--bc-success`,
  `--bc-field`, `--bc-drawer`, `--bc-page`, `--bc-card`, `--bc-hover`,
  `--bc-selection`, `--bc-font`, `--bc-radius`, and `color-scheme`. Also as a
  stylesheet: `<link rel="stylesheet" href="/__host/theme.css">`.
  `document.documentElement.dataset.bcTheme` is `dark` or `light`; a `theme`
  event fires on change.

## Commands and shortcuts (API 1.1)

```toml
[[commands]]
id = "open"
title = "Clipboard manager: paste from history"   # palette label
shortcut = "Ctrl+V"         # optional; Cmd on macOS. Ctrl, Super or Alt needed (F-keys may stand alone);
                             # Super is the Windows/Super key, Cmd on macOS
page = "manager"            # the page it opens
when = "open_on_ctrl_v"     # optional: a bool option that must be on
palette = true              # default; a command needs a palette entry or a shortcut
```

- Running a command (palette, keys) opens its page: a modal, or a tab
  (an open tab with that page is reused). The page learns which command
  opened it from `bc.host.info().command`; when the page is already open it
  gets a `command` event (`bc.host.on("command", c => ...)`) instead.
- Shortcuts work in the chrome (address bar included) and inside web pages
  (the web view's GTK key handler checks the plugin table before the
  browser's own keys), while the plugin is enabled and its `when` setting is
  on. Off: the key does what it always did (Ctrl+V pastes).
- **Conflicts**: a key the browser or text editing uses (see
  `plugins::shortcuts::BROWSER_KEYS` and `EDITING_KEYS`) is refused at load
  time unless the plugin has `shortcuts.override`; then it wins, and the
  drawer says "Takes Ctrl+V (the browser's paste)". Two enabled plugins with
  the same keys: the first in the registry's order gets them, the drawer
  shows the other as not active and why. Two commands of one plugin with the
  same keys are a manifest error.

## Clipboard history

While an enabled plugin holds `clipboard.history`, the browser watches the
system clipboard and records every change into one shared history; the
plugin's page reads and manages it with `bc.host.clipboard.*`.

- **What is captured** (Linux, GTK on X11 or XWayland under GNOME Wayland):
  GTK's `owner-change` signal, driven by the XFixes selection-owner
  notification, wakes the browser on every new owner, without polling. It
  sees copies made inside BrowseCraft (pages, fields, its own commands) and
  in other apps: X11 apps directly, native Wayland apps because XWayland
  mirrors the Wayland clipboard into the X11 `CLIPBOARD` selection. It
  reads `TARGETS`, then: file lists (`x-special/gnome-copied-files`,
  `text/uri-list`: stored as URIs, the files are not copied), images
  (`image/png`, or any `image/*` GdkPixbuf reads, stored as PNG files), HTML
  (markup and text; HTML offered alone gets its text extracted) or text.
  Checked with `wl-copy` (text, `--type image/png`, `--type
  x-special/gnome-copied-files`, `--type text/uri-list`, `--type text/html`)
  and `xclip -selection clipboard`.
- **Not captured**: the PRIMARY selection (select-to-paste); copies while
  the browser is not running; content larger than the "largest item" limit
  or images over 64 Mpx. "Copied in BrowseCraft / in another app" is a guess
  from whether the browser window had the focus at that moment.
- **Never recorded**: copies by BrowseCraft's password manager and
  authenticator (marked when they are made, and offered with
  `x-kde-passwordManagerHint: secret`), copies other apps mark with
  `x-kde-passwordManagerHint` (KeePassXC and others) or
  `application/x-nspasteboard-concealed-type`, and, when the plugin's
  `ignore_private` setting is on, copies made while a temporary-container
  tab is shown.
- **Limits** come from the holding plugin's settings (`max_items`,
  `max_total_mib`, `max_item_mib`, `retention_days`, `ignore_private`):
  the oldest unpinned items go first; pinned items are never evicted but
  count. The same content again (images: the same pixels, even re-encoded by
  another clipboard manager) moves the existing item to the top.
- **Storage**: `profiles/clipboard-history/index.json` and `img/<id>.png`,
  owner only. Turning the plugin off stops the watcher; the history stays
  until the plugin clears it.

### Why it is sensitive

Whatever passes through the clipboard is recorded: messages, addresses,
tokens and keys copied from a terminal, other password managers' passwords
when they do not mark them. A plugin with `clipboard.history` can show all
of it and paste any of it into any page. Only install such a plugin if you
trust its author as you trust the browser; the drawer shows the permission
in red under the plugin's name.

## Screenshots (`screenshot.edit`)

With an enabled plugin declaring `screenshot_editor = "<page>"` and
`screenshot.edit`, the browser's screenshots go to that page instead of
Pictures: Ctrl+Shift+S and *Screenshot of the visible page* (visible area),
*Screenshot of the full page*, and *Screenshot area…* (the visible page,
opened in a modal exactly over the page area so the plugin can let the
user pick an area). The page opens as its manifest says (a tab by default)
with `?shot=<token>&mode=visible|full|area&name=browsecraft-<UTC stamp>.png`;
`bc.host.screenshot.get()` gives `{url, name, mode}`, where `url`
(`/__host/blob/<token>`) is only valid on that plugin's origin. Without such
a plugin the screenshots are saved to Pictures as before (and *Screenshot
area…* says it needs one).

## Host API (`bc.host`, plugin pages only)

Plugin pages get a frozen `window.bc.host`. It exists only in the main
frame of a `bc-plugin:` page; a call is honoured only when the web view's
committed address (reported by WebKit, not by the page) is that plugin's
origin, the runtime's secret nonce matches, the plugin is enabled and has
the permission. Web pages get nothing, not even an answer.

| Call | Needs | Result |
|------|-------|--------|
| `bc.host.info()` | | `{plugin, name, page, command, modal, settings, permissions, theme: {name, dark}, target, platform}` |
| `bc.host.close()` | | Closes the modal, or the tab showing the page. |
| `bc.host.on(name, fn)` | | Events: `command` (`{id}`), `theme` (`{name, dark}`), `clipboard-changed` (`{id}`). Returns an unsubscribe function. |
| `bc.host.settings.get()` / `.set(key, value)` | | The plugin's own settings (checked against `[[options]]`); `set` answers the new settings. |
| `bc.host.notify(title, body)` | `notifications` | A notice in the browser. |
| `bc.host.clipboard.list(query, limit)` | `clipboard.history` | `{items: [{id, kind, created, used, source, pinned, preview, truncated, size, width, height, files, image}], usage}`; newest first; every word of `query` must match the text, file names or kind. `image` is the address of the picture. |
| `bc.host.clipboard.get(id)` | `clipboard.history` | The item with `text`, `html`, `fileUris`, `fileNames`. |
| `bc.host.clipboard.paste(id, {plain})` | `clipboard.history` | Puts the item on the clipboard, closes the modal and pastes it where the keyboard was (WebKit's Paste / PasteAsPlainText in the page, or the address bar). |
| `bc.host.clipboard.copy(id)` | `clipboard.history` | Puts the item back on the clipboard (text, HTML with text, image, file list). |
| `bc.host.clipboard.remove(id)`, `.pin(id, pinned)` | `clipboard.history` | |
| `bc.host.clipboard.clear({pinnedToo})` | `clipboard.history` | Number deleted; pinned items stay unless `pinnedToo`. |
| `bc.host.clipboard.usage()` | `clipboard.history` | `{items, bytes, maxItems, maxBytes, maxItemBytes}` |
| `bc.host.screenshot.get()` | `screenshot.edit` | `{url, name, mode}` of the screenshot the page was opened with. |
| `bc.host.screenshot.copyImage(pngDataUrl)` | `screenshot.edit` | Puts the PNG on the system clipboard. |
| `bc.host.screenshot.saveFile(name, pngDataUrl, {ask})` | `screenshot.edit` | Saves to `~/Pictures/BrowseCraft/<name>` (cleaned, never replacing a file: `-2`, `-3`, ...) and answers `{path}`; `ask: true` opens the system's save dialog (answers `null` when cancelled). |

Wire format: `window.ipc.postMessage("bc-host:" + JSON)` with
`{n, i, m, a}` (nonce, call id, method, args), at most 48 MB (images travel
as data URLs) and 400 calls per second per plugin; answers come back through
`window.__bcHostDeliver(id, ok, json)`, events through
`window.__bcHostEvent(name, json)`. Rust side: `plugins::host`
(`parse_message`, `authorize`, `runtime_script`), `plugins::ui_pages`
(`serve`), `browser/plugins_host.rs` (the browser's handling).

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
- **Plugin pages are privileged.** `bc.host` is bound to the page's
  `bc-plugin://<id>` origin as WebKit reports it and to a nonce the content
  scripts never see, so a web page cannot reach it, and a plugin page
  cannot act for another plugin. Within its own page a plugin can use every
  permission it declared: the CSP keeps remote code out unless the manifest
  lists `remote_sources`.
- Installing trusts the manifest's permission list; nothing prevents a
  plugin with `page.read` from reading what you type on matching pages.

## Versioning policy

- The plugin API has a version `MAJOR.MINOR` (`1.1` now; 1.1 added pages, commands and the `clipboard.history`, `screenshot.edit` and `shortcuts.override` permissions), separate from the
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
| Popups, options pages, devtools panels | yes | settings form generated from the manifest; plugin pages in a tab or a modal (API 1.1). |
| Commands (`commands` API) | yes | `[[commands]]` with shortcuts and palette entries (API 1.1). |
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
