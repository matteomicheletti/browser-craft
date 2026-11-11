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
- Open: dev-tools window not yet seen opening; logged-in flows and
  Microsoft 365 / Google Workspace not tested; one web view for all tabs.

### M1b: AI-first automation (planned, next)

- `browsecraft --headless` on WebKit (offscreen GTK window): `screenshot`,
  `pdf`, `text`, `eval` sub-commands with JSON output, exit codes.
- Local control API on a Unix socket (owner-only), JSON-RPC; an
  **MCP server** (`browsecraft mcp`, stdio) built on it: tabs, navigate,
  read text/DOM snapshot, click/type by selector, run script, screenshot,
  containers. Every tool gated by a permission set in settings; no access
  to password or authenticator vaults.
- Scripting: the same API from a CLI (`browsecraft ctl …`).

### M2: Extensibility and UX core (planned)

- Plugin manager: manifest (`plugin.toml`: id, version, API version,
  permissions, match patterns, scripts/styles), install/remove,
  enable/disable, per-plugin settings, permission display.
- **Local plugin folders**: point the browser at a directory; changes
  reload in development mode.
- Content scripts/styles per domain through WebKit user content manager;
  message bridge with permission checks. WASM (wasmtime) sandbox for
  untrusted logic; no native libraries loaded in process.
- Partial WebExtensions compatibility evaluated, never claimed complete.
- **Dev console** (inspector) working on Linux; JS error console.
- **F1 command palette**; **tabs on top** + bookmarks bar; full Chrome
  shortcut table (doc in `docs/usage.md`).

### M3: Multi-Account Containers (planned)

- One web view per live tab; per-container `WebContext` (own data
  directory, or ephemeral for temporary containers): cookies, storage,
  IndexedDB, cache, service workers isolated by WebKit.
- Create/edit/delete with name, colour, icon; open tab in container;
  domain → container rules ("always open in"); import/export JSON;
  coloured tab indicators.
- Isolation tests: same origin, different containers, cookie/storage not
  shared; persistence across restarts; temporary container wiped on close.

### M4: Authenticator (in progress: module done, integration pending)

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
- Open: toolbar button, drawer and palette wiring, clipboard copy/clear in
  core, visual check in the running browser; clipboard image import;
  tests against files exported by the real Aegis / Google apps.

### M5: Smart dark mode (planned)

- Dynamic colour adaptation (computed styles, CSS variables, mutation
  observer, shadow roots), per-site rules, brightness/contrast/sepia,
  native dark detection, no white flash. Dark Reader is MIT: reuse with
  attribution is possible and will be evaluated before writing our own.

### M6: Performance (planned)

- Tab suspension/unload/restore, lazy tab loading, plugin lazy loading,
  cache limits, process count control, GLib pump cost.
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
