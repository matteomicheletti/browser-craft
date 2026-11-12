//! Plugin system (milestone M2): manifests, registry, page scripts, the
//! page <-> browser bridge and the manager panel. See `docs/EXTENSIONS.md`.
//!
//! Wiring (done by the browser in `browser/features.rs`, not here):
//! - `plugins::shared()` gives the process-wide registry; call
//!   `Registry::poll_changes` about once a second to pick up dev-folder edits.
//! - `page_script(&registry)` is one more init script per web view; rebuild
//!   web views (or reload tabs) when `registry.generation()` changes.
//! - IPC bodies starting with `bridge::PREFIX` go to `parse_page_message`,
//!   then `dispatch`; act on the `PluginEvent`, run the `reply` script.
//! - `PluginsPanel::ui` draws the manager in the drawer.

// Public API consumed by the browser integration, not wired yet.
#![allow(dead_code, unused_imports)]

pub mod bridge;
pub mod clipboard_history;
#[cfg(target_os = "linux")]
pub mod clipboard_watch;
#[cfg(target_os = "macos")]
#[path = "clipboard_watch_macos.rs"]
pub mod clipboard_watch;
pub mod host;
pub mod manifest;
pub mod panel;
pub mod pattern;
pub mod permissions;
pub mod registry;
pub mod script;
pub mod shortcuts;
pub mod ui_pages;
pub mod wasm;

pub use bridge::{dispatch, parse_page_message, PageCall, PluginEvent, Reply};
pub use panel::{PluginsEvent, PluginsPanel};
pub use registry::Registry;
pub use script::page_script;

use std::sync::{Arc, Mutex, OnceLock};

pub type Shared = Arc<Mutex<Registry>>;

/// The process-wide registry, loaded from the per-user config folder on first
/// use. If that folder is unavailable the registry is rooted in a temporary
/// folder, so the browser still runs (settings just do not persist).
pub fn shared() -> Shared {
    static REG: OnceLock<Shared> = OnceLock::new();
    REG.get_or_init(|| {
        let reg = Registry::load().unwrap_or_else(|_| {
            // Unguessable folder, so nobody else can plant plugins in it.
            let dir = tempfile::Builder::new().prefix("browsecraft-plugins-").tempdir().map(tempfile::TempDir::keep);
            Registry::open(dir.unwrap_or_default())
        });
        Arc::new(Mutex::new(reg))
    })
    .clone()
}

/// Is the component or plugin `id` (e.g. `"builtin.darkmode"`) enabled?
pub fn is_enabled(id: &str) -> bool {
    shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_enabled(id)
}
