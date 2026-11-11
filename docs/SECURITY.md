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

## Automation / MCP (M1b, planned)

The control API will listen only on an owner-only Unix socket, require the
user to enable it, and expose a permission-checked tool set. Vaults and
browser settings are out of its reach.

## Dependencies

`cargo deny` configuration from upstream (`deny.toml`) and the advisory
tracking in [dependencies.md](dependencies.md) remain in force; run
`cargo deny check` locally before a release. New dependencies in M1: `gtk`
0.18 (already in the tree through wry).

## Reporting

Open an issue on `matteomicheletti/browser-craft` without exploit details,
or contact the maintainer privately for anything exploitable.
