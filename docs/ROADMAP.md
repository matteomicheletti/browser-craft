# Roadmap

Fork: `matteomicheletti/browser-craft` (upstream `freewright-dev/BrowseCraft`,
GPL-3.0-or-later). Primary platform Manjaro Linux x86_64; Windows and macOS
next, through the same `wry` abstraction (WebView2 / WKWebView).

Status legend: **done** (code + test + doc), **in progress**, **planned**.
A milestone is not marked done while an earlier one has a critical blocker.

## Product requirements

Original brief (2026-11-11), plus the additions of the same day, which are
first-class requirements:

- **AI-first, like the rest of the ArtCraft suite**: scripting, a real
  headless mode on the web engine, and an **MCP server** so agents can drive
  the browser (open/navigate tabs, read page text/DOM, run scripts, take
  screenshots, use containers) under explicit permissions.
- **Chrome/Firefox-like experience**: tabs on top (vertical sidebar stays
  optional), bookmarks bar, Chrome keyboard shortcuts.
- **Developer console** (web inspector) and **local plugins** loaded from a
  folder the user chooses.
- **Command palette on F1** (spotlight-style, as in LWCA, N4S and the easy
  software family): screenshot, screen recording, print, dev console,
  bookmarks, history, containers, authenticator, settings… every action of
  the browser searchable in one place.
- Real modern-web support, Multi-Account Containers, TOTP authenticator,
  smart dark mode, plugin system, ArtCraft Launcher distribution, low RAM.

## Milestones

### M0: Fork and baseline (done)

- GitHub fork `matteomicheletti/browser-craft` (native fork, public);
  remotes `origin` = fork, `upstream` = original with push disabled.
- GitHub Actions disabled on the fork: builds and checks run locally
  (owner's policy, no cloud pipelines). Upstream workflow files are kept
  untouched to stay merge-friendly with upstream.
- Baseline on Manjaro (Rust 1.99.1): `cargo build --release --locked` OK
  (≈2 min), `cargo test --release` **193/193 passed**, 16 clippy warnings
  (all pre-existing, newer toolchain). Headless screenshot of Wikipedia OK.
  On Linux every page used the own engine (no JavaScript).

### M1: Modern engine (done for Linux; see ADR-001)

- WebKitGTK through wry, X11/XWayland, GLib pump, persistent profile,
  bubblewrap sandbox, in-page shortcuts, `--version`, X11 window class.
- Tests: probe 17/17, GitHub/GitLab/React/Vue rendered, sandbox verified on
  the process tree, unit tests 193/193 (+F12/Ctrl+Shift+I assertions).
- Open: logged-in flows and Microsoft 365 / Google Workspace not tested.
  (Dev tools and one web view per tab: done in wave 1 core, see M2/M3.)

### M1b: AI-first automation (done for Linux)

See `docs/AUTOMATION.md`.

- **Done**: `browsecraft headless screenshot|pdf|text|snapshot|eval|run`
  on WebKit (offscreen GTK window, ephemeral context, sandbox), JSON
  output, exit codes. Verified 2026-11-11: probe 17/17, example.com,
  Wikipedia (full page 1024×8855), GitHub (strict CSP, eval with `await`),
  all page scripts on `tests/web/automation.html`, PNG checked visually.
- **Done**: control API on a Unix socket (dir 0700, socket 0600,
  SO_PEERCRED uid check, 1 MiB requests, 16 connections), JSON-RPC 2.0,
  24 methods with strict serde types; permissions in `automation.toml`
  (off by default, eval denied, URL scheme rules, host allow-list); no
  methods for passwords or the authenticator.
- **Done**: `browsecraft ctl …` and `browsecraft mcp` (MCP 2025-06-18 over
  stdio, 24 tools, screenshot as image content, launches the browser when
  needed); console capture script, `bc-console:` parser and ring buffer.
  32 unit tests (`cargo test automation`).
- **Done (integration, 2026-11-11)**: the browser answers every method
  (`src/browser/automation_rpc.rs`): tabs, navigation with load waits, page
  scripts run through WebKitGTK (background tabs too), screenshot and PDF of
  the live view, a console log per tab, containers, bookmarks, history;
  `allowed_hosts` checked against the tab, settings applied live from the
  Automation panel. Verified live: `ctl tabs.list`, `tabs.open` (also in the
  background), `page.navigate`, `page.text`, `page.snapshot`, `page.wait`,
  `page.screenshot` (also full page, 940×8119 on Wikipedia), `page.pdf`,
  `console.read`, `page.eval` denied then allowed from the panel, server
  stopped and restarted from the palette; an MCP stdio session
  (`initialize`, `tools/list` 24 tools, `browser_navigate`, `browser_text`).
- **Open**: `browsecraft mcp` starting a browser that is not running
  (manual test 6.8). Headless needs a display (`xvfb-run` on servers).

### M2: Extensibility and UX core (in progress)

- Plugin manager (done in `src/plugins/`, see `docs/EXTENSIONS.md`; wired
  into the browser window and verified live 2026-11-11): manifest (`plugin.toml`: id, version,
  API version, permissions, match patterns, scripts/styles),
  install/remove, enable/disable, per-plugin settings, permission and API
  display, built-in entries with an enable flag.
- **Local plugin folders** (done): point the browser at a directory; changes
  reload in development mode (polling, about once a second).
- Content scripts/styles per domain through one init script; message bridge
  with nonce, permission checks and quotas (done). WASM sandbox for untrusted
  logic on **wasmi** (cargo feature `wasm`, on by default since the
  integration; wasmtime was evaluated and not chosen, see EXTENSIONS.md); no
  native libraries loaded in process (done).
  Integration (done, verified live): init script in every web view and frame,
  `bc-plugin:` IPC with notify / clipboard / tabs / log answered, the Plugins
  drawer and toolbar button, dev-folder edits picked up within a second with
  *Reload open tabs*, built-ins (dark mode, authenticator) turned off take
  effect at once. Open: one init script per file for strict-CSP pages,
  per-permission prompts.
- Partial WebExtensions compatibility evaluated, never claimed complete
  (done: evaluation in EXTENSIONS.md; no converter).
- **Dev console** (inspector) working on Linux; JS error console (done: the
  Automation drawer's Console view, `Ctrl+Shift+J`, verified live).
- **F1 command palette** (done: Ctrl+P / F1 overlay over a page snapshot,
  every `CommandId` implemented, verified live with keyboard navigation);
  **tabs on top** + bookmarks bar; full Chrome shortcut table (doc in
  `docs/usage.md`).
- Status (core, 2026-11-11): **done** for the inspector (F12 / Ctrl+Shift+I
  dock it at the bottom of the page; it was attached but never laid out
  because wry sizes the GTK window by hand), the tab strip on top (default;
  sidebar left/right/hidden on `browsecraft://flags`), the drawer
  (`Drawer` enum; all five panels since the integration), find in
  page (Ctrl+F, WebKit find controller with match count) and the Chrome
  shortcuts (Ctrl+1..9, Ctrl+PageUp/Down, Ctrl+Shift+T, Ctrl+Shift+N; Ctrl+P
  and F1 open the palette). Overlays show a WebKit snapshot of
  the page on Linux. Detached inspector window: not working (WebKit closes
  it). Palette, plugins and the JS console: wired in the integration step.
- **Window chrome (wave 3, branch `wave3/chrome`)**: own window frame like
  ArtCraft Launcher and Chrome (tabs and window buttons in the title bar row,
  drag / double-click / window menu, resize from every edge, *Use system title
  bar* fallback, hidden in full screen); a theme system with eight themes plus
  Follow system, used by all the chrome and the internal pages, switched live
  from the toolbar, palette (`%`) and settings; the authenticator as a QR-code
  button. Verified on the GNOME session (screenshots, maximize / restore /
  minimize / close through the debug hook); moving and resizing by hand are in
  `MANUAL_TESTS.md` §7.

- **Windows, tab overview and hover cards (wave 4, branch `wave4/windows`)**:
  several windows in one process (`browser::App`, `src/browser/windows.rs`)
  sharing the containers' data stores, tab ids, closed tabs and page pictures;
  tabs moved between windows from the tab menu or by dragging, keeping their
  live page (wry's X11 container reparented); named windows; every window in
  the session. Safari-style overview of all tabs (`Ctrl+Shift+\`) with page
  pictures, search, groups and split pairs. Chrome-style hover cards drawn in
  an X11 child window above the web view (no flicker). Verified on the GNOME
  session through the debug hook (screenshots) and with real input on a nested
  X server; open: drags between windows on GNOME with several monitors and
  fractional scaling, hover feel (`MANUAL_TESTS.md` §8). Not done: dragging a
  tab in the sidebar, reopening a closed window as a whole, moving a whole
  group or both split panes at once.

### M3: Multi-Account Containers (in progress: Linux implemented and verified live)

- One web view per live tab; per-container `WebContext` (own data
  directory, or ephemeral for temporary containers): cookies, storage,
  IndexedDB, cache, service workers isolated by WebKit.
- Create/edit/delete with name, colour, icon; open tab in container;
  domain → container rules ("always open in"); import/export JSON;
  coloured tab indicators.
- Isolation tests: same origin, different containers, cookie/storage not
  shared; persistence across restarts; temporary container wiped on close.
- Status (2026-11-11): one wry web view per live tab (switching keeps the
  page, no reload); `src/containers.rs` (CRUD, colours, icons, rules with
  subdomains, import/export JSON, temporary containers in a 0700 folder under
  `$XDG_RUNTIME_DIR`, deleted with the last tab and swept after a crash);
  one WebKit `WebContext` per container (`profiles/container-<id>`); tab
  strip and sidebar colours, toolbar badge, container picker, tab menu,
  Containers drawer, `--container` / `--temporary-container`, palette `~`
  mode and container badges on palette tabs. Unit tests for
  the model, rules, import/export (hostile input) and temporary cleanup.
  Verified live on a local page: cookie, localStorage and IndexedDB differ
  between Work, Personal, no container and a temporary container in one
  window, persist per container across restarts, and the temporary folder
  is gone after its tab closes. Open: an automated (headless) WebKit
  isolation test; Windows (per-container WebView2 folder, untested) and
  macOS (not isolated).

### M4: Authenticator (done for Linux; manual checks open)

- TOTP (RFC 6238) / HOTP (RFC 4226), configurable algorithm/digits/period;
  `otpauth://` and QR import; encrypted vault (Argon2id + XChaCha20-Poly1305
  or the existing KeePass vault's TOTP fields: decide in design note);
  lock/unlock, encrypted backup/export; toolbar button; never exposed to
  pages, plugins or the MCP server.
- Done (`src/authenticator/`, `docs/AUTHENTICATOR.md`, 40 unit tests):
  separate vault (decision and format in the design note), RFC vectors,
  `otpauth://` and Google export links, QR images (rqrr), encrypted backup,
  plain link export with confirmation, Aegis import (plain and encrypted,
  tested on spec-built samples), auto-lock, drawer panel
  (`AuthenticatorPanel::ui` → `AuthEvent`).
- Done (integration, verified live 2026-11-11): toolbar shield showing the
  lock state, drawer, palette command, `tick()` every frame (auto-lock with
  the drawer closed); in a temporary profile a vault was created, an
  `otpauth://` link added, the code matched an independent TOTP
  computation, and the copied code left the clipboard after 30 s.
- Open: QR image, lock/restart/unlock and export/import checked by hand in
  the browser (unit-tested only); clipboard image import; tests against
  files exported by the real Aegis / Google apps.

### M5: Smart dark mode (done for Linux)

- Dynamic colour adaptation (computed styles, CSS variables, mutation
  observer, shadow roots), per-site rules, brightness/contrast/sepia,
  native dark detection, no white flash. Dark Reader is MIT: its dynamic
  theme engine is vendored (4.9.135) and wrapped; see `docs/DARKMODE.md`.
- Done: settings, page/update scripts, IPC parser, drawer panel, WebKitGTK
  screenshot harness; wired 2026-11-11 (all-frames init script, live
  `update_script` on every tab, follows the browser theme, toolbar toggle for
  the site with right-click for the drawer, three palette commands, native
  dark reports shown in the panel). Verified live on `tests/web/darkmode/`.
- Open: native-side CSS fetch for non-CORS sheets.

### M6: Performance (in progress)

- Tab suspension/unload/restore, lazy tab loading, plugin lazy loading,
  cache limits, process count control, GLib pump cost.
- Status (2026-11-11): tab suspension done: background tabs unload after a
  configurable idle time (default 30 min) and beyond a cap of live web views
  (default 16, least recently used first, never a tab playing audio); an
  unloaded tab shows dimmed and reloads its address when shown; the session
  is restored lazily (only the active tab loads). Unit test for the policy,
  verified live (1-minute setting). Benchmarks, cache limits, process count
  and GLib pump cost: not started.
- Benchmarks vs Firefox (PSS via smem, CPU, start-up, load time) with 1/5/20
  tabs, containers, plugins on/off: `docs/BENCHMARKS.md` with method and raw
  numbers. No numbers are published before they are measured.

### M7: ArtCraft Launcher (planned)

- AppImage `browsecraft-<ver>-linux-x86_64.AppImage` + `SHA256SUMS.txt`
  built by a local script, GitHub Release by `gh` from the local machine.
- Launcher integration prepared on a branch of a launcher fork; PR to
  `storytold/craft-launcher` only after explicit approval. See
  `docs/ARTCRAFT_INTEGRATION.md`.

### M8: Stabilisation (planned)

- End-to-end tests, security review, release candidate.
