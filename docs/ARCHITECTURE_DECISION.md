# ADR-001: Web engine for the BrowseCraft fork

- Status: **accepted** (2026-11-11), Linux implemented in M1
- Scope: the `matteomicheletti/browser-craft` fork. Upstream's own decision
  ([freewright-dev/BrowseCraft#65](https://github.com/freewright-dev/BrowseCraft/issues/65))
  picked Servo as a long-term goal; this fork needs a browser usable every day
  with real accounts now, so it decides differently for Linux.

## Context

Analysis of the code base at upstream `c28778c` (fork baseline):

| Area | What exists | Notes |
| --- | --- | --- |
| Application | Rust 2024, `winit` 0.30 window, `egui`/`wgpu` chrome (toolbar, tab sidebar), software fallback chrome | ~24k lines in `src/`, 193 unit tests, all passing on Manjaro |
| Own engine | HTML/CSS parser, cascade, layout, paint (`html.rs`, `css.rs`, `style.rs`, `layout.rs`, `paint.rs`) | No JavaScript. Used for `about:`, `browsecraft://`, `view-source:` pages, headless screenshots and fallback |
| Native web view | `wry` 0.55: WebView2 (Windows), WKWebView (macOS) | Linux was explicitly disabled in `create_webview` |
| Servo | `--features servo`, Linux only, in-process, `servo_pane.rs` | Upstream calls it "not yet secure for arbitrary browsing"; 38/50 sites OK |
| Ad blocking | uBO-derived network + cosmetic filter engine (`adblock.rs`) | Applied through navigation handler and page script |
| Passwords | KeePass `.kdbx` vault (`vault.rs`, `passwords.rs`) | Reusable for the authenticator (M4) |
| Tabs | One web view shared by all tabs; switching tab reloads the URL | Blocks containers and tab state: see M3/M6 |

`wry` on Linux already contains a WebKitGTK backend (`webkit2gtk-4.1`), but it
needs GTK initialised on the main thread, an **X11** parent window
(`build_as_child` is X11-only) and the GLib main context to be serviced.

## Options compared

| Criterion | A. Servo 0.7 | B. WebKitGTK via wry | C. QuickJS/Boa + own renderer |
| --- | --- | --- | --- |
| Modern sites | Partial (38/50 upstream run); gaps in media, complex apps | Same engine as GNOME Web/Epiphany; GitHub, GitLab, React, Vue apps work (tested, see below) | Very low: would need a full DOM, CSSOM, events, fetch, layout of dynamic content; years of work |
| JS APIs (fetch, XHR, IDB, WS, Workers, SW) | Most present, some missing | All present (17/17 probe) | Each one to implement by hand |
| RAM / CPU | Single process, moderate | Multi-process (UI + network + web processes); ~ comparable to Epiphany, to be measured in M6 | Low but meaningless without compatibility |
| Security / sandboxing | Runs **in the browser process**, no sandbox | Separate web processes inside **bubblewrap** + filtered D-Bus proxy (verified) | Untrusted JS in the browser process |
| Integration complexity | Already integrated, but input/focus/IME/media still open | Small (≈100 lines): GTK init, X11 backend, GLib pump | Enormous |
| Maintainability | Tracks a fast-moving engine API, long compile (clang, ~1h) | System library updated by the distro (security fixes for free) | All on us |
| Isolated containers | Would need per-profile storage support in embedding API | `WebKitWebsiteDataManager` per data directory / ephemeral: real cookie, storage, IDB, cache, SW isolation | To build |
| Extensions | Only via our own script injection | User scripts/styles, script messages, `WebKitWebExtension` (C) available; WebExtensions not provided | To build |
| Portability | Linux only in this repo | Linux/BSD; Windows and macOS keep WebView2/WKWebView through the **same wry API** | Portable but useless |

Rejected: **C** (rewriting a browser engine is not a goal; violates "prefer
mature components"). **A** stays available behind `--features servo` for
experiments but is not used for real accounts while it runs unsandboxed in
process.

## Decision

**B. WebKitGTK through wry on Linux**, keeping Rust, winit and egui for the
application, and the same `wry::WebView` abstraction already used on Windows
(WebView2) and macOS (WKWebView). One code path, three system engines.

Implementation (M1, `src/main.rs`, `src/browser.rs`, `src/storage.rs`):

1. `gtk::init()` with `GDK_BACKEND=x11`, and `winit` forced to X11
   (XWayland under GNOME Wayland). `BROWSECRAFT_NO_WEBKIT=1` falls back to
   the own engine.
2. The GLib main context is serviced from `about_to_wait` every 8 ms while a
   web view exists.
3. Persistent profile store: `wry::WebContext` with data directory
   `$XDG_DATA_HOME/BrowseCraft/profiles/default` (cookies, localStorage,
   IndexedDB, cache). This is the hook the containers (M3) build on: one
   data directory (or ephemeral context) per container.
4. Sandbox: `WEBKIT_FORCE_SANDBOX=1` before GTK starts, because wry starts
   WebKit's processes before the application could call
   `webkit_web_context_set_sandbox_enabled`. `BROWSECRAFT_NO_SANDBOX=1`
   disables it and prints a warning.
5. `WEBKIT_DISABLE_DMABUF_RENDERER=1` by default: on this hybrid
   Intel/NVIDIA machine the DMA-BUF renderer fails under XWayland
   (`Failed to create GBM buffer`) and its X error aborts winit
   (`GLXBadWindow`). Users can set the variable to `0` to try it.
6. Window bounds are applied after the view is shown (GTK only resizes a
   realized window).
7. Keys typed inside the page are captured on the GTK widget and mapped to
   browser shortcuts (same table as WebView2's accelerator hook).

## Verification (2026-11-11, Manjaro, GNOME Shell 50.4 Wayland, kernel 7.1.13, i7-12700, Intel UHD 770 + NVIDIA T400, webkit2gtk-4.1 2.52.6)

- `tests/web/features.html` probe: **17/17** (ES2022, Promise/async, DOM +
  MutationObserver, events, fetch, XHR, cookies, local/sessionStorage,
  IndexedDB, Worker, WebSocket API, Service Worker API, Shadow DOM,
  History API, crypto.subtle, `window.onerror`), with the sandbox on.
- Rendered and interactive: github.com (repo page), gitlab.com (project
  page), react.dev, vuejs.org.
- Process tree: `WebKitWebProcess` and `xdg-dbus-proxy` run under `bwrap`;
  `WebKitNetworkProcess` outside it (WebKit's design).
- Not yet verified: logged-in flows, Microsoft 365, Google Workspace,
  WebSocket against a live server, media/DRM, HiDPI on a scaled display.

## Consequences and known limits

- **XWayland**: the browser is an X11 client under Wayland. Fine on GNOME;
  fractional scaling can be blurrier than native Wayland. Native Wayland
  would mean hosting the chrome in GTK (tao/GTK window, as Tauri does) or
  moving to `webkitgtk-6.0` + GTK4: tracked in the roadmap.
- **Polling**: the 8 ms GLib pump costs a little idle CPU while a web view
  exists; to measure in M6 and replace with fd-based integration if needed.
- **One web view for all tabs** (inherited): switching tab reloads the page
  and loses its state. M3 (containers) and M6 (tab suspension) need one
  web view per live tab.
- **Dev tools**: WebKit's inspector is enabled (`devtools` feature, F12,
  Ctrl+Shift+I/J, `BROWSECRAFT_DEVTOOLS=1`) but its window has not yet been
  seen to open in this X11-child setup; open issue for the dev-console
  milestone.
- Headless screenshots still use the own engine (no JS); an AI-first
  headless mode on WebKit is planned (ROADMAP, M1b).
