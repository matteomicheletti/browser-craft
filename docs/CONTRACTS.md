# Module contracts (wave 1)

How the fork's feature modules fit together, so they can be built in
parallel and wired into the browser afterwards. `src/browser.rs` (≈4.8k
lines) and `src/egui_chrome.rs` are owned by the **core** work; feature
modules live in their own files and expose plain Rust APIs. Wiring them
into the toolbar, menus and web view is the integration step.

## Ground rules

- Rust 2024, no `unwrap`/`expect` in production code (`[lints.clippy]`),
  tests may. `cargo build --locked`-clean except for new dependencies you
  add on purpose; `cargo test` and `cargo clippy` must not get worse.
- Shared crates already in `Cargo.toml`: `serde` (derive), `serde_json`,
  `toml`, `egui` 0.36, `arboard`, `image`, `rust-argon2`, `keepass`,
  `zeroize`, `secstr`, `base64`, `getrandom`, `url`, `psl`, `tempfile`.
  Add others only with a reason written in the commit message.
- Files on disk: always through `crate::storage` (`config_file`,
  `write_private`, `private_dir`, `profile_dir`). Settings are per user in
  `~/.config/BrowseCraft/`, web data in `~/.local/share/BrowseCraft/`.
- Untrusted input (pages, plugin files, imported files) is parsed
  defensively; size limits on everything read.
- Pages, plugins and the automation API never get secrets (passwords,
  TOTP seeds, vault keys).
- Comments and docs in English, in the upstream style (short, explain why).
- Conventional Commits, small commits. End every commit message with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## UI: drawers and the palette

The native web view is a separate X11 window painted **over** egui, so egui
cannot draw on top of the page. Two placements are therefore used:

- **Drawer**: a right-hand `egui::SidePanel` that narrows the page area
  (the web view is resized, never covered). Every feature panel is drawn in
  the drawer. A feature exposes:

  ```rust
  pub struct XPanel { /* state */ }
  impl XPanel {
      pub fn new() -> Self;
      /// Draws the panel body (the drawer draws title and close button).
      pub fn ui(&mut self, ui: &mut egui::Ui, c: &crate::egui_chrome::Colors) -> Vec<XEvent>;
  }
  pub enum XEvent { /* things only the browser can do: copy, reload page, open url, … */ }
  ```

- **Overlay** (command palette, menus): drawn over the page area while the
  web view is hidden; core shows a snapshot of the page behind it on Linux
  (WebKit snapshot), as `snapshot_page` does on Windows.

## Command palette (`src/commands.rs`, `src/palette.rs`)

- Opened with **Ctrl+P** (Cmd+P on macOS) and F1. Printing is a palette
  command (and keeps Ctrl+Shift+P).
- `commands.rs`: `pub enum CommandId` covering every browser action
  (navigation, tabs, containers, bookmarks, history, screenshot, screen
  recording, print, find, zoom, dev tools, JS console, dark mode, plugins,
  authenticator, passwords, settings, MCP/automation toggle, …) and
  `pub fn catalog() -> &'static [Command]` with label, keywords, shortcut
  text and icon.
- `palette.rs`: `pub struct Palette` with `open()`, `is_open()`,
  `ui(ctx, colors, sources: &PaletteSources) -> Option<PaletteOutcome>`.
  `PaletteSources` carries dynamic items (open tabs, bookmarks, recent
  history, containers). `PaletteOutcome` = `Run(CommandId)`,
  `SwitchTab(usize)`, `OpenUrl(String)`, `OpenInContainer{url, container}`.
- The browser implements `fn run_command(&mut self, id: CommandId)`.

## Web view hooks (core)

- One `wry::WebView` per live tab, created with the tab's container
  `WebContext`. Init scripts are assembled once per web view from:
  `adblock`, `passwords`, `darkmode::page_script(..)`,
  `plugins::page_script(..)`, `automation::console_capture_script()`.
- IPC messages from pages arrive through the existing `with_ipc_handler`.
  Each module owns a message prefix and a parser
  (`passwords::parse_page_message` is the model): `bc-plugin:`,
  `bc-console:`, `bc-dark:`. Messages are size-limited and validated.
- Live updates go through `evaluate_script` on the tabs' web views.

## Containers (core)

- `src/containers.rs`: `Container { id: String, name, color, icon,
  temporary: bool }`, persisted in `containers.json`; site rules
  `host -> container id` ("always open in"); import/export JSON.
- Storage: one data directory per container
  (`storage::profile_dir("container-<id>")`), the default profile for
  "no container", an ephemeral context for temporary containers, deleted
  on close.

## Automation (`src/automation/`)

- Control server on a Unix socket `$XDG_RUNTIME_DIR/browsecraft/control.sock`
  (mode 0600, off unless enabled in settings or `--automation`), JSON-RPC
  2.0, one request per line.
- The server never touches browser state: it turns each request into
  `automation::Request { method, params, reply: Sender<Result<Value, RpcError>> }`
  and hands it to a callback given at start-up; core posts it to the event
  loop as `UserMessage::Automation` and answers. Page methods are answered
  with `automation::scripts::PageJob` + `evaluate_script_with_callback`;
  the full checklist is in docs/AUTOMATION.md ("Integration").
- `browsecraft mcp` runs an MCP server on stdio that forwards tool calls to
  the socket. `browsecraft ctl <method> [json]` for shell scripting.
- `browsecraft headless <screenshot|pdf|text|snapshot|eval|run> …` renders
  with WebKit offscreen and prints JSON.
- Permissions in `automation.toml` (`automation::Settings`): off by
  default, `page.eval` denied until allowed, optional `allowed_hosts`.
