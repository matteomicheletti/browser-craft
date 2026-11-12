# Smart dark mode (M5)

Dark mode recolours web pages in the page itself, the way the Dark Reader
extension does. The colour engine is the dynamic theme of
[Dark Reader](https://github.com/darkreader/darkreader) (MIT), vendored in
`src/darkmode/vendor/`; BrowseCraft's feature is called "Dark mode".
Dark Reader is credited in `THIRD_PARTY_NOTICES.md`; its name and logo are
not used in the UI.

## Why the vendored engine

Computed colours, CSS variables, inline styles, constructable and `<link>`
style sheets, the mutation observer, open shadow roots, background images
and image inversion heuristics are a decade of fixes. Writing that again
would be worse. The npm package `darkreader@4.9.135` ships a self-contained
UMD build with `enable(theme, fixes)` / `disable()`; it needs no extension
APIs. License verified from the tarball's `LICENSE`: MIT, "Copyright (c) 2026
Dark Reader Ltd." (the file also says "All rights reserved", which the MIT
grant that follows does not restrict). MIT is compatible with GPL-3.0-or-later.

Vendoring record: `src/darkmode/vendor/README.md` (version, tarball hashes,
the single patch).

## Pieces

| File | Role |
| --- | --- |
| `src/darkmode/settings.rs` | `DarkSettings` (serde), host matching, decisions, `load`/`save` (`darkmode.json` in the config folder) |
| `src/darkmode/controller.js` | per-page logic: decide, no-flash style, native-dark check, schedule, live updates |
| `src/darkmode/mod.rs` | `page_script`, `update_script`, `parse_page_message` |
| `src/darkmode/panel.rs` | `DarkModePanel` for the drawer |
| `tools/darkmode-harness/` | WebKitGTK screenshot harness |
| `tests/web/darkmode/` | test pages |

## Behaviour

Order of decisions for a page's host (the top frame's host in iframes):

1. master switch off: nothing;
2. URL rules (`url_rules`, panel section *URL rules*), in order: the first
   regular expression that finds a match anywhere in the page's full address
   (`RegExp.test(location.href)`; the Rust side checks the same pattern with
   the `regex` crate, so look-around and back-references are refused) decides:
   `on` forces dark like a site override, `off` never;
3. per-site override of the most specific matching host: `on` forces dark
   (ignores mode, schedule and the native-dark check), `off` never;
4. excluded hosts list (a host covers its subdomains, `www.` ignored);
5. global mode: follow system / always dark / always light / off (the
   default: pages stay as they are unless a URL or site rule says `on`);
6. schedule (optional daily window, may wrap midnight; local time);
7. "leave pages that are already dark": after the document has parsed, the
   controller looks at the page as authored (engine off) and samples the
   canvas colour (root/body background propagation, `color-scheme`) and the
   background behind five points of the viewport; a mean relative luminance
   below 0.2 means natively dark and the engine is not started. The result
   is sent once as `bc-dark:{"host":..,"nativeDark":..}`.

No white flash: at document start, when dark mode will apply, a temporary
`html,body{background:#181a1b!important;...}` style is added; it is removed
after the engine is running and the page has loaded (hard limit 4 s).

Settings: brightness, contrast (50..150 %), sepia, grayscale (0..100 %),
optional font family and text stroke, per-site custom CSS (Dark Reader
`fixes.css`, max 64 KB per site), excluded list. All values are clamped on
load; a corrupt file falls back to the defaults.

## API to wire (core)

Wired in `src/browser/features.rs`: the page script goes into every web view
(all frames) unless the `builtin.darkmode` plugin entry is off, settings
changes and browser theme changes run `update_script` on every tab, the
toolbar ◐ button toggles the current site (right-click: the drawer), and
`system_dark` is the browser's effective theme.

```rust
darkmode::page_script(&settings, system_dark: Option<bool>) -> String
darkmode::update_script(&settings, system_dark: Option<bool>) -> String
darkmode::parse_page_message(body: &str) -> Option<PageMessage>   // "bc-dark:" prefix
darkmode::DarkModePanel::new()                                    // loads darkmode.json
panel.ui(ui, &colors, host: Option<&str>) -> Vec<DarkEvent>       // SettingsChanged
panel.settings() -> &DarkSettings
panel.note_native_dark(host, bool)
```

- Put `page_script` in **every** web view's init scripts, injected in **all
  frames** (wry: `with_initialization_script_for_main_only(.., false)`), even
  when dark mode is off, so that live updates work. Only about 360 KB of text
  that is parsed lazily and executed only when dark mode applies.
- On `DarkEvent::SettingsChanged` evaluate `update_script(panel.settings(), ..)`
  on every tab. The panel already saved the file. Subframes keep the
  settings they were created with until reload.
- `system_dark`: pass the app theme / GTK preference if known; `None` makes
  the page ask `matchMedia('(prefers-color-scheme: dark)')`.
- The script has no effect on `browsecraft:` pages and non-HTML documents.

## Tests

- `cargo test darkmode`: settings, matching, escaping of CSS/host names in
  the generated script, valid JavaScript (needs `node`, skipped otherwise),
  IPC parsing, panel draw.
- Visual: `tools/darkmode-harness/run.sh OUT_DIR` renders the pages in
  `tests/web/darkmode/` (light page with table/form/image/canvas, native dark
  page, CSS variables, shadow DOM) in real WebKitGTK 4.1 with each setting
  variant and saves PNGs. Needs python3-gobject, WebKit2 4.1 and a display.
  `shoot.py` takes an optional update script to test live changes.

## Limitations

- Cross-origin style sheets are read with `fetch` from the page, so a sheet
  served without CORS headers cannot be analysed (Dark Reader's extension
  fetches them from the background page). The fallback keeps the page dark
  but less exact. A native-side fetch could fix it later.
- Images stay as they are except for the engine's own heuristics for dark
  icons; white-background images and canvases stay white.
- The native-dark check is a heuristic taken once per document; pages that
  render their theme later (client-side) are judged on the first render.
- Closed shadow roots, and pages with a strict CSP that blocks inline styles
  with no nonce, are not themed. wry's init script runs in the page world, so
  pages could in principle read `window.__bcDark` (it is not enumerable and
  holds only settings).
- Per-site settings are stored by host, not per container.
