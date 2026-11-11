# Security

Status of the fork's security posture. It is updated with every milestone;
anything not listed as verified is not claimed.

## Web content isolation (Linux, M1)

| Control | State | How it was checked |
| --- | --- | --- |
| Web content in separate processes | Yes (WebKitGTK multi-process) | Process tree: `WebKitWebProcess`, `WebKitNetworkProcess` children of the browser |
| Web processes sandboxed | Yes, bubblewrap (`WEBKIT_FORCE_SANDBOX=1`) with a filtered D-Bus proxy | `ps --forest`: `bwrap … WebKitWebProcess`, `bwrap … xdg-dbus-proxy` |
| Sandbox opt-out | `BROWSECRAFT_NO_SANDBOX=1` only, with a warning on stderr | Manual |
| Profile data private | `~/.local/share/BrowseCraft/profiles/default` created `0700`, symlink refused (`storage::private_dir`) | Code review; `ls -ld` shows `drwx------` |
| Navigation policy | Upstream `web_navigation`/`popup_allowed`: internal, `file:` and `view-source:` targets refused from web pages; pop-ups re-opened as tabs | Upstream unit tests |
| Ad/tracker filtering | Upstream filter engine through navigation handler + page script | Upstream tests; native subresource blocking exists only on Windows |

Not isolated yet: all tabs share one profile (containers are M3); the
network process is not sandboxed (WebKit design).

## Secrets

- Passwords: upstream KeePass `.kdbx` vault (`vault.rs`), never written in
  plain text by BrowseCraft.
- Authenticator (M4): planned encrypted vault, never reachable from pages,
  plugins or the automation/MCP API.
- No secrets or credentials are stored in this repository.

## History and imports

- Browsing history is now on disk: `history.sqlite` in the data folder (`~/.local/share/BrowseCraft`), folder 0700, file 0600, `secure_delete` on (cleared rows are zeroed), WAL checkpointed after *Clear everything*. User names and passwords in URLs are stripped; tabs in temporary containers are never recorded; retention defaults to one year. `history.sqlite.bak` (a copy made before each import) holds the same data.
- Importers only read other browsers' profiles, from a copy in a private temporary folder deleted afterwards; inputs are size-, count- and depth-limited. Passwords, cookies and extensions are not read.
- Internal pages that change bookmarks, history or imports require a one-time form token.

## Plugins with pages and the clipboard history (API 1.1)

| Control | State | How it was checked |
| --- | --- | --- |
| Plugin pages served only from their folder | `bc-plugin://<id>/<path>` through `manifest::safe_join` (no `..`, absolute paths, symlinks out), 8 MB per file, only for enabled plugins with pages | Unit tests `plugins::ui_pages` (traversal, `%2e%2e`, symlink, other plugin, disabled) |
| Plugin files not readable by web pages | 403 when `Origin`/`Referer` is another origin; clipboard images need a per-process key only plugin pages get; screenshot blobs use unguessable tokens bound to the plugin | Unit tests |
| Plugin page CSP | `default-src 'none'`, own files only (+ declared `https` `remote_sources`), no frames/objects/forms, `frame-ancestors 'none'` | Unit test; header set on every response |
| `bc.host` only for plugin pages | Authorized by the web view's committed URI (from WebKit) being the plugin's own `bc-plugin:` origin, a host nonce never given to content scripts, the plugin enabled and the manifest permission per method | Unit tests `plugins::host` (web pages, forged nonce, other plugin, missing permission, disabled) |
| No navigation to plugin pages from the web | `bc-plugin:` loads in a tab or frame only when the browser granted that address (or the tab already shows that plugin) | Unit test `navigation_needs_a_grant` |
| Clipboard history is opt-in and visible | Watcher runs only while an enabled plugin holds `clipboard.history` (high risk, shown in red under the plugin's name) | Manual (drawer screenshot) |
| Secrets stay out of the history | Password manager and authenticator copies are marked and carry `x-kde-passwordManagerHint`; hinted copies from other apps are skipped; temporary-container copies skipped by default | Unit tests `plugins::clipboard_history` |
| History storage private | `profiles/clipboard-history/` `0700`, files `0600`, limits on items, size, item size and age | Unit tests (mode check) |
| Keys plugins take | Browser and editing keys only with `shortcuts.override`, shown in the drawer; conflicts between plugins resolved by order and shown | Unit tests `plugins::shortcuts`, `browser::plugins_host` |

The clipboard history records what other apps copy too. That is its purpose,
and the reason the permission is rated high: only enable such a plugin if
you trust it like the browser itself.

## Automation / MCP (M1b, planned)

The control API will listen only on an owner-only Unix socket, require the
user to enable it, and expose a permission-checked tool set. Vaults and
browser settings are out of its reach.

## User-Agent overrides

BrowseCraft pretends to be another browser on some sites (built-in for Microsoft 365, or the user's choice). It changes only the `User-Agent` header and `navigator.userAgent`, never the engine: a site may send code meant for Chrome. Custom strings are checked to be printable ASCII (no header injection). Overrides are local settings, not sent anywhere.

## Dependencies

`cargo deny` configuration from upstream (`deny.toml`) and the advisory
tracking in [dependencies.md](dependencies.md) remain in force; run
`cargo deny check` locally before a release. New dependencies in M1: `gtk`
0.18 (already in the tree through wry).

## Reporting

Open an issue on `matteomicheletti/browser-craft` without exploit details,
or contact the maintainer privately for anything exploitable.
